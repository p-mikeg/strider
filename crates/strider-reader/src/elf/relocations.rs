//! ELF relocation application, as one sorted patch list per image that every
//! region shares and a read applies where it crosses a site.
//!
//! An image that has not been through its linker, or through `ld.so`, holds
//! only an addend, or nothing, in every cross-reference field. An ET_REL's
//! `call rel32` sits as `e8 fc ff ff ff` or `e8 00 00 00 00` under an
//! `R_X86_64_PLT32` the linker would resolve, and an AArch64 `bl` as a branch
//! to itself; an ET_DYN's dispatch-table slots and GOT/PLT entries hold what
//! `ld` left under `.rela.dyn` / `.rela.plt` until `ld.so` fills them. Every
//! one of those decodes as a plausible reference into the image itself. This
//! applies the relocations statically.
//!
//! A relocation whose value is not computed is not left in place: outside a
//! writable mapping its field becomes a hole no read serves, see
//! [`apply_elf_relocations_with`].

use object::{
    Architecture as A, Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationFlags,
    RelocationKind, RelocationTarget,
};

use std::collections::BTreeMap;
use std::sync::Arc;

use super::encodings::{Field, Half, Isa, Kind, Value, classify, encode, implicit_addend, sext};
use super::sections::AddressRanges;
use crate::{MemRegion, Patch, RegionIndex, Result};

/// Adds a possibly-negative relocation `addend` to a base address.
///
/// A negative addend casts to its 2's-complement bit pattern; `wrapping_add`
/// plus `Patch::new`'s truncation to the field width give the modular result
/// every relocation field expects.
#[inline]
fn apply_addend(base: u64, addend: i64) -> u64 {
    base.wrapping_add(addend as u64)
}

/// The relocation's `A`, read out of `holder`, the region a read of the
/// whole field is served from.
///
/// `SHT_REL` tables carry no `r_addend` field and store A in the relocation
/// field itself; `object` reports `r_addend = 0` for them, so it is read back
/// out of the site. A field no region holds contributes A = 0, matching the
/// patch that is likewise skipped.
///
/// The field is read file-initial, unpatched: no linker emits two relocations
/// against one field.
fn reloc_addend(
    reloc: &object::Relocation,
    holder: Option<&MemRegion>,
    field_addr: u64,
    size_bytes: usize,
    endian_le: bool,
) -> i64 {
    debug_assert!(
        (1..=8).contains(&size_bytes),
        "field width {size_bytes} is outside the sign-extend shift's range",
    );
    if !reloc.has_implicit_addend() {
        return reloc.addend();
    }
    // Sign-extended from the field width: a PC-relative site holds a negative A
    // (-4 for an x86 `call rel32`).
    holder.map_or(0, |region| {
        sext(
            read_field(region, field_addr, size_bytes, endian_le),
            8 * size_bytes as u32,
        )
    })
}

/// Attaches one relocation patch list, sorted by address, to every region,
/// walking the table appropriate for `obj.kind()`: `obj.dynamic_relocations()`
/// for ET_EXEC / ET_DYN, per-section `section.relocations()` for ET_REL. Each
/// region keeps the part that can overlap it, so a read searches no more
/// patches than a list of its own would hold.
///
/// # Supported
///
/// Via [`RelocationKind`]:
/// * `Absolute`, `S + A` (`R_X86_64_64`). Symbol-targeted.
/// * `Relative`, `S + A - P` (`R_X86_64_PC32`, `R_AARCH64_PREL16/PREL32/PREL64`,
///   `R_386_PC32`). Symbol-targeted.
/// * `PltRelative`, valued the same as `Relative` (no PLT is materialised, so
///   the symbol's own address is used): the 32-bit `R_X86_64_PLT32` /
///   `R_386_PLT32` apply.
///
/// Via raw `r_type`, which `object` reports as `RelocationKind::Unknown`:
/// * `R_*_RELATIVE` / `R_*_IRELATIVE`: `image_base + addend`, image base
///   modelled as 0. An `SHT_REL` one keeps A in its field, which is then
///   already the whole value, so it is left exactly as assembled.
/// * `R_*_GLOB_DAT` / `R_*_JUMP_SLOT`: the symbol's address at the slot
///   (S semantics, resolved eagerly).
/// * `R_MIPS_REL32`, both the undefined (addend-only) and defined-symbol forms,
///   the `SHT_REL` undefined one again as assembled.
/// * `R_ARM_REL32` / `R_PPC_REL32` / `R_PPC64_REL64`, plain word-sized
///   `S + A - P`. `object` surfaces all three as `Unknown` with `size = 0`, so
///   they dispatch on the raw `r_type` like the GOT/PLT slots; a PowerPC
///   `.rodata` switch table is built out of exactly these.
/// * The instruction, GOT and TOC relocations [`super::encodings::classify`]
///   lists. A GOT reference resolves through a synthetic GOT slot holding the
///   symbol's address, which exists only for an ET_REL.
///
/// On an ET_REL an undefined or `SHN_COMMON` symbol resolves to its
/// [`ElfSectionLayout::extern_address`], outside every mapping. An undefined
/// `STB_WEAK` one is the exception: see `Target::weak_undefined`.
///
/// An instruction field is read and written in the instruction byte order,
/// which on AArch64 is little-endian whatever `EI_DATA` says; a data field
/// follows `EI_DATA`.
///
/// # Not computed
///
/// A relocation whose value is not computed (a kind not listed above, a symbol
/// with no address or a weak undefined one, a value its field cannot hold, a
/// branch needing a veneer) leaves no plausible bytes behind: unless the field
/// is in a writable mapping, which nothing folds, it becomes a hole no read
/// serves, so decoding or folding through it fails.
///
/// Any patch list the regions already carry is replaced, so re-applying over
/// one region set is idempotent.
///
/// `layout` must be the one built for `obj`, `loaded_with` the filter the
/// regions were loaded with and `writable` the image's writable mappings.
///
/// Returns the GOT slots the relocations referenced, as regions of their own.
///
/// # Errors
///
/// When a loaded section's bytes cannot be read.
pub(crate) fn apply_elf_relocations_with(
    regions: &mut [MemRegion],
    obj: &object::File<'_>,
    loaded_with: super::sections::LoadFilter,
    layout: &super::sections::ElfSectionLayout,
    writable: &AddressRanges,
    executable: &AddressRanges,
) -> Result<Vec<MemRegion>> {
    let owners = super::sections::loaded_section_indices(obj, layout, loaded_with)?;
    // One lookup per relocation instead of a scan of every region; an ET_REL
    // carries one region per SHF_ALLOC section.
    let region_index = RegionIndex::new(regions);
    let mut ctx = Ctx {
        obj,
        layout,
        regions,
        region_index: &region_index,
        writable,
        executable,
        endian_le: matches!(obj.endianness(), object::Endianness::Little),
        insn_le: matches!(obj.endianness(), object::Endianness::Little)
            || obj.architecture() == A::Aarch64,
        opd_entries: super::sections::opd_entries(obj, layout),
        got: BTreeMap::new(),
        patches: Vec::new(),
    };
    for_each_reloc_site(obj, &owners, layout, |site_addr, avail, reloc, pair| {
        apply_one_relocation(
            &mut ctx,
            RelocSite {
                addr: site_addr,
                avail,
            },
            reloc,
            pair,
        );
    });
    let got = got_regions(&ctx.got, ctx.endian_le)?;
    let mut patches = ctx.patches;
    // Stable, so the last patch recorded at a site is the one a read applies.
    patches.sort_by_key(|p| p.addr);
    let patches: Arc<[Patch]> = patches.into();
    for region in regions {
        region.attach_patches(&patches);
    }
    Ok(got)
}

/// What every relocation in one walk reads, and what it records.
struct Ctx<'a, 'd> {
    obj: &'a object::File<'d>,
    layout: &'a super::sections::ElfSectionLayout,
    /// Unique starts (`collapse_equal_starts`), so the highest-start region
    /// holding a field is the one a read of it reaches.
    regions: &'a [MemRegion],
    region_index: &'a RegionIndex,
    writable: &'a AddressRanges,
    executable: &'a AddressRanges,
    endian_le: bool,
    /// Byte order of an instruction field: A64 instructions are little-endian
    /// whatever `EI_DATA` says, and only data follows it. ARM BE32 instructions
    /// really are big-endian, BE8's swap being the link's.
    insn_le: bool,
    /// ELFv1 descriptor address -> the code entry its first word relocates to.
    opd_entries: BTreeMap<u64, u64>,
    /// GOT slot address -> (the symbol address it holds, slot width).
    got: BTreeMap<u64, (u64, usize)>,
    /// Every patch the walk records, in record order.
    patches: Vec<Patch>,
}

impl<'a> Ctx<'a, '_> {
    /// The byte order `field` is read and patched in.
    fn field_le(&self, field: Field) -> bool {
        match field {
            Field::Data { .. } => self.endian_le,
            _ => self.insn_le,
        }
    }

    /// The highest-start region holding all of `[addr, addr + width)`: the
    /// one a read of the whole field is served from.
    fn holding(&self, addr: u64, width: usize) -> Option<&'a MemRegion> {
        let regions = self.regions;
        let i = self.region_index.covering(addr, width as u64).next()?;
        Some(&regions[i])
    }

    /// [`holding`](Self::holding) for a field of `site`, and `None` too when
    /// the field runs past `site.avail`, what is left of the section owning
    /// the site. A field past its section, or past the end of every mapping
    /// holding its first byte, would write a neighbour's bytes, so it is
    /// neither patched nor holed.
    fn holder(&self, site: RelocSite, addr: u64, width: usize) -> Option<&'a MemRegion> {
        // `addr` is `site.addr` plus a non-negative `mips_half_field_skew` and
        // `width` is at most 8, so neither wraps; saturating anyway keeps a
        // malformed pair on the `None` path.
        if addr.wrapping_sub(site.addr).saturating_add(width as u64) > site.avail {
            return None;
        }
        self.holding(addr, width)
    }

    /// Records the low `width` bytes of `value` at `addr` when a region holds
    /// the field. Every region overlapping it then serves the patch, clipped
    /// to its own bytes: a narrow read inside a field that straddles an inner
    /// region's end is that region's, a wide one the outer region's.
    fn record(
        &mut self,
        holder: Option<&MemRegion>,
        addr: u64,
        value: u64,
        width: usize,
        endian_le: bool,
    ) {
        if holder.is_some()
            && let Some(patch) = Patch::new(addr, value, width, endian_le)
        {
            self.patches.push(patch);
        }
    }

    /// A relocation whose value is not computed: a hole over the field, when
    /// a region holds it, unless the field is writable and NOT executable at
    /// runtime.
    ///
    /// Writable data the program rewrites anyway is not worth a hole.  A
    /// writable EXECUTABLE field is a different thing: `CodeAndReadOnly` keeps
    /// an RWX mapping on purpose (firmware, `ld -N`), so those bytes reach the
    /// decoder, and serving them unrelocated hands it a plausible wrong
    /// instruction with nothing said about it.
    fn unmodelled(
        &mut self,
        holder: Option<&MemRegion>,
        addr: u64,
        width: usize,
        reloc: &object::Relocation,
    ) {
        let RelocationFlags::Elf { r_type } = reloc.flags() else {
            unreachable!("an ELF relocation carries ELF flags");
        };
        // mips64el `SHT_REL` reports the real `r_sym` in `r_type`.
        let r_type =
            mips_type_word(reloc, self.obj.architecture(), self.endian_le).unwrap_or(r_type);
        if holder.is_some()
            && (!self.writable.touches(addr, width) || self.executable.touches(addr, width))
        {
            self.patches.push(Patch::hole(addr, width, r_type));
        }
    }
}

/// A relocation site: where the field lands, and how many bytes of the section
/// owning it remain from there.
#[derive(Clone, Copy)]
struct RelocSite {
    addr: u64,
    avail: u64,
}

/// Invokes `f(site_addr, avail, reloc, pair)` per relocation site, `site_addr`
/// being the **absolute** virtual address in the coordinate system the loaded
/// regions live in and `avail` the bytes left of the section owning the site
/// (`u64::MAX` for a dynamic site, whose owner is a segment and which therefore
/// has no section end to overrun).
///
/// `pair` is, for an `SHT_REL` `R_MIPS_HI16`, the site of the `R_MIPS_LO16`
/// against the same symbol that follows it, whose field holds the low half of
/// the addend.
///
/// Kind dispatch:
///
/// * ET_REL: per-section tables, restricted to `owners`. A section's
///   `relocations()` yields `(r_offset, Relocation)` where `r_offset` is
///   relative to the section the relocations apply *to* (the one `sh_info`
///   points at), so the site is that section's `layout` base plus the offset.
/// * Everything else: the dynamic table, `owners` and `layout` unused.
///   `dynamic_relocations()` is `Some` for every ELF, but its iterator yields
///   only relocation sections whose `sh_link` is the dynamic symbol table's
///   index, which is 0 when there is no `.dynsym`. A static binary's
///   `.rela.plt` links to `.symtab`, so it walks no sites; `strip` rewrites
///   that link to 0, and the same binary then walks its IRELATIVE slots and
///   serves each resolver's address in place of the file bytes.
fn for_each_reloc_site<F>(
    obj: &object::File<'_>,
    owners: &std::collections::BTreeSet<usize>,
    layout: &super::sections::ElfSectionLayout,
    mut f: F,
) where
    F: FnMut(u64, u64, &object::Relocation, Option<u64>),
{
    match obj.kind() {
        object::ObjectKind::Relocatable => {
            let arch = obj.architecture();
            let endian_le = matches!(obj.endianness(), object::Endianness::Little);
            for sec in obj.sections() {
                if !owners.contains(&sec.index().0) {
                    continue;
                }
                let sec_base = layout.section_base(&sec);
                let sec_size = sec.size();
                let relocs: Vec<(u64, object::Relocation)> = sec.relocations().collect();
                let pairs = mips_hi16_pairs(&relocs, arch, endian_le);
                for (i, (offset, reloc)) in relocs.iter().enumerate() {
                    // The gABI puts `r_offset` inside the section `sh_info`
                    // names.  A malformed object can point it past the end,
                    // where the site still lands in SOME loaded region and
                    // would silently patch an unrelated section's bytes.
                    // `reloc.size()` is 0 for every type dispatched on the raw
                    // `r_type`, so the budget travels to where the width is
                    // actually chosen instead of being checked here.
                    let avail = sec_size.saturating_sub(*offset);
                    let pair = pairs
                        .get(i)
                        .copied()
                        .flatten()
                        .map(|lo| sec_base.wrapping_add(lo));
                    f(sec_base.wrapping_add(*offset), avail, reloc, pair);
                }
            }
        }
        _ => {
            let Some(dyn_relocs) = obj.dynamic_relocations() else {
                return;
            };
            for (site_addr, reloc) in dyn_relocs {
                f(site_addr, u64::MAX, &reloc, None);
            }
        }
    }
}

/// For each `SHT_REL` `R_MIPS_HI16` in `relocs`, the offset of the next
/// `R_MIPS_LO16` against the same symbol; empty unless there are any.
fn mips_hi16_pairs(
    relocs: &[(u64, object::Relocation)],
    arch: object::Architecture,
    endian_le: bool,
) -> Vec<Option<u64>> {
    if !matches!(arch, A::Mips | A::Mips64) {
        return Vec::new();
    }
    let mut pairs = vec![None; relocs.len()];
    let mut next_lo: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for (i, (offset, reloc)) in relocs.iter().enumerate().rev() {
        if !reloc.has_implicit_addend() {
            continue;
        }
        let Some((r_sym, r_type, _)) = mips_reloc_parts(reloc, arch, endian_le) else {
            continue;
        };
        if r_type == object::elf::R_MIPS_LO16 {
            next_lo.insert(r_sym, *offset);
        } else if r_type == object::elf::R_MIPS_HI16 {
            pairs[i] = next_lo.get(&r_sym).copied();
        }
    }
    pairs
}

/// The referenced GOT slots, adjacent ones sharing a region.
fn got_regions(got: &BTreeMap<u64, (u64, usize)>, endian_le: bool) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    let mut run: Option<(u64, Vec<u8>)> = None;
    for (&addr, &(value, width)) in got {
        // `ElfSectionLayout::got_slot`, the only producer, yields the image's
        // word, 4 or 8, so this cannot fire -- but both slices below index
        // blind, and `8 - width` underflows for a wider one. Its twin
        // `Patch::new` already checks.
        if width == 0 || width > 8 {
            anyhow::bail!("GOT slot at {addr:#x} has width {width}, not 1..=8");
        }
        let bytes = if endian_le {
            value.to_le_bytes()[..width].to_vec()
        } else {
            value.to_be_bytes()[8 - width..].to_vec()
        };
        match &mut run {
            Some((start, data)) if *start + data.len() as u64 == addr => data.extend(bytes),
            _ => {
                if let Some((start, data)) = run.take() {
                    out.push(MemRegion::new(start, data)?);
                }
                run = Some((addr, bytes));
            }
        }
    }
    if let Some((start, data)) = run {
        out.push(MemRegion::new(start, data)?);
    }
    Ok(out)
}

/// Applies one relocation entry at the already-absolute `site.addr`.
///
/// Sites with no backing region are skipped; a relocation whose value is not
/// computed becomes a hole, see [`apply_elf_relocations_with`].
fn apply_one_relocation(
    ctx: &mut Ctx<'_, '_>,
    site: RelocSite,
    reloc: &object::Relocation,
    pair: Option<u64>,
) {
    let obj = ctx.obj;
    let site_addr = site.addr;
    let endian_le = ctx.endian_le;
    let arch = obj.architecture();

    // Image-relative relocations store `image_base + addend` with no symbol or
    // section reference, so `object` surfaces them as an `Absolute` target with
    // an `Unknown` kind. Image base is modelled as the link-time base (0 for an
    // ET_DYN), making the patched value the addend itself; width comes from the
    // relocation type. Without this branch a PIE's dispatch-table slot reads
    // its unrelocated file bytes wherever the slot IS mapped; under the
    // read-only filters those slots sit in the RW `PT_LOAD` and are not mapped
    // at all, since `PT_GNU_RELRO` is not modelled.
    if let Some((value, size_bytes)) = image_relative_reloc(reloc, arch, endian_le) {
        // An `SHT_REL` site keeps A in its field, which with the base modelled
        // as 0 is already the whole value: nothing to patch, and nothing to
        // hole either.
        if reloc.has_implicit_addend() {
            return;
        }
        let holder = ctx.holder(site, site_addr, size_bytes);
        ctx.record(holder, site_addr, value, size_bytes, endian_le);
        return;
    }

    // Symbol-targeted families `object` reports as `Unknown` with `size = 0`,
    // which the general `match reloc.kind()` below would mis-bucket as
    // unsupported. Each yields `(size_bytes, pc_relative,
    // read_implicit_addend)`; first match wins.
    let word_sized = got_or_plt_slot_reloc_size(reloc, arch, endian_le)
        // GOT/PLT slot, `S`. Its own field is the PLT push offset, not an
        // addend, so it is never read back; `reloc.addend()` is the RELA one,
        // zero under REL.
        .map(|size_bytes| (size_bytes, false, false))
        // Defined-symbol `R_MIPS_REL32`, `S + A`.
        .or_else(|| {
            mips_rel32_symbol_reloc_size(reloc, arch, endian_le)
                .map(|size_bytes| (size_bytes, false, true))
        })
        // `R_PPC_REL32` / `R_ARM_REL32` / `R_PPC64_REL64`, `S + A - P`.
        .or_else(|| pc_relative_word_reloc(reloc, arch).map(|size_bytes| (size_bytes, true, true)));
    if let Some((size_bytes, pc_relative, read_implicit_addend)) = word_sized {
        let holder = ctx.holder(site, site_addr, size_bytes);
        let Some(target) = resolve_symbol_target(obj, ctx.layout, reloc, endian_le) else {
            ctx.unmodelled(holder, site_addr, size_bytes, reloc);
            return;
        };
        // A plain word holds the address itself, see `Target::weak_undefined`.
        if target.weak_undefined {
            ctx.unmodelled(holder, site_addr, size_bytes, reloc);
            return;
        }
        let addend = if read_implicit_addend {
            reloc_addend(reloc, holder, site_addr, size_bytes, endian_le)
        } else {
            reloc.addend()
        };
        let value = apply_addend(target.addr, addend);
        let value = if pc_relative {
            value.wrapping_sub(site_addr)
        } else {
            value
        };
        let field = Field::Data {
            bytes: size_bytes as u8,
            signed: pc_relative,
        };
        match fit(obj, field, value) {
            Some(value) => ctx.record(holder, site_addr, value, size_bytes, endian_le),
            None => ctx.unmodelled(holder, site_addr, size_bytes, reloc),
        }
        return;
    }

    // mips64el `SHT_REL`: `object` reads `r_info` as one little-endian `u64`, so
    // the `kind` and `size` consulted below come from the real `r_sym`, matched
    // against `R_MIPS_16` / `R_MIPS_32` / `R_MIPS_64` (symbol index 1, 2, 18).
    // Every MIPS relocation handled here dispatches on the raw `r_type` above.
    if matches!(arch, A::Mips64) && endian_le && reloc.has_implicit_addend() {
        let holder = ctx.holder(site, site_addr, UNKNOWN_FIELD_BYTES);
        ctx.unmodelled(holder, site_addr, UNKNOWN_FIELD_BYTES, reloc);
        return;
    }

    if let Some(kind) = classified(reloc, arch, endian_le) {
        apply_classified(ctx, site, reloc, pair, kind);
        return;
    }

    // `size` is in bits, and 0 nominally means "the kind's default". Absolute /
    // Relative / PltRelative all set it explicitly on every arch of interest,
    // so 0 here signals an arch-specific encoding that isn't modelled. The
    // width is needed before the value: an `SHT_REL` addend is read back out
    // of the field.
    let size_bits = reloc.size();
    let plain = size_bits != 0
        && size_bits.is_multiple_of(8)
        && size_bits <= 64
        // A byte-multiple size still does not make the field plain low bytes
        // at the offset: s390x's `*DBL` types hold `(S + A - P) >> 1`,
        // `R_LARCH_B16` a branch displacement, the SHARC `*_V3` family
        // instruction-encoded operands. `X86Signed` is the one non-`Generic`
        // encoding that IS plain: it only names the sign extension
        // `R_X86_64_32S` applies at runtime.
        && matches!(
            reloc.encoding(),
            object::RelocationEncoding::Generic | object::RelocationEncoding::X86Signed
        );
    // `PltRelative`'s L collapses to S here since no PLT is materialised.
    let pc_relative = match reloc.kind() {
        RelocationKind::Absolute => Some(false),
        RelocationKind::Relative | RelocationKind::PltRelative => Some(true),
        _ => None,
    };
    let (Some(pc_relative), true) = (pc_relative, plain) else {
        let width = if plain {
            (size_bits / 8) as usize
        } else {
            UNKNOWN_FIELD_BYTES
        };
        let holder = ctx.holder(site, site_addr, width);
        ctx.unmodelled(holder, site_addr, width, reloc);
        return;
    };
    let size_bytes = (size_bits / 8) as usize;

    // `r_offset` addresses the storage unit, inside which the field can be
    // offset. A `r_offset` at the very top of the address space is malformed;
    // the skew would carry the field out of it.
    let Some(field_addr) = site_addr.checked_add(mips_half_field_skew(reloc, arch, endian_le))
    else {
        return;
    };

    let holder = ctx.holder(site, field_addr, size_bytes);
    // `object`'s ELF `parse_relocation` yields only `Symbol` or `Absolute`;
    // the latter (an immediate with no symbol) has no target to add.
    let target = match reloc.target() {
        RelocationTarget::Symbol(_) => resolve_symbol_target(obj, ctx.layout, reloc, endian_le),
        _ => None,
    };
    let Some(target) = target else {
        ctx.unmodelled(holder, field_addr, size_bytes, reloc);
        return;
    };
    // `PltRelative` is a call or jump, whose displacement hides the address it
    // was built from; every other plain field holds that address, see
    // `Target::weak_undefined`.
    if target.weak_undefined && !matches!(reloc.kind(), RelocationKind::PltRelative) {
        ctx.unmodelled(holder, field_addr, size_bytes, reloc);
        return;
    }
    let addend = reloc_addend(reloc, holder, field_addr, size_bytes, endian_le);
    // S, A, P follow the System V ABI generic relocation formula:
    // S = target_addr, A = addend, P = site_addr. P is the storage unit, not
    // the field inside it.
    let value = apply_addend(target.addr, addend);
    let value = if pc_relative {
        value.wrapping_sub(site_addr)
    } else {
        value
    };
    let field = Field::Data {
        bytes: size_bytes as u8,
        signed: pc_relative || matches!(reloc.encoding(), object::RelocationEncoding::X86Signed),
    };
    match fit(obj, field, value) {
        Some(value) => ctx.record(holder, field_addr, value, size_bytes, endian_le),
        None => ctx.unmodelled(holder, field_addr, size_bytes, reloc),
    }
}

/// Field width assumed for a relocation whose type gives none: an instruction
/// word, or the 32-bit displacement of an x86 one.
const UNKNOWN_FIELD_BYTES: usize = 4;

/// `value` checked against a plain data `field`. An ELF32 image's address
/// arithmetic wraps at 32 bits, so there a word always fits and a narrower
/// field is checked against the sign-extended 32-bit value.
fn fit(obj: &object::File<'_>, field: Field, value: u64) -> Option<u64> {
    if obj.is_64() {
        return encode(field, 0, value, Isa::Same, 0);
    }
    let value = i64::from(value as u32 as i32) as u64;
    if field.width() >= 4 {
        return Some(value);
    }
    encode(field, 0, value, Isa::Same, 0)
}

/// The [`classify`] answer for `reloc`, `None` for a type it does not list or
/// a MIPS composite of more than one type.
fn classified(
    reloc: &object::Relocation,
    arch: object::Architecture,
    endian_le: bool,
) -> Option<Kind> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    match mips_reloc_parts(reloc, arch, endian_le) {
        Some((_, first, _)) => {
            (mips_type_word(reloc, arch, endian_le)? >> 8 == 0).then_some(())?;
            classify(arch, first)
        }
        None => classify(arch, r_type),
    }
}

/// A [`classify`]-listed relocation, see [`super::encodings`].
fn apply_classified(
    ctx: &mut Ctx<'_, '_>,
    site: RelocSite,
    reloc: &object::Relocation,
    pair: Option<u64>,
    kind: Kind,
) {
    let (value_kind, field) = match kind {
        Kind::Marker => return,
        Kind::Compute(value, field) => (value, field),
        Kind::Ppc64Call { .. } | Kind::PpcPltCall => (Value::Pcrel, Field::PpcBranch24),
    };
    let p = site.addr;
    let width = field.width();
    let holder = ctx.holder(site, p, width);
    let field_le = ctx.field_le(field);
    match compute_classified(ctx, reloc, pair, kind, value_kind, field, p, holder) {
        Some(word) => ctx.record(holder, p, word, width, field_le),
        None => ctx.unmodelled(holder, p, width, reloc),
    }
}

/// The patched field of a [`classify`]-listed relocation at `p`, `None` when
/// it is not computed.
#[allow(clippy::too_many_arguments)]
fn compute_classified(
    ctx: &mut Ctx<'_, '_>,
    reloc: &object::Relocation,
    pair: Option<u64>,
    kind: Kind,
    value_kind: Value,
    field: Field,
    p: u64,
    holder: Option<&MemRegion>,
) -> Option<u64> {
    let (obj, layout, endian_le) = (ctx.obj, ctx.layout, ctx.endian_le);
    let field_le = ctx.field_le(field);
    let thumb_le = field_le && matches!(field, Field::ThumbBranch { .. } | Field::ThumbMovw { .. });
    let raw = read_field(holder?, p, field.width(), field_le);
    let raw = if thumb_le { swap_halfwords(raw) } else { raw };
    let target = match reloc.target() {
        RelocationTarget::Symbol(_) => Some(resolve_symbol_target(obj, layout, reloc, endian_le)?),
        _ => None,
    };
    // Only a branch displacement and a GOT slot reference hide the address
    // they were built from, see `Target::weak_undefined`.
    if target.is_some_and(|t| t.weak_undefined)
        && !field.is_branch()
        && !value_kind_through_got(value_kind)
    {
        return None;
    }
    let s = target.map_or(0, |t| t.addr);

    let mut a = if reloc.has_implicit_addend() {
        implicit_addend(field, raw)?
    } else {
        reloc.addend()
    };
    match field {
        // The low half of an `SHT_REL` HI16's addend is its LO16's field.
        Field::MipsHalf16(Half::Ha) if reloc.has_implicit_addend() => {
            let pair = pair?;
            let lo = read_field(ctx.holding(pair, 4)?, pair, 4, ctx.endian_le);
            a = (a << 16) + sext(lo & 0xffff, 16);
        }
        Field::Mips26 if reloc.has_implicit_addend() && !target.is_some_and(|t| t.local) => {
            a = sext(a as u64, 28);
        }
        _ => {}
    }
    // `_gp_disp` is the distance to the GP of a PIC function, never modelled.
    if matches!(field, Field::MipsHalf16(_)) && target.is_some_and(|t| t.gp_disp) {
        return None;
    }

    let got_slot = |ctx: &mut Ctx<'_, '_>| -> Option<u64> {
        // A GOT exists only for an ET_REL, and a slot holds `S` alone.
        if !layout.is_rebased() || (value_kind_needs_zero(value_kind) && a != 0) {
            return None;
        }
        let t = target?;
        let (slot, width) = layout.got_slot(t.index)?;
        // A weak undefined symbol's slot holds 0 as readily as an address, so
        // it is left unmapped and the load through it stays opaque.
        if !t.weak_undefined {
            ctx.got.insert(slot, (t.addr, width));
        }
        Some(slot)
    };
    let got_base = layout.is_rebased().then(|| layout.got_base());
    let isa = target.map_or(Isa::Same, |t| t.isa);
    // A branch enters the instruction, not the symbol's ISA bit.
    let branch_s = match field {
        Field::ArmBranch24 { .. } | Field::ThumbBranch { .. } if isa == Isa::Thumb => s & !1,
        _ => s,
    };
    let sa = apply_addend(branch_s, a);
    let page = |x: u64| x & !0xfff;
    let value = match (kind, value_kind) {
        (Kind::PpcPltCall, _) => s.wrapping_sub(p),
        // A descriptor's entry is where `S + A` in `.opd` relocates to.
        (Kind::Ppc64Call { .. }, _) if target?.in_opd => ctx.opd_entries.get(&sa)?.wrapping_sub(p),
        (Kind::Ppc64Call { notoc }, _) => {
            let entry = if notoc {
                sa
            } else {
                sa.wrapping_add(target?.ppc64_local_entry)
            };
            entry.wrapping_sub(p)
        }
        (_, Value::Abs) => sa,
        (_, Value::Pcrel) => sa.wrapping_sub(p),
        (_, Value::PagePcrel) => page(sa).wrapping_sub(page(p)),
        (_, Value::GotPcrel) => apply_addend(got_slot(ctx)?, a).wrapping_sub(p),
        (_, Value::GotPagePcrel) => page(got_slot(ctx)?).wrapping_sub(page(p)),
        (_, Value::Got) => got_slot(ctx)?,
        (_, Value::GotOffset) => apply_addend(got_slot(ctx)?.wrapping_sub(got_base?), a),
        (_, Value::GotRelative) => sa.wrapping_sub(got_base?),
        (_, Value::GotBasePcrel) => apply_addend(got_base?, a).wrapping_sub(p),
        (_, Value::TocRelative) => sa.wrapping_sub(layout.toc_base()?),
        (_, Value::Toc) => apply_addend(layout.toc_base()?, a),
    };
    let word = match field {
        Field::Data { .. } => fit(obj, field, value)?,
        _ => encode(field, raw, value, isa, p)?,
    };
    Some(if thumb_le { swap_halfwords(word) } else { word })
}

/// Whether a GOT value only holds for a zero addend: the slot is `S`, so a
/// `Page(G)` or `G` cannot carry one.
fn value_kind_needs_zero(value: Value) -> bool {
    matches!(value, Value::GotPagePcrel | Value::Got)
}

/// Whether the value reaches the symbol only through its GOT slot, which is
/// then the one place its address appears.
fn value_kind_through_got(value: Value) -> bool {
    matches!(
        value,
        Value::GotPcrel | Value::GotPagePcrel | Value::Got | Value::GotOffset
    )
}

/// The file-initial `width`-byte field at `addr`, which `region` holds whole.
fn read_field(region: &MemRegion, addr: u64, width: usize, endian_le: bool) -> u64 {
    let off = (addr - region.start_addr()) as usize;
    let field = &region.raw()[off..off + width];
    let mut raw = 0u64;
    if endian_le {
        for (n, &b) in field.iter().enumerate() {
            raw |= u64::from(b) << (8 * n);
        }
    } else {
        for &b in field {
            raw = (raw << 8) | u64::from(b);
        }
    }
    raw
}

/// A Thumb-2 instruction read as one little-endian word, with its halfwords
/// swapped to put the first one high, or back.
fn swap_halfwords(word: u64) -> u64 {
    ((word & 0xffff) << 16) | ((word >> 16) & 0xffff)
}

/// The real symbol index of a MIPS64 relocation whose `r_info` `object`
/// transposed, or `None` when the reported index is already right.
fn mips_corrected_symbol(
    reloc: &object::Relocation,
    arch: object::Architecture,
    endian_le: bool,
) -> Option<object::read::SymbolIndex> {
    if !matches!(arch, A::Mips64) || !endian_le || !reloc.has_implicit_addend() {
        return None;
    }
    let (r_sym, _, _) = mips_reloc_parts(reloc, arch, endian_le)?;
    (r_sym != 0).then_some(object::read::SymbolIndex(r_sym as usize))
}

/// A relocation's resolved symbol.
#[derive(Clone, Copy)]
struct Target {
    /// `S`.
    addr: u64,
    /// Its symbol-table index, which picks its GOT slot.
    index: usize,
    /// The ISA an ARM function is entered in.
    isa: Isa,
    /// `STB_LOCAL`.
    local: bool,
    /// `STB_WEAK` and undefined, so the link or the module loader resolves it
    /// to 0 as readily as to an address and `if (&w)` can go either way. The
    /// synthetic extern address stands in only where it cannot be observed,
    /// everything else being left uncomputed rather than folded one way.
    weak_undefined: bool,
    /// MIPS `_gp_disp`.
    gp_disp: bool,
    /// Defined in `.opd`, so `S` is a ppc64 ELFv1 descriptor.
    in_opd: bool,
    /// How far past `S` a ppc64 ELFv2 local call enters.
    ppc64_local_entry: u64,
}

/// `reloc`'s `RelocationTarget::Symbol`.
///
/// Dispatches through `obj.dynamic_symbol_table()` first and falls back to the
/// static `.symtab` only when there is none. Indices in the dynamic table
/// reference `.dynsym`, so `obj.symbol_by_index` alone would return the wrong
/// entry; ET_REL's per-section relocations reference `.symtab`, which is what
/// the fallback resolves.
///
/// The address is rebased through `layout`: an ET_REL symbol's `st_value` is
/// an offset into its section, every section after the first at a given
/// `sh_addr` sits at a synthetic base, and an undefined or `SHN_COMMON` one
/// resolves to its [`ElfSectionLayout::extern_address`].
///
/// `None` when the index doesn't resolve (malformed ELF), when its `st_shndx`
/// names no section header, when a linked image's symbol is undefined with a
/// zero value or `SHN_COMMON`, or when the target isn't a `Symbol`.
///
/// [`ElfSectionLayout::extern_address`]: super::sections::ElfSectionLayout::extern_address
fn resolve_symbol_target(
    obj: &object::File<'_>,
    layout: &super::sections::ElfSectionLayout,
    reloc: &object::Relocation,
    endian_le: bool,
) -> Option<Target> {
    let RelocationTarget::Symbol(idx) = mips_corrected_symbol(reloc, obj.architecture(), endian_le)
        .map_or_else(|| reloc.target(), RelocationTarget::Symbol)
    else {
        return None;
    };
    // An ET_REL's per-section `SHT_REL`/`SHT_RELA` indexes the `.symtab` its
    // `sh_link` names, never the dynamic table. `object`'s
    // `dynamic_symbol_table` is simply the first `SHT_DYNSYM` section in the
    // file, populated whatever the `e_type`, so an object file carrying one
    // otherwise sends every relocation to the wrong table and patches in an
    // unrelated symbol's `st_value`.
    let relocatable = obj.kind() == object::ObjectKind::Relocatable;
    let sym = obj
        .dynamic_symbol_table()
        .filter(|_| !relocatable)
        .map_or_else(|| obj.symbol_by_index(idx), |t| t.symbol_by_index(idx))
        .ok()?;
    let addr = match layout.extern_address(idx.0) {
        Some(addr) => addr,
        None => {
            // `(0, undefined)` is a legitimate undefined or weak extern of a
            // linked image, tested on the raw `st_value`, which is what
            // "undefined" is expressed in.
            if sym.address() == 0 && sym.is_undefined() {
                return None;
            }
            // `SHN_COMMON` holds the symbol's alignment in `st_value`; its
            // address exists only once the link allocates it in `.bss`.
            if sym.is_common() {
                return None;
            }
            // An `st_shndx` past the section table: the offset it declares has
            // no base, so there is no address to patch.
            layout.try_symbol_address(&sym)?
        }
    };
    let arch = obj.architecture();
    let isa = match (arch, sym.kind()) {
        (A::Arm, object::SymbolKind::Text) if !sym.is_undefined() => {
            if sym.address() & 1 == 1 {
                Isa::Thumb
            } else {
                Isa::Arm
            }
        }
        _ => Isa::Same,
    };
    let st_other = match sym.flags() {
        object::SymbolFlags::Elf { st_other, .. } => st_other,
        _ => 0,
    };
    Some(Target {
        addr,
        index: idx.0,
        isa,
        local: sym.is_local(),
        weak_undefined: sym.is_weak() && sym.is_undefined(),
        gp_disp: matches!(arch, A::Mips | A::Mips64) && sym.name() == Ok("_gp_disp"),
        // ppc64 ELFv1 only, and the arch test comes first: the name lookup
        // below reads `.shstrtab` and this runs once per relocation, so on
        // every other arch it was a string compare per relocation for a field
        // nothing reads. `gp_disp` above short-circuits the same way.
        in_opd: arch == A::PowerPc64
            && sym
                .section_index()
                .and_then(|i| obj.section_by_index(i).ok())
                .is_some_and(|sec| sec.name() == Ok(".opd")),
        ppc64_local_entry: if arch == A::PowerPc64 && !sym.is_undefined() {
            ppc64_local_entry_offset(st_other)
        } else {
            0
        },
    })
}

/// `PPC64_LOCAL_ENTRY_OFFSET`: bytes from a function's global entry to its
/// local one, from `st_other`'s top three bits.
fn ppc64_local_entry_offset(st_other: u8) -> u64 {
    match st_other >> 5 {
        v @ 2..=6 => ((1u64 << v) >> 2) << 2,
        _ => 0,
    }
}

/// Matches the `R_*_RELATIVE` / `R_*_IRELATIVE` families: image-base + addend
/// relocations with no symbol or section target. Yields
/// `(value_to_write, size_bytes)`; image base is modelled as 0, so the value is
/// the addend itself, and 0 for an `SHT_REL` site whose A is in the field.
///
/// `r_type` constants collide across arches (`R_X86_64_RELATIVE` and
/// `R_386_RELATIVE` are both 8), so dispatch is on `Architecture` first and
/// `r_type` only against that arch's constant.
///
/// IRELATIVE is the IFUNC variant, whose addend is the address of a resolver
/// the dynamic linker would call to compute the slot's runtime value. Writing
/// the resolver's address is the soundest static approximation, so it is
/// treated exactly like RELATIVE.
fn image_relative_reloc(
    reloc: &object::Relocation,
    arch: object::Architecture,
    endian_le: bool,
) -> Option<(u64, usize)> {
    // These arrive with an `Absolute` target and an `Unknown` kind (`object`
    // doesn't enumerate them), so the raw type code is all there is to go on.
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    // `R_MIPS_REL32` is MIPS's closest analogue to RELATIVE and writes `S + A`.
    // For an undefined / index-0 (STN_UNDEF) symbol `S` is 0, reducing it to
    // addend-only, which is the case handled here; the `Symbol`-target gate
    // bails out to the defined-symbol path in the main loop. MIPS defines no
    // separate IRELATIVE.
    if let Some((r_sym, mips_type, mips_type2)) = mips_reloc_parts(reloc, arch, endian_le)
        && mips_type == object::elf::R_MIPS_REL32
        && r_sym == 0
    {
        return Some((
            apply_addend(0, reloc.addend()),
            mips_rel32_field_bytes(mips_type2),
        ));
    }
    let size_bytes = match arch {
        A::X86_64
            if r_type == object::elf::R_X86_64_RELATIVE
                || r_type == object::elf::R_X86_64_IRELATIVE =>
        {
            8
        }
        A::I386
            if r_type == object::elf::R_386_RELATIVE || r_type == object::elf::R_386_IRELATIVE =>
        {
            4
        }
        A::Aarch64
            if r_type == object::elf::R_AARCH64_RELATIVE
                || r_type == object::elf::R_AARCH64_IRELATIVE =>
        {
            8
        }
        A::Arm
            if r_type == object::elf::R_ARM_RELATIVE || r_type == object::elf::R_ARM_IRELATIVE =>
        {
            4
        }
        // `R_PPC64_RELATIVE == R_PPC_RELATIVE == 22`; the arch dispatch is what
        // separates them. PPC64 ET_DYN binaries use these for function-pointer
        // tables, which would otherwise read zero.
        A::PowerPc64
            if r_type == object::elf::R_PPC64_RELATIVE
                || r_type == object::elf::R_PPC64_IRELATIVE =>
        {
            8
        }
        A::PowerPc
            if r_type == object::elf::R_PPC_RELATIVE || r_type == object::elf::R_PPC_IRELATIVE =>
        {
            4
        }
        _ => return None,
    };
    Some((apply_addend(0, reloc.addend()), size_bytes))
}

/// Matches `R_*_GLOB_DAT` (GOT data slot) and `R_*_JUMP_SLOT` (PLT lazy-bind
/// slot): write the symbol's address S at the site, no PC subtraction, resolved
/// eagerly.
///
/// `object` reports both as `Unknown` with `size = 0`, so the size comes from
/// the arch instead: 8 bytes on 64-bit, 4 on 32-bit. The caller computes the
/// value (`target_addr + addend`).
fn got_or_plt_slot_reloc_size(
    reloc: &object::Relocation,
    arch: object::Architecture,
    endian_le: bool,
) -> Option<usize> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    if let Some((_, mips_type, _)) = mips_reloc_parts(reloc, arch, endian_le) {
        // A GOT slot is one target word wide regardless of the composite.
        return (mips_type == object::elf::R_MIPS_GLOB_DAT
            || mips_type == object::elf::R_MIPS_JUMP_SLOT)
            .then_some(if matches!(arch, A::Mips64) { 8 } else { 4 });
    }
    match arch {
        A::X86_64
            if r_type == object::elf::R_X86_64_GLOB_DAT
                || r_type == object::elf::R_X86_64_JUMP_SLOT =>
        {
            Some(8)
        }
        A::I386
            if r_type == object::elf::R_386_GLOB_DAT || r_type == object::elf::R_386_JMP_SLOT =>
        {
            Some(4)
        }
        A::Aarch64
            if r_type == object::elf::R_AARCH64_GLOB_DAT
                || r_type == object::elf::R_AARCH64_JUMP_SLOT =>
        {
            Some(8)
        }
        A::Arm
            if r_type == object::elf::R_ARM_GLOB_DAT || r_type == object::elf::R_ARM_JUMP_SLOT =>
        {
            Some(4)
        }
        A::PowerPc64
            if r_type == object::elf::R_PPC64_GLOB_DAT
                || r_type == object::elf::R_PPC64_JMP_SLOT =>
        {
            Some(8)
        }
        A::PowerPc
            if r_type == object::elf::R_PPC_GLOB_DAT || r_type == object::elf::R_PPC_JMP_SLOT =>
        {
            Some(4)
        }
        _ => None,
    }
}

/// `R_PPC_REL32` / `R_ARM_REL32` / `R_PPC64_REL64`: a PC-relative `S + A - P`
/// one target word wide.
///
/// `object` maps none of them (its `EM_ARM` table carries only `R_ARM_ABS32`,
/// and its PowerPC tables only the `ADDR` forms), so all three arrive as
/// `Unknown` and would fall through as unsupported. A `.rodata` switch table is
/// built out of exactly these on PowerPC, and leaving it unpatched costs the
/// whole table.
fn pc_relative_word_reloc(reloc: &object::Relocation, arch: object::Architecture) -> Option<usize> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    match arch {
        // `R_PPC64_REL32` shares the value 26 with `R_PPC_REL32`, and a 64-bit
        // object builds its switch tables out of it the same way.
        A::PowerPc | A::PowerPc64 if r_type == object::elf::R_PPC_REL32 => Some(4),
        A::PowerPc64 if r_type == object::elf::R_PPC64_REL64 => Some(8),
        A::Arm if r_type == object::elf::R_ARM_REL32 => Some(4),
        _ => None,
    }
}

/// The real `(r_sym, r_type, r_type2)` of a MIPS relocation.
///
/// MIPS64 packs `r_info` as `r_sym:32 | r_ssym:8 | r_type3:8 | r_type2:8 |
/// r_type:8`, a composite of up to three relocations applied in sequence.
/// `object` un-transposes that for `Elf64_Rela` but reads a little-endian
/// `Elf64_Rel` as one little-endian `u64`, which swaps the halves: the reported
/// type is the real `r_sym`, and the reported symbol index is the type word
/// with its bytes reversed. mips64el's `.rel.dyn` is exactly that case.
///
/// MIPS32 has the single 8-bit type, reported as `r_type2 = R_MIPS_NONE`.
/// `r_sym == 0` is STN_UNDEF, i.e. the addend-only half of `R_MIPS_REL32`.
fn mips_reloc_parts(
    reloc: &object::Relocation,
    arch: object::Architecture,
    endian_le: bool,
) -> Option<(u32, u32, u32)> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    let reported_sym = match reloc.target() {
        RelocationTarget::Symbol(idx) => u32::try_from(idx.0).ok()?,
        _ => 0,
    };
    match arch {
        A::Mips => Some((reported_sym, r_type, object::elf::R_MIPS_NONE)),
        A::Mips64 => {
            let (r_sym, word) = if endian_le && reloc.has_implicit_addend() {
                (r_type, reported_sym.swap_bytes())
            } else {
                (reported_sym, r_type)
            };
            Some((r_sym, word & 0xff, (word >> 8) & 0xff))
        }
        _ => None,
    }
}

/// The MIPS64 `r_type3 << 16 | r_type2 << 8 | r_type` word, MIPS32's single
/// type.
fn mips_type_word(
    reloc: &object::Relocation,
    arch: object::Architecture,
    endian_le: bool,
) -> Option<u32> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    match arch {
        A::Mips => Some(r_type),
        A::Mips64 if endian_le && reloc.has_implicit_addend() => match reloc.target() {
            RelocationTarget::Symbol(idx) => {
                Some(u32::try_from(idx.0).ok()?.swap_bytes() & 0x00ff_ffff)
            }
            _ => Some(0),
        },
        A::Mips64 => Some(r_type & 0x00ff_ffff),
        _ => None,
    }
}

/// `R_MIPS_REL32`'s field width. "REL32" names a 32-bit field, but MIPS64
/// linkers emit it composed with `R_MIPS_64`, and that pair is the 64-bit
/// pointer slot glibc's `ld.so` patches as one word.
fn mips_rel32_field_bytes(r_type2: u32) -> usize {
    if r_type2 == object::elf::R_MIPS_64 {
        8
    } else {
        4
    }
}

/// Where a MIPS relocation's field starts relative to `r_offset`.
///
/// `R_MIPS_16`'s storage unit is the 32-bit word at `r_offset` and its field is
/// that word's low half, so on a big-endian target the two bytes to patch,
/// and the implicit addend to read back, start two bytes in. `R_MIPS_32` and
/// `R_MIPS_64`, the only other MIPS types `object` gives a width, fill their
/// storage unit exactly.
fn mips_half_field_skew(
    reloc: &object::Relocation,
    arch: object::Architecture,
    endian_le: bool,
) -> u64 {
    if endian_le {
        return 0;
    }
    match mips_reloc_parts(reloc, arch, endian_le) {
        Some((_, r_type, _)) if r_type == object::elf::R_MIPS_16 => 2,
        _ => 0,
    }
}

/// Matches the defined-symbol half of `R_MIPS_REL32`, which `object` surfaces
/// as a `Symbol` target and which needs the symbol's address. The undefined
/// half (`S = 0`, addend-only) goes through [`image_relative_reloc`].
fn mips_rel32_symbol_reloc_size(
    reloc: &object::Relocation,
    arch: object::Architecture,
    endian_le: bool,
) -> Option<usize> {
    let (r_sym, r_type, r_type2) = mips_reloc_parts(reloc, arch, endian_le)?;
    (r_type == object::elf::R_MIPS_REL32 && r_sym != 0).then(|| mips_rel32_field_bytes(r_type2))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-applying over one region set overwrites the patch list rather than
    /// appending to it, so the bytes a second pass serves are the first pass's.
    /// Only reachable from inside the crate: a caller gets its relocations
    /// through [`crate::OwnedElf::regions`], which loads a fresh set each time.
    #[test]
    fn applying_twice_serves_what_applying_once_served() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/out/x64/elf_relocs.elf");
        assert!(
            path.exists(),
            "{} is not built; run `make -C fixtures`",
            path.display()
        );
        let filter = super::super::sections::LoadFilter::AllAllocatable;
        let owned = crate::OwnedElf::open(&path).expect("open");
        let obj = owned.checked_file().expect("the mapped file is unchanged");
        let layout = super::super::sections::ElfSectionLayout::new(&obj);
        let image = owned
            .regions_with(
                &obj,
                &layout,
                super::super::sections::RegionSource::Auto,
                filter,
                false,
            )
            .expect("regions");
        let (mut regions, writable, executable) = (image.regions, image.writable, image.executable);

        let bytes = |regions: &[MemRegion]| -> Vec<Vec<u8>> {
            regions
                .iter()
                .map(|r| {
                    let mut out = vec![0u8; (r.end_addr() - r.start_addr()) as usize];
                    r.read(r.start_addr(), &mut out);
                    out
                })
                .collect()
        };
        apply_elf_relocations_with(&mut regions, &obj, filter, &layout, &writable, &executable)
            .expect("apply");
        let once = bytes(&regions);
        apply_elf_relocations_with(&mut regions, &obj, filter, &layout, &writable, &executable)
            .expect("re-apply");
        assert_eq!(once, bytes(&regions));
    }

    /// Which region serves a read depends on the read's WIDTH, so a field
    /// straddling an inner region's end has to read back patched at every
    /// width: every region overlapping it keeps the patch.
    #[test]
    fn a_field_over_an_inner_regions_end_reads_back_patched_at_every_width() {
        let field = 0x1084;
        let value = 0xdead_beef_feed_face_u64;
        let mut regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
            MemRegion::new(0x1080, vec![0u8; 8]).unwrap(),
        ];
        let all: Arc<[Patch]> =
            vec![Patch::new(field, value, 8, true).expect("an 8 byte field")].into();
        for region in &mut regions {
            region.attach_patches(&all);
        }

        let table = crate::MemRegionsLookupTable::new(regions);
        let mut narrow = [0u8; 4];
        table
            .read_exact(field, &mut narrow)
            .expect("the inner region");
        let mut wide = [0u8; 8];
        table
            .read_exact(field, &mut wide)
            .expect("the outer region");
        assert_eq!(wide, value.to_le_bytes());
        assert_eq!(narrow, wide[..4]);
    }

    /// A field starting below a region's start still lands in it: a region
    /// keeps the patches up to `MAX_PATCH_LEN - 1` bytes before it, so the
    /// inner region serves the field's tail patched.
    #[test]
    fn a_field_over_an_inner_regions_start_reads_back_patched() {
        let field = 0x107c;
        let value = 0xdead_beef_feed_face_u64;
        let mut regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
            MemRegion::new(0x1080, vec![0u8; 8]).unwrap(),
        ];
        let all: Arc<[Patch]> =
            vec![Patch::new(field, value, 8, true).expect("an 8 byte field")].into();
        for region in &mut regions {
            region.attach_patches(&all);
        }

        let table = crate::MemRegionsLookupTable::new(regions);
        let mut tail = [0u8; 4];
        table
            .read_exact(0x1080, &mut tail)
            .expect("the inner region");
        assert_eq!(tail, value.to_le_bytes()[4..]);
    }
}
