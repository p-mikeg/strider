//! A linked image's relocations as the dynamic loader finds them: the last
//! `PT_DYNAMIC`, read at its `p_vaddr` through the `PT_LOAD`s, the last
//! entry of each tag, and the tables those tags place (`DT_RELA`, `DT_REL`,
//! `DT_JMPREL` by `DT_PLTREL`, `DT_CREL`, `DT_RELR`, and Android's packed
//! and RELR ones). No section header takes part.

use object::Object as _;
use object::elf;
use object::read::elf::{FileHeader, ProgramHeader as _};

/// lld's `--pack-dyn-relocs=android` tags.
const DT_ANDROID_REL: u32 = 0x6000_000f;
const DT_ANDROID_RELSZ: u32 = 0x6000_0010;
const DT_ANDROID_RELA: u32 = 0x6000_0011;
const DT_ANDROID_RELASZ: u32 = 0x6000_0012;
const DT_ANDROID_RELR: u32 = 0x6fff_e000;
const DT_ANDROID_RELRSZ: u32 = 0x6fff_e001;
const DT_ANDROID_RELRENT: u32 = 0x6fff_e003;
/// LLVM's compact table; it has no size tag.
const DT_CREL: u32 = 0x4000_0026;
/// The gABI's RELR tags, which `object` does not name.
const DT_RELRSZ: u32 = 35;
const DT_RELR: u32 = 36;
const DT_RELRENT: u32 = 37;

/// The file bytes each `PT_LOAD` maps: `(p_vaddr, p_filesz, p_offset)`.
struct Loads(Vec<(u64, u64, u64)>);

impl Loads {
    fn of<Elf: FileHeader>(f: &object::read::elf::ElfFile<'_, Elf>) -> Self {
        let endian = f.endian();
        Self(
            f.elf_program_headers()
                .iter()
                .filter(|ph| ph.p_type(endian) == elf::PT_LOAD)
                .map(|ph| {
                    (
                        ph.p_vaddr(endian).into(),
                        ph.p_filesz(endian).into(),
                        ph.p_offset(endian).into(),
                    )
                })
                .collect(),
        )
    }

    /// The file bytes from `addr` to the end of the last `PT_LOAD` mapping
    /// it, which the loader maps over any earlier one.
    fn rest<'d>(&self, data: &'d [u8], addr: u64) -> Option<&'d [u8]> {
        let &(vaddr, filesz, offset) = self
            .0
            .iter()
            .rev()
            .find(|&&(vaddr, filesz, _)| vaddr <= addr && addr - vaddr < filesz)?;
        let start = usize::try_from(offset.checked_add(addr - vaddr)?).ok()?;
        let end = usize::try_from(offset.checked_add(filesz)?).ok()?;
        data.get(start..end)
    }

    /// The `len` file bytes at `addr`, all in one `PT_LOAD`.
    fn bytes<'d>(&self, data: &'d [u8], addr: u64, len: u64) -> Option<&'d [u8]> {
        self.rest(data, addr)?.get(..usize::try_from(len).ok()?)
    }

    fn offset(&self, data: &[u8], addr: u64) -> Option<usize> {
        let rest = self.rest(data, addr)?;
        Some(rest.as_ptr() as usize - data.as_ptr() as usize)
    }
}

/// The dynamic array's `(d_tag, d_val)` entries before `DT_NULL`.
pub(crate) struct DynamicTags(Vec<(u64, u64)>);

impl DynamicTags {
    /// The value of the last `want` entry, which is the one every loader
    /// keeps.
    pub(crate) fn get(&self, want: u32) -> Option<u64> {
        self.0
            .iter()
            .rev()
            .find(|&&(t, _)| t == u64::from(want))
            .map(|&(_, v)| v)
    }
}

/// `None` for an image without a `PT_DYNAMIC`, `Some(None)` when the last
/// one's `p_vaddr` lies in no `PT_LOAD`'s file bytes. The array runs to its
/// `DT_NULL`, or to the end of those bytes.
pub(crate) fn dynamic_tags(obj: &object::File<'_>) -> Option<Option<DynamicTags>> {
    match obj {
        object::File::Elf32(f) => dynamic_tags_of(f),
        object::File::Elf64(f) => dynamic_tags_of(f),
        _ => None,
    }
}

fn dynamic_tags_of<Elf: FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
) -> Option<Option<DynamicTags>> {
    let endian = f.endian();
    let vaddr: u64 = f
        .elf_program_headers()
        .iter()
        .rev()
        .find(|ph| ph.p_type(endian) == elf::PT_DYNAMIC)?
        .p_vaddr(endian)
        .into();
    let Some(bytes) = Loads::of(f).rest(f.data(), vaddr) else {
        return Some(None);
    };
    let le = f.is_little_endian();
    let word = if f.is_64() { 8 } else { 4 };
    let read = |b: &[u8]| -> u64 {
        b.iter().enumerate().fold(0, |acc, (i, &x)| {
            if le {
                acc | (u64::from(x) << (8 * i))
            } else {
                (acc << 8) | u64::from(x)
            }
        })
    };
    Some(Some(DynamicTags(
        bytes
            .chunks_exact(2 * word)
            .map(|d| (read(&d[..word]), read(&d[word..])))
            .take_while(|&(t, _)| t != u64::from(elf::DT_NULL))
            .collect(),
    )))
}

/// A relocation table the tags place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Table {
    /// `SHT_REL` / `SHT_RELA` entries.
    Plain { sh_type: u32, addr: u64, size: u64 },
    /// A CREL stream, which carries its own count.
    Crel { addr: u64 },
    /// An Android `APS2` stream of `size` bytes, `RELA` or `REL`.
    Android { rela: bool, addr: u64, size: u64 },
}

/// What the loader reads to relocate a linked image.
pub(crate) enum LoaderView {
    /// No `PT_DYNAMIC`, or an object file: the section headers' tables.
    NotDynamic,
    /// The tables the tags place, as an ELF of their own that `object`
    /// walks as a dynamic table: [`synthesized`].
    Tables { walked: Option<Vec<u8>> },
    /// A table the tags place that cannot be read or decoded, or an array
    /// that cannot be: what it relocates is unknown.
    Unreadable(&'static str),
    /// The tables name more entries than the budget allows.
    OverBudget { entries: u64, budget: u64 },
}

/// The loader's view of `obj`, see [`LoaderView`].
pub(crate) fn loader_view(obj: &object::File<'_>) -> LoaderView {
    if obj.kind() == object::ObjectKind::Relocatable {
        return LoaderView::NotDynamic;
    }
    match obj {
        object::File::Elf32(f) => loader_view_of(f),
        object::File::Elf64(f) => loader_view_of(f),
        _ => LoaderView::NotDynamic,
    }
}

fn loader_view_of<Elf: FileHeader>(f: &object::read::elf::ElfFile<'_, Elf>) -> LoaderView {
    let tags = match dynamic_tags_of(f) {
        None => return LoaderView::NotDynamic,
        Some(None) => return LoaderView::Unreadable("the dynamic array is in no PT_LOAD"),
        Some(Some(tags)) => tags,
    };
    let data = f.data();
    let loads = Loads::of(f);
    let is_64 = f.is_64();
    let (rel_ent, rela_ent, word) = if is_64 { (16, 24, 8) } else { (8, 12, 4) };
    let mut tables = Vec::new();
    // `(table, size, entry size)` tags of each fixed-entry table.
    let plain = [
        (
            elf::DT_RELA,
            elf::DT_RELASZ,
            Some(elf::DT_RELAENT),
            elf::SHT_RELA,
        ),
        (
            elf::DT_REL,
            elf::DT_RELSZ,
            Some(elf::DT_RELENT),
            elf::SHT_REL,
        ),
    ];
    for (at, size, ent, sh_type) in plain {
        let Some(addr) = tags.get(at) else {
            continue;
        };
        let want = if sh_type == elf::SHT_RELA {
            rela_ent
        } else {
            rel_ent
        };
        let Some(size) = tags.get(size) else {
            return LoaderView::Unreadable("a relocation table has no size tag");
        };
        if ent.and_then(|t| tags.get(t)).is_some_and(|e| e != want) {
            return LoaderView::Unreadable("a relocation table's entry size is not its type's");
        }
        tables.push((
            Table::Plain {
                sh_type,
                addr,
                size,
            },
            want,
        ));
    }
    if let Some(addr) = tags.get(elf::DT_JMPREL) {
        let (Some(size), Some(kind)) = (tags.get(elf::DT_PLTRELSZ), tags.get(elf::DT_PLTREL))
        else {
            return LoaderView::Unreadable("DT_JMPREL has no DT_PLTRELSZ or DT_PLTREL");
        };
        let (sh_type, want) = match kind {
            k if k == u64::from(elf::DT_RELA) => (elf::SHT_RELA, rela_ent),
            k if k == u64::from(elf::DT_REL) => (elf::SHT_REL, rel_ent),
            _ => return LoaderView::Unreadable("DT_PLTREL names neither DT_REL nor DT_RELA"),
        };
        tables.push((
            Table::Plain {
                sh_type,
                addr,
                size,
            },
            want,
        ));
    }
    if let Some(addr) = tags.get(DT_CREL) {
        tables.push((Table::Crel { addr }, 1));
    }
    for (at, size, rela) in [
        (DT_ANDROID_RELA, DT_ANDROID_RELASZ, true),
        (DT_ANDROID_REL, DT_ANDROID_RELSZ, false),
    ] {
        let Some(addr) = tags.get(at) else {
            continue;
        };
        let Some(size) = tags.get(size) else {
            return LoaderView::Unreadable("an Android packed table has no size tag");
        };
        tables.push((Table::Android { rela, addr, size }, 1));
    }
    // RELR words hold `B + A`, `B` modelled 0, as the file has them: a
    // table is only checked to be there.
    for (at, size, ent) in [
        (DT_RELR, DT_RELRSZ, DT_RELRENT),
        (DT_ANDROID_RELR, DT_ANDROID_RELRSZ, DT_ANDROID_RELRENT),
    ] {
        let Some(addr) = tags.get(at) else {
            continue;
        };
        let Some(size) = tags.get(size) else {
            return LoaderView::Unreadable("a RELR table has no size tag");
        };
        if tags.get(ent).is_some_and(|e| e != word) || size % word != 0 {
            return LoaderView::Unreadable("a RELR table's entry size is not a word");
        }
        if loads.bytes(data, addr, size).is_none() {
            return LoaderView::Unreadable("a RELR table is in no PT_LOAD");
        }
    }
    let android_mips64el = f.elf_header().is_mips64el(f.endian());
    let budget =
        |smallest: u64| super::relocations::RELOCATION_ENTRY_BUDGET * data.len() as u64 / smallest;
    let mut decoded = Decoded::default();
    for &(table, ent) in &tables {
        let ok = match table {
            Table::Plain { addr, size, .. } => {
                size % ent == 0 && (size == 0 || loads.bytes(data, addr, size).is_some())
            }
            Table::Crel { addr } => crel_count(loads.rest(data, addr)).is_some_and(|n| {
                decoded.crel = n;
                true
            }),
            Table::Android { rela, addr, size } => {
                match (!android_mips64el)
                    .then(|| loads.bytes(data, addr, size))
                    .flatten()
                    .and_then(|b| android_entries(b, rela, is_64, budget(1)))
                {
                    Some(Ok(e)) => {
                        decoded.android.push((table, e));
                        true
                    }
                    Some(Err(count)) => {
                        return LoaderView::OverBudget {
                            entries: count,
                            budget: budget(1),
                        };
                    }
                    None => false,
                }
            }
        };
        if !ok {
            return LoaderView::Unreadable("a relocation table cannot be read or decoded");
        }
    }
    let entries: u64 = tables
        .iter()
        .map(|&(table, ent)| match table {
            Table::Plain { size, .. } => size / ent,
            Table::Crel { .. } => decoded.crel,
            Table::Android { .. } => 0,
        })
        .sum::<u64>()
        + decoded
            .android
            .iter()
            .map(|(_, e)| e.len() as u64)
            .sum::<u64>();
    let smallest = tables.iter().map(|&(_, ent)| ent).min().unwrap_or(rel_ent);
    if entries > budget(smallest) {
        return LoaderView::OverBudget {
            entries,
            budget: budget(smallest),
        };
    }
    LoaderView::Tables {
        walked: (!tables.is_empty()).then(|| synthesized(f, &loads, &tables, &decoded)),
    }
}

/// One decoded relocation: `(r_offset, r_info, r_addend)`.
type Entry = (u64, u64, i64);

/// What validating the tables decoded, kept for the synthesis.
#[derive(Default)]
struct Decoded {
    crel: u64,
    android: Vec<(Table, Vec<Entry>)>,
}

/// How many entries a CREL stream at the start of `data` holds, `None`
/// when it does not decode.
fn crel_count(data: Option<&[u8]>) -> Option<u64> {
    let iter = object::read::elf::CrelIterator::new(data?).ok()?;
    let mut n = 0u64;
    for e in iter {
        e.ok()?;
        n += 1;
    }
    Some(n)
}

/// `(r_offset, r_info, r_addend)` of an Android `APS2` stream, `None` when
/// it does not decode (bionic's `packed_reloc_iterator`): a relocation
/// count and a start offset, then groups of a size and flags, each field
/// a SLEB128 the group may fix for all its members. `Err(count)` when the
/// count passes `limit`, decoding nothing: a group fixing every field
/// costs no stream byte per member.
fn android_entries(
    data: &[u8],
    rela: bool,
    is_64: bool,
    limit: u64,
) -> Option<Result<Vec<Entry>, u64>> {
    const BY_INFO: i64 = 1;
    const BY_OFFSET_DELTA: i64 = 2;
    const BY_ADDEND: i64 = 4;
    const HAS_ADDEND: i64 = 8;
    let mut stream = data.strip_prefix(b"APS2")?;
    let mut next = || -> Option<i64> {
        let (mut value, mut shift) = (0i64, 0u32);
        loop {
            let (&byte, rest) = stream.split_first()?;
            stream = rest;
            if shift < 64 {
                value |= i64::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    value |= -1i64 << shift;
                }
                return Some(value);
            }
        }
    };
    let count = u64::try_from(next()?).ok()?;
    if count > limit {
        return Some(Err(count));
    }
    let mut out = Vec::new();
    let mut offset = next()? as u64;
    let (mut info, mut addend) = (0u64, 0i64);
    let narrow = |v: u64| if is_64 { v } else { v & 0xffff_ffff };
    while (out.len() as u64) < count {
        let size = u64::try_from(next()?).ok().filter(|&n| n != 0)?;
        let flags = next()?;
        let delta = if flags & BY_OFFSET_DELTA != 0 {
            Some(next()? as u64)
        } else {
            None
        };
        if flags & BY_INFO != 0 {
            info = next()? as u64;
        }
        if flags & HAS_ADDEND != 0 && !rela {
            return None;
        }
        if flags & HAS_ADDEND != 0 && flags & BY_ADDEND != 0 {
            addend = addend.wrapping_add(next()?);
        } else if flags & HAS_ADDEND == 0 {
            addend = 0;
        }
        let members = size.min(count - out.len() as u64);
        for _ in 0..members {
            offset = offset.wrapping_add(match delta {
                Some(d) => d,
                None => next()? as u64,
            });
            if flags & BY_INFO == 0 {
                info = next()? as u64;
            }
            if flags & HAS_ADDEND != 0 && flags & BY_ADDEND == 0 {
                addend = addend.wrapping_add(next()?);
            }
            out.push((narrow(offset), narrow(info), addend));
        }
    }
    Some(Ok(out))
}

/// `ranges` sorted, those that overlap merged: two tables over the same
/// entries are walked once, adjacent ones each as it lies.
fn merged_overlapping(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (lo, hi) in ranges {
        match out.last_mut() {
            Some(last) if lo < last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

/// An ELF holding `f`'s file header and one section header per table,
/// `REL` / `RELA` extents of one type merged where they overlap, a CREL
/// stream as it lies, and an Android table re-encoded as `RELA` / `REL`:
/// linked to no symbol table, so `object` walks each as a dynamic table.
fn synthesized<Elf: FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
    loads: &Loads,
    tables: &[(Table, u64)],
    decoded: &Decoded,
) -> Vec<u8> {
    let data = f.data();
    let le = f.is_little_endian();
    let is_64 = f.is_64();
    let put = |out: &mut Vec<u8>, v: u64, n: usize| {
        if le {
            out.extend_from_slice(&v.to_le_bytes()[..n]);
        } else {
            out.extend_from_slice(&v.to_be_bytes()[8 - n..]);
        }
    };
    let word = if is_64 { 8 } else { 4 };
    let (ehsize, shentsize) = if is_64 { (64, 64) } else { (52, 40) };
    let mut out = data[..ehsize].to_vec();
    // Section header fields: (sh_type, sh_addr, file bytes, sh_entsize),
    // after the empty names table `object` asks for.
    let mut sections: Vec<(u32, u64, Vec<u8>, u64)> = vec![(elf::SHT_STRTAB, 0, vec![0], 0)];
    for sh_type in [elf::SHT_RELA, elf::SHT_REL] {
        let extents = merged_overlapping(
            tables
                .iter()
                .filter_map(|&(t, _)| match t {
                    Table::Plain {
                        sh_type: ty,
                        addr,
                        size,
                    } if ty == sh_type && size != 0 => Some((addr, addr + size)),
                    _ => None,
                })
                .collect(),
        );
        let ent = tables
            .iter()
            .find(|&&(t, _)| matches!(t, Table::Plain { sh_type: ty, .. } if ty == sh_type))
            .map_or(0, |&(_, ent)| ent);
        for (lo, hi) in extents {
            // Overlapping tables lie in one mapping, each having been read.
            if let Some(bytes) = loads.bytes(data, lo, hi - lo) {
                sections.push((sh_type, lo, bytes.to_vec(), ent));
            }
        }
    }
    for &(t, _) in tables {
        if let Table::Crel { addr } = t
            && let Some(bytes) = loads.rest(data, addr)
        {
            sections.push((elf::SHT_CREL, addr, bytes.to_vec(), 1));
        }
    }
    for (t, entries) in &decoded.android {
        let Table::Android { rela, addr, .. } = *t else {
            continue;
        };
        let mut bytes = Vec::new();
        for &(offset, info, addend) in entries {
            put(&mut bytes, offset, word);
            put(&mut bytes, info, word);
            if rela {
                put(&mut bytes, addend as u64, word);
            }
        }
        let (sh_type, ent) = if rela {
            (elf::SHT_RELA, 3 * word as u64)
        } else {
            (elf::SHT_REL, 2 * word as u64)
        };
        sections.push((sh_type, addr, bytes, ent));
    }
    let mut headers = vec![0u8; shentsize];
    for (sh_type, addr, bytes, ent) in sections {
        out.resize(out.len().next_multiple_of(8), 0);
        let offset = out.len() as u64;
        let size = bytes.len() as u64;
        out.extend_from_slice(&bytes);
        let alloc = if sh_type == elf::SHT_STRTAB {
            0
        } else {
            u64::from(elf::SHF_ALLOC)
        };
        let h = &mut headers;
        put(h, 0, 4);
        put(h, u64::from(sh_type), 4);
        put(h, alloc, word);
        put(h, addr, word);
        put(h, offset, word);
        put(h, size, word);
        put(h, 0, 4);
        put(h, 0, 4);
        put(h, 8, word);
        put(h, ent, word);
    }
    out.resize(out.len().next_multiple_of(8), 0);
    let shoff = out.len() as u64;
    let shnum = (headers.len() / shentsize) as u64;
    out.extend_from_slice(&headers);
    // e_phoff, e_shoff, e_phnum, e_shentsize, e_shnum, e_shstrndx.
    let (phoff_at, shoff_at, phnum_at) = if is_64 {
        (0x20, 0x28, 0x38)
    } else {
        (0x1c, 0x20, 0x2c)
    };
    let mut set = |at: usize, v: u64, n: usize| {
        let mut b = Vec::new();
        put(&mut b, v, n);
        out[at..at + n].copy_from_slice(&b);
    };
    set(phoff_at, 0, word);
    set(shoff_at, shoff, word);
    set(phnum_at, 0, 2);
    set(phnum_at + 2, shentsize as u64, 2);
    set(phnum_at + 4, shnum, 2);
    set(phnum_at + 6, 1, 2);
    out
}

/// Whether the section header `object` takes for `.dynsym` holds the table
/// `DT_SYMTAB` places: its address, and the file bytes the `PT_LOAD`
/// mapping that address holds. Otherwise a relocation's symbol index names
/// a table the loader does not read, and no symbol resolves.
pub(crate) fn dynsym_is_the_loaders(obj: &object::File<'_>) -> bool {
    fn of<Elf: FileHeader>(f: &object::read::elf::ElfFile<'_, Elf>) -> bool {
        use object::read::elf::SectionHeader as _;
        let endian = f.endian();
        let Some(Some(tags)) = dynamic_tags_of(f) else {
            return false;
        };
        let (Some(symtab), Ok(sh)) = (
            tags.get(elf::DT_SYMTAB),
            f.elf_section_table()
                .section(f.elf_dynamic_symbol_table().section()),
        ) else {
            return false;
        };
        let addr: u64 = sh.sh_addr(endian).into();
        let offset: u64 = sh.sh_offset(endian).into();
        f.elf_dynamic_symbol_table().section().0 != 0
            && addr == symtab
            && Loads::of(f).offset(f.data(), symtab) == usize::try_from(offset).ok()
    }
    match obj {
        object::File::Elf32(f) => of(f),
        object::File::Elf64(f) => of(f),
        _ => false,
    }
}
