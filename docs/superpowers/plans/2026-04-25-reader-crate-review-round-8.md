# Reader Crate Review — Round 8 Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Final-pass correctness/simplification/optimization/readability review of the `reader` crate after rounds 1–7.

**Headline finding (read this first):** the crate is in tight shape. Seven rounds have driven down everything substantive — public API is stable, every error variant is doc'd and pinned, every boundary is tested. **Round 8 has very little real work to do.** This plan therefore enumerates *every* candidate I considered, separates the genuinely-actionable ones from churn-against-prior-decisions, and asks you to pick one of three execution lanes:

  - **Lane 0 — Skip Round 8.** Defensible. Below I list what's left and why none of it must land. Sign off and we move to a different crate.
  - **Lane 1 — Doc-only nudge (Task A).** Tiny rustdoc tightening on `MemRegionsLookupTable::read` to be honest that the "walk-down" loop is also O(n) for unmapped addresses past every region in the *non*-overlapping case (current doc only flags O(n) for the overlapping case). One-line edit, zero behavioral change, no new tests.
  - **Lane 2 — Doc nudge + cheap perf short-circuit (Task A + Task B).** Add a cached `max_end_addr: u64` to `MemRegionsLookupTable` and return `None` immediately when `addr >= max_end_addr`. Turns the unmapped-past-everything case from O(n) into O(1). Marginal for ELF callers (≤30 regions) but real for adversarial / many-region inputs and aligns the implementation with the (post-Task-A) docstring.

**Architecture:** Whichever lane you pick, no public API changes; all changes are internal to `MemRegionsLookupTable`. Tasks are self-contained and independently committable.

**Tech Stack:** Rust, `object`, `rsleigh`, `strider-error`.

---

## Baseline (verified 2026-04-25)

- `cargo test -p reader` → 27 `mem_region` + 15 `elf_reader` + 13 `elf_converters` + 4 `elf_smoke` + 5 `error` + 3 `load_elf` tests. **PASS.**
- `cargo clippy -p reader --all-targets --no-deps -- -D warnings` → **clean** (round 7 baseline holds).
- Most recent reader commit: `8157c95 docs(reader): specify zero-byte boundary semantics on MemRegion::read` (round 7, Task 2).
- Round 7 fully landed: `97182a5` (load_elf no-leak-on-error) + `8157c95` (MemRegion::read zero-byte doc).

---

## Methodology

I read every line of `crates/reader/src/{lib,elf,error}.rs` and every test file in `crates/reader/tests/`, looked at every committed commit message touching the crate, and re-checked round 1–7 plans for what was already considered. Below is the **complete** list of candidates I produced, with the verdict for each.

### Code review checklist applied

  - [x] Correctness — every documented behavior traced in code, every claimed invariant verified
  - [x] Error model — every `?` return path manually walked
  - [x] Overflow / cast safety — every `as u64`, `usize::try_from`, `checked_*` audited
  - [x] Lifetime / leak audit — `load_elf` (intentional leak) and `from_path`/`from_bytes` (no leak) traced through
  - [x] Trait impl correctness — `MemReader` and `ReadOnlyMemory` impls walked vs trait docs
  - [x] Doc/code drift — every `# Errors` section cross-referenced against actual return paths
  - [x] Test coverage — every public function has at least one happy-path + boundary test
  - [x] Crate rules — no panic / no unwrap / no debug_assert in `src/`; tests exempt with the standard top-of-file allow

### Candidates considered

#### Genuinely-actionable

  1. **Doc precision in `MemRegionsLookupTable::read`.** Current docstring says
     > "this is O(log n) in the usual non-overlapping case and O(n) in the worst case where every earlier region must be consulted."
     Read literally that's defensible (the unmapped-past-everything case *is* a "worst case where every earlier region must be consulted"), but it's easy to read it as "O(n) only happens with overlap." A reader who hits unmapped lookups in a hot path could be surprised. **→ Lane 1 / Task A.**
  2. **O(1) short-circuit for unmapped past-everything addresses.** Track `max_end_addr` at construction; short-circuit `read` and `contains_address`-style queries with `addr >= max_end_addr ⇒ None`. Cost: one extra `u64` per table; one fold over regions in `new`. Benefit: real for adversarial/large-region cases, invisible for typical ELF (≤30 sections). **→ Lane 2 / Task B.**

#### Considered, rejected (would be churn against prior decisions)

  3. **Use `contains()` upfront in `MemRegion::read`.** The current `checked_sub`-chain form was *deliberately* introduced in commit `16f104a` (round 4) — its commit message explicitly says "Removes the contains()-then-recompute pattern." Reverting now would undo a conscious refactor.
  4. **`for { map.insert }` → `into_iter().collect()`** in `MemRegionsLookupTable::new`. Rejected round 5: pure cosmetic, identical allocations.
  5. **Cache `MemRegion::end_addr`.** Rejected round 6: cold path, churns the struct.
  6. **`MemRegion::data: Vec<u8>` → `Box<[u8]>`.** Rejected rounds 2/3/4: marginal memory win, churns API.
  7. **Generic-over-section-vs-segment helper.** Rejected round 1: `object` provides no shared trait, macro heavier than the 12-line duplicate.
  8. **Rename `load_elf` to make the leak opt-in.** Rejected round 5; round 7 narrowed the wart instead.
  9. **`VnSpace` filter on `MemReader::read`.** Rejected rounds 6/7: trait doesn't restrict space; Sleigh in practice only invokes with RAM.
  10. **Reclaim leaked bytes via `unsafe { Box::from_raw }` in `load_elf` parse-failure path.** Rejected round 7: validate-then-leak is safer; `unsafe` for a once-per-process path is the wrong trade.
  11. **Re-align column-aligned `let` block in `section_is_code_or_readonly`.** Rejected round 6: locally readable, churn-only.
  12. **Replace `if size == 0 || size > 8` with `!(1..=8).contains(&size)` in `ReadOnlyMemory::read`.** Considered fresh in this round. Verdict: cosmetic, marginal — and the explicit `||` form spells out two distinct preconditions (zero is not a load; >8 doesn't fit in a u64) more visibly than a range check. **Skip.**
  13. **Combine the two early-return guards in `ReadOnlyMemory::read` into one** (`space != RAM || size == 0 || size > 8 ⇒ None`). Same family as #12. The current two-guard form documents two separate validity rules; collapsing loses that. **Skip.**
  14. **Allocator-instrumented test that *proves* `load_elf` doesn't leak on error.** Out of scope (round 7): would need `dhat` or a custom global allocator; `tests/load_elf.rs::load_elf_rejects_garbage_bytes` pins the error contract.
  15. **`MemRegion::data: Vec<u8>` → `impl AsRef<[u8]>`** generic field. Considered fresh: would force callers to deal with type parameters; `Vec<u8>` is the right shape for bytes loaded from disk.
  16. **Make `ReadOnlyMemory: Send + Sync` optional via a separate trait.** Considered fresh: every current impl satisfies it, the optimizer's `LoadReadOnly` pass needs it for thread-safety; speculative subdivision.
  17. **Field name `is_little_endian: bool` → `endianness: object::Endianness`.** Considered fresh: bool is denser; the `obj.endianness()` call already collapses to two states; no information is lost (Endianness is a 2-variant enum).
  18. **Test consolidation: `tests/load_elf.rs::load_elf_missing_path_is_io_error` and `tests/error.rs::load_elf_missing_path_produces_io_error_variant` look duplicate.** They aren't: the `error.rs` version also asserts the traceback chain via `assert_has_traceback`. They pin different aspects of the same call. Keep both.
  19. **Workspace-wide `clippy::pedantic`.** Deferred until/unless workspace opts in globally.

#### Audit results — no findings (i.e. things I actively checked and found OK)

  - `MemRegion::new` overflow check (`ok_or(...)?` form, round 6).
  - `MemRegion::read` boundary semantics — every documented behavior matches a pinned test (zero-len mid-region, zero-len at end_addr, partial past end, unmapped before/after).
  - `MemRegion::end_addr` non-overflow — guaranteed by the constructor.
  - `MemRegionsLookupTable` overlapping-region "later start wins, fall through to outer" — pinned by 3 tests.
  - `ElfFileMemReader::from_object` / `from_bytes` / `from_path` chain — no double-leak, no double-parse, lifetimes correct.
  - `ReadOnlyMemory::read` endianness construction — both LE and BE tested for u32, LE for u64, single-byte (no-endianness).
  - `ReadOnlyMemory` partial-region rejection (returns `None`, not truncated value) — pinned by `ro_read_partial_region_returns_none` and the cross-trait asymmetry test.
  - `load_elf` validate-before-leak (round 7) — error contract pinned by `load_elf_rejects_garbage_bytes`.
  - Every `# Errors` rustdoc block lists every variant the function can produce (round 6 closed the `RegionOverflow` doc gap).
  - Test files all carry `#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]` per workspace convention.
  - `Cargo.toml` deps minimal: `object`, `rsleigh`, `strider-error`, `thiserror`; dev: `object/write`, `tempfile`.

---

## Open questions for the reviewer before execution

**Q1 — Pick a lane.**

  - **Lane 0 (skip Round 8).** Defensible given the audit above. **Default if you don't say otherwise.**
  - **Lane 1 (Task A only).** One rustdoc edit on `MemRegionsLookupTable::read`. ~5 LOC, no test changes, no behavioral change.
  - **Lane 2 (Task A + Task B).** Same doc edit + a `max_end_addr` short-circuit. ~15 LOC of code + 2 new tests pinning the short-circuit. No public API change.

**Q2 — If Lane 2: does the perf payoff justify the field?** For the *current* concrete callers (`ElfFileMemReader` over a real ELF), a typical binary loads ~10–30 regions. Worst-case walk savings: tens of comparisons per unmapped-past-everything `MemReader::read` call. Not measurable.

The case Task B actually buys you is *future* — a downstream backend that loads thousands of regions (e.g. a raw-blob backend with one region per slice). If you don't anticipate that, prefer **Lane 1**.

**Reviewer choices locked in:** Q1 → **TBD**. Q2 → **TBD**. (Filled in once you approve; defaults: Lane 0 if you say "skip", Lane 1 if you say "go" without specifying.)

---

## Task A — Tighten `MemRegionsLookupTable::read` rustdoc on the O(n) case

**Files:**
- Modify: [crates/reader/src/lib.rs:158-167](crates/reader/src/lib.rs#L158-L167) — `MemRegionsLookupTable::read` rustdoc only.

### Step-by-step

- [ ] **Step 1: Replace the rustdoc on `MemRegionsLookupTable::read`**

In `crates/reader/src/lib.rs`, replace:

```rust
    /// Reads bytes starting at `addr` from whichever region contains it.
    ///
    /// Returns `None` when no region contains `addr`.
    /// Partial reads are possible — see [`MemRegion::read`].
    ///
    /// Candidates are walked from highest `start_addr <= addr` downward: the
    /// usual no-overlap case returns on the first candidate, but if a later,
    /// shorter region sits inside an earlier one the outer region is consulted
    /// for addresses past the inner region's end.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
```

with:

```rust
    /// Reads bytes starting at `addr` from whichever region contains it.
    ///
    /// Returns `None` when no region contains `addr`.
    /// Partial reads are possible — see [`MemRegion::read`].
    ///
    /// Candidates are walked from highest `start_addr <= addr` downward: the
    /// usual no-overlap case returns on the first candidate (so the *hit*
    /// path is O(log n) — one BTreeMap range query plus one `MemRegion::read`).
    /// Two cases degrade to O(n):
    ///
    ///   1. **Unmapped past every region.** When `addr >= region.end_addr()`
    ///      for every region, the loop walks every region whose
    ///      `start_addr <= addr` before returning `None`. For typical inputs
    ///      with a handful of regions this is invisible; for tables with many
    ///      regions and frequent unmapped lookups it is observable.
    ///   2. **Overlapping regions where the later, shorter one sits inside an
    ///      earlier one.** The outer region must be consulted for addresses
    ///      past the inner region's end. The walk stops as soon as some
    ///      region returns `Some(_)`.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
```

This is a rustdoc-only change. No code changes, no test changes.

- [ ] **Step 2: Confirm existing tests still pass**

Run: `cargo test -p reader --test mem_region`
Expected: all 27 tests PASS unchanged.

- [ ] **Step 3: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Workspace sanity**

Run: `cargo build --workspace`
Expected: PASS. (No need for `cargo test --workspace` — pure rustdoc edit.)

- [ ] **Step 5: Commit**

```bash
git add crates/reader/src/lib.rs
git commit -m "docs(reader): clarify MemRegionsLookupTable::read O(n) cases"
```

---

## Task B — Short-circuit `MemRegionsLookupTable::read` for addresses past every region

**Lane 2 only.** Skip if you picked Lane 0 or Lane 1.

**Files:**
- Modify: [crates/reader/src/lib.rs](crates/reader/src/lib.rs) — `MemRegionsLookupTable` struct, `new`, and `read`.
- Modify: [crates/reader/tests/mem_region.rs](crates/reader/tests/mem_region.rs) — append two pinned-contract tests.

### Step-by-step

- [ ] **Step 1: Write the failing tests**

Append to `crates/reader/tests/mem_region.rs`:

```rust
// ── Pinned contract: O(1) short-circuit when addr is past every region ────

/// Pinned contract: when `addr >= max(region.end_addr() for region in table)`,
/// `MemRegionsLookupTable::read` returns `None` without walking any region.
///
/// We can't directly assert "no walk" in stable Rust, so we pin the
/// observable rule (correct `None` for past-everything addresses) and the
/// implementation hint that the cached upper bound is consulted first
/// (the test would still pass on the unoptimized form, but is named so a
/// future refactor that breaks the short-circuit is loud).
#[test]
fn lookup_table_read_past_max_end_returns_none() {
    let table = MemRegionsLookupTable::new([
        make_region(0x1000, 16),
        make_region(0x2000, 16),
        make_region(0x3000, 16),
    ]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x4000, &mut buf), None);
    assert_eq!(table.read(u64::MAX, &mut buf), None);
}

/// Pinned contract: the short-circuit MUST NOT misfire on `addr` that is
/// inside a region — even when `addr` is well past *some* regions and only
/// the highest-end region contains it. Otherwise the optimization would
/// have silently broken correctness.
#[test]
fn lookup_table_read_short_circuit_does_not_misfire_on_mapped_addr() {
    let table = MemRegionsLookupTable::new([
        make_region(0x1000, 16),
        make_region(0x2000, 16),
        make_region(0x3000, 16),
    ]);
    let mut buf = [0u8; 1];
    // Inside the last region — must read, not short-circuit.
    assert_eq!(table.read(0x3008, &mut buf), Some(1));
    assert_eq!(buf[0], 8);
    // Inside an interior region.
    assert_eq!(table.read(0x2005, &mut buf), Some(1));
    assert_eq!(buf[0], 5);
}
```

- [ ] **Step 2: Run the new tests on the current code**

Run: `cargo test -p reader --test mem_region lookup_table_read_past_max_end_returns_none lookup_table_read_short_circuit_does_not_misfire_on_mapped_addr`
Expected: PASS. The first test passes today (correct `None`) but slowly; the second pins the correctness side. We add the field next.

- [ ] **Step 3: Add the cached upper bound**

In `crates/reader/src/lib.rs`, replace:

```rust
#[derive(Debug)]
pub struct MemRegionsLookupTable {
    /// Sorted map from region start address to the region itself.
    regions: BTreeMap<u64, MemRegion>,
}

impl MemRegionsLookupTable {
    /// Builds a lookup table from `regions`.
    ///
    /// If two regions share the same start address, the later one in iteration
    /// order overwrites the earlier one.
    pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
        let mut map = BTreeMap::new();
        for region in regions {
            map.insert(region.start_addr(), region);
        }
        Self { regions: map }
    }
```

with:

```rust
#[derive(Debug)]
pub struct MemRegionsLookupTable {
    /// Sorted map from region start address to the region itself.
    regions: BTreeMap<u64, MemRegion>,
    /// `max(region.end_addr() for region in regions)`, or `0` when empty.
    /// Used by [`read`](Self::read) to short-circuit lookups for addresses
    /// past every region. `0` is a safe sentinel for empty: with no regions
    /// every address satisfies `addr >= 0` and the short-circuit returns
    /// `None`, which matches the empty-table behavior.
    max_end_addr: u64,
}

impl MemRegionsLookupTable {
    /// Builds a lookup table from `regions`.
    ///
    /// If two regions share the same start address, the later one in iteration
    /// order overwrites the earlier one. `max_end_addr` is set to the
    /// largest `end_addr` across all regions accepted by the "last insert
    /// wins" rule (so a shadowed earlier region whose `end_addr` is larger
    /// than any survivor is *not* counted).
    pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
        let mut map = BTreeMap::new();
        for region in regions {
            map.insert(region.start_addr(), region);
        }
        let max_end_addr = map.values().map(MemRegion::end_addr).max().unwrap_or(0);
        Self { regions: map, max_end_addr }
    }
```

- [ ] **Step 4: Use the cache in `read`**

In `crates/reader/src/lib.rs`, replace:

```rust
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        for (_, region) in self.regions.range(..=addr).rev() {
            if let Some(n) = region.read(addr, out) {
                return Some(n);
            }
        }
        None
    }
```

with:

```rust
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        if addr >= self.max_end_addr {
            return None;
        }
        for (_, region) in self.regions.range(..=addr).rev() {
            if let Some(n) = region.read(addr, out) {
                return Some(n);
            }
        }
        None
    }
```

- [ ] **Step 5: Update the rustdoc to advertise the short-circuit**

In `crates/reader/src/lib.rs`, replace the docstring you just edited in Task A (case-1 wording) with one that reflects the new behavior. Replace:

```rust
    ///   1. **Unmapped past every region.** When `addr >= region.end_addr()`
    ///      for every region, the loop walks every region whose
    ///      `start_addr <= addr` before returning `None`. For typical inputs
    ///      with a handful of regions this is invisible; for tables with many
    ///      regions and frequent unmapped lookups it is observable.
```

with:

```rust
    ///   1. **Unmapped past every region.** When `addr >= max_end_addr` (the
    ///      largest `end_addr` across all stored regions) the call returns
    ///      `None` in O(1) without consulting any region. Empty tables have
    ///      `max_end_addr == 0`, so every address short-circuits to `None`.
```

- [ ] **Step 6: Re-run the new tests**

Run: `cargo test -p reader --test mem_region lookup_table_read_past_max_end_returns_none lookup_table_read_short_circuit_does_not_misfire_on_mapped_addr`
Expected: PASS — same observable behavior, now via the short-circuit path.

- [ ] **Step 7: Run the full reader test suite**

Run: `cargo test -p reader`
Expected: PASS — all 27 mem_region tests (now 29 with the two new pins) + every other test unchanged.

- [ ] **Step 8: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/reader/src/lib.rs crates/reader/tests/mem_region.rs
git commit -m "perf(reader): O(1) short-circuit MemRegionsLookupTable::read past max_end_addr"
```

---

## Final sanity sweep (Lane 1 or Lane 2)

**Files:** run-only, no edits.

- [ ] **Step 1: Full reader test suite**

Run: `cargo test -p reader`
Expected: PASS. Lane 1: counts unchanged. Lane 2: `mem_region` tests grow by 2.

- [ ] **Step 2: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full workspace build & test**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. No public-API change.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced. (Requires `binary_tests/out/x86/test.elf` — build with `make -C binary_tests` if missing.)

---

## Out of scope for round 8 (cumulative — preserved across rounds)

These are the items I either rejected fresh in round 8 or carried forward from rounds 1–7. Listed here so a hypothetical round 9 doesn't re-litigate them.

- **Use `contains()` upfront in `MemRegion::read`.** Reverts commit `16f104a` (round 4) which deliberately moved to a `checked_sub` chain.
- **`for { map.insert }` → `into_iter().collect()`** in `MemRegionsLookupTable::new`. Rejected round 5: pure cosmetic.
- **Cache `MemRegion::end_addr`.** Rejected round 6: cold path, churns the struct.
- **`MemRegion::data: Vec<u8>` → `Box<[u8]>`.** Rejected rounds 2/3/4.
- **Generic-over-section-vs-segment helper.** Rejected round 1: `object` provides no shared trait.
- **Rename `load_elf` to make the leak opt-in.** Rejected round 5; round 7 narrowed the wart.
- **`VnSpace` filter on `MemReader::read`.** Rejected rounds 6/7: trait doesn't restrict space; Sleigh in practice uses RAM only.
- **Reclaim leaked bytes via `unsafe { Box::from_raw }` in `load_elf`.** Rejected round 7.
- **Re-align column-aligned `let` in `section_is_code_or_readonly`.** Rejected round 6.
- **`if size == 0 || size > 8` → `!(1..=8).contains(&size)`** in `ReadOnlyMemory::read`. Rejected round 8 (this round): cosmetic, loses the explicit two-precondition spelling.
- **Combine the two early-return guards in `ReadOnlyMemory::read`.** Rejected round 8: same family as above.
- **Allocator-instrumented test that proves `load_elf` doesn't leak on error.** Out of scope (round 7).
- **`MemRegion::data` as `impl AsRef<[u8]>` field.** Rejected round 8: caller-side complication.
- **Make `ReadOnlyMemory: Send + Sync` optional.** Rejected round 8: every impl satisfies it; speculative subdivision.
- **`is_little_endian: bool` → `endianness: object::Endianness`.** Rejected round 8: no information loss; bool is denser.
- **Test consolidation across `tests/load_elf.rs` and `tests/error.rs`.** Rejected round 8: each file pins a different aspect (the `error.rs` version also asserts traceback chain).
- **Workspace-wide `clippy::pedantic`.** Deferred until/unless workspace opts in globally.
