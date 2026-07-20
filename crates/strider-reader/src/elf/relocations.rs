//! ELF relocation application, in place on the loaded `MemRegion`s.
//!
//! ET_DYN binaries ship with unresolved relocations: a `call rel32` to a
//! function in the same image sits as `e8 00 00 00 00` until the runtime loader
//! patches the immediate with `target - rip`. Unpatched, the analyser follows
//! rel32 = 0 as control flow into the next instruction (call site + 5) and
//! prunes the code that only fed the real target. This replicates the loader's
//! work statically.
//!
//! Unrecognised relocation kinds and unknown architectures are skipped
//! silently rather than mis-patched.

use anyhow::Context as _;
use object::{
    Architecture as A, Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationFlags,
    RelocationKind, RelocationTarget,
};

use crate::{MemRegion, Result};

/// Adds a possibly-negative relocation `addend` to a base address.
///
/// The `i64`-to-`u64` cast reinterprets a negative addend as its 2's-complement
/// bit pattern, and `wrapping_add` then gives the correct fixed-width modular
/// result, which is what every relocation field expects given `write_at`
/// truncates to the field width.
#[inline]
fn apply_addend(base: u64, addend: i64) -> u64 {
    base.wrapping_add(addend as u64)
}

/// Patches relocations in `regions` in place, walking the table appropriate for
/// `obj.kind()`: `obj.dynamic_relocations()` for ET_EXEC / ET_DYN, per-section
/// `section.relocations()` for ET_REL.
///
/// # Supported
///
/// Via [`RelocationKind`]:
/// * `Absolute`, `S + A` (`R_X86_64_64`). Symbol-targeted.
/// * `Relative`, `S + A - P` (`R_X86_64_PC32`, `R_X86_64_PC64`,
///   `R_AARCH64_PREL32/PREL64`, `R_386_PC32`). Symbol-targeted.
/// * `PltRelative`, valued the same as `Relative` (no PLT is materialised, so
///   the symbol's own address is used): the 32-bit `R_X86_64_PLT32` /
///   `R_386_PLT32` apply. `R_AARCH64_CALL26` also arrives as `PltRelative`, but
///   its 26-bit field fails the byte-width write gate and is left unpatched
///   (branch-immediate encodings are not modelled).
///
/// Via raw `r_type`, which `object` reports as `RelocationKind::Unknown`:
/// * `R_*_RELATIVE` / `R_*_IRELATIVE`: write `image_base + addend`, image base
///   modelled as 0.
/// * `R_*_GLOB_DAT` / `R_*_JUMP_SLOT`: write the symbol's address at the slot
///   (S semantics, resolved eagerly).
/// * `R_MIPS_REL32`, both the undefined (addend-only) and defined-symbol forms.
///
/// # Not supported
///
/// * `Got` / `GotRelative` / `GotBaseRelative` / `GotBaseOffset`: would need a
///   synthesised GOT section to write into, which is never allocated.
/// * Encodings that don't fit a plain low-bytes-at-offset write: Thumb
///   branches, AArch64 ADR_PREL_PG_HI21 + ADD_ABS_LO12_NC pairs, MIPS HI16/LO16
///   splits, PPC TOC relocations. These arrive as `Unknown` with `size = 0`.
///
/// Everything unsupported is skipped silently, leaving the site at its
/// file-initial bytes.
///
/// # Errors
///
/// Only on a malformed ELF (a relocation whose target symbol or section index
/// doesn't resolve). A legitimate non-resolution such as `STN_UNDEF` for an
/// external lib is not an error; that relocation is skipped.
pub fn apply_elf_relocations(regions: &mut [MemRegion], obj: &object::File<'_>) -> Result<()> {
    // Index once so `locate_and_write`'s per-relocation region lookup is
    // O(log N) rather than a linear `regions.iter().find`, taking the patch
    // loop from O(R*N) to O(R*log N). Matters for ET_REL `.o` files, which
    // carry one region per SHF_ALLOC section.
    let region_index = RegionStartIndex::from_regions(regions);

    for_each_reloc_site(obj, |site_addr, reloc| {
        apply_one_relocation(obj, regions, &region_index, site_addr, reloc);
        Ok(())
    })
}

/// Invokes `f(site_addr, reloc)` per relocation site, `site_addr` being the
/// **absolute** virtual address in the coordinate system the loaded regions
/// live in.
///
/// Kind dispatch:
///
/// * ET_REL: per-section tables. A section's `relocations()` yields
///   `(r_offset, Relocation)` where `r_offset` is relative to the section the
///   relocations apply *to* (the one `sh_info` points at). `sh_addr` is 0 in
///   practice, but `sec.address()` is added explicitly to stay correct for an
///   ET_REL that does set it.
/// * Everything else: the dynamic table. `dynamic_relocations()` is `None` both
///   for ET_REL and for a statically-linked ELF shipping no dynamic table;
///   either way this iterates nothing.
fn for_each_reloc_site<F>(obj: &object::File<'_>, mut f: F) -> Result<()>
where
    F: FnMut(u64, &object::Relocation) -> Result<()>,
{
    match obj.kind() {
        object::ObjectKind::Relocatable => {
            for sec in obj.sections() {
                let sec_base = sec.address();
                for (offset, reloc) in sec.relocations() {
                    f(sec_base.wrapping_add(offset), &reloc)?;
                }
            }
        }
        _ => {
            let Some(dyn_relocs) = obj.dynamic_relocations() else {
                return Ok(());
            };
            for (site_addr, reloc) in dyn_relocs {
                f(site_addr, &reloc)?;
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
    regions: &mut [MemRegion],
    region_index: &RegionStartIndex,
    site_addr: u64,
    reloc: &object::Relocation,
) {
    let endian_le = matches!(obj.endianness(), object::Endianness::Little);

    // Image-relative relocations store `image_base + addend` with no symbol or
    // section reference, so `object` surfaces them as an `Absolute` target with
    // an `Unknown` kind. Image base is modelled as the link-time base (0 for an
    // ET_DYN), making the patched value the addend itself; width comes from the
    // relocation type. Without this branch every PIE binary's dispatch-table
    // slot reads zero post-load.
    if let Some((value, size_bytes)) = image_relative_reloc(reloc, obj.architecture()) {
        locate_and_write(
            regions,
            region_index,
            site_addr,
            value,
            size_bytes,
            endian_le,
        );
        return;
    }

    // GOT/PLT slots and defined-symbol MIPS `R_MIPS_REL32` share `S + A` write
    // semantics and differ only in how the field size is derived, so one
    // resolve-or-skip arm serves both. The two classifiers match disjoint
    // `(architecture, r_type)` predicates, so at most one returns `Some`.
    //
    // Both arrive as `RelocationKind::Unknown` with `size = 0`, so the general
    // `match reloc.kind()` below would mis-bucket them as unsupported. GLOB_DAT
    // / JUMP_SLOT resolved eagerly means an analysis-time `Load(GOT[...])`
    // reads the real target with no PLT model. REL32's undefined variant went
    // through `image_relative_reloc` above (addend-only, since `S = 0`); only
    // the defined-symbol form reaches here.
    if let Some(size_bytes) = got_or_plt_slot_reloc_size(reloc, obj.architecture())
        .or_else(|| mips_rel32_symbol_reloc_size(reloc, obj.architecture()))
    {
        let Some(target_addr) = resolve_symbol_target(obj, reloc) else {
            return;
        };
        let value = apply_addend(target_addr, reloc.addend());
        locate_and_write(
            regions,
            region_index,
            site_addr,
            value,
            size_bytes,
            endian_le,
        );
        return;
    }

    // Unlike the paths above, this one also handles
    // `RelocationTarget::Section`. `Absolute` (an immediate with no
    // symbol/section) and future variants fall through as unsupported.
    let target_addr = match reloc.target() {
        RelocationTarget::Symbol(_) => {
            let Some(addr) = resolve_symbol_target(obj, reloc) else {
                return;
            };
            addr
        }
        RelocationTarget::Section(idx) => match obj.section_by_index(idx) {
            Ok(sec) => sec.address(),
            // Bad section index: structurally malformed, skip.
            Err(_) => return,
        },
        _ => return,
    };

    let addend = reloc.addend();
    // S, A, P follow the System V ABI generic relocation formula:
    // S = target_addr, A = addend, P = site_addr. `PltRelative`'s L collapses
    // to S here since no PLT is materialised.
    let value = match reloc.kind() {
        RelocationKind::Absolute => apply_addend(target_addr, addend),
        RelocationKind::Relative | RelocationKind::PltRelative => {
            apply_addend(target_addr, addend).wrapping_sub(site_addr)
        }
        _ => return,
    };

    // `size` is in bits, and 0 nominally means "the kind's default". Absolute /
    // Relative / PltRelative all set it explicitly on every arch of interest,
    // so 0 here signals an arch-specific encoding (ARM Thumb branch, ...) that
    // isn't modelled.
    let size_bits = reloc.size();
    if size_bits == 0 || !size_bits.is_multiple_of(8) || size_bits > 64 {
        return;
    }
    let size_bytes = (size_bits / 8) as usize;

    locate_and_write(
        regions,
        region_index,
        site_addr,
        value,
        size_bytes,
        endian_le,
    );
}

/// Start-keyed index over a set of `MemRegion`s, answering two coverage
/// questions:
///
/// * [`covers`](Self::covers): does any region contain this point?
/// * [`covering_index`](Self::covering_index): which region fully covers a
///   `[site, site+len)` field?
///
/// One `(start, end, index)` list sorted by `start`, plus a prefix-maximum of
/// `end` so `covers` is a binary search and one array read.
///
/// Same-start collapse (last-insert-wins, mirroring
/// [`crate::MemRegionsLookupTable`]) applies to `covering_index` only. `covers`
/// keeps every interval, so an overlap whose later interval starts lower still
/// registers through the prefix-max.
struct RegionStartIndex {
    /// Sorted by `start`; equal-start entries hold insertion order, so the
    /// last-inserted is last within its run.
    entries: Vec<(u64, u64, usize)>,
    /// `max_end[i]` is the maximum `end` over `entries[0..=i]`, letting `covers`
    /// ask "does any `start <= addr` interval reach past `addr`?" without
    /// scanning the candidates.
    max_end: Vec<u64>,
}

impl RegionStartIndex {
    /// Equal-start regions keep their slice order, so the higher-index one wins
    /// `covering_index`.
    fn from_regions(regions: &[MemRegion]) -> Self {
        let mut entries: Vec<(u64, u64, usize)> = regions
            .iter()
            .enumerate()
            .map(|(i, r)| (r.start_addr(), r.end_addr(), i))
            .collect();
        entries.sort_by_key(|&(start, _, _)| start);
        let mut this = Self {
            entries,
            max_end: Vec::new(),
        };
        this.rebuild_max_end();
        this
    }

    fn rebuild_max_end(&mut self) {
        self.max_end.clear();
        self.max_end.reserve(self.entries.len());
        let mut running = 0u64;
        for &(_, end, _) in &self.entries {
            running = running.max(end);
            self.max_end.push(running);
        }
    }

    /// Inserts `[start, end)` keeping the sort by `start`. The slice index is a
    /// placeholder that is never read.
    fn insert(&mut self, start: u64, end: u64) {
        let pos = self.entries.partition_point(|&(s, _, _)| s <= start);
        self.entries.insert(pos, (start, end, usize::MAX));
        self.rebuild_max_end();
    }

    /// Exactly the `.any(|r| r.contains(addr))` predicate.
    fn covers(&self, addr: u64) -> bool {
        // `upper` counts the entries with `start <= addr`, the only ones that
        // could contain it. The furthest-reaching of those is `max_end[upper-1]`,
        // so coverage holds iff that end is strictly past `addr`.
        let upper = self.entries.partition_point(|&(start, _, _)| start <= addr);
        upper > 0 && self.max_end[upper - 1] > addr
    }

    /// Slice index of the region fully covering
    /// `[site_addr, site_addr + size_bytes)`, if any.
    ///
    /// Walks candidates from the highest `start <= site_addr` downward, so a
    /// field straddling a shorter higher-start region's end still resolves to
    /// a fully-covering lower-start region. Among entries sharing a `start`
    /// only the last-inserted is tested. On disjoint regions (the well-formed
    /// case) the first candidate matches, making this O(log N).
    fn covering_index(
        &self,
        regions: &[MemRegion],
        site_addr: u64,
        size_bytes: usize,
    ) -> Option<usize> {
        let upper = self
            .entries
            .partition_point(|&(start, _, _)| start <= site_addr);
        let mut prev_start: Option<u64> = None;
        for &(start, _, index) in self.entries[..upper].iter().rev() {
            // Keep only the first (last-inserted) entry of each equal-start run.
            if prev_start == Some(start) {
                continue;
            }
            prev_start = Some(start);
            if regions[index].fully_covers(site_addr, size_bytes) {
                return Some(index);
            }
        }
        None
    }
}

/// Walks the relocation table once to find sites no region covers, asks
/// `extender` to materialise each missing site's region, appends what it
/// returns, then runs the patch loop.
///
/// The per-site dedup check spans both pre-existing and staged regions, so one
/// staged `MemRegion` satisfies every later site inside it.
///
/// # Errors
///
/// Whatever the extender produces, plus [`apply_elf_relocations`]'s errors.
///
/// # Rollback semantics on `Err`
///
/// **Partial only.** A patch loop that fails partway leaves `regions` truncated
/// back to its pre-call length, but byte mutations already made to pre-existing
/// regions are NOT reverted.
pub(crate) fn apply_elf_relocations_with_extender<F>(
    regions: &mut Vec<MemRegion>,
    obj: &object::File<'_>,
    mut extender: F,
) -> Result<()>
where
    F: FnMut(u64, &object::File<'_>) -> Result<Option<MemRegion>>,
{
    // Stage first, mutating nothing, so an extender error mid-pass leaves
    // `regions` untouched. Coverage goes through `RegionStartIndex` rather than
    // a per-site `.any(contains)` scan, which was quadratic on binaries with
    // many dynamic relocs and many staged sections.
    let mut coverage = RegionStartIndex::from_regions(regions);
    let mut staged: Vec<MemRegion> = Vec::new();
    for_each_reloc_site(obj, |site_addr, _reloc| {
        if coverage.covers(site_addr) {
            return Ok(());
        }
        if let Some(region) = extender(site_addr, obj)? {
            coverage.insert(region.start_addr(), region.end_addr());
            staged.push(region);
        }
        Ok(())
    })?;
    let base_len = regions.len();
    regions.extend(staged);

    // Truncating restores the pre-call length only; see the rollback note above.
    apply_elf_relocations(regions, obj).inspect_err(|_| regions.truncate(base_len))
}

/// [`apply_elf_relocations`], but first extends `regions` with any `SHF_ALLOC`
/// file-backed section holding a relocation site no existing region covers.
///
/// `SHT_NOBITS` sections (`.bss`) are never added, having nothing to patch, and
/// their relocs are skipped by the inner call.
///
/// Dedup is per-site, not per-section. On a well-formed ELF the `SHF_ALLOC`
/// ranges are disjoint, so two missing sites in one section unify to a single
/// staged `MemRegion`. On a malformed ELF with overlapping `SHF_ALLOC` sections
/// both may be staged for different sites, and `MemRegionsLookupTable`'s
/// last-inserted-wins rule then resolves the reads.
///
/// # Errors
///
/// Same as [`apply_elf_relocations`]. The staging step itself only fails on a
/// malformed ELF: an unreadable `data()`, or an `address() + len()` overflow.
pub fn apply_elf_relocations_autoload(
    regions: &mut Vec<MemRegion>,
    obj: &object::File<'_>,
) -> Result<()> {
    apply_elf_relocations_with_extender(regions, obj, |site_addr, obj| {
        let Some(sec) = find_loadable_section_containing(obj, site_addr) else {
            return Ok(None);
        };
        let data = sec.data().context("failed to parse ELF")?;
        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some(MemRegion::new(sec.address(), data.to_vec())?))
    })
}

/// First `SHF_ALLOC`, file-backed (not `SHT_NOBITS`) section containing `addr`.
/// `None` means the caller skips that relocation.
fn find_loadable_section_containing<'data, 'a>(
    obj: &'a object::File<'data>,
    addr: u64,
) -> Option<object::read::Section<'data, 'a>> {
    obj.sections().find(|sec| {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            return false;
        };
        if sh_flags & u64::from(object::elf::SHF_ALLOC) == 0 {
            return false;
        }
        // SHT_NOBITS and unparseable data both yield no usable bytes, so both
        // skip. No `eprintln!` on the error: stderr writes from a deep helper
        // in library code are noise an embedder cannot suppress.
        let data_len = match sec.data() {
            Ok(d) => {
                if d.is_empty() {
                    return false;
                }
                d.len() as u64
            }
            Err(_) => return false,
        };
        // Bound by the staged byte count, not `sh_size`: the `MemRegion` spans
        // only `data().len()`, so a site in `[addr + data_len, addr + sh_size)`
        // would pass an `sh_size` check yet have no region to patch.
        let lo = sec.address();
        let hi = lo.saturating_add(data_len);
        addr >= lo && addr < hi
    })
}

/// Address of `reloc`'s `RelocationTarget::Symbol` index.
///
/// Dispatches through `obj.dynamic_symbol_table()` first and falls back to the
/// static `.symtab` only when there is none. Indices in the dynamic table
/// reference `.dynsym`, so `obj.symbol_by_index` alone would return the wrong
/// entry; ET_REL's per-section relocations reference `.symtab`, which is what
/// the fallback resolves.
///
/// `None` (caller skips the relocation) when the index doesn't resolve
/// (malformed ELF), when it resolves to the legitimate weak / undef case
/// (`address == 0 && is_undefined`), or when the target isn't a `Symbol`.
fn resolve_symbol_target(obj: &object::File<'_>, reloc: &object::Relocation) -> Option<u64> {
    match reloc.target() {
        RelocationTarget::Symbol(idx) => {
            let resolved = obj
                .dynamic_symbol_table()
                .map_or_else(|| obj.symbol_by_index(idx), |t| t.symbol_by_index(idx))
                .map(|s| (s.address(), s.is_undefined()))
                .ok();
            // `None` is an invalid index (malformed ELF); `(0, true)` is a
            // legitimate undefined or weak extern. Skip either way.
            let (addr, undef) = resolved?;
            if addr == 0 && undef {
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
) -> Option<(u64, usize)> {
    // These arrive with an `Absolute` target and an `Unknown` kind (`object`
    // doesn't enumerate them), so the raw type code is all there is to go on.
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
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
        // `R_MIPS_REL32` (type 3) is MIPS's closest analogue to RELATIVE and
        // writes `S + A`. For an undefined / index-0 (STN_UNDEF) symbol `S` is
        // 0, reducing it to addend-only, which is the case this arm handles;
        // the `Symbol`-target gate bails out to the defined-symbol path in the
        // main loop. MIPS defines no separate IRELATIVE.
        //
        // **The field is 4 bytes on MIPS64 as well as MIPS32.** "REL32" names
        // the field size, not the address width: the MIPS64 ELF supplement
        // defines it as a 32-bit field holding the low 32 bits of `S + A`.
        // Writing 8 bytes on MIPS64 corrupts the four bytes after the site.
        A::Mips | A::Mips64
            if r_type == object::elf::R_MIPS_REL32
                && !matches!(reloc.target(), RelocationTarget::Symbol(_)) =>
        {
            4
        }
        _ => return None,
    };
    // Image base 0, so the addend is the value. `object` types addends as i64
    // though these represent unsigned virtual addresses; `apply_addend` does
    // the 2's-complement bitcast from a base of 0.
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
) -> Option<usize> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
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
        A::Mips | A::Mips64
            if r_type == object::elf::R_MIPS_GLOB_DAT
                || r_type == object::elf::R_MIPS_JUMP_SLOT =>
        {
            Some(if matches!(arch, A::Mips64) { 8 } else { 4 })
        }
        _ => None,
    }
}

/// Matches the defined-symbol half of `R_MIPS_REL32`, which `object` surfaces
/// as a `Symbol` target and which needs the symbol's address. The undefined
/// half (`S = 0`, addend-only) goes through [`image_relative_reloc`].
///
/// The `Some(4)` field width is fixed on both MIPS32 and MIPS64; see
/// [`image_relative_reloc`].
fn mips_rel32_symbol_reloc_size(
    reloc: &object::Relocation,
    arch: object::Architecture,
) -> Option<usize> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    if matches!(arch, A::Mips | A::Mips64)
        && r_type == object::elf::R_MIPS_REL32
        && matches!(reloc.target(), RelocationTarget::Symbol(_))
    {
        Some(4)
    } else {
        None
    }
}

/// Writes the low `size_bytes` of `value` at `site_addr`, in the region that
/// fully covers the field.
///
/// Silently skips when no region does: either the site is unmapped, or its
/// field width runs past the end of the region its first byte lands in.
fn locate_and_write(
    regions: &mut [MemRegion],
    region_index: &RegionStartIndex,
    site_addr: u64,
    value: u64,
    size_bytes: usize,
    endian_le: bool,
) {
    if let Some(i) = region_index.covering_index(regions, site_addr, size_bytes) {
        let region = &mut regions[i];
        let off = (site_addr - region.start_addr()) as usize;
        write_at(region.data_mut(), off, value, size_bytes, endian_le);
    }
}

/// Writes `value`'s low `size_bytes` bytes into `bytes` at `off`, in the
/// target's endianness.
///
/// # Preconditions
///
/// - `off + size_bytes <= bytes.len()`.
/// - `size_bytes <= 8`, since `value` is a `u64` and its `to_le_bytes()` is
///   exactly 8 long. Every relocation kind that reaches here picks a size in
///   `{1, 2, 4, 8}`.
fn write_at(bytes: &mut [u8], off: usize, value: u64, size_bytes: usize, endian_le: bool) {
    // No-op rather than panic in release: a future relocation kind that forgets
    // to constrain its width would otherwise surface as an opaque slice panic.
    if size_bytes > 8 {
        debug_assert!(
            false,
            "write_at: size_bytes={size_bytes} exceeds u64 width; every ELF \
             relocation kind must select size_bytes in {{1, 2, 4, 8}}"
        );
        return;
    }
    if off
        .checked_add(size_bytes)
        .is_none_or(|end| end > bytes.len())
    {
        debug_assert!(
            false,
            "write_at: off={off} + size_bytes={size_bytes} > bytes.len()={}",
            bytes.len()
        );
        return;
    }
    // Truncation to the field width; signedness is irrelevant for fixed-width
    // 2's-complement bit patterns.
    let v_bytes = value.to_le_bytes();
    if endian_le {
        bytes[off..off + size_bytes].copy_from_slice(&v_bytes[..size_bytes]);
    } else {
        // Low N bytes, most-significant first.
        bytes[off..off + size_bytes].copy_from_slice(&value.to_be_bytes()[8 - size_bytes..]);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod coverage_index_tests {
    use super::RegionStartIndex;
    use crate::MemRegion;

    fn region(start: u64, len: usize) -> MemRegion {
        MemRegion::new(start, vec![0u8; len]).unwrap()
    }

    /// The linear predicate the index has to reproduce exactly.
    fn naive_covers(intervals: &[(u64, u64)], addr: u64) -> bool {
        intervals
            .iter()
            .any(|&(start, end)| addr >= start && addr < end)
    }

    #[test]
    fn covers_matches_naive_predicate_disjoint_overlapping_and_empty() {
        let regions = [
            region(0x1000, 0x100), // [0x1000, 0x1100)
            region(0x1100, 0x10),  // adjacent to the previous
            region(0x2000, 0x200), // [0x2000, 0x2200)
            region(0x2100, 0x300), // overlaps the previous: [0x2100, 0x2400)
            region(0x3000, 0),     // zero-length: covers nothing
        ];
        let intervals: Vec<(u64, u64)> = regions
            .iter()
            .map(|r| (r.start_addr(), r.end_addr()))
            .collect();
        let idx = RegionStartIndex::from_regions(&regions);

        // Boundaries, interiors, gaps, and the zero-length point.
        for addr in [
            0u64,
            0xfff,
            0x1000,
            0x10ff,
            0x1100,
            0x110f,
            0x1110,
            0x1fff,
            0x2000,
            0x21ff,
            0x2200,
            0x23ff,
            0x2400,
            0x2fff,
            0x3000,
            0x3001,
            u64::MAX,
        ] {
            assert_eq!(
                idx.covers(addr),
                naive_covers(&intervals, addr),
                "covers disagrees with naive scan at {addr:#x}"
            );
        }
    }

    #[test]
    fn insert_keeps_covers_correct_and_sorted() {
        // Mimics staging: one seed region, then more inserted out of start
        // order.
        let mut idx = RegionStartIndex::from_regions(&[region(0x2000, 0x100)]);
        assert!(idx.covers(0x2050));
        assert!(!idx.covers(0x1050));
        assert!(!idx.covers(0x3050));

        idx.insert(0x3000, 0x3100);
        idx.insert(0x1000, 0x1100); // sorts before the seed

        assert!(idx.covers(0x1050));
        assert!(idx.covers(0x2050));
        assert!(idx.covers(0x3050));
        assert!(!idx.covers(0x10ff + 1)); // exclusive end
        assert!(!idx.covers(0x3100)); // exclusive end

        let starts: Vec<u64> = idx.entries.iter().map(|&(s, _, _)| s).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted, "intervals must remain sorted by start");
    }

    #[test]
    fn empty_index_covers_nothing() {
        let idx = RegionStartIndex::from_regions(&[]);
        assert!(!idx.covers(0));
        assert!(!idx.covers(0x1000));
        assert!(!idx.covers(u64::MAX));
    }
}
