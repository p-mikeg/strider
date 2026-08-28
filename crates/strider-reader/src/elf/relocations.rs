//! ELF relocation application, as a per-region patch list applied when a read
//! crosses a site.
//!
//! An image that has not been through its linker, or through `ld.so`, leaves
//! every cross-reference field at zero. An ET_REL's `call rel32` sits as
//! `e8 00 00 00 00` under an `R_X86_64_PLT32` the linker would resolve; an
//! ET_DYN's dispatch-table slots and GOT/PLT entries sit at zero under
//! `.rela.dyn` / `.rela.plt` until `ld.so` fills them. Unpatched, the analyser
//! follows rel32 = 0 as control flow into the next instruction (call site + 5)
//! and reads every table slot as a null pointer. This replicates both
//! statically.
//!
//! Unrecognised relocation kinds and unknown architectures are skipped
//! silently rather than mis-patched.

use object::{
    Architecture as A, Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationFlags,
    RelocationKind, RelocationTarget,
};

use crate::{MemRegion, Patch, Result};

/// Adds a possibly-negative relocation `addend` to a base address.
///
/// A negative addend casts to its 2's-complement bit pattern; `wrapping_add`
/// plus `Patch::new`'s truncation to the field width give the modular result
/// every relocation field expects.
#[inline]
fn apply_addend(base: u64, addend: i64) -> u64 {
    base.wrapping_add(addend as u64)
}

/// The relocation's `A`, `covering` being the region the field sits in.
///
/// `SHT_REL` tables carry no `r_addend` field and store A in the relocation
/// field itself; `object` reports `r_addend = 0` for them, so it is read back
/// out of the site. A site no region covers contributes A = 0, matching the
/// patch that is likewise skipped.
///
/// The field is read file-initial, unpatched: no linker emits two relocations
/// against one field.
fn reloc_addend(
    reloc: &object::Relocation,
    regions: &[MemRegion],
    covering: Option<usize>,
    site_addr: u64,
    size_bytes: usize,
    endian_le: bool,
) -> i64 {
    if !reloc.has_implicit_addend() {
        return reloc.addend();
    }
    let Some(region) = covering.map(|i| &regions[i]) else {
        return 0;
    };
    let off = (site_addr - region.start_addr()) as usize;
    let field = &region.raw()[off..off + size_bytes];
    // Read by hand rather than through `Endianness::read_uint`: this crate
    // depends only on `read-only-memory`, and pulling in `strider-target` for
    // one loop would put a back-edge in the dependency graph.
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
    // Sign-extend from the field width: a PC-relative site holds a negative A
    // (-4 for an x86 `call rel32`).
    let shift = 64 - 8 * size_bytes as u32;
    ((raw << shift) as i64) >> shift
}

/// Installs a relocation patch list on each region, walking the table
/// appropriate for `obj.kind()`: `obj.dynamic_relocations()` for ET_EXEC /
/// ET_DYN, per-section `section.relocations()` for ET_REL.
///
/// # Supported
///
/// Via [`RelocationKind`]:
/// * `Absolute`, `S + A` (`R_X86_64_64`). Symbol-targeted.
/// * `Relative`, `S + A - P` (`R_X86_64_PC32`, `R_AARCH64_PREL16/PREL32/PREL64`,
///   `R_386_PC32`). Symbol-targeted.
/// * `PltRelative`, valued the same as `Relative` (no PLT is materialised, so
///   the symbol's own address is used): the 32-bit `R_X86_64_PLT32` /
///   `R_386_PLT32` apply. `R_AARCH64_CALL26` also arrives as `PltRelative`, but
///   its 26-bit field fails the byte-width gate and is left unpatched
///   (branch-immediate encodings are not modelled).
///
/// Via raw `r_type`, which `object` reports as `RelocationKind::Unknown`:
/// * `R_*_RELATIVE` / `R_*_IRELATIVE`: `image_base + addend`, image base
///   modelled as 0.
/// * `R_*_GLOB_DAT` / `R_*_JUMP_SLOT`: the symbol's address at the slot
///   (S semantics, resolved eagerly).
/// * `R_MIPS_REL32`, both the undefined (addend-only) and defined-symbol forms.
/// * `R_ARM_REL32` / `R_PPC_REL32` / `R_PPC64_REL64`, plain word-sized
///   `S + A - P`. `object` surfaces all three as `Unknown` with `size = 0`, so
///   they dispatch on the raw `r_type` like the GOT/PLT slots; a PowerPC
///   `.rodata` switch table is built out of exactly these.
///
/// # Not supported
///
/// * `Got` / `GotRelative` / `GotBaseRelative` / `GotBaseOffset`: would need a
///   synthesised GOT section, which is never allocated.
/// * Encodings that don't fit a plain low-bytes-at-offset field: Thumb
///   branches, AArch64 ADR_PREL_PG_HI21 + ADD_ABS_LO12_NC pairs, MIPS HI16/LO16
///   splits, PPC TOC relocations. Most arrive as `Unknown` with `size = 0`;
///   the rest carry a byte-multiple size with a non-plain
///   [`object::RelocationEncoding`] (s390x's `*DBL` halved displacements,
///   `R_LARCH_B16`, the SHARC instruction fields) and are rejected on that.
/// * Every mips64el `SHT_REL` type but `R_MIPS_REL32` / `R_MIPS_GLOB_DAT` /
///   `R_MIPS_JUMP_SLOT`: `object` transposes that table's `r_info`, so the
///   reported kind and size describe the symbol index instead of the type.
///
/// Everything unsupported is skipped silently, leaving the site at its
/// file-initial bytes.
///
/// Any patch list the regions already carry is replaced.
///
/// # Errors
///
/// Only when a loaded section's bytes cannot be read. A relocation whose target
/// symbol or section index does not resolve is NOT an error (neither a
/// legitimate `STN_UNDEF` for an external lib nor a corrupt index); it is
/// skipped, leaving the site at its file-initial bytes.
pub fn apply_elf_relocations(
    regions: &mut [MemRegion],
    obj: &object::File<'_>,
    loaded_with: super::sections::LoadFilter,
) -> Result<()> {
    let layout = super::sections::ElfSectionLayout::new(obj);
    apply_elf_relocations_with(regions, obj, loaded_with, &layout)
}

/// [`apply_elf_relocations`] over the layout built for `obj`.
///
/// # Errors
///
/// Same as [`apply_elf_relocations`].
pub(crate) fn apply_elf_relocations_with(
    regions: &mut [MemRegion],
    obj: &object::File<'_>,
    loaded_with: super::sections::LoadFilter,
    layout: &super::sections::ElfSectionLayout,
) -> Result<()> {
    let owners = super::sections::loaded_section_indices(obj, layout, loaded_with)?;
    // One lookup per relocation instead of a scan of every region; an ET_REL
    // carries one region per SHF_ALLOC section.
    let region_index = RegionStartIndex::from_regions(regions);

    let mut patches: Vec<Vec<Patch>> = vec![Vec::new(); regions.len()];
    for_each_reloc_site(obj, &owners, layout, |site_addr, avail, reloc| {
        apply_one_relocation(
            obj,
            layout,
            regions,
            &region_index,
            &mut patches,
            RelocSite {
                addr: site_addr,
                avail,
            },
            reloc,
        );
        Ok(())
    })?;
    for (region, patches) in regions.iter_mut().zip(patches) {
        region.set_patches(patches);
    }
    Ok(())
}

/// A relocation site: where the field lands, and how many bytes of the section
/// owning it remain from there.
#[derive(Clone, Copy)]
struct RelocSite {
    addr: u64,
    avail: u64,
}

/// Invokes `f(site_addr, avail, reloc)` per relocation site, `site_addr` being
/// the **absolute** virtual address in the coordinate system the loaded regions
/// live in and `avail` the bytes left of the section owning the site
/// (`u64::MAX` for a dynamic site, whose owner is a segment and which therefore
/// has no section end to overrun).
///
/// Kind dispatch:
///
/// * ET_REL: per-section tables, restricted to `owners`. A section's
///   `relocations()` yields `(r_offset, Relocation)` where `r_offset` is
///   relative to the section the relocations apply *to* (the one `sh_info`
///   points at), so the site is that section's `layout` base plus the offset.
/// * Everything else: the dynamic table, `owners` and `layout` unused.
///   `dynamic_relocations()` is `Some` for every ELF, but its iterator yields
///   nothing unless an `SHT_REL` / `SHT_RELA` section links to `.dynsym`, so a
///   statically-linked image walks no sites.
fn for_each_reloc_site<F>(
    obj: &object::File<'_>,
    owners: &std::collections::BTreeSet<usize>,
    layout: &super::sections::ElfSectionLayout,
    mut f: F,
) -> Result<()>
where
    F: FnMut(u64, u64, &object::Relocation) -> Result<()>,
{
    match obj.kind() {
        object::ObjectKind::Relocatable => {
            for sec in obj.sections() {
                if !owners.contains(&sec.index().0) {
                    continue;
                }
                let sec_base = layout.section_base(&sec);
                let sec_size = sec.size();
                for (offset, reloc) in sec.relocations() {
                    // The gABI puts `r_offset` inside the section `sh_info`
                    // names.  A malformed object can point it past the end,
                    // where the site still lands in SOME loaded region and
                    // would silently patch an unrelated section's bytes.
                    // `reloc.size()` is 0 for every type dispatched on the raw
                    // `r_type`, so the budget travels to where the width is
                    // actually chosen instead of being checked here.
                    let avail = sec_size.saturating_sub(offset);
                    f(sec_base.wrapping_add(offset), avail, &reloc)?;
                }
            }
        }
        _ => {
            let Some(dyn_relocs) = obj.dynamic_relocations() else {
                return Ok(());
            };
            for (site_addr, reloc) in dyn_relocs {
                f(site_addr, u64::MAX, &reloc)?;
            }
        }
    }
    Ok(())
}

/// Applies one relocation entry at the already-absolute `site_addr`.
///
/// Anything that can't be resolved or patched (weak externs, malformed targets,
/// unsupported kinds, sites with no backing region) is silently skipped.
fn apply_one_relocation(
    obj: &object::File<'_>,
    layout: &super::sections::ElfSectionLayout,
    regions: &[MemRegion],
    region_index: &RegionStartIndex,
    patches: &mut [Vec<Patch>],
    site: RelocSite,
    reloc: &object::Relocation,
) {
    let RelocSite {
        addr: site_addr,
        avail,
    } = site;
    let endian_le = matches!(obj.endianness(), object::Endianness::Little);
    // `avail` is what is left of the section owning the site. A field running
    // past it belongs to no section and would patch a neighbour's bytes.
    let covering = |field_addr: u64, size_bytes: usize| {
        let used = (field_addr - site_addr) + size_bytes as u64;
        if used > avail {
            return Vec::new();
        }
        region_index.covering_indices(regions, field_addr, size_bytes)
    };

    // Image-relative relocations store `image_base + addend` with no symbol or
    // section reference, so `object` surfaces them as an `Absolute` target with
    // an `Unknown` kind. Image base is modelled as the link-time base (0 for an
    // ET_DYN), making the patched value the addend itself; width comes from the
    // relocation type. Without this branch a PIE's dispatch-table slot reads
    // its unrelocated file bytes wherever the slot IS mapped; under the
    // read-only filters those slots sit in the RW `PT_LOAD` and are not mapped
    // at all, since `PT_GNU_RELRO` is not modelled.
    if let Some((value, size_bytes)) = image_relative_reloc(reloc, obj.architecture(), endian_le) {
        record_patch(
            patches,
            &covering(site_addr, size_bytes),
            site_addr,
            value,
            size_bytes,
            endian_le,
        );
        return;
    }

    // Symbol-targeted families `object` reports as `Unknown` with `size = 0`,
    // which the general `match reloc.kind()` below would mis-bucket as
    // unsupported. Each yields `(size_bytes, pc_relative,
    // read_implicit_addend)`; first match wins.
    let word_sized = got_or_plt_slot_reloc_size(reloc, obj.architecture(), endian_le)
        // GOT/PLT slot, `S`. Its own field is the PLT push offset, not an
        // addend, so it is never read back; `reloc.addend()` is the RELA one,
        // zero under REL.
        .map(|size_bytes| (size_bytes, false, false))
        // Defined-symbol `R_MIPS_REL32`, `S + A`.
        .or_else(|| {
            mips_rel32_symbol_reloc_size(reloc, obj.architecture(), endian_le)
                .map(|size_bytes| (size_bytes, false, true))
        })
        // `R_PPC_REL32` / `R_ARM_REL32` / `R_PPC64_REL64`, `S + A - P`.
        .or_else(|| {
            pc_relative_word_reloc(reloc, obj.architecture())
                .map(|size_bytes| (size_bytes, true, true))
        });
    if let Some((size_bytes, pc_relative, read_implicit_addend)) = word_sized {
        let Some(target_addr) = resolve_symbol_target(obj, layout, reloc, endian_le) else {
            return;
        };
        let site_regions = covering(site_addr, size_bytes);
        let addend = if read_implicit_addend {
            reloc_addend(
                reloc,
                regions,
                site_regions.first().copied(),
                site_addr,
                size_bytes,
                endian_le,
            )
        } else {
            reloc.addend()
        };
        let value = apply_addend(target_addr, addend);
        record_patch(
            patches,
            &site_regions,
            site_addr,
            if pc_relative {
                value.wrapping_sub(site_addr)
            } else {
                value
            },
            size_bytes,
            endian_le,
        );
        return;
    }

    // mips64el `SHT_REL`: `object` reads `r_info` as one little-endian `u64`, so
    // the `kind` and `size` consulted below come from the real `r_sym`, matched
    // against `R_MIPS_16` / `R_MIPS_32` / `R_MIPS_64` (symbol index 1, 2, 18).
    // Every MIPS relocation handled here dispatches on the raw `r_type` above.
    if matches!(obj.architecture(), A::Mips64) && endian_le && reloc.has_implicit_addend() {
        return;
    }

    // `object`'s ELF `parse_relocation` yields only `Symbol` or `Absolute`;
    // the latter (an immediate with no symbol) and any future variant fall
    // through as unsupported.
    let RelocationTarget::Symbol(_) = reloc.target() else {
        return;
    };
    let Some(target_addr) = resolve_symbol_target(obj, layout, reloc, endian_le) else {
        return;
    };

    // `size` is in bits, and 0 nominally means "the kind's default". Absolute /
    // Relative / PltRelative all set it explicitly on every arch of interest,
    // so 0 here signals an arch-specific encoding (ARM Thumb branch, ...) that
    // isn't modelled. The width is needed before the value: an `SHT_REL` addend
    // is read back out of the field.
    let size_bits = reloc.size();
    if size_bits == 0 || !size_bits.is_multiple_of(8) || size_bits > 64 {
        return;
    }
    let size_bytes = (size_bits / 8) as usize;

    // A byte-multiple size still does not make the field plain low bytes at the
    // offset: s390x's `*DBL` types hold `(S + A - P) >> 1`, `R_LARCH_B16` a
    // branch displacement, the SHARC `*_V3` family instruction-encoded operands.
    // `X86Signed` is the one non-`Generic` encoding that IS plain: it only names
    // the sign extension `R_X86_64_32S` applies at runtime.
    if !matches!(
        reloc.encoding(),
        object::RelocationEncoding::Generic | object::RelocationEncoding::X86Signed
    ) {
        return;
    }

    // `r_offset` addresses the storage unit, inside which the field can be
    // offset. A `r_offset` at the very top of the address space is malformed;
    // the skew would carry the field out of it.
    let Some(field_addr) =
        site_addr.checked_add(mips_half_field_skew(reloc, obj.architecture(), endian_le))
    else {
        return;
    };

    let site_regions = covering(field_addr, size_bytes);
    let addend = reloc_addend(
        reloc,
        regions,
        site_regions.first().copied(),
        field_addr,
        size_bytes,
        endian_le,
    );
    // S, A, P follow the System V ABI generic relocation formula:
    // S = target_addr, A = addend, P = site_addr. P is the storage unit, not
    // the field inside it. `PltRelative`'s L collapses to S here since no PLT
    // is materialised.
    let value = match reloc.kind() {
        RelocationKind::Absolute => apply_addend(target_addr, addend),
        RelocationKind::Relative | RelocationKind::PltRelative => {
            apply_addend(target_addr, addend).wrapping_sub(site_addr)
        }
        _ => return,
    };

    record_patch(
        patches,
        &site_regions,
        field_addr,
        value,
        size_bytes,
        endian_le,
    );
}

/// Start-keyed index over a set of `MemRegion`s, answering which region fully
/// covers a `[site, site + len)` field: one entry list sorted by `start`,
/// binary-searched.
///
/// Same-start collapse (last-insert-wins, mirroring
/// [`crate::MemRegionsLookupTable`]) applies.
struct RegionStartIndex {
    /// Sorted by `start`; equal-start entries hold insertion order, so the
    /// last-inserted is last within its run.
    entries: Vec<IndexEntry>,
}

struct IndexEntry {
    start: u64,
    /// Highest `end_addr` of this entry and every lower-`start` one.
    max_end: u64,
    /// Index into the region slice.
    index: usize,
}

impl RegionStartIndex {
    /// Equal-start regions keep their slice order, so the higher-index one is
    /// the only one [`covering_indices`](Self::covering_indices) tests.
    fn from_regions(regions: &[MemRegion]) -> Self {
        let mut order: Vec<usize> = (0..regions.len()).collect();
        order.sort_by_key(|&i| regions[i].start_addr());
        let mut max_end = 0u64;
        let entries = order
            .into_iter()
            .map(|index| {
                max_end = max_end.max(regions[index].end_addr());
                IndexEntry {
                    start: regions[index].start_addr(),
                    max_end,
                    index,
                }
            })
            .collect();
        Self { entries }
    }

    /// Slice indices of EVERY region fully covering
    /// `[site_addr, site_addr + size_bytes)`, highest `start` first.
    ///
    /// All of them, because a read is served by whichever region covers the
    /// REQUEST: with overlapping regions a wide read falls through to an outer
    /// one, which would serve unpatched bytes if only the winner were patched.
    ///
    /// Walks candidates from the highest `start <= site_addr` downward, so a
    /// field straddling a shorter higher-start region's end still resolves to
    /// a fully-covering lower-start region. Among entries sharing a `start`
    /// only the last-inserted is tested. The walk stops once `max_end` drops
    /// below the field's end, so an uncovered site (every `.got` relocation
    /// when only the immutable image is loaded) costs the binary search alone.
    fn covering_indices(
        &self,
        regions: &[MemRegion],
        site_addr: u64,
        size_bytes: usize,
    ) -> Vec<usize> {
        let mut out = Vec::new();
        let Some(site_end) = site_addr.checked_add(size_bytes as u64) else {
            return out;
        };
        let upper = self.entries.partition_point(|e| e.start <= site_addr);
        let mut prev_start: Option<u64> = None;
        for entry in self.entries[..upper].iter().rev() {
            if entry.max_end < site_end {
                break;
            }
            // Keep only the first (last-inserted) entry of each equal-start run.
            if prev_start == Some(entry.start) {
                continue;
            }
            prev_start = Some(entry.start);
            if regions[entry.index].fully_covers(site_addr, size_bytes) {
                out.push(entry.index);
            }
        }
        out
    }
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

/// Address of `reloc`'s `RelocationTarget::Symbol` index.
///
/// Dispatches through `obj.dynamic_symbol_table()` first and falls back to the
/// static `.symtab` only when there is none. Indices in the dynamic table
/// reference `.dynsym`, so `obj.symbol_by_index` alone would return the wrong
/// entry; ET_REL's per-section relocations reference `.symtab`, which is what
/// the fallback resolves.
///
/// The address is rebased through `layout`: an ET_REL symbol's `st_value` is
/// an offset into its section, and every section after the first at a given
/// `sh_addr` sits at a synthetic base.
///
/// `None` (caller skips the relocation) when the index doesn't resolve
/// (malformed ELF), when it resolves to the legitimate weak / undef case
/// (`address == 0 && is_undefined`), when the symbol is an unallocated
/// `SHN_COMMON`, or when the target isn't a `Symbol`.
fn resolve_symbol_target(
    obj: &object::File<'_>,
    layout: &super::sections::ElfSectionLayout,
    reloc: &object::Relocation,
    endian_le: bool,
) -> Option<u64> {
    match mips_corrected_symbol(reloc, obj.architecture(), endian_le)
        .map_or_else(|| reloc.target(), RelocationTarget::Symbol)
    {
        RelocationTarget::Symbol(idx) => {
            let resolved = obj
                .dynamic_symbol_table()
                .map_or_else(|| obj.symbol_by_index(idx), |t| t.symbol_by_index(idx))
                .map(|s| {
                    (
                        s.address(),
                        layout.symbol_address(&s),
                        s.is_undefined(),
                        s.is_common(),
                    )
                })
                .ok();
            // `None` is an invalid index (malformed ELF); `(0, true)` is a
            // legitimate undefined or weak extern. Skip either way. Tested on
            // the raw `st_value`, which is what "undefined" is expressed in.
            let (raw, addr, undef, common) = resolved?;
            if raw == 0 && undef {
                return None;
            }
            // `SHN_COMMON` holds the symbol's alignment in `st_value`; its
            // address exists only once the link allocates it in `.bss`.
            if common {
                return None;
            }
            Some(addr)
        }
        _ => None,
    }
}

/// Matches the `R_*_RELATIVE` / `R_*_IRELATIVE` families: image-base + addend
/// relocations with no symbol or section target. Yields
/// `(value_to_write, size_bytes)`; image base is modelled as 0, so the value is
/// the addend itself.
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
    // SHT_REL carries its addend IN the field, and `object` reports r_addend = 0
    // for it. With the image base modelled as 0 the field already holds the
    // answer, so writing `0 + 0` would erase it.
    if reloc.has_implicit_addend() {
        return None;
    }
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

/// Records the low `size_bytes` of `value` at `site_addr`, on every region in
/// `covering`.
///
/// An empty `covering` silently skips: either the site is unmapped, or its
/// field width runs past the end of the region its first byte lands in.
fn record_patch(
    patches: &mut [Vec<Patch>],
    covering: &[usize],
    site_addr: u64,
    value: u64,
    size_bytes: usize,
    endian_le: bool,
) {
    let Some(patch) = Patch::new(site_addr, value, size_bytes, endian_le) else {
        return;
    };
    for &i in covering {
        patches[i].push(patch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field straddling a shorter higher-start region's end must resolve to
    /// the lower-start region that fully covers it, and a site past every
    /// region to nothing.
    #[test]
    fn covering_index_falls_through_to_a_fully_covering_lower_start_region() {
        let regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
            MemRegion::new(0x1080, vec![0u8; 4]).unwrap(),
        ];
        let index = RegionStartIndex::from_regions(&regions);
        assert_eq!(index.covering_indices(&regions, 0x1080, 4), vec![1, 0]);
        assert_eq!(index.covering_indices(&regions, 0x1080, 8), vec![0]);
        assert!(index.covering_indices(&regions, 0x10fc, 8).is_empty());
        assert!(index.covering_indices(&regions, 0x9000, 1).is_empty());
    }

    /// A field two regions both fully cover is patched on both: a read wide
    /// enough to miss the inner one falls through to the outer, which must
    /// serve the same relocated bytes.
    #[test]
    fn covering_indices_reports_every_fully_covering_region() {
        let regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x20]).unwrap(),
            MemRegion::new(0x1010, vec![0u8; 0x08]).unwrap(),
        ];
        let index = RegionStartIndex::from_regions(&regions);
        assert_eq!(index.covering_indices(&regions, 0x1014, 4), vec![1, 0]);
    }
}
