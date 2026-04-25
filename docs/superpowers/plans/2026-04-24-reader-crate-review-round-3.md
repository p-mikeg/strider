# Reader Crate Review — Round 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the one remaining correctness gap (public fields let callers bypass the overflow-rejecting `MemRegion::new` added in Round 2) and fix one stale/mismatched pinned-contract banner in the test file.

**Architecture:** Two self-contained, independently-committable changes. No external crate reads or writes `MemRegion` fields (verified: `grep -rn 'MemRegion' --include='*.rs' | grep -v '^crates/reader/'` shows only trait and type references, never field access), so privatization is safe without rippling through the workspace.

**Tech Stack:** Rust, `object` crate, `rsleigh`, `strider-error`.

---

## Baseline (verified 2026-04-24)

- `cargo test -p reader` → all targets pass (25 tests in `mem_region`, plus `elf_reader`, `elf_converters`, `elf_smoke`, `error`, `load_elf`).
- `cargo clippy -p reader --all-targets --no-deps -- -D warnings` → clean.
- Round-1 (`2026-04-24-reader-crate-review.md`) and Round-2 (`2026-04-24-reader-crate-review-round-2.md`) plans are fully landed (latest reader-crate commit: `dd0bab3`).

---

## Open questions for the reviewer before execution

**Q1 — Privatize `MemRegion` fields?** Round 2 Task 4 added a fallible `MemRegion::new` that rejects `(start_addr, data)` pairs whose end would overflow `u64::MAX`. The downstream methods `end_addr` / `contains` / `read` depend on that invariant (they use plain-`u64` arithmetic). Today the fields stay `pub`, so a caller can bypass `new` entirely — via struct-literal construction, `region.data.push(0xff)`, or `region.start_addr = u64::MAX - 1`. The test suite itself does this at [mem_region.rs:148-151](crates/reader/tests/mem_region.rs#L148-L151).
  - **(A)** Privatize both fields; add `#[must_use] pub fn start_addr(&self) -> u64` and `#[must_use] pub fn data(&self) -> &[u8]` accessors. External callers (all in tests) switch to accessors. **Default choice** (correctness).
  - **(B)** Leave fields `pub`. Document "don't bypass `new`" and accept the gap. Cheap but leaves the invariant unenforceable.

  Assume **(A)**.

**Q2 — Move-semantics access for `data()`?** If Q1 is (A), the accessor returns `&[u8]`. An alternative is `into_data(self) -> Vec<u8>` for consumers that want the owned buffer without a re-copy. No current consumer wants this (the only consumption site is `MemRegionsLookupTable::new` which stores the whole `MemRegion`, not just its bytes). Skip. Not a task below.

---

## Task 1: Privatize `MemRegion` fields; add `start_addr()` / `data()` accessors

`MemRegion::new` establishes the "no overflow" invariant once at the boundary; the `pub` fields invite every caller to re-break it. Lock the invariant.

**Files:**
- Modify: [crates/reader/src/lib.rs:46-70](crates/reader/src/lib.rs#L46-L70) — struct + impl head.
- Modify: [crates/reader/src/lib.rs:128-134](crates/reader/src/lib.rs#L128-L134) — `MemRegionsLookupTable::new` reads `region.start_addr` directly.
- Modify: [crates/reader/tests/mem_region.rs](crates/reader/tests/mem_region.rs) — 1 read-field site + the `lookup_table_same_start_last_wins` mutation pattern.
- Modify: [crates/reader/tests/elf_converters.rs](crates/reader/tests/elf_converters.rs) — 6 read-field sites.

### Step-by-step

- [ ] **Step 1: Write a failing test that calls the new accessors**

Append to `crates/reader/tests/mem_region.rs`:

```rust
// ── accessor contract ─────────────────────────────────────────────────────

/// `start_addr()` and `data()` expose the region's invariants without
/// allowing callers to mutate them. If the fields stay `pub` this test
/// still passes; it's only meaningful once Task 1 privatizes them.
#[test]
fn mem_region_accessors_expose_start_and_data() {
    let r = MemRegion::new(0x1234, vec![0xaa, 0xbb, 0xcc]).expect("valid region");
    assert_eq!(r.start_addr(), 0x1234);
    assert_eq!(r.data(), &[0xaa, 0xbb, 0xcc]);
}
```

- [ ] **Step 2: Verify the test fails to compile**

Run: `cargo test -p reader --test mem_region mem_region_accessors_expose_start_and_data`
Expected: `error[E0599]: no method named 'start_addr' found for struct 'MemRegion'` (and similarly for `data`). That counts as the failing step per TDD — the type system is telling us to add the accessors.

- [ ] **Step 3: Privatize the fields and add accessors**

Replace the `MemRegion` struct + impl head in `crates/reader/src/lib.rs` with:

```rust
/// A contiguous range of bytes loaded at a fixed virtual address.
///
/// Corresponds to one backend-specific mapping (e.g. an ELF section or an
/// entry from a raw blob manifest) into the virtual address space of the
/// target binary.
///
/// Fields are private so the "no overflow" invariant established by
/// [`new`](Self::new) cannot be bypassed after construction. Read access
/// is via [`start_addr`](Self::start_addr) and [`data`](Self::data).
#[derive(Clone, Debug)]
pub struct MemRegion {
    start_addr: u64,
    data: Vec<u8>,
}

impl MemRegion {
    /// Creates a new `MemRegion` loaded at `start_addr`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::RegionOverflow`](error::ErrorKind::RegionOverflow)
    /// when `start_addr + data.len()` would exceed `u64::MAX`. This guarantees
    /// that downstream methods ([`end_addr`](Self::end_addr),
    /// [`contains`](Self::contains), [`read`](Self::read)) can treat the
    /// region's end as a plain `u64`.
    pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
        let len = data.len() as u64;
        if start_addr.checked_add(len).is_none() {
            return Err(error::ErrorKind::RegionOverflow { start_addr, len }.into());
        }
        Ok(Self { start_addr, data })
    }

    /// First virtual address covered by this region.
    #[must_use]
    pub fn start_addr(&self) -> u64 {
        self.start_addr
    }

    /// Raw bytes of the region, starting at [`start_addr`](Self::start_addr).
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
```

(The rest of the impl — `end_addr`, `contains`, `read` — is unchanged. Their bodies already use `self.start_addr` / `self.data` through the struct, which still works for private fields inside the same `impl` block.)

- [ ] **Step 4: Update the internal field access in `MemRegionsLookupTable::new`**

In `crates/reader/src/lib.rs`, change:

```rust
pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
    let mut map = BTreeMap::new();
    for region in regions {
        map.insert(region.start_addr, region);
    }
    Self { regions: map }
}
```

to use the accessor:

```rust
pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
    let mut map = BTreeMap::new();
    for region in regions {
        map.insert(region.start_addr(), region);
    }
    Self { regions: map }
}
```

- [ ] **Step 5: Fix the contrived mutation in `lookup_table_same_start_last_wins`**

In `crates/reader/tests/mem_region.rs`, replace the body of `lookup_table_same_start_last_wins` (lines 147-156) with:

```rust
#[test]
fn lookup_table_same_start_last_wins() {
    let r1 = MemRegion::new(0x1000, vec![0xaa; 4]).expect("valid region");
    let r2 = MemRegion::new(0x1000, vec![0xbb; 4]).expect("valid region");
    let table = MemRegionsLookupTable::new([r1, r2]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "last region with same start must win");
}
```

The behavior is identical — both regions still cover `0x1000..0x1004` and the second one still wins — but we no longer need to reach past `new()` to replace the bytes.

- [ ] **Step 6: Update the 6 read-field sites in `tests/elf_converters.rs`**

Change every `region.start_addr` / `region.data` (and `regions[0].start_addr` / `regions[0].data`) in `crates/reader/tests/elf_converters.rs` to its accessor form:

- [elf_converters.rs:35-36](crates/reader/tests/elf_converters.rs#L35-L36):
  ```rust
  assert_eq!(region.start_addr(), 0x1000);
  assert_eq!(region.data(), &[1, 2, 3, 4]);
  ```
- [elf_converters.rs:65-66](crates/reader/tests/elf_converters.rs#L65-L66):
  ```rust
  assert_eq!(regions[0].start_addr(), 0x1000);
  assert_eq!(regions[0].data(), &[1]);
  ```
- [elf_converters.rs:87](crates/reader/tests/elf_converters.rs#L87):
  ```rust
  let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
  ```
- [elf_converters.rs:125](crates/reader/tests/elf_converters.rs#L125):
  ```rust
  assert_eq!(regions[0].start_addr(), 0x1000);
  ```
- [elf_converters.rs:141](crates/reader/tests/elf_converters.rs#L141):
  ```rust
  let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr()).collect();
  ```
- [elf_converters.rs:233-234](crates/reader/tests/elf_converters.rs#L233-L234):
  ```rust
  assert_eq!(region.start_addr(), 0x1000);
  assert_eq!(region.data(), &[1, 2, 3, 4]);
  ```
- [elf_converters.rs:264](crates/reader/tests/elf_converters.rs#L264):
  ```rust
  assert_eq!(regions[0].start_addr(), 0x1000);
  ```
- [elf_converters.rs:278](crates/reader/tests/elf_converters.rs#L278):
  ```rust
  assert_eq!(regions[0].start_addr(), 0x1000);
  ```

- [ ] **Step 7: Run the reader test suite**

Run: `cargo test -p reader`
Expected: PASS, including the new `mem_region_accessors_expose_start_and_data` test. Total ≥ 26 mem_region tests (baseline 25 + the accessor contract test).

- [ ] **Step 8: Strict clippy + workspace build**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings && cargo build --workspace`
Expected: PASS. No external crate reads `MemRegion` fields, so the workspace compiles unchanged.

- [ ] **Step 9: Commit**

```bash
git add crates/reader/src/lib.rs crates/reader/tests/mem_region.rs crates/reader/tests/elf_converters.rs
git commit -m "refactor(reader): privatize MemRegion fields behind start_addr() / data() accessors"
```

---

## Task 2: Fix mismatched "pinned contract" banner in `tests/mem_region.rs`

The banner "Pinned contract #2: overlapping regions, later-start-wins" at [tests/mem_region.rs:201](crates/reader/tests/mem_region.rs#L201) sits above `lookup_table_shorter_inner_region_does_not_shadow_outer_tail`, which pins the *fall-through-to-outer* rule — a different contract. The test that actually pins "later-start-wins" (`lookup_table_overlapping_regions_later_start_shadows_earlier`) sits further down at [line 263](crates/reader/tests/mem_region.rs#L263) with no banner. The doc block is also stacked twice on the wrong test.

**Files:**
- Modify: [crates/reader/tests/mem_region.rs:200-277](crates/reader/tests/mem_region.rs#L200-L277)

### Step-by-step

- [ ] **Step 1: Restructure the two pinned-contract sections**

In `crates/reader/tests/mem_region.rs`, replace lines 200-277 with the following. This:
  1. Keeps both tests' bodies verbatim (no behavior change).
  2. Splits the stacked docblock so each banner sits above its matching test.
  3. Renumbers: #2 = later-start-wins on same-size overlap, #3 = fall-through to outer when inner is shorter.
  4. Leaves the unrelated `MemRegion::new overflow rejection` section (currently at lines 230-260) alone — it's a separate concern and its heading is already accurate.

```rust
// ── Pinned contract #2: overlapping regions, later-start-wins ─────────────

/// Pinned contract: when two regions overlap but have different start
/// addresses, the region whose `start_addr` is the latest <= `addr` wins.
/// The earlier region's bytes in the overlap are shadowed.
///
/// This falls out of the BTreeMap range query but the BEHAVIOR matters to
/// callers; future backends that register overlapping regions must know.
#[test]
fn lookup_table_overlapping_regions_later_start_shadows_earlier() {
    let a = MemRegion::new(0x1000, vec![0xaa; 0x20]).expect("valid region"); // [0x1000..0x1020)
    let b = MemRegion::new(0x1010, vec![0xbb; 0x20]).expect("valid region"); // [0x1010..0x1030)
    let table = MemRegionsLookupTable::new([a, b]);
    let mut buf = [0u8; 1];

    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xaa, "pre-overlap resolves to A");

    assert_eq!(table.read(0x1010, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "overlap resolves to B (later start wins)");

    assert_eq!(table.read(0x101f, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "A's tail in overlap is shadowed");
}

// ── Pinned contract #3: fall-through when later region is shorter ─────────

/// Pinned contract: when a later-starting region is *shorter* and does not
/// cover `addr`, lookup must fall through to an earlier region that does.
/// Without this, overlapping regions silently lose data.
#[test]
fn lookup_table_shorter_inner_region_does_not_shadow_outer_tail() {
    // Outer A: [0x1000..0x1020), all 0xaa
    // Inner B: [0x1010..0x1014), all 0xbb  (shorter, starts inside A)
    let a = MemRegion::new(0x1000, vec![0xaa; 0x20]).expect("valid region");
    let b = MemRegion::new(0x1010, vec![0xbb; 0x04]).expect("valid region");
    let table = MemRegionsLookupTable::new([a, b]);
    let mut buf = [0u8; 1];

    // 0x1018 is in A's tail but past B's end.
    assert_eq!(table.read(0x1018, &mut buf), Some(1));
    assert_eq!(buf[0], 0xaa, "should fall through to A when B does not cover addr");

    // Inside B's range, B still wins (existing "later start wins" rule).
    assert_eq!(table.read(0x1011, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb);
}

// ── MemRegion::new overflow rejection ─────────────────────────────────────

/// `MemRegion::new` rejects any (start_addr, data) whose end exceeds u64::MAX.
/// The returned error carries the offending start and length for diagnostics.
#[test]
fn mem_region_new_rejects_overflow() {
    use reader::ErrorKind;
    let start = u64::MAX - 3;
    // len = 4 ⇒ end would be u64::MAX + 1 — reject.
    let err = MemRegion::new(start, vec![0u8; 4])
        .expect_err("overflowing region must be rejected");
    match err.kind() {
        ErrorKind::RegionOverflow { start_addr, len } => {
            assert_eq!(*start_addr, start);
            assert_eq!(*len, 4u64);
        }
        other => panic!("expected RegionOverflow, got {other:?}"),
    }
}

/// Exact-fit at the top of the address space is accepted: start = u64::MAX - 3,
/// len = 3 makes end_addr = u64::MAX (representable as u64).
#[test]
fn mem_region_new_accepts_exact_fit_at_top_of_address_space() {
    let start = u64::MAX - 3;
    let r = MemRegion::new(start, vec![1u8, 2, 3]).expect("exact-fit region is legal");
    assert_eq!(r.end_addr(), u64::MAX);
    assert!(r.contains(start));
    assert!(r.contains(u64::MAX - 1));
    assert!(!r.contains(u64::MAX), "end_addr is exclusive");
}
```

(Net result: the `lookup_table_overlapping_regions_later_start_shadows_earlier` test moves *up* a few lines so both pinned-contract sections are adjacent; the two `MemRegion::new` tests stay where they are; no test is added, removed, or renamed.)

- [ ] **Step 2: Run the reader suite**

Run: `cargo test -p reader --test mem_region`
Expected: PASS — same test names, same bodies. Only comment/order shifted.

- [ ] **Step 3: Strict clippy**

Run: `cargo clippy -p reader --tests --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader/tests/mem_region.rs
git commit -m "docs(reader): realign pinned-contract banners with the tests they describe"
```

---

## Task 3: Final sanity sweep

**Files:** run-only, no edits.

- [ ] **Step 1: Full reader test suite**

Run: `cargo test -p reader`
Expected: PASS. Total mem_region tests: 26 (baseline 25 + Task 1's accessor-contract test). All other targets unchanged.

- [ ] **Step 2: Reader-only clippy (strict)**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full workspace build & test**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. No external crate touches `MemRegion` fields directly, so workspace compiles unchanged.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced.

---

## Out of scope (considered, deferred or unchanged)

- **Cross-crate cleanups carried over from Round 2**: the two `ElfFileMemReader` instances in `crates/analyzer/examples/analyzer.rs` and the three `load_elf(path.to_str().ok_or(...)?)` calls in `crates/analyzer/tests/analyze_binary.rs` (still present; since Round 1 broadened `load_elf` to `impl AsRef<Path>`, the `.to_str()` dance is dead weight). Belongs to the analyzer crate's own review, not this crate.
- **Pedantic clippy lints**: `clippy::pedantic` surfaces ~40 warnings across `crates/reader` (mostly "doc item missing backticks" and "long literal lacking separators" in tests). Stylistic noise, no correctness or readability win, and enabling them would churn unrelated lines — deferred until/unless the workspace decides to turn on pedantic globally.
- **`MemRegion::data` as `Box<[u8]>`**: marginal memory savings, observable API churn — same conclusion as Round 2.
- **`elf_segment_to_mem_region` / `elf_section_to_mem_region` as public API**: still no production callers, still useful for backends. Keep per Round 2 Q2 (A).
- **`MemRegion::into_data(self) -> Vec<u8>`**: no consumer wants it today (see Q2 above).
