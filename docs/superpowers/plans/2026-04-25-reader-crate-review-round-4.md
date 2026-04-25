# Reader Crate Review — Round 4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close three small gaps that survived rounds 1–3 (a test coverage hole at the `MemRegionsLookupTable::read` zero-length boundary, mid-file `use` block in one integration test, an unused accumulator in the shared ELF fixture) plus one cross-crate ergonomics smell directly caused by the round-1 `load_elf` signature broadening, and add a converter-API index to the `elf` module doc.

**Architecture:** Each task is self-contained and independently committable. The only observable behavior change is Task 5 (dead-code removal in another crate). Tasks 1–4 are tests/docs/fixture only.

**Tech Stack:** Rust, `object` crate, `rsleigh`, `strider-error`.

---

## Baseline (verified 2026-04-25)

- `cargo test -p reader` → 26 mem_region tests + full `elf_reader`, `elf_converters`, `elf_smoke`, `error`, `load_elf` suites green.
- `cargo clippy -p reader --all-targets --no-deps -- -D warnings` → clean.
- Rounds 1 (`2026-04-24-reader-crate-review.md`), 2, and 3 are fully landed (latest reader-crate commit: `529ad0f`).

---

## Task 1: Pin `MemRegionsLookupTable::read` zero-length-buffer behavior

`MemRegion::read` has a pinned test (`mem_region_read_zero_length_buf`) asserting that a zero-length read into a valid region returns `Some(0)`. The lookup-table wrapper has no such pin. A future optimization (e.g. an early `if out.is_empty() { return Some(0); }`) could break the behavior at the table level without failing any existing test — and the semantic is subtle because for unmapped addresses the answer is still `None`.

**Files:**
- Modify: [crates/reader/tests/mem_region.rs](crates/reader/tests/mem_region.rs)

- [ ] **Step 1: Append the pinned test**

Append to `crates/reader/tests/mem_region.rs`:

```rust
// ── MemRegionsLookupTable: zero-length buffer boundary ───────────────────

/// Pinned contract: a zero-length read on `MemRegionsLookupTable::read`
/// succeeds for mapped addresses (`Some(0)`) and fails for unmapped
/// addresses (`None`). Mirrors the `MemRegion::read` pin and prevents a
/// future "early return if out.is_empty()" optimization from short-
/// circuiting the unmapped arm.
#[test]
fn lookup_table_read_zero_length_buf() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut empty: [u8; 0] = [];

    // Mapped address → Some(0). No bytes requested, but the address is real.
    assert_eq!(table.read(0x1000, &mut empty), Some(0));
    assert_eq!(table.read(0x1008, &mut empty), Some(0));

    // Unmapped address → None. Zero-length does not spuriously succeed.
    assert_eq!(table.read(0x0fff, &mut empty), None);
    assert_eq!(table.read(0x2000, &mut empty), None);
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p reader --test mem_region lookup_table_read_zero_length_buf`
Expected: PASS. The current implementation already satisfies this contract — we're pinning it so a future edit can't silently break it.

- [ ] **Step 3: Full reader suite**

Run: `cargo test -p reader`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader/tests/mem_region.rs
git commit -m "test(reader): pin zero-length read contract on MemRegionsLookupTable"
```

---

## Task 2: Hoist mid-file `use` block in `tests/elf_converters.rs`

The file has a second `use` block at [tests/elf_converters.rs:149-155](crates/reader/tests/elf_converters.rs#L149-L155) (importing `SegmentSpec`, `ObjectSegment`, and three segment converters). Round-2 Task 7 applied the same hoist to `elf_reader.rs`; this is the last file with the pattern.

**Files:**
- Modify: [crates/reader/tests/elf_converters.rs](crates/reader/tests/elf_converters.rs)

- [ ] **Step 1: Merge into the top-of-file `use` block**

Replace the top-of-file `use` block (lines 9–17) with:

```rust
use common::elf_fixture::{
    SectionSpec, SegmentSpec, build_elf_with_sections, build_elf_with_segments,
};
use object::Object;
use object::read::{ObjectSection, ObjectSegment};
use reader::elf::{
    elf_get_code_and_readonly_sections_as_mem_regions,
    elf_get_executable_sections_as_mem_regions,
    elf_get_executable_segments_as_mem_regions,
    elf_section_to_mem_region,
    elf_sections_to_mem_regions,
    elf_segment_to_mem_region,
    elf_segments_to_mem_regions,
};
```

- [ ] **Step 2: Delete the mid-file `use` block**

Remove lines 149–155 (the `use common::elf_fixture::{SegmentSpec, build_elf_with_segments};` / `use object::read::ObjectSegment;` / `use reader::elf::{...}` block before the "malformed section" test).

- [ ] **Step 3: Run the test target**

Run: `cargo test -p reader --test elf_converters`
Expected: PASS — same test names, same bodies, only imports moved.

- [ ] **Step 4: Strict clippy on tests**

Run: `cargo clippy -p reader --tests --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/reader/tests/elf_converters.rs
git commit -m "refactor(reader): hoist all use statements to the top of elf_converters integration test"
```

---

## Task 3: Drop unused `sec_indices` accumulators in `tests/common/elf_fixture.rs`

Both `build_sections_elf` and `build_elf_with_segments` construct a `sec_indices: Vec<SectionIndex>` by calling `w.reserve_section_index()` in a loop, then never read the vec. The segment builder even has a `let _ = sec_indices;` suppression at the end (line 360). The `reserve_section_index()` *calls* are load-bearing (they mutate the writer's state); the returned indices are not. `build_one_section_elf` is unaffected — its `sec_idx` / `shstrtab_idx` are used in assertions at lines 112–113.

**Files:**
- Modify: [crates/reader/tests/common/elf_fixture.rs](crates/reader/tests/common/elf_fixture.rs)

- [ ] **Step 1: Simplify `build_sections_elf`**

In `crates/reader/tests/common/elf_fixture.rs`, replace the accumulator loop (around lines 178–183):

```rust
let mut name_ids = Vec::with_capacity(sections.len());
let mut sec_indices = Vec::with_capacity(sections.len());
for spec in sections {
    name_ids.push(w.add_section_name(spec.name));
    sec_indices.push(w.reserve_section_index());
}
```

with:

```rust
let mut name_ids = Vec::with_capacity(sections.len());
for spec in sections {
    name_ids.push(w.add_section_name(spec.name));
    w.reserve_section_index();
}
```

- [ ] **Step 2: Simplify `build_elf_with_segments`**

In the same file, around lines 277–287, replace:

```rust
let mut name_ids = Vec::with_capacity(segments.len());
let mut sec_indices = Vec::with_capacity(segments.len());
for i in 0..segments.len() {
    // Writer::add_section_name takes &'a [u8] bound to the writer's
    // lifetime. Leaking per-call is acceptable in test-fixture code
    // that runs a handful of times per test binary.
    let owned: &'static [u8] =
        Box::leak(format!(".seg{i}").into_boxed_str().into_boxed_bytes());
    name_ids.push(w.add_section_name(owned));
    sec_indices.push(w.reserve_section_index());
}
```

with:

```rust
let mut name_ids = Vec::with_capacity(segments.len());
for i in 0..segments.len() {
    // Writer::add_section_name takes &'a [u8] bound to the writer's
    // lifetime. Leaking per-call is acceptable in test-fixture code
    // that runs a handful of times per test binary.
    let owned: &'static [u8] =
        Box::leak(format!(".seg{i}").into_boxed_str().into_boxed_bytes());
    name_ids.push(w.add_section_name(owned));
    w.reserve_section_index();
}
```

Also delete the trailing `let _ = sec_indices;` line (around line 360).

- [ ] **Step 3: Run tests**

Run: `cargo test -p reader`
Expected: PASS — the writer still reserves the same number of section headers; no observable fixture change.

- [ ] **Step 4: Strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/reader/tests/common/elf_fixture.rs
git commit -m "refactor(reader): drop unused sec_indices accumulators in elf fixture builders"
```

---

## Task 4: Index the converter API in the `elf` module doc

`crates/reader/src/elf.rs` has six public converters plus one reader type plus `load_elf`; a contributor reading the top of the file today learns only that it's "the ELF-specific half of the reader crate." A short listing in the module doc gives them the API surface at a glance without having to scroll through ~280 lines of function bodies.

**Files:**
- Modify: [crates/reader/src/elf.rs:1-6](crates/reader/src/elf.rs#L1-L6)

- [ ] **Step 1: Extend the module doc**

Replace the module-level doc block (lines 1–6) in `crates/reader/src/elf.rs`:

```rust
//! ELF-backed implementation of [`crate::MemRegion`]s and the
//! [`rsleigh::MemReader`] trait.
//!
//! This module is the ELF-specific half of the `reader` crate. The generic
//! region-lookup machinery (`MemRegion`, `MemRegionsLookupTable`) lives in
//! [`crate`] so other backends (raw blobs, PE, Mach-O, …) can reuse it.
```

with:

```rust
//! ELF-backed implementation of [`crate::MemRegion`]s and the
//! [`rsleigh::MemReader`] trait.
//!
//! This module is the ELF-specific half of the `reader` crate. The generic
//! region-lookup machinery (`MemRegion`, `MemRegionsLookupTable`) lives in
//! [`crate`] so other backends (raw blobs, PE, Mach-O, …) can reuse it.
//!
//! # Converter API
//!
//! Single-item converters (take one segment/section, return one region):
//! - [`elf_segment_to_mem_region`]
//! - [`elf_section_to_mem_region`]
//!
//! Batch converters (iterate all segments/sections, return matching regions):
//! - [`elf_segments_to_mem_regions`] (filter by any predicate)
//! - [`elf_sections_to_mem_regions`] (filter by any predicate)
//!
//! Filter presets (batch converters wired to common predicates):
//! - [`elf_get_executable_segments_as_mem_regions`] — `PF_X`
//! - [`elf_get_executable_sections_as_mem_regions`] — `SHF_EXECINSTR`
//! - [`elf_get_code_and_readonly_sections_as_mem_regions`] — `SHF_ALLOC &&
//!   (SHF_EXECINSTR || !SHF_WRITE)`; the preset used by [`ElfFileMemReader`].
//!
//! Top-level helpers:
//! - [`ElfFileMemReader`] — owns its regions; implements both
//!   [`rsleigh::MemReader`] and [`crate::ReadOnlyMemory`].
//! - [`load_elf`] — reads an ELF from disk and returns a `'static`-lifetime
//!   parsed `object::File` (intentionally leaks the backing bytes).
```

- [ ] **Step 2: Verify `cargo doc` renders the links**

Run: `cargo doc -p reader --no-deps`
Expected: builds with no `rustdoc::broken_intra_doc_links` warnings. (If any link fails, the diagnostic will name the missing item.)

- [ ] **Step 3: Full reader test suite + strict clippy**

Run: `cargo test -p reader && cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader/src/elf.rs
git commit -m "docs(reader): add converter API index to elf module doc"
```

---

## Task 5: Strip obsolete `.to_str().ok_or(...)` dance in analyzer tests

Round-1 Task 6 of the reader review broadened `reader::load_elf` to accept `impl AsRef<Path>`. Three callers in the analyzer tests still do `reader::load_elf(path.to_str().ok_or("non-utf8 path")?)`. That's pure dead weight: `Path` already implements `AsRef<Path>`.

**Files:**
- Modify: [crates/analyzer/tests/analyze_binary.rs:184](crates/analyzer/tests/analyze_binary.rs#L184)
- Modify: [crates/analyzer/tests/analyze_binary.rs:199](crates/analyzer/tests/analyze_binary.rs#L199)
- Modify: [crates/analyzer/tests/analyze_binary.rs:214](crates/analyzer/tests/analyze_binary.rs#L214)

- [ ] **Step 1: Replace each call site**

At each of the three lines, change:

```rust
let obj = reader::load_elf(path.to_str().ok_or("non-utf8 path")?)?;
```

to:

```rust
let obj = reader::load_elf(&path)?;
```

(`path` is a `PathBuf` in these setup helpers — `&PathBuf` coerces to `&Path` which is `AsRef<Path>`.)

- [ ] **Step 2: Run the analyzer test target**

Run: `cargo test -p analyzer --test analyze_binary`
Expected: PASS. The `.to_str()` dance was the only thing the non-utf8 error path ever produced — in a test that writes its own tempfile paths, the path is always utf-8.

- [ ] **Step 3: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/analyzer/tests/analyze_binary.rs
git commit -m "refactor(analyzer-tests): drop obsolete .to_str() dance around reader::load_elf"
```

---

## Task 6: Final sanity sweep

**Files:** run-only, no edits.

- [ ] **Step 1: Full reader test suite**

Run: `cargo test -p reader`
Expected: PASS. Total mem_region tests: 27 (baseline 26 + Task 1's zero-length pin).

- [ ] **Step 2: Reader-only clippy (strict)**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full workspace build & test**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced. (Requires `binary_tests/out/x86/test.elf` — build with `make -C binary_tests` if missing.)

---

## Out of scope (considered, rejected or deferred)

- **Double `ElfFileMemReader::from_object(&obj)` in `crates/analyzer/examples/analyzer.rs`** (lines 14–15): copies sections twice. Real concern, but the fix is an analyzer-side refactor (share via `Arc` or restructure `rsleigh::Sleigh::new` + `opt::LoadReadOnly` plumbing), not a reader-crate change. Belongs to an analyzer review.
- **`MemRegion::data: Vec<u8>` → `Box<[u8]>`**: marginal memory win, churns API; rejected in rounds 2 and 3.
- **Pedantic clippy opt-in**: surfaces ~40 stylistic warnings; not worth the churn without a workspace-wide decision.
- **`Clone` on `ElfFileMemReader`/`MemRegionsLookupTable`**: no concrete consumer yet. Wait.
- **Generic-over-section-vs-segment helper**: no common `object` trait; a macro would be heavier than the duplicated bodies. Rejected in round 1.
- **`into_data(self) -> Vec<u8>` accessor**: no consumer. Rejected in round 3.
