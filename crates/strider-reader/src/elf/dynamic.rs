//! A linked image's relocations as the dynamic loader finds them: the last
//! `PT_DYNAMIC` at its `p_vaddr`, the last entry of each tag, and the tables
//! those tags place. No section header places a table; symbols still come from
//! `.dynsym`'s section header. A plain-table entry an earlier table already
//! holds is walked once (ppc32 `DT_RELA` spans `DT_JMPREL`).
//!
//! Where loaders differ, it takes the last `PT_DYNAMIC` (bionic: the first),
//! applies both `DT_REL` and `DT_RELA` (glibc: the arch's only), applies each
//! entry by its own type (glibc: the first `DT_REL[A]COUNT` as RELATIVE), and
//! reads `DT_CREL`, which no production loader does.

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
}

/// The dynamic array's `(d_tag, d_val)` entries before `DT_NULL`.
pub(crate) struct DynamicTags(Vec<(u64, u64)>);

impl DynamicTags {
    /// The value of the last `want` entry, as every loader keeps.
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
    let read = |b: &[u8]| crate::bytes::read_uint(b, le);
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
    /// A CREL stream, which carries its own count; its extent is the rest
    /// of its mapping.
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
    /// The dynamic array or a table it places cannot be read or decoded:
    /// what it relocates is unknown.
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

/// A placed table, read.
enum Walked<'d> {
    /// `SHT_REL` / `SHT_RELA` entries of `ent` bytes, each `(address, bytes)`.
    Plain {
        sh_type: u32,
        addr: u64,
        ent: u64,
        entries: Vec<(u64, &'d [u8])>,
    },
    /// A CREL stream's bytes, to the end of its mapping.
    Crel { addr: u64, bytes: &'d [u8] },
    /// An Android table's entries.
    Android {
        rela: bool,
        addr: u64,
        entries: Vec<Entry>,
    },
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
    let (rel_ent, rela_ent) = entry_sizes(is_64);
    let tables = match placed_tables(&tags, &loads, data, is_64) {
        Ok(tables) => tables,
        Err(why) => return LoaderView::Unreadable(why),
    };
    // The budget is counted at the smallest entry size of any table.
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
    for table in tables {
        let read = match table {
            Table::Plain {
                sh_type,
                addr,
                size,
                ent,
            } => {
                let bytes = match size {
                    _ if size % ent != 0 => None,
                    0 => Some(&[][..]),
                    _ => loads.bytes(data, addr, size),
                };
                bytes.map(|bytes| {
                    entries += size / ent;
                    let entries = bytes.chunks_exact(ent as usize).enumerate();
                    Walked::Plain {
                        sh_type,
                        addr,
                        ent,
                        entries: entries
                            .map(|(i, e)| (addr.wrapping_add(i as u64 * ent), e))
                            .collect(),
                    }
                })
            }
            Table::Crel { addr } => loads.rest(data, addr).and_then(|bytes| {
                entries += crel_count(bytes)?;
                Some(Walked::Crel { addr, bytes })
            }),
            Table::Android { rela, addr, size } => {
                let limit = budget.saturating_sub(entries);
                match (!f.elf_header().is_mips64el(f.endian()))
                    .then(|| loads.bytes(data, addr, size))
                    .flatten()
                    .and_then(|b| android_entries(b, rela, is_64, limit))
                {
                    Some(Ok(found)) => {
                        entries += found.len() as u64;
                        Some(Walked::Android {
                            rela,
                            addr,
                            entries: found,
                        })
                    }
                    Some(Err(count)) => {
                        return LoaderView::OverBudget {
                            entries: entries.saturating_add(count),
                            budget,
                        };
                    }
                    None => None,
                }
            }
        };
        let Some(read) = read else {
            return LoaderView::Unreadable("a relocation table cannot be read or decoded");
        };
        if entries > budget {
            return LoaderView::OverBudget { entries, budget };
        }
        walked.push(read);
    }
    LoaderView::Tables {
        walked: (!walked.is_empty()).then(|| synthesized(f, &walked)),
    }
}

/// `(SHT_REL, SHT_RELA)` entry sizes.
fn entry_sizes(is_64: bool) -> (u64, u64) {
    if is_64 { (16, 24) } else { (8, 12) }
}

/// The relocation tables `tags` place, or why they cannot be read. A RELR
/// table is only checked to be there: its words hold `B + A`, `B` modelled
/// 0, as the file has them.
fn placed_tables(
    tags: &DynamicTags,
    loads: &Loads,
    data: &[u8],
    is_64: bool,
) -> Result<Vec<Table>, &'static str> {
    let (rel_ent, rela_ent) = entry_sizes(is_64);
    let word = if is_64 { 8 } else { 4 };
    let mut tables = Vec::new();
    for (at, size, ent, sh_type, want) in [
        (
            elf::DT_RELA,
            elf::DT_RELASZ,
            elf::DT_RELAENT,
            elf::SHT_RELA,
            rela_ent,
        ),
        (
            elf::DT_REL,
            elf::DT_RELSZ,
            elf::DT_RELENT,
            elf::SHT_REL,
            rel_ent,
        ),
    ] {
        let Some(addr) = tags.get(at) else {
            continue;
        };
        let size = tags.get(size).ok_or("a relocation table has no size tag")?;
        if tags.get(ent).is_some_and(|e| e != want) {
            return Err("a relocation table's entry size is not its type's");
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
            return Err("DT_JMPREL has no DT_PLTRELSZ or DT_PLTREL");
        };
        let (sh_type, ent) = match kind {
            k if k == u64::from(elf::DT_RELA) => (elf::SHT_RELA, rela_ent),
            k if k == u64::from(elf::DT_REL) => (elf::SHT_REL, rel_ent),
            _ => return Err("DT_PLTREL names neither DT_REL nor DT_RELA"),
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
        let size = tags
            .get(size)
            .ok_or("an Android packed table has no size tag")?;
        tables.push(Table::Android { rela, addr, size });
    }
    for (at, size, ent) in [
        (DT_RELR, DT_RELRSZ, DT_RELRENT),
        (DT_ANDROID_RELR, DT_ANDROID_RELRSZ, DT_ANDROID_RELRENT),
    ] {
        let Some(addr) = tags.get(at) else {
            continue;
        };
        let size = tags.get(size).ok_or("a RELR table has no size tag")?;
        if tags.get(ent).is_some_and(|e| e != word) || size % word != 0 {
            return Err("a RELR table's entry size is not a word");
        }
        if loads.bytes(data, addr, size).is_none() {
            return Err("a RELR table is in no PT_LOAD");
        }
    }
    Ok(tables)
}

/// How many entries the CREL stream at the start of `data` holds, `None`
/// when it does not decode.
fn crel_count(data: &[u8]) -> Option<u64> {
    object::read::elf::CrelIterator::new(data)
        .ok()?
        .try_fold(0, |n, e| e.ok().map(|_| n + 1))
}

/// One decoded relocation: `(r_offset, r_info, r_addend)`.
type Entry = (u64, u64, i64);

/// The entries of an Android `APS2` stream, as bionic's
/// `packed_reloc_iterator` decodes it; `None` when it does not decode,
/// `Err(count)` when its count passes `limit`.
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
        let at = out.len();
        out.resize(at + n, 0);
        crate::bytes::write_uint(&mut out[at..], v, le);
    };
    let word = if is_64 { 8 } else { 4 };
    let (ehsize, shentsize) = if is_64 { (64, 64) } else { (52, 40) };
    let mut out = data[..ehsize].to_vec();
    // Section header fields: (sh_type, sh_addr, file bytes, sh_entsize),
    // after the empty names table `object` asks for.
    let mut sections: Vec<(u32, u64, Vec<u8>, u64)> = vec![(elf::SHT_STRTAB, 0, vec![0], 0)];
    let mut emitted = std::collections::HashSet::new();
    for w in walked {
        match w {
            Walked::Plain {
                sh_type,
                addr,
                ent,
                entries,
            } => {
                let mut bytes = Vec::new();
                for &(at, raw) in entries {
                    if emitted.insert(at) {
                        bytes.extend_from_slice(raw);
                    }
                }
                if !bytes.is_empty() {
                    sections.push((*sh_type, *addr, bytes, *ent));
                }
            }
            Walked::Crel { addr, bytes } => {
                sections.push((elf::SHT_CREL, *addr, bytes.to_vec(), 1));
            }
            Walked::Android {
                rela,
                addr,
                entries,
            } => {
                if entries.is_empty() {
                    continue;
                }
                let mut bytes = Vec::new();
                for &(offset, info, addend) in entries {
                    put(&mut bytes, offset, word);
                    put(&mut bytes, info, word);
                    if *rela {
                        put(&mut bytes, addend as u64, word);
                    }
                }
                let (sh_type, ent) = if *rela {
                    (elf::SHT_RELA, 3 * word as u64)
                } else {
                    (elf::SHT_REL, 2 * word as u64)
                };
                sections.push((sh_type, *addr, bytes, ent));
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
    let mut set =
        |at: usize, v: u64, n: usize| crate::bytes::write_uint(&mut out[at..at + n], v, le);
    set(phoff_at, 0, word);
    set(shoff_at, shoff, word);
    set(phnum_at, 0, 2);
    set(phnum_at + 2, shentsize as u64, 2);
    set(phnum_at + 4, shnum, 2);
    set(phnum_at + 6, 1, 2);
    out
}
