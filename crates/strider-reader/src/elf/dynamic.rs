//! A linked image's relocations as the dynamic loader finds them: the last
//! `PT_DYNAMIC`, read at its `p_vaddr` through the `PT_LOAD`s, the last
//! entry of each tag, and the tables those tags place (`DT_RELA`, `DT_REL`,
//! `DT_JMPREL` by `DT_PLTREL`, `DT_CREL`, `DT_RELR`, and Android's packed
//! and RELR ones). No section header takes part.
//!
//! Each table is walked as it lies; an entry two tables share, at one file
//! address in one entry layout, is walked once. GNU ld's ppc32 `DT_RELA`
//! spans `DT_JMPREL`, and a `REL` entry walked twice would stack its
//! implicit addend.
//!
//! Deliberate choices where loaders differ:
//!
//! * bionic, the one loader of the `DT_ANDROID_*` tags, takes the FIRST
//!   `PT_DYNAMIC`; glibc and musl, and this view, the last;
//! * glibc ignores `DT_REL` on a RELA-only arch and `DT_RELA` on a REL-only
//!   one; musl, and this view, apply both;
//! * no production loader reads `DT_CREL`, which is walked all the same;
//! * glibc's arm and aarch64 ports apply the first `DT_RELCOUNT` /
//!   `DT_RELACOUNT` entries as RELATIVE whatever their type; this view
//!   applies each entry by its own type.

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

    /// The file offset of `[addr, addr + len)` when one `PT_LOAD` maps all
    /// of it, the last mapping `addr`, and no later one overlaps it.
    fn whole(&self, data: &[u8], addr: u64, len: u64) -> Option<usize> {
        let end = addr.checked_add(len)?;
        let at = self
            .0
            .iter()
            .rposition(|&(vaddr, filesz, _)| vaddr <= addr && addr - vaddr < filesz)?;
        let later = self.0[at + 1..]
            .iter()
            .any(|&(vaddr, filesz, _)| vaddr < end && addr < vaddr.saturating_add(filesz));
        let bytes = self.bytes(data, addr, len)?;
        (!later).then(|| bytes.as_ptr() as usize - data.as_ptr() as usize)
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
    let entries: Vec<(u64, u64)> = bytes
        .chunks_exact(2 * word)
        .map(|d| (read(&d[..word]), read(&d[word..])))
        .take_while(|&(t, _)| t != u64::from(elf::DT_NULL))
        .collect();
    Some(Some(DynamicTags(entries)))
}

/// A relocation table the tags place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Table {
    /// `SHT_REL` / `SHT_RELA` entries of `ent` bytes.
    Plain {
        sh_type: u32,
        addr: u64,
        size: u64,
        ent: u64,
    },
    /// A CREL stream, which carries its own count; it runs to the end of
    /// its mapping, as far as the extent goes.
    Crel { addr: u64 },
    /// An Android `APS2` stream of `size` bytes, `RELA` or `REL`.
    Android { rela: bool, addr: u64, size: u64 },
}

/// What the loader reads to relocate a linked image.
pub(crate) enum LoaderView {
    /// No `PT_DYNAMIC`, or an object file: the section headers' tables.
    NotDynamic,
    /// The tables the tags place, as an ELF of their own that `object`
    /// walks as a dynamic table ([`synthesized`]).
    Tables { walked: Option<Vec<u8>> },
    /// A table the tags place that cannot be read or decoded, or an array
    /// that cannot be: what it relocates is unknown.
    Unreadable(&'static str),
    /// The tables name more entries than the budget allows.
    OverBudget { entries: u64, budget: u64 },
}

impl LoaderView {
    /// Whether what the tags relocate is unknown.
    pub(crate) fn is_unread(&self) -> bool {
        matches!(
            self,
            LoaderView::Unreadable(_) | LoaderView::OverBudget { .. }
        )
    }
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

/// A table and its relocations.
struct Walked<'d> {
    table: Table,
    /// A plain table's entries: `(file address, bytes)` each.
    raw: Vec<(u64, &'d [u8])>,
    /// A CREL table's bytes; an Android table's entries.
    crel: Option<&'d [u8]>,
    android: Vec<Entry>,
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
    let plain = [
        (elf::DT_RELA, elf::DT_RELASZ, elf::DT_RELAENT, elf::SHT_RELA),
        (elf::DT_REL, elf::DT_RELSZ, elf::DT_RELENT, elf::SHT_REL),
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
        if tags.get(ent).is_some_and(|e| e != want) {
            return LoaderView::Unreadable("a relocation table's entry size is not its type's");
        }
        tables.push(Table::Plain {
            sh_type,
            addr,
            size,
            ent: want,
        });
    }
    if let Some(addr) = tags.get(elf::DT_JMPREL) {
        let (Some(size), Some(kind)) = (tags.get(elf::DT_PLTRELSZ), tags.get(elf::DT_PLTREL))
        else {
            return LoaderView::Unreadable("DT_JMPREL has no DT_PLTRELSZ or DT_PLTREL");
        };
        let (sh_type, ent) = match kind {
            k if k == u64::from(elf::DT_RELA) => (elf::SHT_RELA, rela_ent),
            k if k == u64::from(elf::DT_REL) => (elf::SHT_REL, rel_ent),
            _ => return LoaderView::Unreadable("DT_PLTREL names neither DT_REL nor DT_RELA"),
        };
        tables.push(Table::Plain {
            sh_type,
            addr,
            size,
            ent,
        });
    }
    if let Some(addr) = tags.get(DT_CREL) {
        tables.push(Table::Crel { addr });
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
        tables.push(Table::Android { rela, addr, size });
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
    // The entries counted against the budget at the smallest entry size of
    // any table, an Android one's being its plain twin's.
    let smallest = tables
        .iter()
        .map(|t| match *t {
            Table::Plain { ent, .. } => ent,
            Table::Crel { .. } => 1,
            Table::Android { rela, .. } => {
                if rela {
                    rela_ent
                } else {
                    rel_ent
                }
            }
        })
        .min()
        .unwrap_or(rel_ent);
    let budget = super::relocations::RELOCATION_ENTRY_BUDGET * data.len() as u64 / smallest;
    let mut entries = 0u64;
    let mut walked = Vec::new();
    for &table in &tables {
        let mut w = Walked {
            table,
            raw: Vec::new(),
            crel: None,
            android: Vec::new(),
        };
        let ok = match table {
            Table::Plain {
                addr, size, ent, ..
            } => {
                if size % ent != 0 {
                    false
                } else if size == 0 {
                    true
                } else if let Some(bytes) = loads.bytes(data, addr, size) {
                    entries += size / ent;
                    for (i, e) in bytes.chunks_exact(ent as usize).enumerate() {
                        w.raw.push((addr + i as u64 * ent, e));
                    }
                    true
                } else {
                    false
                }
            }
            Table::Crel { addr } => match loads.rest(data, addr).and_then(crel_entries) {
                Some(found) => {
                    entries += found.len() as u64;
                    w.crel = loads.rest(data, addr);
                    true
                }
                None => false,
            },
            Table::Android { rela, addr, size } => {
                let limit = budget.saturating_sub(entries);
                match (!f.elf_header().is_mips64el(f.endian()))
                    .then(|| loads.bytes(data, addr, size))
                    .flatten()
                    .and_then(|b| android_entries(b, rela, is_64, limit))
                {
                    Some(Ok(found)) => {
                        entries += found.len() as u64;
                        w.android = found;
                        true
                    }
                    Some(Err(count)) => {
                        return LoaderView::OverBudget {
                            entries: entries.saturating_add(count),
                            budget,
                        };
                    }
                    None => false,
                }
            }
        };
        if !ok {
            return LoaderView::Unreadable("a relocation table cannot be read or decoded");
        }
        if entries > budget {
            return LoaderView::OverBudget { entries, budget };
        }
        walked.push(w);
    }
    LoaderView::Tables {
        walked: (!walked.is_empty()).then(|| synthesized(f, &walked)),
    }
}

fn read_word(b: &[u8], le: bool) -> u64 {
    b.iter().enumerate().fold(0, |acc, (i, &x)| {
        if le {
            acc | (u64::from(x) << (8 * i))
        } else {
            (acc << 8) | u64::from(x)
        }
    })
}

fn put_word(b: &mut [u8], v: u64, le: bool) {
    let n = b.len();
    if le {
        b.copy_from_slice(&v.to_le_bytes()[..n]);
    } else {
        b.copy_from_slice(&v.to_be_bytes()[8 - n..]);
    }
}

/// `(r_offset, r_type)` of each entry of the CREL stream at the start of
/// `data`, `None` when it does not decode.
fn crel_entries(data: &[u8]) -> Option<Vec<(u64, u32)>> {
    object::read::elf::CrelIterator::new(data)
        .ok()?
        .map(|e| e.ok().map(|e| (e.r_offset, e.r_type)))
        .collect()
}

/// One decoded relocation: `(r_offset, r_info, r_addend)`.
type Entry = (u64, u64, i64);

/// `(r_offset, r_info, r_addend)` of an Android `APS2` stream, `None` when
/// it does not decode (bionic's `packed_reloc_iterator`): a relocation
/// count and a start offset, then groups of a size and flags, each field
/// a SLEB128 the group may fix for all its members, read at the image's
/// word width as bionic's `size_t` reads it. `Err(count)` when the count
/// passes `limit`, decoding nothing.
fn android_entries(
    data: &[u8],
    rela: bool,
    is_64: bool,
    limit: u64,
) -> Option<Result<Vec<Entry>, u64>> {
    const BY_INFO: u64 = 1;
    const BY_OFFSET_DELTA: u64 = 2;
    const BY_ADDEND: u64 = 4;
    const HAS_ADDEND: u64 = 8;
    // bionic's `size_t`: on ELF32 a field keeps its low 32 bits.
    let mask = if is_64 { u64::MAX } else { 0xffff_ffff };
    let mut stream = data.strip_prefix(b"APS2")?;
    let mut next = || -> Option<u64> {
        let (mut value, mut shift) = (0u64, 0u32);
        loop {
            let (&byte, rest) = stream.split_first()?;
            stream = rest;
            if shift < 64 {
                value |= u64::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    value |= u64::MAX << shift;
                }
                return Some(value & mask);
            }
        }
    };
    let count = next()?;
    if count > limit {
        return Some(Err(count));
    }
    let mut out = Vec::new();
    let mut offset = next()?;
    let (mut info, mut addend) = (0u64, 0u64);
    while (out.len() as u64) < count {
        let size = next().filter(|&n| n != 0)?;
        let flags = next()?;
        let delta = if flags & BY_OFFSET_DELTA != 0 {
            Some(next()?)
        } else {
            None
        };
        if flags & BY_INFO != 0 {
            info = next()?;
        }
        if flags & HAS_ADDEND != 0 && !rela {
            return None;
        }
        if flags & HAS_ADDEND != 0 && flags & BY_ADDEND != 0 {
            addend = addend.wrapping_add(next()?) & mask;
        } else if flags & HAS_ADDEND == 0 {
            addend = 0;
        }
        for _ in 0..size.min(count - out.len() as u64) {
            offset = offset.wrapping_add(match delta {
                Some(d) => d,
                None => next()?,
            }) & mask;
            if flags & BY_INFO == 0 {
                info = next()?;
            }
            if flags & HAS_ADDEND != 0 && flags & BY_ADDEND == 0 {
                addend = addend.wrapping_add(next()?) & mask;
            }
            out.push((offset, info, addend as i64));
        }
    }
    Some(Ok(out))
}

/// An ELF holding `f`'s file header and one section header per table that
/// is walked: a plain table's entries no earlier table holds, a CREL stream
/// as it lies, and an Android table re-encoded as `RELA` / `REL`. Linked to
/// no symbol table, so `object` walks each as a dynamic table.
fn synthesized<Elf: FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
    walked: &[Walked<'_>],
) -> Vec<u8> {
    let data = f.data();
    let le = f.is_little_endian();
    let is_64 = f.is_64();
    let put = |out: &mut Vec<u8>, v: u64, n: usize| {
        let mut b = vec![0u8; n];
        put_word(&mut b, v, le);
        out.extend_from_slice(&b);
    };
    let word = if is_64 { 8 } else { 4 };
    let (ehsize, shentsize) = if is_64 { (64, 64) } else { (52, 40) };
    let mut out = data[..ehsize].to_vec();
    // Section header fields: (sh_type, sh_addr, file bytes, sh_entsize),
    // after the empty names table `object` asks for.
    let mut sections: Vec<(u32, u64, Vec<u8>, u64)> = vec![(elf::SHT_STRTAB, 0, vec![0], 0)];
    let mut emitted = std::collections::HashSet::new();
    for w in walked {
        match w.table {
            Table::Plain {
                sh_type, addr, ent, ..
            } => {
                let mut bytes = Vec::new();
                for &(at, raw) in &w.raw {
                    if emitted.insert(at) {
                        bytes.extend_from_slice(raw);
                    }
                }
                if !bytes.is_empty() {
                    sections.push((sh_type, addr, bytes, ent));
                }
            }
            Table::Crel { addr } => {
                if let Some(bytes) = w.crel {
                    sections.push((elf::SHT_CREL, addr, bytes.to_vec(), 1));
                }
            }
            Table::Android { rela, addr, .. } => {
                if w.android.is_empty() {
                    continue;
                }
                let mut bytes = Vec::new();
                for &(offset, info, addend) in &w.android {
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
        }
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
    let mut set = |at: usize, v: u64, n: usize| put_word(&mut out[at..at + n], v, le);
    set(phoff_at, 0, word);
    set(shoff_at, shoff, word);
    set(phnum_at, 0, 2);
    set(phnum_at + 2, shentsize as u64, 2);
    set(phnum_at + 4, shnum, 2);
    set(phnum_at + 6, 1, 2);
    out
}

/// Whether the section header `object` takes for `.dynsym` holds the table
/// `DT_SYMTAB` places, every symbol of it: its address, and the file bytes
/// of the one `PT_LOAD` mapping all of it, which no later one overlaps.
/// The table runs to the symbol count `DT_HASH` or `DT_GNU_HASH` gives, and
/// to the header's `sh_size`, whichever is more. Otherwise a relocation's
/// symbol index names a table the loader does not read, and no symbol
/// resolves.
pub(crate) fn dynsym_is_the_loaders(obj: &object::File<'_>) -> bool {
    fn of<Elf: FileHeader>(f: &object::read::elf::ElfFile<'_, Elf>) -> bool {
        use object::read::elf::SectionHeader as _;
        let endian = f.endian();
        let Some(Some(tags)) = dynamic_tags_of(f) else {
            return false;
        };
        let index = f.elf_dynamic_symbol_table().section();
        let (Some(symtab), Ok(sh)) = (
            tags.get(elf::DT_SYMTAB),
            f.elf_section_table().section(index),
        ) else {
            return false;
        };
        let data = f.data();
        let loads = Loads::of(f);
        let le = f.is_little_endian();
        let ent: u64 = if f.is_64() { 24 } else { 16 };
        let addr: u64 = sh.sh_addr(endian).into();
        let offset: u64 = sh.sh_offset(endian).into();
        let size: u64 = sh.sh_size(endian).into();
        let count = symbol_count(&tags, &loads, data, le, f.is_64()).unwrap_or(0);
        let len = size.max(count.saturating_mul(ent));
        index.0 != 0
            && addr == symtab
            && loads.whole(data, symtab, len) == usize::try_from(offset).ok()
    }
    match obj {
        object::File::Elf32(f) => of(f),
        object::File::Elf64(f) => of(f),
        _ => false,
    }
}

/// The symbol count `DT_HASH`'s `nchain` gives, else the one past the last
/// `DT_GNU_HASH` chain's end; `None` without either, or one that does not
/// read.
fn symbol_count(
    tags: &DynamicTags,
    loads: &Loads,
    data: &[u8],
    le: bool,
    is_64: bool,
) -> Option<u64> {
    let u32_at = |addr: u64| -> Option<u64> { Some(read_word(loads.bytes(data, addr, 4)?, le)) };
    if let Some(hash) = tags.get(elf::DT_HASH) {
        return u32_at(hash.checked_add(4)?);
    }
    let gnu = tags.get(elf::DT_GNU_HASH)?;
    let (nbuckets, symoffset, bloom) = (u32_at(gnu)?, u32_at(gnu + 4)?, u32_at(gnu + 8)?);
    let buckets = gnu + 16 + bloom * if is_64 { 8 } else { 4 };
    let mut last = 0;
    for i in 0..nbuckets {
        last = last.max(u32_at(buckets + 4 * i)?);
    }
    if last < symoffset {
        return Some(symoffset);
    }
    let chains = buckets + 4 * nbuckets;
    loop {
        if u32_at(chains + 4 * (last - symoffset))? & 1 != 0 {
            return Some(last + 1);
        }
        last += 1;
    }
}
