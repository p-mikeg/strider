# Deep audit — `strider-reader` + `read-only-memory`

Date: 2026-06-14 · Scope (read-only): `crates/strider-reader/src` (ElfFileMemReader,
MemRegion/MemRegionsLookupTable, sections, relocations, load) and
`crates/read-only-memory/src` (ReadOnlyMemory trait, blanket impls).

Verified against actual code + real call paths (strider-opt `LoadReadOnly`,
`indirect_branch_resolve::table`, strider-py `reader.rs`, orchestrator).
Comments/CLAUDE.md treated as suspect, not ground truth.

---

## Severity summary

| Severity | Count |
|----------|-------|
| HIGH     | 0     |
| MED      | 4     |
| LOW      | 5     |

The crates are in good shape: the fill-all-or-error contract is honored through a
single SSoT (`read_exact`), the raw-byte / no-endianness-swap contract holds, and
overflow is guarded at construction and at every write. Findings below are
hardening + simplification, not correctness blockers.

---

## MED findings

### R-1 — `Send + Sync` bound on `ReadOnlyMemory` violates the workspace "no Send/Sync" rule and over-constrains impls
- **Dimension:** Simplify / generalize (code vs project invariants)
- **Severity:** MED · **Confidence:** HIGH
- **Location:** `crates/read-only-memory/src/lib.rs:30` (`pub trait ReadOnlyMemory: Send + Sync`), and the `Arc` blanket impl at `:52`.
- **What & why:** The project memory note *No Arc/Send/Sync* states this is a
  single-threaded workspace and core traits must not bake in `Send + Sync` /
  refcounting. Yet `ReadOnlyMemory` requires `Send + Sync` and ships an
  `impl ... for std::sync::Arc<T>`. The trait object is only ever used as
  `&dyn ReadOnlyMemory` / `Box<dyn ReadOnlyMemory>` (verified: `strider-opt`
  `OptCtx.rom: Option<&'mem dyn ReadOnlyMemory>`, orchestrator
  `Option<Box<dyn ReadOnlyMemory>>`, table.rs `Option<&dyn ReadOnlyMemory>`) —
  never `Arc<dyn>`, never across threads. The `Send + Sync` bound forces every
  impl (including strider-py's `PyReadOnlyMemoryAdapter`, which holds a
  `Py<...>`) to satisfy thread-safety it never needs. This is the one place the
  memory note's "read-only-memory has Arc/Send/Sync vs the no-Arc rule" was
  flagged at crate-split and never reconciled.
- **Proposed fix:** Drop the `Send + Sync` supertrait bound and delete the
  `Arc<T>` blanket impl (keep the `Box<T>` one). If a future threaded use
  appears, add `+ Send + Sync` at that specific `dyn` site, not on the trait.
  Confirm `PyReadOnlyMemoryAdapter` still compiles (it holds a non-Sync
  `Py<PyAny>` today only because PyO3 makes it `Send`-able; removing the bound
  is strictly looser).

### R-2 — autoload coverage check is single-byte but the patch write needs the full field width → silent `skipped_no_region` at a region's tail
- **Dimension:** Soundness (ELF correctness) / edge case
- **Severity:** MED · **Confidence:** HIGH
- **Location:** `relocations.rs:478` (`coverage.covers(site_addr)` in
  `apply_elf_relocations_with_extender`'s `consider` closure) vs
  `relocations.rs:944-949` (`locate_and_write` requires
  `site_addr + size_bytes <= region.end_addr()`), and the autoload extender
  bound at `find_loadable_section_containing` `:639` (`addr >= lo && addr < hi`).
- **What & why:** Pass 1 only asks the extender to materialize a region when the
  *first byte* (`site_addr`) is uncovered, and the extender's own containment
  test (`:639`) is also single-byte. If a relocation site sits within
  `[hi - size_bytes + 1, hi)` of a section's file-backed bytes — i.e. the field
  straddles the staged region's end — pass 1 considers it covered/staged but
  the pass-2 `locate_and_write` range check rejects it (full
  `[site, site+size)` not inside one region), incrementing
  `skipped_no_region`. The relocation is silently dropped even though autoload
  was asked to "just work." On a well-formed ELF a reloc field never straddles
  its own section's end, so this is an edge case, but it is exactly the class of
  malformed/synthesized-ELF the autoload path advertises tolerance for, and the
  failure is silent (a counter, not an error).
- **Proposed fix:** Make the coverage/extender bound width-aware: thread the
  relocation's field size into pass 1 and require
  `covers(site_addr) && covers(site_addr + size - 1)` (or have the extender's
  section-containment test the full field range). Simpler: in pass 2, when
  `skipped_no_region` fires for a site whose first byte *is* covered, surface it
  under a distinct `skipped_straddles_region` counter so the silent drop is at
  least observable.

### R-3 — `MemRegionsLookupTable::read` "best partial" path can mix the winning region selection in a way that hides a fully-covering region behind a same-`n` earlier one
- **Dimension:** Soundness (region overlap) / edge case
- **Severity:** MED · **Confidence:** MED
- **Location:** `crates/strider-reader/src/lib.rs:225-242`.
- **What & why:** The loop returns immediately when a candidate covers the whole
  request (`n == out.len()`), which is correct. But when no single region fully
  covers, it keeps the candidate with strictly-greater `n`
  (`best.is_none_or(|(_, best_n)| n > best_n)`), iterating highest-start-first.
  Because the comparison is strict `>`, the *first* (highest-start) candidate at
  a given `n` wins — fine for the documented "later start wins" rule. The subtle
  case: two overlapping regions with the same start-relative availability but
  *different bytes* (a malformed/synthesized region set) resolve to the
  highest-start one for the partial, while a same-start collision earlier
  collapsed via last-insert-wins. For the `ReadOnlyMemory`/`read_exact` consumer
  any partial is an error, so this never produces a *wrong fold* — but for the
  `rsleigh::MemReader` consumer (instruction fetch, which accepts partials) an
  overlapping-region disagreement silently picks one region's bytes with no
  diagnostic. Low real-world incidence (loadable regions are disjoint on
  well-formed ELFs) but the behavior is unspecified for the overlap-with-
  differing-bytes case and only partially pinned by tests.
- **Proposed fix:** Document the resolution rule for overlapping-with-differing-
  bytes explicitly at the `read` doc-comment (it currently only covers the
  shadowing/fall-through geometry), and add a debug-assert (or test) that two
  overlapping regions at distinct starts are treated as the audit assumes. No
  runtime change needed if the rule is intentional; the gap is specification +
  test, not logic.

### R-4 — relocation patch loop is `O(relocs × regions)` linear scan; fine today, but the only un-indexed hot path
- **Dimension:** Runtime
- **Severity:** MED · **Confidence:** HIGH
- **Location:** `relocations.rs:944` (`regions.iter_mut().find(...)` inside
  `locate_and_write`, called once per relocation from `apply_one_relocation`).
- **What & why:** Pass 1 was deliberately de-quadratified with `CoverageIndex`
  (O(sites · log regions)), but pass 2's actual *write* still does a linear
  `regions.iter_mut().find` per relocation. With R relocations and N regions
  that is O(R·N). `region_count` is small for typical ELFs (a handful of PT_LOAD
  segments), so this is acceptable today, and the inline comment at `:333`
  acknowledges it. But it is the one remaining O(R·N) path and contradicts the
  effort spent indexing pass 1. For an ET_REL `.o` with many SHF_ALLOC sections
  (each a region) and many per-section relocs, N grows and this becomes the
  bottleneck.
- **Proposed fix:** Reuse a sorted start→index map (the same shape as
  `MemRegionsLookupTable`'s `BTreeMap`) for pass-2 region location, or build a
  `BTreeMap<start, idx>` once before the patch loop and binary-search per site.
  Mechanical; keeps the contains+full-range check. Only worth doing if N is
  expected to grow (ET_REL with hundreds of sections); otherwise downgrade to
  LOW and just note it.

---

## LOW findings

### R-5 — `load_elf` parses the ELF twice (once to validate, once after leak)
- **Dimension:** Simplify / runtime
- **Severity:** LOW · **Confidence:** HIGH
- **Location:** `load.rs:108` and `:113` (two `object::File::parse` calls).
- **What & why:** `load_elf` parses to validate, then leaks the bytes and
  re-parses the identical bytes. The doc-comment justifies this ("no leak on
  error"). Correct but the second parse's `?` is dead in practice — the comment
  even says "cannot fail." The double parse is O(file) wasted work for the
  success path. Acceptable since `load_elf` is documented test/CLI-only, but the
  cleaner shape is: leak first, parse once, and on parse-`Err` reclaim via
  `Box::from_raw` (or accept the leak-on-error since the function is non-hot).
- **Proposed fix:** Either accept a one-time leak on the rare malformed-input
  error and parse once, or keep as-is and downgrade the comment's "cannot fail"
  claim to a real invariant note. Low priority — correctness is fine.

### R-6 — `write_at` size guard duplicated; `size_bytes > 8` is checked in two places
- **Dimension:** Simplify
- **Severity:** LOW · **Confidence:** HIGH
- **Location:** `relocations.rs:327` (`size_bits > 64` reject in the generic
  path) and `relocations.rs:979` (`size_bytes > 8` guard in `write_at`).
- **What & why:** The generic Absolute/Relative path rejects `size_bits > 64`
  before calling `locate_and_write`; the image-relative / GOT-PLT / MIPS paths
  hand fixed `{4, 8}` sizes. So `write_at`'s `size_bytes > 8` branch is
  unreachable in production (the comment at `:976` admits this). It is defensive
  belt-and-suspenders, which is reasonable, but the `debug_assert!(size_bytes <=
  8)` immediately after the `if size_bytes > 8 { return; }` is redundant — the
  branch is only entered when the assert would fire, so the assert always
  panics in debug inside an already-taken `> 8` branch. The two checks could be
  one `debug_assert!` + early `return`.
- **Proposed fix:** Collapse to a single `if size_bytes > 8 { debug_assert!(...);
  return; }` (the current code already does this — but the nested
  `debug_assert!(size_bytes <= 8, ...)` inside the `> 8` block is tautologically
  false; rephrase the message or drop the redundant inner assert). Cosmetic.

### R-7 — negative `addend()` cast to `u64` then truncated relies on 2's-complement; correct but undocumented at the cast sites
- **Dimension:** Soundness (code vs itself)
- **Severity:** LOW · **Confidence:** MED
- **Location:** `relocations.rs:237`, `:257`, `:308`, `:312-314`, `:818`
  (`reloc.addend() as u64`).
- **What & why:** `object::Relocation::addend()` returns `i64`. Several paths do
  `target_addr.wrapping_add(addend as u64)`. For a negative addend the
  `i64 as u64` is the 2's-complement bit pattern and `wrapping_add` produces the
  correct modular result, then `write_at` truncates to the field width. This is
  correct for fixed-width 2's-complement, and `:817` documents the intent for
  the image-relative path only. The other sites (`:237`, `:257`, `:308`) rely on
  the same reasoning without a note. A future reader could "fix" one to a
  checked/saturating add and silently break negative-addend relocations
  (common: PC-relative `S + A - P` with negative A).
- **Proposed fix:** One shared comment (or a tiny `fn apply_addend(base: u64,
  addend: i64) -> u64`) wrapping the `wrapping_add(addend as u64)` idiom so the
  2's-complement contract is stated once and the cast can't drift to a checked
  variant.

### R-8 — `collect_loadable_segments` PT_LOAD restriction relies on `obj.segments()` semantics; non-PT_LOAD with `SegmentFlags::Elf` would be accepted
- **Dimension:** Soundness (edge case)
- **Severity:** LOW · **Confidence:** MED
- **Location:** `sections.rs:156-173`.
- **What & why:** The comment at `:159-161` claims `obj.segments()` already
  filters to PT_LOAD, then the code reads `p_flags` "to check explicitly" — but
  it never actually checks `p_type == PT_LOAD`; it only matches
  `SegmentFlags::Elf { p_flags }` (true for *any* ELF segment kind) and tests
  writability. If a backend/object version ever surfaces a non-PT_LOAD segment
  (e.g. PT_GNU_RELRO, PT_DYNAMIC) through `segments()`, it would be loaded as a
  region. The `object` crate does restrict `segments()` to PT_LOAD today, so
  this is latent, not live.
- **Proposed fix:** If `object` exposes `p_type`, assert/filter `== PT_LOAD`
  explicitly to match the comment; otherwise downgrade the comment to "object
  guarantees PT_LOAD-only" so it stops claiming a check that isn't there.

### R-9 — `RelocationStats.unsupported_r_types` insert is `O(n)` per unique type (sorted-vec insert)
- **Dimension:** Runtime
- **Severity:** LOW · **Confidence:** HIGH
- **Location:** `relocations.rs:714-717` (`binary_search` + `Vec::insert`).
- **What & why:** Each newly-seen unsupported `r_type` does a `Vec::insert` at
  the sorted position — O(k) shift where k is the distinct-type count. Distinct
  reloc *types* in any ELF is a tiny constant (single digits), so total cost is
  negligible. Flagged only for completeness against the "no O(n²)" rule: it is
  O(distinct_types²) worst case but distinct_types is bounded by a small
  constant, so this is fine as-is. No action recommended.

---

## Edge-case test gaps (names + scenarios; not written)

1. **`apply_elf_relocations_autoload_field_straddling_section_end_is_observable`**
   — R-2: a synthesized ELF whose relocation field's first byte is within a
   section but whose last byte runs past `data().len()`. Assert it does NOT
   silently apply and that the outcome is observable (distinct counter or error),
   not a plain `skipped_no_region`.

2. **`lookup_table_overlapping_regions_differing_bytes_partial_read_is_specified`**
   — R-3: two overlapping regions at distinct starts with *different* byte
   contents; a partial (`MemReader`-style) read that straddles both. Pin which
   region's bytes win so the unspecified case is locked.

3. **`apply_elf_relocations_negative_addend_pc_relative`** — R-7: a `Relative`
   (`S + A - P`) reloc with a negative addend; assert the patched field equals
   the modular `S + A - P` low bytes (guards against a future checked-add
   regression).

4. **`read_only_memory_box_dyn_compiles_without_send_sync`** — R-1: a
   non-`Send`/non-`Sync` impl behind `Box<dyn ReadOnlyMemory>` (compile-only).
   Currently impossible because the trait demands `Send + Sync`; this test would
   fail today and pass after R-1, pinning the relaxed bound.

5. **`elf_get_loadable_regions_skips_non_pt_load_segment`** — R-8: a crafted ELF
   carrying a PT_GNU_RELRO/PT_DYNAMIC segment surfaced by `segments()`; assert it
   is not turned into a `MemRegion`.

6. **`mem_region_read_exact_zero_length_at_exact_end_addr`** — boundary: confirm
   `read_exact(end_addr, &mut [])` errors ("not mapped") rather than succeeding,
   matching the documented rule that `end_addr` is outside the region even for a
   zero-length request (current `mem_region.rs` tests cover zero-length *inside*
   a region but not at exactly `end_addr`).

---

## What is sound (verified, no finding)

- **Fill-all-or-error:** single SSoT (`MemRegionsLookupTable::read_exact`,
  lib.rs:256) used by `ElfFileMemReader::ReadOnlyMemory::read`; partial → error,
  unmapped → error. No partial fold path reaches `LoadReadOnly`.
- **Raw bytes / no endianness swap:** `MemRegion::read` is a plain
  `copy_from_slice`; only `write_at` applies endianness, and only to relocation
  *patches* (correct — the field's stored encoding is endian-specific).
- **Overflow:** `MemRegion::new` rejects `start + len > u64::MAX`;
  `locate_and_write` uses `checked_add` for the field end; `CoverageIndex` uses
  `partition_point` (no arithmetic overflow). `end_addr` is overflow-free by the
  constructor invariant.
- **ET_REL first-wins VMA dedup** (sections.rs:187) correctly avoids the
  last-insert-wins non-determinism of `MemRegionsLookupTable` for `.o` files.
- **Rollback semantics** (relocations.rs:527-533) truncate staged regions on
  patch-loop error; partial-rollback limitation is documented and acceptable.
- **Symbol resolution** dispatches dynsym-first then `.symtab` fallback per
  `object`'s documented dynamic-table contract; malformed vs weak-extern buckets
  are correctly distinguished.
