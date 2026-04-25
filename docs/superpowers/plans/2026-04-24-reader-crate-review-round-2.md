# Reader Crate Review — Round 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Second-pass review of the `reader` crate after the Round 1 cleanup landed (commits `f7755ba`..`890b0d8`). Fix one latent correctness hole (silent error swallowing in batch ELF converters), harden one arithmetic site, correct an outdated doc claim, tighten readability of two hot spots, and drop a redundant Cargo entry. No behavior change for the happy path.

**Architecture:** Each task is self-contained and independently committable. External callers of the crate are unchanged: `opt::load_readonly` uses the `ReadOnlyMemory` trait only; `analyzer::examples::analyzer` and the `cfg` / `analyzer` tests use `load_elf` + `ElfFileMemReader::{from_path, from_object}`. The only observable change is Task 1, which makes a currently-silent error visible.

**Tech Stack:** Rust, `object` crate, `rsleigh`, `strider-error`.

---

## Baseline (verified 2026-04-24)

- `cargo test -p reader` → 23 passed, 0 failed.
- `cargo clippy -p reader --all-targets --no-deps -- -D warnings` → clean.
- Round-1 plan (`2026-04-24-reader-crate-review.md`) is fully landed through commit `890b0d8`.

This plan only covers items I found in a second read of `crates/reader/src/{lib,elf,error}.rs` and its test suite.

---

## Open questions for the reviewer before execution

**Q1 — batch converters silently swallow `object::Error` (Task 1).** `elf_segments_to_mem_regions` and `elf_sections_to_mem_regions` both do `let Ok(data) = seg.data() else { continue };`. For `SHT_NOBITS`, `data()` returns `Ok(&[])`, so NOBITS is handled by the existing `data.is_empty()` check — the `Err` arm only fires on a genuinely malformed segment/section. Today those are silently dropped, producing a reader that misses data and subsequently reports `NotMapped` for addresses callers expect to be live.
  - **(A)** Propagate the error with `?`. `from_object` fails at construction rather than reading-time. NOBITS still skipped via `is_empty()`. **Default choice** (correctness).
  - **(B)** Keep swallowing but drop the `Result` return type from both helpers (they become infallible). Faster happy path; hides real bugs.
  - **(C)** Keep both current behavior and `Result` shape. Pin the silent-skip rule with a test.

  Assume **(A)**. Tasks below reflect that.

**Q2 — `elf_segment_to_mem_region` / `elf_section_to_mem_region` as public API (Task 7 candidate, deferred).** These single-item converters have no production callers — only the `elf_converters.rs` integration tests exercise them. Options:
  - **(A)** Keep as-is. Small public surface, useful for external backends that want to build `MemRegion`s one at a time.
  - **(B)** Delete; tests inline `MemRegion::new(s.address(), s.data()?.to_vec())`.
  - **(C)** Downgrade to `pub(crate)`. Breaks the integration tests (separate crate).

  Default: **(A), no change.** Not worth the API churn. Listed only to close the loop.

**Q3 — overflow in `MemRegion::end_addr` (Task 4). RESOLVED — reject at construction with a new error variant.** Make `MemRegion::new` fallible: it returns `Err(ErrorKind::RegionOverflow { … })` when `start_addr + data.len()` would exceed `u64::MAX`. Once a `MemRegion` exists the "no overflow" invariant holds and `end_addr` / `contains` / `read` can keep their plain-`u64` shape. All 11 callsites in-tree (`src/elf.rs` x4, `tests/mem_region.rs` x7) need a `?` / `.expect(...)`. No external callers of `MemRegion::new` exist.

**Q4 — `Result<Vec<MemRegion>>` in the three `elf_get_*_as_mem_regions` helpers after Task 1.** Task 1 keeps them fallible (propagates `object::Error`). Leave as-is — consistent with the batch helpers and with `from_object`. (No separate task.)

---

## Task 1: Propagate `object::Error` from batch ELF converters

**Files:**
- Modify: [crates/reader/src/elf.rs:46-87](crates/reader/src/elf.rs#L46-L87)
- Modify: [crates/reader/tests/elf_converters.rs](crates/reader/tests/elf_converters.rs)

- [ ] **Step 1: Write a failing test pinning the new behavior**

Append to `crates/reader/tests/elf_converters.rs`:

```rust
// ── Task 1: malformed section surfaces as an error, not a silent skip ─────

/// Pinned contract: `elf_sections_to_mem_regions` propagates any
/// `object::Error` from `section.data()` rather than silently skipping the
/// offending section. NOBITS sections (where `data()` returns `Ok(&[])`) are
/// the *only* legitimate skip path; a real `Err` means the ELF is malformed
/// and silently dropping it would hand the caller a partially-loaded reader.
///
/// We synthesize the failure by pointing a PROGBITS section at a file offset
/// past the end of the buffer, which makes `section.data()` return Err.
#[test]
fn elf_sections_to_mem_regions_propagates_data_error() {
    use object::elf;
    use object::write::elf::{FileHeader, SectionHeader, Writer};
    use object::Endianness;

    // Hand-rolled ELF: one PROGBITS section whose sh_offset points past EOF.
    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let name = w.add_section_name(b".broken");
        let _sec = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_EXEC,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0x1000,
            sh_offset: 0xdead_beef, // past EOF → data() must fail
            sh_size: 4,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);
    let err = elf_sections_to_mem_regions(&obj, |_| true)
        .expect_err("malformed section must surface an error");
    assert!(
        matches!(err.kind(), reader::ErrorKind::Object(_)),
        "got {:?}",
        err.kind(),
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p reader --test elf_converters elf_sections_to_mem_regions_propagates_data_error`
Expected: FAIL — the current implementation swallows the error and returns `Ok(vec![])`.

- [ ] **Step 3: Change the two batch converters in `crates/reader/src/elf.rs`**

Replace the bodies of `elf_segments_to_mem_regions` and `elf_sections_to_mem_regions` with:

```rust
pub fn elf_segments_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Segment<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for seg in obj.segments() {
        let data = seg.data()?;
        if data.is_empty() || !filter(&seg) {
            continue;
        }
        out.push(MemRegion::new(seg.address(), data.to_vec()));
    }
    Ok(out)
}

pub fn elf_sections_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Section<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for sec in obj.sections() {
        let data = sec.data()?;
        if data.is_empty() || !filter(&sec) {
            continue;
        }
        out.push(MemRegion::new(sec.address(), data.to_vec()));
    }
    Ok(out)
}
```

Update the `# Errors` section on both helpers:

```rust
/// # Errors
///
/// Returns an error wrapping the underlying `object::Error` if any
/// segment's file-backed data cannot be read. NOBITS-style segments
/// (those with empty `data()`) are skipped rather than reported.
```

(Mirror the wording for sections.)

- [ ] **Step 4: Run the new test and the full reader suite**

Run: `cargo test -p reader`
Expected: all pass, including `elf_sections_to_mem_regions_propagates_data_error`.

- [ ] **Step 5: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. `from_object` / `from_bytes` / `from_path` unchanged for every valid ELF; the only behavior change is that a malformed ELF now fails at construction rather than reading-time, which is strictly more correct.

- [ ] **Step 6: Commit**

```bash
git add crates/reader/src/elf.rs crates/reader/tests/elf_converters.rs
git commit -m "fix(reader): propagate object::Error from batch ELF converters instead of silently skipping"
```

---

## Task 2: Fix outdated "non-overlapping" claim in `MemRegionsLookupTable` docs

Round 1's Task 1 taught `MemRegionsLookupTable::read` to fall through to earlier regions when a later, shorter region doesn't cover `addr`. The struct-level doc still calls the collection "non-overlapping," which is now stale and misleading.

**Files:**
- Modify: [crates/reader/src/lib.rs:96-107](crates/reader/src/lib.rs#L96-L107)

- [ ] **Step 1: Rewrite the struct-level doc**

Replace lines 96–107 of `crates/reader/src/lib.rs` with:

```rust
// ── MemRegionsLookupTable ─────────────────────────────────────────────────────

/// A fast lookup table over a collection of [`MemRegion`]s, possibly overlapping.
///
/// Regions are indexed by start address in a `BTreeMap`, giving O(log n)
/// candidate lookup via a range query. Two regions sharing the same start
/// address collapse: the last-inserted one wins. When regions overlap at
/// different start addresses, reads resolve by walking candidates from the
/// highest `start_addr <= addr` downward and returning the first region that
/// contains `addr`; this is O(log n) in the usual non-overlapping case and
/// O(n) in the worst case where every earlier region must be consulted.
#[derive(Debug)]
pub struct MemRegionsLookupTable {
    /// Sorted map from region start address to the region itself.
    regions: BTreeMap<u64, MemRegion>,
}
```

- [ ] **Step 2: Verify tests still pass**

Run: `cargo test -p reader && cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/reader/src/lib.rs
git commit -m "docs(reader): correct MemRegionsLookupTable doc to acknowledge overlap support"
```

---

## Task 3: Remove redundant `rsleigh` dev-dependency

`Cargo.toml` lists `rsleigh.workspace = true` in both `[dependencies]` and `[dev-dependencies]`. The first entry is already visible to tests (Cargo unifies the graph); the second is pure redundancy.

**Files:**
- Modify: [crates/reader/Cargo.toml](crates/reader/Cargo.toml)

- [ ] **Step 1: Drop the duplicate line**

In `crates/reader/Cargo.toml`, remove the trailing `rsleigh.workspace = true` from the `[dev-dependencies]` section. Final section should read:

```toml
[dev-dependencies]
object = { workspace = true, features = ["write"] }
tempfile = { workspace = true }
```

(Keep the `object` override — tests need the `"write"` feature which the main dep does not enable.)

- [ ] **Step 2: Verify tests still build & pass**

Run: `cargo test -p reader`
Expected: PASS (tests still see `rsleigh` via the normal dependency).

- [ ] **Step 3: Commit**

```bash
git add crates/reader/Cargo.toml
git commit -m "chore(reader): drop redundant rsleigh dev-dependency"
```

---

## Task 4: Reject overflowing `MemRegion`s at construction time

Make `MemRegion::new` fallible so the "no overflow" invariant is established once, at the boundary, rather than every time `end_addr` / `contains` / `read` run. Downstream code keeps its plain-`u64` shape. A new `ErrorKind::RegionOverflow { start_addr, len }` variant carries the offending values for diagnostics.

**Files:**
- Modify: [crates/reader/src/error.rs](crates/reader/src/error.rs)
- Modify: [crates/reader/src/lib.rs:54-67](crates/reader/src/lib.rs#L54-L67)
- Modify: [crates/reader/src/elf.rs:20-86](crates/reader/src/elf.rs#L20-L86)
- Modify: [crates/reader/tests/mem_region.rs](crates/reader/tests/mem_region.rs)

All 11 in-tree `MemRegion::new(...)` callsites are touched (verified by `grep -rn "MemRegion::new" crates/reader`). No callers exist outside the reader crate.

- [ ] **Step 1: Write the failing test first**

Append to `crates/reader/tests/mem_region.rs`:

```rust
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
            assert_eq!(*len, 4);
        }
        other => panic!("expected RegionOverflow, got {other:?}"),
    }
}

/// Exact-fit at the top of the address space is accepted: start = u64::MAX - 3,
/// len = 4 makes end_addr = u64::MAX - 3 + 4 = u64::MAX (representable as u64).
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

- [ ] **Step 2: Run the failing test**

Run: `cargo test -p reader --test mem_region mem_region_new_rejects_overflow`
Expected: compile error (signature mismatch: `MemRegion::new` currently returns `Self`, test expects `Result`). That counts as "fails" per TDD — the type system is telling us to implement the change.

- [ ] **Step 3: Add the `RegionOverflow` variant**

Replace `crates/reader/src/error.rs` with:

```rust
strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [std::io::Error, object::Error];

    /// Errors that can be produced by the reader crate.
    #[derive(Debug, thiserror::Error)]
    pub enum ErrorKind {
        /// The requested address is not mapped in any loaded region.
        #[error("address {0:#x} is not mapped")]
        NotMapped(u64),

        /// A `MemRegion` was constructed with a (start_addr, len) pair
        /// whose end would exceed `u64::MAX`.
        #[error("region at {start_addr:#x} with length {len} would overflow u64")]
        RegionOverflow { start_addr: u64, len: u64 },

        /// An I/O error occurred while reading a file.
        #[error("failed to read file: {0}")]
        Io(#[from] std::io::Error),

        /// An `object` crate error occurred while parsing or loading an ELF.
        #[error("failed to parse ELF: {0}")]
        Object(#[from] object::Error),
    }
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
```

- [ ] **Step 4: Make `MemRegion::new` fallible**

In `crates/reader/src/lib.rs`, replace the `impl MemRegion { pub fn new ... }` block with:

```rust
impl MemRegion {
    /// Creates a new `MemRegion` loaded at `start_addr`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::RegionOverflow`] when `start_addr + data.len()`
    /// would exceed `u64::MAX`. This guarantees that downstream methods
    /// ([`end_addr`](Self::end_addr), [`contains`](Self::contains),
    /// [`read`](Self::read)) can treat the region's end as a plain `u64`.
    pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
        let len = data.len() as u64;
        if start_addr.checked_add(len).is_none() {
            return Err(error::ErrorKind::RegionOverflow { start_addr, len }.into());
        }
        Ok(Self { start_addr, data })
    }
```

Leave the rest of the impl (`end_addr`, `contains`, `read`) alone — its `#[must_use]` attributes and doc comments are unchanged. The body of `end_addr` stays `self.start_addr + self.data.len() as u64`; update its doc to note the invariant:

```rust
/// One past the last virtual address covered by this region.
///
/// `end_addr == start_addr + data.len()`. Cannot overflow: the constructor
/// [`new`](Self::new) rejects any `(start_addr, data)` pair that would.
#[must_use]
pub fn end_addr(&self) -> u64 {
    self.start_addr + self.data.len() as u64
}
```

- [ ] **Step 5: Update the four `src/elf.rs` callsites**

`elf_segment_to_mem_region` and `elf_section_to_mem_region` already return `Result`; add `?` to the `new` call:

```rust
pub fn elf_segment_to_mem_region(segment: &object::read::Segment<'_, '_>) -> Result<MemRegion> {
    MemRegion::new(segment.address(), segment.data()?.to_vec())
}

pub fn elf_section_to_mem_region(section: &object::read::Section<'_, '_>) -> Result<MemRegion> {
    MemRegion::new(section.address(), section.data()?.to_vec())
}
```

In the two batch converters (already touched by Task 1), change the push lines to:

```rust
out.push(MemRegion::new(seg.address(), data.to_vec())?);
```

```rust
out.push(MemRegion::new(sec.address(), data.to_vec())?);
```

- [ ] **Step 6: Update the seven test callsites in `tests/mem_region.rs`**

Each of these now returns `Result<MemRegion>`; unwrap with `.expect("valid region")`:

- Line ~14 (`make_region`): change return to unwrap internally:
  ```rust
  fn make_region(start: u64, len: usize) -> MemRegion {
      let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
      MemRegion::new(start, data).expect("test region fits in u64")
  }
  ```
- Line ~27 (`mem_region_end_addr_empty`): `MemRegion::new(0x2000, vec![]).expect("valid region")`
- Line ~59 (`mem_region_empty_contains_nothing`): same pattern.
- Lines ~216–217 (`lookup_table_shorter_inner_region_does_not_shadow_outer_tail`): unwrap both `MemRegion::new` calls.
- Lines ~232–233 (`lookup_table_overlapping_regions_later_start_shadows_earlier`): unwrap both.

Leave the two new tests from Step 1 as-is (they intentionally match on `Result`).

- [ ] **Step 7: Run the full reader suite**

Run: `cargo test -p reader`
Expected: PASS — all prior tests (with `.expect(...)` added) plus the two new overflow tests.

- [ ] **Step 8: Workspace sanity + strict clippy**

Run: `cargo build --workspace && cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS. No external crate touches `MemRegion::new`, so the workspace compiles unchanged.

- [ ] **Step 9: Commit**

```bash
git add crates/reader/src/error.rs crates/reader/src/lib.rs crates/reader/src/elf.rs crates/reader/tests/mem_region.rs
git commit -m "fix(reader): reject overflowing MemRegion at construction with RegionOverflow error"
```

---

## Task 5: Simplify `MemRegion::read` with a single `checked_sub` chain

The current body calls `contains` (which re-computes the range) and then recomputes `addr - self.start_addr` with a trust-me comment ("safe: contains() guarantees offset < len"). A single `checked_sub` chain expresses the same logic with no comment needed and no double range check.

**Files:**
- Modify: [crates/reader/src/lib.rs:75-93](crates/reader/src/lib.rs#L75-L93)

- [ ] **Step 1: Rewrite the function**

Replace the body of `MemRegion::read` in `crates/reader/src/lib.rs` with:

```rust
/// Reads bytes starting at `addr` into `out`.
///
/// Returns the number of bytes copied, which may be less than `out.len()`
/// if `addr + out.len()` extends past the end of this region.
///
/// Returns `None` when `addr` is not within this region at all.
pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
    let offset = usize::try_from(addr.checked_sub(self.start_addr)?).ok()?;
    let available = self.data.len().checked_sub(offset)?;
    if available == 0 {
        return None;
    }
    let to_copy = available.min(out.len());
    out[..to_copy].copy_from_slice(&self.data[offset..offset + to_copy]);
    Some(to_copy)
}
```

(The existing `#[must_use] pub fn contains` is still useful for callers; leave it unchanged.)

- [ ] **Step 2: Run the full reader suite**

Run: `cargo test -p reader`
Expected: PASS. The existing pins for `mem_region_read_{full_at_start, mid_region, partial_past_end, zero_length_buf, outside_returns_none, at_end_addr_returns_none}` all cover this function.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader/src/lib.rs
git commit -m "refactor(reader): rewrite MemRegion::read around checked_sub"
```

---

## Task 6: Flatten the endianness double-match in `ReadOnlyMemory::read`

The impl matches on `self.endianness` twice — once to pick the slot, once to pick the conversion. A single boolean removes the duplicate and removes a real bug surface (adding a new arm to `object::Endianness` would require updating both matches).

**Files:**
- Modify: [crates/reader/src/elf.rs:231-257](crates/reader/src/elf.rs#L231-L257)

- [ ] **Step 1: Rewrite the impl body**

Replace the `impl crate::ReadOnlyMemory for ElfFileMemReader` block in `crates/reader/src/elf.rs` with:

```rust
impl crate::ReadOnlyMemory for ElfFileMemReader {
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        if space != rsleigh::VnSpace::RAM {
            return None;
        }
        if size == 0 || size > 8 {
            return None;
        }
        let is_little = matches!(self.endianness, object::Endianness::Little);
        // Place the read bytes at the endianness-appropriate end of an 8-byte
        // buffer so the final from_{le,be}_bytes produces the same numeric
        // value for an N-byte load as the target machine would.
        let mut buf = [0u8; 8];
        let slot = if is_little {
            &mut buf[..size]
        } else {
            &mut buf[8 - size..]
        };
        if self.lookup.read(addr, slot)? != size {
            return None;
        }
        Some(if is_little {
            u64::from_le_bytes(buf)
        } else {
            u64::from_be_bytes(buf)
        })
    }
}
```

- [ ] **Step 2: Run the reader suite — the LE/BE round-trip tests already pin this**

Run: `cargo test -p reader --test elf_reader`
Expected: PASS, including `ro_read_little_endian_u32`, `ro_read_big_endian_u32`, `ro_read_little_endian_u64`, `ro_read_single_byte`.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader/src/elf.rs
git commit -m "refactor(reader): collapse ReadOnlyMemory::read endianness match into a single branch"
```

---

## Task 7: Consolidate scattered `use` statements in `tests/elf_reader.rs`

The test file has `use` statements at lines 9, 26, 28, 135, 165–166, 190–195, 223–224. Cognitive load for a reader. All to the top.

**Files:**
- Modify: [crates/reader/tests/elf_reader.rs](crates/reader/tests/elf_reader.rs)

- [ ] **Step 1: Replace the top-of-file `use` block**

Replace lines 1–9 of `crates/reader/tests/elf_reader.rs` with:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for `reader::ElfFileMemReader` and its trait impls.

#[path = "common/mod.rs"]
mod common;

use std::io::Write as _;

use common::elf_fixture::{
    SegmentSpec, build_elf_with_segments, simple_text_elf, simple_text_elf_with_endian,
};
use common::reader_contract::{
    assert_mem_reader_partial_read_ok, assert_mem_reader_reads,
    assert_mem_reader_unmapped_is_not_mapped_error, assert_readonly_reads,
    assert_readonly_rejects_bad_sizes, assert_readonly_rejects_non_ram_spaces,
    assert_readonly_returns_none,
};
use object::{Endianness, File};
use reader::{ElfFileMemReader, ReadOnlyMemory};
use rsleigh::{MemReader, VnAddr, VnSpace};
use tempfile::NamedTempFile;
```

- [ ] **Step 2: Delete the now-duplicate `use` lines inside the body**

Remove every interior `use` statement from this file:
- Line 26: `use object::Endianness;`
- Line 28: `use common::elf_fixture::simple_text_elf_with_endian;`
- Line 135: `use rsleigh::{MemReader, VnAddr, VnSpace};`
- Lines 165–166: `use common::elf_fixture::{SegmentSpec, build_elf_with_segments};` and `use object::File;`
- Lines 190–195: the `use common::reader_contract::{...};` block
- Lines 223–224: `use std::io::Write as _;` and `use tempfile::NamedTempFile;`

Also delete the `use reader::MemRegionsLookupTable;` / `use reader::elf::elf_get_executable_segments_as_mem_regions;` lines inside the `elf_exec_segments_only_yield_mapped_regions` function body (lines 173–174). Replace the two callsites at lines 181 / 182 with fully-qualified paths to keep the test's imports entirely at the top:

```rust
let regions = reader::elf::elf_get_executable_segments_as_mem_regions(&obj).unwrap();
let table = reader::MemRegionsLookupTable::new(regions);
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p reader --test elf_reader`
Expected: PASS — all 13 tests.

- [ ] **Step 4: Run clippy on the test target**

Run: `cargo clippy -p reader --tests --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/reader/tests/elf_reader.rs
git commit -m "refactor(reader): hoist all use statements to the top of elf_reader integration test"
```

---

## Task 8: Final sanity sweep

**Files:** run-only, no edits.

- [ ] **Step 1: Full reader test suite**

Run: `cargo test -p reader`
Expected: PASS (now ≥ 25 tests: the baseline 23 plus Task 1 and Task 4 additions).

- [ ] **Step 2: Reader-only clippy (strict)**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full workspace build & test**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced.

---

## Out of scope (considered, rejected or deferred)

- **Drop `Result` return from `elf_{segments,sections}_to_mem_regions`**: chosen path in Q1 was (A) — propagate. (B) was tempting but hides real bugs.
- **Delete single-item converters `elf_segment_to_mem_region` / `elf_section_to_mem_region`**: Q2 default (A), keep. No production callers, but they're cheap public conveniences.
- **`Box<[u8]>` instead of `Vec<u8>` for `MemRegion::data`**: marginal memory savings (one word per region), observable API churn — not worth it.
- **Reduce `MemRegionsLookupTable::read` worst-case**: it's O(n) on overlap-heavy workloads. Real ELFs have ≤ ~30 regions, so measured O(log n) stands.
- **Two `ElfFileMemReader` instances in `analyzer::examples::analyzer`**: outside the reader crate. Flag as a separate micro-refactor in the `analyzer` crate review.
- **`.to_str().ok_or("non-utf8 path")?` in `analyzer/tests/analyze_binary.rs`**: redundant since Round 1 broadened `load_elf` to `impl AsRef<Path>`. Outside the reader crate.
