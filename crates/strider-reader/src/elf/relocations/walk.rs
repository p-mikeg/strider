use object::{
    Architecture as A, Object, ObjectSection, RelocationFlags, RelocationKind, RelocationTarget,
};

use crate::Result;
use crate::elf::encodings::{Kind, classify};

use super::apply::apply_one_relocation;
use super::implicit::note_copy;
use super::{Ctx, RelocSite};

/// One relocation, decoded once. `object` reads a mips64el `Elf64_Rel`
/// `r_info` as one little-endian `u64`, which swaps `r_sym` with the type
/// word (`r_ssym | r_type3 | r_type2 | r_type`); that is undone here and
/// nowhere else.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RelocInfo {
    pub(crate) r_type: u32,
    /// MIPS64's composite second and third types; `R_MIPS_NONE` elsewhere.
    pub(crate) r_type2: u32,
    pub(crate) r_type3: u32,
    /// `None` is STN_UNDEF.
    pub(crate) sym: Option<object::SymbolIndex>,
    /// `r_addend`; `None` under `SHT_REL`, A then being in the field.
    pub(crate) rela_addend: Option<i64>,
    /// What `object` derived from the type; `None` for a mips64el
    /// implicit-addend entry, which it may have read off the wrong field.
    pub(crate) generic: Option<Generic>,
}

/// `object`'s own reading of a relocation type.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Generic {
    pub(super) kind: RelocationKind,
    /// In bits; 0 for a type it does not size.
    pub(super) size_bits: u8,
    pub(super) encoding: object::RelocationEncoding,
}

impl RelocInfo {
    /// `crel`: the image holds an `SHT_CREL` table, whose entries `object`
    /// reads as written although it reports them implicit-addend too.
    fn decode(reloc: &object::Relocation, arch: A, endian_le: bool, crel: bool) -> Self {
        let RelocationFlags::Elf { r_type } = reloc.flags() else {
            unreachable!("an ELF relocation carries ELF flags");
        };
        let reported = match reloc.target() {
            RelocationTarget::Symbol(idx) => Some(idx),
            _ => None,
        };
        let rela_addend = (!reloc.has_implicit_addend()).then(|| reloc.addend());
        let mips64el_rel = arch == A::Mips64 && endian_le && rela_addend.is_none();
        let (sym, word) = if mips64el_rel && !crel {
            let word = reported.map_or(0, |idx| (idx.0 as u32).swap_bytes());
            let sym = (r_type != 0).then_some(object::SymbolIndex(r_type as usize));
            (sym, word)
        } else {
            (reported, r_type)
        };
        let (r_type, r_type2, r_type3) = if arch == A::Mips64 {
            (word & 0xff, (word >> 8) & 0xff, (word >> 16) & 0xff)
        } else {
            (word, object::elf::R_MIPS_NONE, object::elf::R_MIPS_NONE)
        };
        Self {
            r_type,
            r_type2,
            r_type3,
            sym,
            rela_addend,
            generic: (!mips64el_rel).then(|| Generic {
                kind: reloc.kind(),
                size_bits: reloc.size(),
                encoding: reloc.encoding(),
            }),
        }
    }

    /// `r_type3 << 16 | r_type2 << 8 | r_type`, the type a hole names.
    pub(crate) fn type_word(self) -> u32 {
        (self.r_type3 << 16) | (self.r_type2 << 8) | self.r_type
    }
}

/// Whether `obj` holds an `SHT_CREL` table. MIPS64 is always ELF64.
pub(super) fn has_crel(obj: &object::File<'_>) -> bool {
    use object::read::elf::SectionHeader as _;
    match obj {
        object::File::Elf64(f) => f
            .elf_section_table()
            .iter()
            .any(|s| s.sh_type(f.endian()) == object::elf::SHT_CREL),
        _ => false,
    }
}

/// Invokes `f(site_addr, avail, reloc, pair)` per relocation site:
/// `site_addr` in the regions' address space, `avail` the bytes left of the
/// owning section (`u64::MAX` for a dynamic site), `pair` for an `SHT_REL`
/// `R_MIPS_HI16` the site of the following `R_MIPS_LO16` on its symbol.
///
/// An ET_REL walks its per-section tables targeting `owners`, rebased
/// through `layout`. Anything else walks `dynamic_relocations()`, which skips
/// a table not linked to `.dynsym` (an unstripped static `.rela.plt`;
/// [`relocations_off_dynsym`] walks those).
pub(super) fn for_each_reloc_site<F>(
    obj: &object::File<'_>,
    owners: &std::collections::BTreeSet<usize>,
    layout: &crate::elf::sections::ElfSectionLayout,
    mut f: F,
) where
    F: FnMut(u64, u64, &RelocInfo, Option<u64>),
{
    let arch = obj.architecture();
    let endian_le = matches!(obj.endianness(), object::Endianness::Little);
    let crel = arch == A::Mips64 && has_crel(obj);
    match obj.kind() {
        object::ObjectKind::Relocatable => {
            for sec in obj.sections() {
                if !owners.contains(&sec.index().0) {
                    continue;
                }
                let sec_base = layout.section_base(&sec);
                let sec_size = sec.size();
                let relocs: Vec<(u64, RelocInfo)> = sec
                    .relocations()
                    .map(|(offset, reloc)| {
                        (offset, RelocInfo::decode(&reloc, arch, endian_le, crel))
                    })
                    .collect();
                let pairs = mips_hi16_pairs(&relocs, arch);
                for (i, (offset, r)) in relocs.iter().enumerate() {
                    // `avail` keeps a field past its section from patching
                    // a neighbour; checked where the width is known.
                    let avail = sec_size.saturating_sub(*offset);
                    let pair = pairs
                        .get(i)
                        .copied()
                        .flatten()
                        .map(|lo| sec_base.wrapping_add(lo));
                    f(sec_base.wrapping_add(*offset), avail, r, pair);
                }
            }
        }
        _ => {
            let Some(dyn_relocs) = obj.dynamic_relocations() else {
                return;
            };
            for (site_addr, reloc) in dyn_relocs {
                f(
                    site_addr,
                    u64::MAX,
                    &RelocInfo::decode(&reloc, arch, endian_le, crel),
                    None,
                );
            }
        }
    }
}

/// The entries a walk's relocation headers may name, as a multiple of what
/// the file holds at its smallest entry; past it the image is refused.
pub(crate) const RELOCATION_ENTRY_BUDGET: u64 = 2;

/// `obj`'s bytes with each walked relocation header that repeats an earlier
/// one's table (`sh_type`, `sh_offset`, `sh_size`, and an ET_REL's target)
/// turned `SHT_NULL`; `None` when none repeats. Repeats disagreeing on
/// whether entries index `.dynsym` leave the kept header linked to none, so
/// every entry naming a symbol is a hole.
///
/// # Errors
///
/// When the headers name more than [`RELOCATION_ENTRY_BUDGET`] times the
/// file's length over its smallest entry. A table past the file names none.
pub(super) fn without_repeated_tables(obj: &object::File<'_>) -> Result<Option<Vec<u8>>> {
    match obj {
        object::File::Elf32(f) => without_repeated_tables_of(f),
        object::File::Elf64(f) => without_repeated_tables_of(f),
        _ => Ok(None),
    }
}

pub(super) fn without_repeated_tables_of<Elf: object::read::elf::FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
) -> Result<Option<Vec<u8>>> {
    use object::read::elf::SectionHeader as _;
    let endian = f.endian();
    let relocatable = f.kind() == object::ObjectKind::Relocatable;
    let dynsym = f.elf_dynamic_symbol_table().section();
    let symtab = f.elf_symbol_table().section();
    let is_64 = f.is_64();
    let alloc = u64::from(object::elf::SHF_ALLOC);
    let file_len = f.data().len() as u64;
    let rel_size: u64 = if is_64 { 16 } else { 8 };
    // Table -> (the header kept, whether it resolves through `.dynsym`,
    // whether a repeat disagrees).
    let mut kept = std::collections::HashMap::new();
    let mut repeats = Vec::new();
    let (mut entries, mut smallest) = (0u64, rel_size);
    for (index, sh) in f.elf_section_table().iter().enumerate() {
        let ty = sh.sh_type(endian);
        let entry: u64 = match ty {
            object::elf::SHT_REL => rel_size,
            object::elf::SHT_RELA => rel_size / 2 * 3,
            object::elf::SHT_CREL => 1,
            _ => continue,
        };
        let link = sh.link(endian);
        let (target, resolves) = if relocatable {
            if link != symtab || sh.sh_info(endian) == 0 {
                continue;
            }
            (sh.sh_info(endian), true)
        } else if link == dynsym {
            (0, dynsym.0 != 0)
        } else if sh.sh_flags(endian).into() & alloc != 0 {
            (0, false)
        } else {
            continue;
        };
        let start: u64 = sh.sh_offset(endian).into();
        let size: u64 = sh.sh_size(endian).into();
        match kept.entry((ty, start, size, target)) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert((index, resolves, false));
                if start.checked_add(size).is_some_and(|end| end <= file_len) {
                    entries += size / entry;
                    smallest = smallest.min(entry);
                }
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let (_, first, mixed) = o.get_mut();
                *mixed |= *first != resolves;
                repeats.push(index);
            }
        }
    }
    let budget = RELOCATION_ENTRY_BUDGET * file_len / smallest;
    if entries > budget {
        anyhow::bail!(
            "relocation section headers name {entries} relocation entries, more than \
             {RELOCATION_ENTRY_BUDGET} times the {} a {file_len}-byte file holds",
            file_len / smallest
        );
    }
    if repeats.is_empty() {
        return Ok(None);
    }
    let header = f.elf_header();
    let shoff: u64 = header.e_shoff(endian).into();
    let shentsize = u64::from(header.e_shentsize(endian));
    let le = f.is_little_endian();
    let mut bytes = f.data().to_vec();
    let mut put = |index: usize, field: u64, value: u64, width: usize| -> Option<()> {
        let at = usize::try_from(shoff + index as u64 * shentsize + field).ok()?;
        crate::bytes::write_uint(bytes.get_mut(at..at + width)?, value, le);
        Some(())
    };
    // Elf32_Shdr / Elf64_Shdr: sh_type at 4, sh_flags at 8, sh_link at
    // 0x18 / 0x28.
    let (word, link_at) = if is_64 { (8, 0x28) } else { (4, 0x18) };
    let unlinked = kept.values().filter(|k| k.2).map(|k| k.0);
    let rewritten = || -> Option<()> {
        for index in repeats {
            put(index, 4, u64::from(object::elf::SHT_NULL), 4)?;
        }
        for index in unlinked {
            let sh = f
                .elf_section_table()
                .section(object::SectionIndex(index))
                .ok()?;
            put(index, 8, sh.sh_flags(endian).into() | alloc, word)?;
            put(index, link_at, 0, 4)?;
        }
        Some(())
    };
    // `object` checked the header table lies in-file.
    rewritten().ok_or_else(|| anyhow::anyhow!("a section header lies outside the file"))?;
    Ok(Some(bytes))
}

/// For each `SHT_REL` `R_MIPS_HI16` in `relocs`, the offset of the next
/// `R_MIPS_LO16` against the same symbol; empty unless there are any.
pub(super) fn mips_hi16_pairs(relocs: &[(u64, RelocInfo)], arch: A) -> Vec<Option<u64>> {
    if !matches!(arch, A::Mips | A::Mips64) {
        return Vec::new();
    }
    let mut pairs = vec![None; relocs.len()];
    let mut next_lo: std::collections::HashMap<Option<object::SymbolIndex>, u64> =
        std::collections::HashMap::new();
    for (i, (offset, r)) in relocs.iter().enumerate().rev() {
        if r.rela_addend.is_some() {
            continue;
        }
        if r.r_type == object::elf::R_MIPS_LO16 {
            next_lo.insert(r.sym, *offset);
        } else if r.r_type == object::elf::R_MIPS_HI16 {
            pairs[i] = next_lo.get(&r.sym).copied();
        }
    }
    pairs
}

/// Applies a linked image's allocated relocation sections that
/// `dynamic_relocations()` skips (not linked to `.dynsym`, e.g. an unstripped
/// static `.rela.plt`): `r_sym == 0` entries of a [`classify`] row as usual,
/// every other entry a hole over its row's field, else a word, since which
/// table its index means is in doubt. On MIPS everything but `R_MIPS_NONE`
/// and a MIPS32 `R_MIPS_COPY` is a hole (the ELF64 type word is not decoded).
///
/// `walked` is `ctx.obj`, or its copy with repeated tables dropped.
pub(super) fn relocations_off_dynsym(ctx: &mut Ctx<'_, '_>, walked: &object::File<'_>) {
    let obj = ctx.obj;
    if matches!(obj.kind(), object::ObjectKind::Relocatable) {
        return;
    }
    let entries = match walked {
        object::File::Elf32(f) => entries_off_dynsym(f),
        object::File::Elf64(f) => entries_off_dynsym(f),
        _ => return,
    };
    let mips = matches!(obj.architecture(), A::Mips | A::Mips64);
    let word = if obj.is_64() { 8 } else { 4 };
    for (addr, r) in entries {
        let site = RelocSite {
            addr,
            avail: u64::MAX,
        };
        if mips && r.type_word() == object::elf::R_MIPS_NONE {
            continue;
        }
        // A MIPS32 type word is the type alone, so its COPY is known too.
        let kind = if !mips {
            classify(obj.architecture(), &r)
        } else if obj.architecture() == A::Mips && r.type_word() == object::elf::R_MIPS_COPY {
            Some(Kind::Copy)
        } else {
            None
        };
        let width = match kind {
            Some(Kind::Marker) => continue,
            // Which table the index means is in doubt, and with it the size.
            Some(Kind::Copy) => {
                note_copy(ctx, addr, None, r.type_word());
                continue;
            }
            Some(Kind::Compute(..)) if r.sym.is_none() => {
                apply_one_relocation(ctx, site, &r, None);
                continue;
            }
            Some(Kind::Compute(_, field)) => field.width(),
            None => word,
        };
        // Each region the hole overlaps serves its share.
        let holder = ctx.holder(site, addr, 1);
        ctx.unmodelled(holder, addr, width, r.type_word());
    }
}

/// `(r_offset, entry)` of every `SHT_REL` / `SHT_RELA` / `SHT_CREL` entry in
/// an allocated section `object`'s `dynamic_relocations()` skips.
pub(super) fn entries_off_dynsym<Elf: object::read::elf::FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
) -> Vec<(u64, RelocInfo)> {
    use object::read::elf::SectionHeader as _;
    let endian = f.endian();
    let dynsym = f.elf_dynamic_symbol_table().section();
    f.elf_section_table()
        .iter()
        .filter(|sh| {
            sh.sh_flags(endian).into() & u64::from(object::elf::SHF_ALLOC) != 0
                && sh.link(endian) != dynsym
        })
        .flat_map(|sh| raw_entries(f, sh))
        .map(|(offset, r_type, sym, rela_addend)| {
            let r = RelocInfo {
                r_type,
                r_type2: object::elf::R_MIPS_NONE,
                r_type3: object::elf::R_MIPS_NONE,
                sym: (sym != 0).then_some(object::SymbolIndex(sym as usize)),
                rela_addend,
                generic: None,
            };
            (offset, r)
        })
        .collect()
}

/// `(r_offset, r_type, r_sym, r_addend)` of each entry of the relocation
/// section `sh`, as it lies: `r_addend` is `None` in an implicit-addend
/// table. Empty when `sh` holds no table that decodes; a CREL stream stops
/// at its first bad entry.
pub(crate) fn raw_entries<Elf: object::read::elf::FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
    sh: &Elf::SectionHeader,
) -> Vec<(u64, u32, u32, Option<i64>)> {
    use object::read::elf::{Rel as _, Rela as _, SectionHeader as _};
    let endian = f.endian();
    let data = f.data();
    let mips64el = f.elf_header().is_mips64el(endian);
    if let Ok(Some((rela, _))) = sh.rela(endian, data) {
        rela.iter()
            .map(|e| {
                (
                    e.r_offset(endian).into(),
                    e.r_type(endian, mips64el),
                    e.r_sym(endian, mips64el),
                    Some(e.r_addend(endian).into()),
                )
            })
            .collect()
    } else if let Ok(Some((rel, _))) = sh.rel(endian, data) {
        rel.iter()
            .map(|e| {
                (
                    e.r_offset(endian).into(),
                    e.r_type(endian),
                    e.r_sym(endian),
                    None,
                )
            })
            .collect()
    } else if sh.sh_type(endian) == object::elf::SHT_CREL
        && let Ok(bytes) = sh.data(endian, data)
        && let Ok(crel) = object::read::elf::CrelIterator::new(bytes)
    {
        let rela = crel.is_rela();
        crel.map_while(|e| e.ok())
            .map(|e| (e.r_offset, e.r_type, e.r_sym, rela.then_some(e.r_addend)))
            .collect()
    } else {
        Vec::new()
    }
}
