//! ELF relocation application.
//!
//! FreeBSD kernels and other ET_DYN binaries ship with unresolved
//! relocations: a `call rel32` to a function in the same image is
//! stored as `e8 00 00 00 00` until the loader patches the 4-byte
//! immediate with `target - (rip)`.  Without this patch the analyser
//! follows the call as control flow into the next instruction (rel32
//! = 0 → call site + 5 bytes) and prunes any code that only fed the
//! real call target.  The loader does this work at runtime; for
//! static analysis we replicate it here, in-place on the loaded
//! `MemRegion`s.
//!
//! Architecture-independence is delegated to the `object` crate's
//! `RelocationKind` enum (`Absolute` = `S + A`, `Relative` = `S + A
//! - P`, `PltRelative` = `L + A - P` — modelled as `Relative` here
//!   because we don't materialise PLT stubs).  Anything else we
//!   recognise but skip on; unknown architectures are skipped silently
//!   rather than producing partial / mis-applied patches.

use anyhow::Context as _;
use object::{
    Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationFlags, RelocationKind,
    RelocationTarget,
};

use crate::{MemRegion, Result};

/// Adds a (possibly negative) relocation `addend` to a base address.
///
/// `object::Relocation::addend()` returns an `i64`.  Casting it to `u64`
/// reinterprets a negative addend as its 2's-complement bit pattern, and
/// `wrapping_add` then produces the correct fixed-width modular result —
/// exactly what every relocation field expects, since `write_at`
/// subsequently truncates to the field width.  Centralised here so the
/// 2's-complement contract is stated once and the cast can't drift to a
/// checked / saturating variant (which would silently break common
/// negative-addend relocations such as PC-relative `S + A - P` with `A < 0`).
#[inline]
fn apply_addend(base: u64, addend: i64) -> u64 {
    base.wrapping_add(addend as u64)
}

/// Patches relocations in `regions` in-place.
///
/// Walks the relocation table appropriate for `obj.kind()`:
///
/// - **`Executable` / `Dynamic`** (ET_EXEC / ET_DYN): walks
///   `obj.dynamic_relocations()` (entries from `.rela.dyn`,
///   `.rela.plt`, …).
/// - **`Relocatable`** (ET_REL — an `.o` object file): walks each
///   section's `section.relocations()` table.  ET_REL keeps its
///   relocations on per-section tables (`.rela.text`, `.rela.data`,
///   …); `obj.dynamic_relocations()` returns `None` for ET_REL, so
///   without this branch the loader silently skips every reloc the
///   `.o` carries.  The site address for a per-section reloc is
///   `target_section.address() + r_offset` (the relocation's
///   `r_offset` is relative to the section it targets, not absolute).
///
/// For each entry, computes the target address from the relocation's
/// symbol or section, then writes the encoded value into the region
/// containing the relocation site.
///
/// # Supported relocation kinds
///
/// Per [`RelocationKind`] (object's high-level enum):
/// * `Absolute` — `S + A` (e.g. `R_X86_64_64`).  Symbol-targeted.
/// * `Relative` — `S + A - P` (e.g. `R_X86_64_PC32`,
///   `R_X86_64_PC64`, `R_AARCH64_PREL32/PREL64`,
///   `R_386_PC32`).  Symbol-targeted.
/// * `PltRelative` — same value as `Relative` (e.g.
///   `R_X86_64_PLT32`, `R_AARCH64_CALL26`, `R_386_PLT32`).  We
///   don't materialise a PLT — the symbol's own address is used.
///
/// Per raw `r_type` (object reports these as
/// `RelocationKind::Unknown`):
/// * `R_X86_64_RELATIVE` / `R_386_RELATIVE` /
///   `R_AARCH64_RELATIVE` / `R_ARM_RELATIVE` — write
///   `image_base + addend` (image_base modelled as 0).
/// * `R_X86_64_GLOB_DAT` / `R_X86_64_JUMP_SLOT` and the
///   i386 / aarch64 / arm equivalents — write the symbol's
///   address at the slot (S semantics, eagerly resolved).
///
/// Symbol indices in dynamic relocations reference the dynamic
/// symbol table (`.dynsym`); the applier dispatches through
/// `obj.dynamic_symbol_table()` and falls back to `.symtab` only
/// for ET_REL files where `dynamic_symbol_table()` is `None`.
///
/// # NOT supported
///
/// * `Got` / `GotRelative` / `GotBaseRelative` / `GotBaseOffset` —
///   the analyser would need a synthesised GOT section to write
///   into, which we don't allocate.
/// * Architecture-specific encodings whose payload doesn't fit a
///   simple `value_low_bytes_at_offset` write (Thumb branches,
///   AArch64 ADR_PREL_PG_HI21 + ADD_ABS_LO12_NC pairs, MIPS
///   HI16/LO16 splits, PPC TOC relocations).  These come back as
///   `RelocationKind::Unknown` with `size = 0` and are silently
///   skipped.
/// * MIPS / PowerPC kernel/userland binaries' specialised
///   relocation types — none of strider's current users hit them,
///   so they're left as a future-work item.
///
/// # Errors
///
/// Returns an error only on a malformed ELF (a relocation whose
/// target symbol or section index doesn't resolve).  A bad symbol
/// resolution that arises legitimately (e.g. `STN_UNDEF` for an
/// external lib) is *not* an error — the relocation is simply
/// skipped.
pub fn apply_elf_relocations(regions: &mut [MemRegion], obj: &object::File<'_>) -> Result<()> {
    // Build a sorted `start_addr -> index` map once so the per-relocation
    // region lookup in `locate_and_write` is an O(log N) BTree query
    // instead of an O(N) `regions.iter().find` linear scan.  With R
    // relocations and N regions this turns the patch loop from O(R·N) to
    // O(R·log N) — relevant for ET_REL `.o` files with many SHF_ALLOC
    // sections (one region each).  Two regions at the same start collapse
    // (BTreeMap last-insert-wins), matching `MemRegionsLookupTable`'s rule.
    let region_index = build_region_index(regions);

    for_each_reloc_site(obj, |site_addr, reloc| {
        apply_one_relocation(obj, regions, &region_index, site_addr, reloc);
        Ok(())
    })
}

/// Iterates every relocation site in `obj`, invoking `f(site_addr, reloc)`
/// for each, where `site_addr` is the *absolute* virtual address of the
/// site in the same coordinate system the loaded regions live in.
///
/// Owns the ET_REL-vs-dynamic kind dispatch and the site-address
/// derivation so the patching ([`apply_elf_relocations`]) and autoload
/// staging ([`apply_elf_relocations_with_extender`]) paths can't drift
/// apart on the address contract:
///
/// * ET_REL (`Relocatable`): relocations live on per-section tables.
///   Each section's `relocations()` iterator yields `(r_offset,
///   Relocation)` pairs where `r_offset` is relative to the section the
///   relocations apply *to* (the "info" section pointed at by `sh_info`
///   on the SHT_REL/RELA table).  In practice for ET_REL `sh_addr == 0`,
///   so the absolute site address equals `r_offset` for non-overlapping
///   VMAs; but to stay correct for any future ET_REL shape that does set
///   `sh_addr`, we add `sec.address()` explicitly.
/// * ET_EXEC / ET_DYN / Core / Unknown: use the dynamic table.
///   `dynamic_relocations()` returns `None` for ET_REL and any
///   ET_EXEC/ET_DYN binary that doesn't ship a dynamic table
///   (statically-linked, fully-resolved ELF); both short-circuit to
///   iterating nothing.
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

/// Applies one relocation entry to `regions`.  Shared body between the
/// ET_DYN (`dynamic_relocations()`) and ET_REL (per-section
/// `relocations()`) iteration paths in [`apply_elf_relocations`].
///
/// `site_addr` is the *absolute* virtual address of the relocation
/// site, already resolved to the same coordinate system the loaded
/// `regions` live in.  Relocations that can't be resolved or patched
/// (legitimate weak externs, malformed targets, unsupported kinds,
/// sites with no backing region) are silently skipped.
fn apply_one_relocation(
    obj: &object::File<'_>,
    regions: &mut [MemRegion],
    region_index: &RegionIndex,
    site_addr: u64,
    reloc: &object::Relocation,
) {
    // Endianness of the patched field, derived from `obj` (its sole source)
    // rather than threaded in alongside it.
    let endian_le = matches!(obj.endianness(), object::Endianness::Little);

    // Image-relative relocations (`R_X86_64_RELATIVE` /
    // `R_AARCH64_RELATIVE` / `R_386_RELATIVE` / `R_ARM_RELATIVE`)
    // store `image_base + addend` at the site, with no symbol or
    // section reference — `RelocationTarget::Absolute` and a
    // `RelocationKind::Unknown` come out the other side of object
    // crate's mapping table.  We model the analyser's image base as
    // the binary's link-time-chosen base (typically 0 for an ET_DYN),
    // so the patched value is `addend` directly.  Width is fixed by
    // the relocation type (64-bit on 64-bit ABIs, 32-bit on 32-bit).
    // Without this branch every PIE binary's `dispatch_table[]` slot
    // reads zero post-load.
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

    // GOT-data and PLT-jump slots (`R_*_GLOB_DAT` / `R_*_JUMP_SLOT`).
    // Object 0.38 reports these as `RelocationKind::Unknown` with
    // `size = 0`, but they have well-defined "S" semantics: write
    // the symbol's address at the site.  Resolving eagerly means
    // analysis-time `Load(GOT[...])` reads the real target without
    // having to model a PLT.
    if let Some(size_bytes) = got_or_plt_slot_reloc_size(reloc, obj.architecture()) {
        // Need the target symbol for these (no symbol → skip); pass
        // `require_symbol_target = true`.
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

    // Defined-symbol MIPS `R_MIPS_REL32` — `S + A` semantics.  The
    // undefined / index-0 case is handled by `image_relative_reloc`
    // above (addend-only, since `S = 0`); a REL32 against a defined
    // symbol carries a `RelocationTarget::Symbol(_)` and needs the
    // symbol's address.  `object` reports REL32 as
    // `RelocationKind::Unknown`, so the general `match reloc.kind()`
    // below would mis-bucket it as unsupported — resolve it here
    // (4-byte field on both MIPS32 and MIPS64).  Pass
    // `require_symbol_target = true`: the `Symbol`-target gate in
    // `mips_rel32_symbol_reloc_size` guarantees the target is a
    // `Symbol`, so this only ever takes the resolve-or-skip arms.
    if let Some(size_bytes) = mips_rel32_symbol_reloc_size(reloc, obj.architecture()) {
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

    // Resolve the target.  Per `Object::dynamic_relocations`'s
    // doc-comment, symbol indices in the *dynamic* table reference
    // the dynamic symbol table — `obj.symbol_by_index` looks at
    // `.symtab` and returns the wrong entry for a given index, so we
    // must dispatch through `dynamic_symbol_table()` first and fall
    // back to the static `.symtab` only if the dynamic table is
    // absent.  Per-section relocations (ET_REL) reference the static
    // `.symtab`, which is also what the fallback resolves; the
    // fallback path is the right one for ET_REL.  The general path
    // also handles `RelocationTarget::Section` (the GOT/PLT path
    // doesn't see those); pass `require_symbol_target = false` so
    // `Absolute` and unknown future variants are skipped as
    // unsupported.
    let target_addr = match reloc.target() {
        RelocationTarget::Symbol(_) => {
            let Some(addr) = resolve_symbol_target(obj, reloc) else {
                return;
            };
            addr
        }
        RelocationTarget::Section(idx) => match obj.section_by_index(idx) {
            Ok(sec) => sec.address(),
            // Bad section index — structurally malformed; skip.
            Err(_) => return,
        },
        // `Absolute` (sentinel for immediate-value relocations with
        // no symbol/section) and any future variants are skipped as
        // unsupported.
        _ => return,
    };

    let addend = reloc.addend();
    // S, A, P naming follows the System V ABI generic relocation
    // formula (see object::common::RelocationKind doc-comment):
    //   S = target_addr, A = addend, P = site_addr.
    let value = match reloc.kind() {
        RelocationKind::Absolute => apply_addend(target_addr, addend),
        // L (PLT entry) is treated as the symbol's own address — we
        // don't materialise a PLT.  Functionally identical to
        // `Relative` for analysis purposes.
        RelocationKind::Relative | RelocationKind::PltRelative => {
            apply_addend(target_addr, addend).wrapping_sub(site_addr)
        }
        // Unsupported kind — skip.
        _ => return,
    };

    // The `size` field is in bits; `size == 0` means "use the kind's
    // default", but the only kinds we patch (Absolute / Relative /
    // PltRelative) all set `size` explicitly on every arch we care
    // about, so a 0 size signals an arch-specific encoding (e.g.
    // ARM Thumb branch) we don't model.
    let size_bits = reloc.size();
    if size_bits == 0 || !size_bits.is_multiple_of(8) || size_bits > 64 {
        return;
    }
    let size_bytes = (size_bits / 8) as usize;

    // Find the region that contains the [site_addr, site_addr +
    // size_bytes) range.  Linear scan inside `locate_and_write` is
    // fine — relocation counts are small relative to the
    // per-relocation work.
    locate_and_write(
        regions,
        region_index,
        site_addr,
        value,
        size_bytes,
        endian_le,
    );
}

/// A sorted `[start, end)` interval index for O(log n) address-coverage
/// queries during the relocation extender's pass 1.
///
/// Replaces the per-site `regions.iter().chain(staged.iter())
/// .any(|r| r.contains(addr))` linear scan (quadratic in
/// sites × regions) used to decide whether a relocation site already
/// has a backing region.  Intervals are kept sorted by `start`
/// alongside a prefix-maximum of `end`, so [`covers`](Self::covers) is
/// a binary search plus one array read.
///
/// [`covers`](Self::covers) reproduces the exact `.any(contains)`
/// predicate, *including* the overlap case: a site is covered iff some
/// interval with `start <= addr` also has `end > addr`, i.e. the
/// maximum `end` among all `start <= addr` intervals exceeds `addr`.
/// The prefix-max array answers that in O(1) after the O(log n)
/// position search.
struct CoverageIndex {
    /// `(start, end)` intervals, sorted by `start`.
    intervals: Vec<(u64, u64)>,
    /// `max_end[i]` is the maximum `end` over `intervals[0..=i]`.  Lets
    /// `covers` answer "any `start <= addr` interval reaches past
    /// `addr`?" without scanning every candidate.  Empty when there are
    /// no intervals.
    max_end: Vec<u64>,
}

impl CoverageIndex {
    /// Seeds the index from a region iterator (one interval per region).
    fn from_regions<'a, I>(regions: I) -> Self
    where
        I: IntoIterator<Item = &'a MemRegion>,
    {
        let mut intervals: Vec<(u64, u64)> = regions
            .into_iter()
            .map(|r| (r.start_addr(), r.end_addr()))
            .collect();
        intervals.sort_unstable_by_key(|&(start, _)| start);
        let mut this = Self {
            intervals,
            max_end: Vec::new(),
        };
        this.rebuild_max_end();
        this
    }

    /// Recomputes the prefix-max-of-`end` array from `intervals`.
    fn rebuild_max_end(&mut self) {
        self.max_end.clear();
        self.max_end.reserve(self.intervals.len());
        let mut running = 0u64;
        for &(_, end) in &self.intervals {
            running = running.max(end);
            self.max_end.push(running);
        }
    }

    /// Inserts `[start, end)`, preserving the sort by `start`.  Called
    /// once per staged section (a small count), so the O(n) prefix-max
    /// rebuild it triggers is cheap relative to the per-site queries.
    fn insert(&mut self, start: u64, end: u64) {
        let pos = self.intervals.partition_point(|&(s, _)| s <= start);
        self.intervals.insert(pos, (start, end));
        self.rebuild_max_end();
    }

    /// Returns `true` iff some interval satisfies `start <= addr < end`
    /// — the exact `.any(|r| r.contains(addr))` predicate.
    fn covers(&self, addr: u64) -> bool {
        // `upper` = count of intervals with `start <= addr`; those are
        // the only ones that could contain `addr`.  Among them the one
        // reaching furthest is `max_end[upper - 1]`; the site is covered
        // iff that maximum end is strictly past `addr`.
        let upper = self.intervals.partition_point(|&(start, _)| start <= addr);
        upper > 0 && self.max_end[upper - 1] > addr
    }
}

/// Shared body of [`apply_elf_relocations`] and
/// [`apply_elf_relocations_autoload`]: walks the dynamic relocation
/// table once to find sites not yet covered by any region, queries
/// `extender` for each missing site, appends every returned
/// [`MemRegion`] to `regions`, then delegates to the patch loop.
///
/// The non-autoload variant passes an extender that always returns
/// `Ok(None)` (so `regions` is never grown and uncovered sites are
/// simply skipped inside the patch loop); the autoload variant passes
/// an extender that returns the `SHF_ALLOC` file-backed section
/// containing the site, if any.
///
/// Sections are appended in iteration order of `obj.dynamic_relocations()`,
/// and the per-site dedup check covers both the pre-existing `regions`
/// and the in-progress staged set, so a single staged `MemRegion`
/// satisfies every later site that falls inside it.
///
/// # Errors
///
/// Returns any error the extender produces, plus the same set of
/// errors as the underlying [`apply_elf_relocations`] patch loop.
///
/// # Rollback semantics on `Err`
///
/// **Partial rollback only.**  If the patch loop fails partway through,
/// the staged region extensions are truncated off the tail of `regions`
/// (restoring the pre-call *length*), but byte mutations the patch loop
/// already performed on pre-existing regions before the failure are NOT
/// reverted.  Snapshotting every mutated byte range would double the
/// memory cost of relocation application for a corner case that only
/// matters when an extender materialises a region we then fail to
/// patch.  Callers needing strict atomicity should re-load the binary
/// from disk on `Err`.
pub(crate) fn apply_elf_relocations_with_extender<F>(
    regions: &mut Vec<MemRegion>,
    obj: &object::File<'_>,
    mut extender: F,
) -> Result<()>
where
    F: FnMut(u64, &object::File<'_>) -> Result<Option<MemRegion>>,
{
    // Pass 1 — collect site addresses not yet covered, ask `extender`
    // to materialise the owning region for each, and stage them.  We
    // never mutate `regions` here so an extender error mid-pass leaves
    // it untouched.
    //
    // Coverage is queried through a `CoverageIndex` (a sorted
    // `[start, end)` interval list) seeded from `regions` and grown as
    // sections are staged.  This replaces the per-site
    // `regions.iter().chain(staged.iter()).any(contains)` linear scan
    // (O(sites × regions), quadratic on binaries with many dynamic
    // relocs and many staged sections) with an O(sites · log regions)
    // query.  Behaviour is identical: `covers` matches the exact
    // `.any(contains)` predicate, including the overlap case.
    let mut coverage = CoverageIndex::from_regions(regions.iter());
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

    // Pass 2 — the patch loop.  On `Err` we truncate the staged
    // extension off the tail of `regions`, restoring the pre-call
    // *length*.  This is a **partial rollback only**: any byte
    // mutations the patch loop performed on pre-existing regions
    // before the error fired are *not* reverted (snapshotting every
    // mutated byte range would double the memory cost of relocation
    // application for a corner case that only matters when an extender
    // produces a region we then fail to patch).  Callers that need
    // strict atomicity should re-load the binary from disk on `Err`.
    match apply_elf_relocations(regions, obj) {
        Ok(()) => Ok(()),
        Err(e) => {
            regions.truncate(base_len);
            Err(e)
        }
    }
}

/// Like [`apply_elf_relocations`], but pre-walks the dynamic
/// relocation table and lazily extends `regions` with any
/// `SHF_ALLOC` file-backed section from `obj` that contains a
/// relocation site not yet covered by an existing region.  Then
/// delegates to the pure [`apply_elf_relocations`].
///
/// Use this when the caller has already loaded a curated subset
/// of the ELF (e.g. only code+rodata) but wants relocation
/// application to "just work" without needing to know upfront
/// which writable sections (`.got.plt`, `.data.rel.ro`, …) the
/// dynamic relocs target.  Avoids the silent-failure mode of
/// the pure variant where every relocation is silently skipped
/// because the caller didn't pre-load the right sections.
///
/// Sections are added in iteration order of `obj.sections()`,
/// each appended once even when multiple relocs target the same
/// section.  An ELF section that has no file-backed bytes
/// (`SHT_NOBITS`, e.g. `.bss`) is *not* added — there's nothing
/// to patch — and the corresponding relocs are simply skipped
/// from inside the inner call.
///
/// The dedup check is per-site (does any region cover this exact
/// `site_addr`), not per-section.  On a well-formed ELF every
/// `SHF_ALLOC` section has a disjoint address range, so two
/// missing sites in the same section get unified to one staged
/// `MemRegion`.  On a malformed/synthesised ELF where two
/// `SHF_ALLOC` sections overlap, both can end up staged for two
/// different sites — `MemRegionsLookupTable`'s "last one
/// inserted wins" rule resolves the subsequent reads.
///
/// # Errors
///
/// Same as [`apply_elf_relocations`].  The lazy-load step itself
/// only fails on a malformed ELF (a section whose `data()` can't
/// be read or whose `address() + len()` overflows `u64`).
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

/// Returns the first section in `obj` that contains `addr` and is
/// safe to materialise as a `MemRegion`: `SHF_ALLOC` set, file-
/// backed (i.e. *not* `SHT_NOBITS`).  Returns `None` when no
/// section matches — caller treats that as "skip this relocation".
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
        // SHT_NOBITS sections (BSS) and sections whose data fails to
        // parse both yield no usable bytes — treat them identically as
        // "skip this section for site-coverage".  No `eprintln!`: this
        // is library code, and stderr writes from a deep helper are
        // un-suppressable noise for embedders.
        let data_len = match sec.data() {
            Ok(d) => {
                if d.is_empty() {
                    return false;
                }
                d.len() as u64
            }
            Err(_) => return false,
        };
        // Bound coverage by the actual staged byte count (`data().len()`),
        // not `sh_size`: the staged `MemRegion` spans only `data().len()`
        // bytes, so a site in `[addr + data_len, addr + sh_size)` could pass
        // an `sh_size`-based check yet have no region to patch.
        let lo = sec.address();
        let hi = lo.saturating_add(data_len);
        addr >= lo && addr < hi
    })
}

/// Look up the address of `reloc`'s `RelocationTarget::Symbol` index,
/// preferring the dynamic symbol table (`obj.dynamic_symbol_table()`)
/// over the static `.symtab` per `Object::dynamic_relocations`'s
/// doc-comment.  Returns `None` (the caller skips the relocation)
/// when:
///
/// - the symbol index doesn't resolve at all (malformed ELF),
/// - the symbol resolves cleanly but is the legitimate weak / undef
///   case (`address == 0 && is_undefined`),
/// - the target isn't a `Symbol` (an immediate `Absolute` or a
///   `Section` the caller routes here only on the symbol-required
///   GOT/PLT and MIPS-REL32 paths).
///
/// Otherwise returns `Some(address)`.
fn resolve_symbol_target(obj: &object::File<'_>, reloc: &object::Relocation) -> Option<u64> {
    match reloc.target() {
        RelocationTarget::Symbol(idx) => {
            let resolved = obj
                .dynamic_symbol_table()
                .map_or_else(|| obj.symbol_by_index(idx), |t| t.symbol_by_index(idx))
                .map(|s| (s.address(), s.is_undefined()))
                .ok();
            // None: symbol_by_index returned Err (invalid index,
            // malformed ELF).  Some((0, true)): legitimate undefined /
            // weak extern.  Either way, skip.
            let (addr, undef) = resolved?;
            if addr == 0 && undef {
                return None;
            }
            Some(addr)
        }
        // Non-Symbol relocation target (e.g. a SectionIndex we don't
        // model, or an immediate-value `Absolute`) — skip.
        _ => None,
    }
}

/// Detects the `R_*_RELATIVE` and `R_*_IRELATIVE` families —
/// image-base + addend relocations that have no symbol/section
/// target.  Returns `Some((value_to_write, size_bytes))` when
/// matched.  Image base is modelled as 0 (the binary's link-time
/// virtual address layout), so the value is just the addend.
///
/// The `r_type` constants for `R_X86_64_RELATIVE` and
/// `R_386_RELATIVE` collide (both = 8), as do several others across
/// arches, so we dispatch on the file's `Architecture` first and
/// only check `r_type` against the appropriate arch's constant.
///
/// `IRELATIVE` is the IFUNC-resolver variant: its addend is the
/// address of an indirect-resolver function the dynamic linker
/// would call to compute the actual slot value at runtime.  For
/// static analysis we write the resolver's address into the slot
/// — that's what the analyser sees in lieu of running the
/// resolver.  Treating IRELATIVE the same as RELATIVE is the
/// soundest static approximation.
fn image_relative_reloc(
    reloc: &object::Relocation,
    arch: object::Architecture,
) -> Option<(u64, usize)> {
    // R_*_RELATIVE / IRELATIVE come through with an `Absolute`
    // target (no symbol) and an `Unknown` kind (object 0.38 doesn't
    // enumerate them), so we look at the raw type code.
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    use object::Architecture as A;
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
        // PPC64 RELATIVE / IRELATIVE — type codes shared with PPC32 in
        // the ELF spec (`R_PPC64_RELATIVE = R_PPC_RELATIVE = 22`).
        // PPC64 ET_DYN binaries (Linux distributions) commonly use this
        // for function-pointer tables; without this arm those relocs
        // are skipped as unsupported and the resulting pointer reads as
        // zero.
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
        // MIPS REL32 — closest analogue to RELATIVE on MIPS.
        // `R_MIPS_REL32` (type 3) writes `S + A` (symbol value plus
        // addend).  For an **undefined / index-0 (STN_UNDEF)** symbol
        // `S` is 0, so the relocation reduces to image-relative (addend
        // only) — that's the case this arm handles.  A REL32 against a
        // **defined** symbol carries a non-null `r_sym`, which `object`
        // surfaces as `RelocationTarget::Symbol(_)`; those need the
        // symbol's address and are routed through
        // `mips_rel32_symbol_reloc_size` + `resolve_symbol_target` in
        // the main loop instead — so this arm bails (returns `None`) on
        // a `Symbol` target.  MIPS does not define a separate IRELATIVE.
        //
        // **Field width is 4 bytes on both MIPS32 and MIPS64.**  The
        // "REL32" suffix is the relocation field size, not the address
        // width — the MIPS64 ELF supplement defines REL32 as a 32-bit
        // relocation field that writes the low 32 bits of (S + A).
        // Writing 8 bytes here on MIPS64 corrupts the four bytes
        // immediately following the relocation site.
        A::Mips | A::Mips64
            if r_type == object::elf::R_MIPS_REL32
                && !matches!(reloc.target(), RelocationTarget::Symbol(_)) =>
        {
            4
        }
        _ => return None,
    };
    // For image-relative, the addend is the resolved value (image
    // base = 0).  Addends are i64 in object's API but represent
    // unsigned virtual addresses for these types — `apply_addend`
    // performs the 2's-complement bitcast from a base of 0.
    Some((apply_addend(0, reloc.addend()), size_bytes))
}

/// Detects the GOT/PLT-slot relocation family — relocations whose
/// runtime semantics are "write the symbol's address (S) at the
/// site", with no PC subtraction or addend mixing.  These are the
/// `R_*_GLOB_DAT` (GOT data slot) and `R_*_JUMP_SLOT` (PLT lazy-bind
/// slot) types.  At analysis time we resolve them eagerly: the
/// symbol's address goes into the slot, so a `Load(GOT[...])` reads
/// the real target and indirect-call patterns work without a PLT
/// model.
///
/// Object 0.38 reports both as `RelocationKind::Unknown` with
/// `size = 0`, so the size is determined by the arch (8 bytes on
/// 64-bit, 4 bytes on 32-bit).  Returns `Some(size_bytes)` when the
/// relocation is one of the recognised GLOB_DAT / JUMP_SLOT types
/// AND has a symbol target — caller is responsible for computing
/// the value (`target_addr + addend`).
fn got_or_plt_slot_reloc_size(
    reloc: &object::Relocation,
    arch: object::Architecture,
) -> Option<usize> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    use object::Architecture as A;
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

/// Detects a **defined-symbol** `R_MIPS_REL32` — the symbol-targeted
/// half of the REL32 family.  REL32 has `S + A` semantics; the
/// undefined / index-0 (STN_UNDEF) case (where `S = 0`, so the value is
/// addend-only) is handled by [`image_relative_reloc`], while a REL32
/// against a *defined* symbol — which `object` surfaces as
/// `RelocationTarget::Symbol(_)` — needs the symbol's address and is
/// routed here.  Common in MIPS shared objects for GOT / function-
/// pointer slots; without this the slot reads `addend` (usually 0)
/// instead of `symbol + addend`.
///
/// Returns `Some(4)` (the REL32 field width, fixed at 4 bytes on both
/// MIPS32 and MIPS64 — see [`image_relative_reloc`]) when matched; the
/// caller resolves the symbol value and computes `target_addr + addend`.
fn mips_rel32_symbol_reloc_size(
    reloc: &object::Relocation,
    arch: object::Architecture,
) -> Option<usize> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    use object::Architecture as A;
    if matches!(arch, A::Mips | A::Mips64)
        && r_type == object::elf::R_MIPS_REL32
        && matches!(reloc.target(), RelocationTarget::Symbol(_))
    {
        Some(4)
    } else {
        None
    }
}

/// Sorted `start_addr -> index-into-regions` map, built once per
/// [`apply_elf_relocations`] call so the per-relocation region lookup in
/// [`locate_and_write`] is an O(log N) BTree query instead of the former
/// O(N) `regions.iter().find` linear scan.
///
/// Mirrors the shape of [`crate::MemRegionsLookupTable`]'s own
/// start-keyed `BTreeMap`: two regions sharing a start collapse with
/// last-insert-wins, so the stored index points at the last region
/// inserted at that start.
type RegionIndex = std::collections::BTreeMap<u64, usize>;

/// Builds a [`RegionIndex`] over `regions` (start address → slice index).
fn build_region_index(regions: &[MemRegion]) -> RegionIndex {
    regions
        .iter()
        .enumerate()
        .map(|(i, r)| (r.start_addr(), i))
        .collect()
}

/// Returns the index of the region that fully covers
/// `[site_addr, site_addr + size_bytes)`, or `None` if no region does.
///
/// Walks candidate regions from the highest `start_addr <= site_addr`
/// downward (the same fall-through geometry as
/// [`crate::MemRegionsLookupTable::read`]) so a request straddling a
/// shorter higher-start region's end still resolves to a fully-covering
/// lower-start region.  On disjoint regions (the well-formed-ELF case)
/// the first candidate matches, making this O(log N).
fn find_covering_region(
    regions: &[MemRegion],
    region_index: &RegionIndex,
    site_addr: u64,
    size_bytes: usize,
) -> Option<usize> {
    region_index
        .range(..=site_addr)
        .rev()
        .map(|(_, &i)| i)
        .find(|&i| regions[i].fully_covers(site_addr, size_bytes))
}

/// Locates the region in `regions` whose `[start, end)` covers
/// `[site_addr, site_addr + size_bytes)` (via the precomputed
/// `region_index`), computes the in-region offset, and writes the low
/// `size_bytes` of `value` there using `endian_le`.  When no region
/// fully covers the field (the site is unmapped, or its field width runs
/// past the end of the region its first byte lands in) the relocation is
/// silently skipped.
///
/// Consolidates the three identical "find region / compute offset /
/// write_at" blocks in [`apply_elf_relocations`] (image-relative,
/// GOT/PLT-slot, generic Absolute/Relative paths) into a single helper.
fn locate_and_write(
    regions: &mut [MemRegion],
    region_index: &RegionIndex,
    site_addr: u64,
    value: u64,
    size_bytes: usize,
    endian_le: bool,
) {
    if let Some(i) = find_covering_region(regions, region_index, site_addr, size_bytes) {
        let region = &mut regions[i];
        let off = (site_addr - region.start_addr()) as usize;
        write_at(region.data_mut(), off, value, size_bytes, endian_le);
    }
}

/// Writes `value`'s low `size_bytes` bytes into `bytes` starting at
/// `off`, using the target's endianness.
///
/// # Preconditions
///
/// - `off + size_bytes <= bytes.len()` (panics in release on slice
///   bounds violation).
/// - `size_bytes <= 8` — `value` is a `u64`, so its `to_le_bytes()`
///   yields exactly 8 bytes; reading more than 8 bytes from that array
///   would panic on the LE arm or read garbage from the trailing
///   zero-padding on the BE arm.  Every ELF relocation kind dispatched
///   by [`locate_and_write`] picks a `size_bytes` in `{1, 2, 4, 8}`, so
///   this bound is satisfied at every reachable call site.
fn write_at(bytes: &mut [u8], off: usize, value: u64, size_bytes: usize, endian_le: bool) {
    // Release-build bounds check: silently no-op on a precondition
    // violation rather than panicking via slice indexing or
    // out-of-range `v_bytes` reads.  The dispatch in `locate_and_write`
    // never produces `size_bytes > 8` in production, but a future
    // RelocationKind addition that forgets to constrain its width
    // would otherwise surface as a less-helpful slice panic in
    // release builds.
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
    // Truncate `value` to the field width; signed/unsigned doesn't
    // matter for fixed-width 2's-complement bit patterns.
    let v_bytes = value.to_le_bytes();
    if endian_le {
        bytes[off..off + size_bytes].copy_from_slice(&v_bytes[..size_bytes]);
    } else {
        // Big-endian: write the low N bytes most-significant-first.
        bytes[off..off + size_bytes].copy_from_slice(&value.to_be_bytes()[8 - size_bytes..]);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod coverage_index_tests {
    use super::CoverageIndex;
    use crate::MemRegion;

    fn region(start: u64, len: usize) -> MemRegion {
        MemRegion::new(start, vec![0u8; len]).unwrap()
    }

    /// The naive predicate the index replaces: scan every interval for
    /// `start <= addr < end`.
    fn naive_covers(intervals: &[(u64, u64)], addr: u64) -> bool {
        intervals
            .iter()
            .any(|&(start, end)| addr >= start && addr < end)
    }

    #[test]
    fn covers_matches_naive_predicate_disjoint_overlapping_and_empty() {
        // Disjoint, overlapping, adjacent, and zero-length intervals all
        // in one index; `covers` must agree with the naive `.any` scan
        // at every probed address.
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
        let idx = CoverageIndex::from_regions(regions.iter());

        // Probe boundaries, interiors, gaps, and the zero-length point.
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
        // Mimics pass 1: start from one seed region, then stage more out
        // of start order; `covers` must reflect every inserted interval.
        let mut idx = CoverageIndex::from_regions([region(0x2000, 0x100)].iter());
        assert!(idx.covers(0x2050));
        assert!(!idx.covers(0x1050));
        assert!(!idx.covers(0x3050));

        idx.insert(0x3000, 0x3100); // ends-exclusive [0x3000, 0x3100)
        idx.insert(0x1000, 0x1100); // inserted before the seed

        assert!(idx.covers(0x1050));
        assert!(idx.covers(0x2050));
        assert!(idx.covers(0x3050));
        assert!(!idx.covers(0x10ff + 1)); // 0x1100 is exclusive end
        assert!(!idx.covers(0x3100)); // exclusive end of staged interval

        // Intervals stay sorted by start after the out-of-order inserts.
        let starts: Vec<u64> = idx.intervals.iter().map(|&(s, _)| s).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted, "intervals must remain sorted by start");
    }

    #[test]
    fn empty_index_covers_nothing() {
        let idx = CoverageIndex::from_regions(std::iter::empty());
        assert!(!idx.covers(0));
        assert!(!idx.covers(0x1000));
        assert!(!idx.covers(u64::MAX));
    }
}
