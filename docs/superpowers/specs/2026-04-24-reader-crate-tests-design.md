# Reader crate test suite — design

**Date:** 2026-04-24
**Scope:** `crates/reader/`
**Goal:** Basic and comprehensive tests for the `reader` crate, organized so future backends (PE, Mach-O, raw blob) slot in without rewriting scaffolding.

---

## Context

The `reader` crate has two layers:

1. **Backend-agnostic memory storage** (`src/lib.rs`): `MemRegion`, `MemRegionsLookupTable`, `RegionsMemReader`, the `ReadOnlyMemory` trait.
2. **ELF backend** (`src/elf.rs`): conversion helpers (`elf_segment_to_mem_region`, `elf_sections_to_mem_regions`, filter predicates), the `ElfFileMemReader` type, its `rsleigh::MemReader` and `ReadOnlyMemory` trait impls, and the top-level `load_elf` function.

Errors are produced via the `strider_error::define_error!` macro (`src/error.rs`), giving every `Error` a location chain and backtrace.

### Current test state (before this work)

- `src/lib.rs` — 20 in-source tests covering `MemRegion`, `MemRegionsLookupTable`, `RegionsMemReader` happy paths + common failure modes.
- `src/elf.rs` — 9 in-source tests covering the `ReadOnlyMemory` impl (endianness, bad sizes, non-RAM spaces, unmapped, partial). Uses a `#[cfg(test)] pub(crate) from_parts` constructor.
- `tests/traceback.rs` — 2 integration tests for traceback invariants.

### Gaps

- Every free `elf_*` conversion and filter function is untested.
- `ElfFileMemReader::{from_object, from_bytes, from_path, from_elf_segments, from_elf_sections}` are untested against real (or synthetic) ELF bytes.
- `ElfFileMemReader`'s `rsleigh::MemReader` impl has no tests (only `ReadOnlyMemory` does).
- `load_elf` success path, malformed-bytes path.
- Error conversions: `From<io::Error>` and `From<object::Error>` paths are only indirectly covered.
- Three silent behavioral contracts are untested (see "Pinned contracts" below).

---

## Design decisions

### D1 — Fully integration-style tests

All tests live in `crates/reader/tests/`. The in-source `#[cfg(test)] mod tests` blocks in `src/lib.rs` and `src/elf.rs` are removed, and the `#[cfg(test)] pub(crate) from_parts` test-only constructor is deleted. Integration tests construct an `ElfFileMemReader` from synthetic ELF bytes via `from_bytes` instead — no test-only API surface leaks into the crate.

**Why:** matches the recent precedent in `pattern` (commit `586cba4`), gives black-box coverage, avoids widening public surface for tests.

### D2 — Hybrid ELF fixtures (synthetic + one real smoke)

Most tests build ELF bytes on the fly using `object::write` (dev-dep with the `write` feature). One end-to-end smoke test loads `binary_tests/out/x64/test.elf` to prove the stack composes against a real toolchain-produced ELF; it skips with a printed message (no panic) if the file is absent, matching `cfg::cfg_integration` and `analyzer::analyze_binary` convention.

**Why:** synthetic ELFs give deterministic coverage of every filter predicate and dedup branch (`PF_X`, `SHF_EXECINSTR`, `SHF_WRITE`, `SHT_NOBITS`, duplicate start addresses, empty data) without forcing callers to build C fixtures. One real-binary smoke test preserves end-to-end confidence.

**Fallback if `object::write` balloons:** check in 2–3 tiny pre-built ELFs at `tests/fixtures/*.elf` built by a one-shot shell script. Committed to only on contact with reality.

### D3 — Shared reader-contract helpers

`tests/common/reader_contract.rs` exposes generic assertions over `rsleigh::MemReader<Err = reader::Error>` and `reader::ReadOnlyMemory`. Each backend test file builds its reader and runs the contract helpers plus backend-specific tests.

**Why:** when `tests/pe_reader.rs` arrives, it calls the same helpers; no duplication, no abstraction debt today (one concrete backend still benefits from the named assertions as readable tests).

### D4 — Pin three silent contracts

These behaviors are today implicit. Tests assert them as deliberate design choices with comments explaining the rationale, so future backend authors find the reasoning:

1. **Cross-region boundary reads** — `MemRegionsLookupTable::read` that spans adjacent regions returns only the first region's bytes. The table does not continue into the next region.
2. **Overlapping-region shadowing** — when two regions overlap with different start addresses, the region with the latest `start_addr <= addr` wins; the earlier region's bytes in the overlap are shadowed.
3. **`ReadOnlyMemory` strict vs `MemReader` permissive** — a partial read produces `None` from `ReadOnlyMemory` but `Ok(n)` with `n < buf.len()` from `MemReader`.

---

## File layout

```
crates/reader/tests/
├── common/
│   ├── mod.rs                 # re-exports
│   ├── reader_contract.rs     # generic assertions over MemReader + ReadOnlyMemory
│   └── elf_fixture.rs         # synthetic-ELF builder helpers (object::write)
├── mem_region.rs              # MemRegion + MemRegionsLookupTable + RegionsMemReader
├── elf_converters.rs          # free elf_* conversion helpers + filter predicates
├── elf_reader.rs              # ElfFileMemReader constructors + trait impls
├── elf_smoke.rs               # single end-to-end test against binary_tests/out/x64/test.elf
├── load_elf.rs                # load_elf success + malformed + missing-path
└── error.rs                   # every ErrorKind variant + From paths (absorbs tests/traceback.rs)
```

---

## Components

### `common/reader_contract.rs` — backend-agnostic assertions

```rust
// MemReader
pub fn assert_mem_reader_reads<R>(r: &R, addr: u64, expected: &[u8])
where
    R: rsleigh::MemReader,
    R::Err: std::fmt::Debug;

pub fn assert_mem_reader_unmapped_is_not_mapped_error<R>(r: &R, addr: u64)
where
    R: rsleigh::MemReader<Err = reader::Error>;

pub fn assert_mem_reader_partial_read_ok<R>(
    r: &R, addr: u64, buf_len: usize, expected_n: usize,
)
where
    R: rsleigh::MemReader,
    R::Err: std::fmt::Debug;

// ReadOnlyMemory
pub fn assert_readonly_reads(
    r: &impl reader::ReadOnlyMemory,
    space: rsleigh::VnSpace,
    addr: u64,
    size: usize,
    expected: u64,
);

pub fn assert_readonly_returns_none(
    r: &impl reader::ReadOnlyMemory,
    space: rsleigh::VnSpace,
    addr: u64,
    size: usize,
);

pub fn assert_readonly_rejects_non_ram_spaces(
    r: &impl reader::ReadOnlyMemory, mapped_addr: u64,
);

pub fn assert_readonly_rejects_bad_sizes(
    r: &impl reader::ReadOnlyMemory, mapped_addr: u64,
);
```

Endianness assertions are **not** part of the contract (the trait doesn't surface endianness); those tests live in `elf_reader.rs`.

### `common/elf_fixture.rs` — synthetic ELF builders

```rust
pub struct SectionSpec {
    pub name: &'static str,
    pub addr: u64,
    pub data: Vec<u8>,
    pub exec: bool,
    pub writable: bool,
    pub nobits: bool, // SHT_NOBITS (no file-backed data)
}

pub struct SegmentSpec {
    pub addr: u64,
    pub data: Vec<u8>,
    pub exec: bool, // PF_X
}

pub fn build_elf_with_sections(
    sections: &[SectionSpec],
    endianness: object::Endianness,
    arch: object::Architecture,
) -> Vec<u8>;

pub fn build_elf_with_segments(
    segments: &[SegmentSpec],
    endianness: object::Endianness,
    arch: object::Architecture,
) -> Vec<u8>;

/// Convenience: a single-.text-section LE x86-64 ELF with `bytes` at `addr`.
pub fn simple_text_elf(addr: u64, bytes: &[u8]) -> Vec<u8>;
```

### `tests/mem_region.rs`

- Port all 20 in-source tests from `src/lib.rs` verbatim (with `use reader::*` imports).
- **New:** `lookup_table_cross_boundary_read_stops_at_first_region_end` (pinned contract #1).
- **New:** `lookup_table_overlapping_regions_later_start_shadows_earlier` (pinned contract #2).

### `tests/elf_converters.rs`

All tests drive synthetic ELFs through the public conversion helpers. Coverage:

- `elf_segment_to_mem_region` — round-trip (addr + data match source segment).
- `elf_section_to_mem_region` — round-trip.
- `elf_segments_to_mem_regions` —
  - `filter` predicate is honored.
  - Empty-data segments are skipped.
  - Duplicate `start_addr`: last segment wins.
- `elf_sections_to_mem_regions` —
  - `filter` predicate is honored.
  - Empty-data sections are skipped.
  - `SHT_NOBITS` section is skipped (empty `data()`).
  - Duplicate `start_addr`: last section wins.
- `elf_get_executable_segments_as_mem_regions` — includes `PF_X` segments; excludes non-exec.
- `elf_get_executable_sections_as_mem_regions` — includes `SHF_EXECINSTR`; excludes otherwise.
- `elf_get_code_and_readonly_sections_as_mem_regions` —
  - Includes executable (`.text`).
  - Includes non-writable, non-exec (`.rodata`).
  - Excludes writable non-exec (`.data`).
  - Excludes `SHT_NOBITS` (`.bss`).

### `tests/elf_reader.rs`

- Port all 9 in-source `ReadOnlyMemory` tests from `src/elf.rs`, replacing the `reader_with(...)` helper (which uses `from_parts`) with `simple_text_elf(...)` + `ElfFileMemReader::from_bytes`.
- **New:** `elf_reader_partial_read_asymmetry_between_traits` (pinned contract #3).
- **New — constructors:** `from_object`, `from_bytes`, `from_path` all succeed on a synthetic ELF and produce equivalent region contents. `from_path` via `tempfile::NamedTempFile`.
- **New — legacy constructors:** `from_elf_segments` and `from_elf_sections` on an ELF with distinct segment/section mappings pick up only the expected one.
- **New — endianness round-trip:** build both LE and BE synthetic ELFs containing the same 4-byte value; `ReadOnlyMemory::read` produces the correct integer for each.
- **New — contract run:** exercise `common::reader_contract::*` helpers against a synthetic-ELF-backed `ElfFileMemReader`, covering MemReader reads, unmapped → `NotMapped`, MemReader partial reads, ReadOnlyMemory non-RAM-space rejection, and ReadOnlyMemory bad-size rejection.

### `tests/elf_smoke.rs`

One test: `ElfFileMemReader::from_path("<workspace>/binary_tests/out/x64/test.elf")` succeeds and reads at least one byte at the ELF entry point. Skip with a printed message (not panic) if the file is absent, matching `cfg::cfg_integration` convention. The path is resolved via `env!("CARGO_MANIFEST_DIR")`.

### `tests/load_elf.rs`

- Success: `load_elf` on a `NamedTempFile` containing synthetic-ELF bytes returns an `object::File<'static>` with expected architecture and endianness.
- Malformed: a temp file containing non-ELF bytes → `ErrorKind::Object(_)`.
- Missing path: `ErrorKind::Io(_)`.

### `tests/error.rs`

Replaces `tests/traceback.rs`. Local helper:

```rust
fn assert_has_traceback(err: &reader::Error) {
    assert!(!err.locations().is_empty());
    let s = err.backtrace().status();
    assert!(matches!(s, BacktraceStatus::Captured | BacktraceStatus::Disabled));
}
```

Tests:

- `not_mapped_carries_traceback_and_address` — direct construction; `Display` contains the hex addr; `kind()` matches `NotMapped`.
- `assertion_failed_carries_traceback_and_message` — direct construction; `Display` contains the message.
- `load_elf_missing_path_produces_io_error_variant` — real `?` path producing `ErrorKind::Io(_)`.
- `elf_reader_from_path_missing_produces_io_error_variant` — same via `ElfFileMemReader::from_path`.
- `elf_reader_from_bytes_garbage_produces_object_error_variant` — malformed bytes → `ErrorKind::Object(_)`.
- `question_mark_propagation_extends_location_chain` — `inner() -> outer()` with `?` at each boundary yields a chain of length ≥ 2.

**Note:** assumes `strider_error::define_error!` exposes `Error::kind(&self) -> &ErrorKind`. If not, fall back to `err.to_string()` prefix checks or `err.source()` matching — verified against `crates/strider-error/src/wrapper.rs` during implementation.

---

## Dev-dependency changes

In `crates/reader/Cargo.toml` under `[dev-dependencies]`:

- `object = { workspace = true, features = ["write"] }` — synthetic-ELF construction. (Runtime `object` dep stays feature-minimal.)
- `tempfile = "3"` — `from_path` and `load_elf` success paths. Add to `[workspace.dependencies]` if not already present.

---

## Migration plan

The migration is one logical change but will be split into review-sized commits:

Each step is a review-sized commit that leaves the crate building and all tests passing:

1. Add `common/elf_fixture.rs` with only `simple_text_elf` implemented, plus its first consumer: create `tests/elf_reader.rs` with ported `ReadOnlyMemory` tests using `simple_text_elf`, pinned contract #3, and new constructor tests. Delete the ported tests and the `#[cfg(test)] from_parts` from `src/elf.rs`. (`common/mod.rs` and `common/reader_contract.rs` are introduced in the step that first uses them.)
2. Create `tests/mem_region.rs` with ported in-source tests + pinned contracts #1 and #2. Delete the ported tests from `src/lib.rs`.
3. Flesh out `common/elf_fixture.rs` with `build_elf_with_sections` / `build_elf_with_segments`. Add `tests/elf_converters.rs`.
4. Add `common/reader_contract.rs` + `common/mod.rs`; wire the contract-helper runs into `tests/elf_reader.rs` and add the endianness round-trip tests there.
5. Add `tests/load_elf.rs`.
6. Add `tests/error.rs`; delete `tests/traceback.rs`.
7. Add `tests/elf_smoke.rs`.

**Expected final count:** ~55–65 tests (up from 31 today).

---

## Out of scope

- Property-based tests (proptest/quickcheck). Current assertions suffice; property tests can be layered later if needed.
- Fuzzing of `from_bytes`. A real concern for production robustness, but the task is tests for current functionality.
- Benchmarks. None exist for the crate today.
- Coverage tooling (`cargo-llvm-cov`, etc.). Separate concern.
- Tests for future backends (PE, Mach-O, raw blob). Out of scope by definition; the contract helpers make adding them cheap.
