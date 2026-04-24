# Reader crate test suite — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the reader crate's in-source unit tests with a comprehensive integration-style test suite organized for future reader backends (PE, Mach-O, raw blob) via a shared reader-contract helper module.

**Architecture:** All tests in `crates/reader/tests/`. Synthetic ELF bytes built at runtime via `object::write::elf::Writer` low-level API (high-level API was ruled out — it cannot set `sh_addr`, confirmed at `object-0.38.1/src/write/elf/object.rs:864`). One real-binary smoke test uses `binary_tests/out/x64/test.elf`. Shared contract helpers in `tests/common/`.

**Tech Stack:** Rust 2024, `object` crate (with `write` feature as dev-dep), `tempfile`, existing `strider-error` / `rsleigh` deps.

**Spec:** [docs/superpowers/specs/2026-04-24-reader-crate-tests-design.md](../specs/2026-04-24-reader-crate-tests-design.md)

**Deviation from spec:** Spec's section D2 implies the high-level `object::write::Object` API. That API emits `sh_addr: 0` unconditionally, so it can't produce ELFs with sections at chosen virtual addresses — which every read test needs. Plan instead uses the **low-level** `object::write::elf::Writer` (same crate, same feature flag). The dev-dep addition is identical; only the fixture code changes. Static pre-built `.elf` fallback is not needed.

---

## File map

Final `crates/reader/` layout after this plan:

```
crates/reader/
├── Cargo.toml                     # MODIFIED: add object write feature + tempfile under [dev-dependencies]
├── src/
│   ├── lib.rs                     # MODIFIED: delete in-source `mod tests`
│   ├── elf.rs                     # MODIFIED: delete in-source `mod tests` + delete `from_parts`
│   └── error.rs                   # unchanged
└── tests/
    ├── common/
    │   ├── mod.rs                 # CREATED
    │   ├── reader_contract.rs     # CREATED
    │   └── elf_fixture.rs         # CREATED
    ├── mem_region.rs              # CREATED
    ├── elf_converters.rs          # CREATED
    ├── elf_reader.rs              # CREATED
    ├── elf_smoke.rs               # CREATED
    ├── load_elf.rs                # CREATED
    ├── error.rs                   # CREATED (replaces traceback.rs)
    └── traceback.rs               # DELETED
```

Each integration-test file in `tests/` is an independent crate. `common/` is included via `#[path = "common/mod.rs"] mod common;` from each consumer.

**Key fact about `tests/common/` in Rust:** unused items in `common/` will trigger `dead_code` warnings in any test crate that doesn't use them. The pattern is `#[allow(dead_code)] pub fn ...` in `common/` modules, which is safe because each crate only exercises a subset.

---

## Workspace-level prerequisite

Add `tempfile` to `[workspace.dependencies]` in the root `Cargo.toml` before any task needs it. Done in Task 1.

---

## Task 1: Add dev-deps and create `common/mod.rs` scaffold

**Files:**
- Modify: `Cargo.toml` (workspace)
- Modify: `crates/reader/Cargo.toml`
- Create: `crates/reader/tests/common/mod.rs`

- [ ] **Step 1: Add `tempfile` to workspace deps**

Edit `/home/mike/Desktop/strider/Cargo.toml`, adding under `[workspace.dependencies]` (placement: after the `object = "0.38.1"` line):

```toml
tempfile = "3.10"
```

- [ ] **Step 2: Add reader dev-deps**

Edit `/home/mike/Desktop/strider/crates/reader/Cargo.toml`, replacing the existing `[dev-dependencies]` block with:

```toml
[dev-dependencies]
object = { workspace = true, features = ["write"] }
tempfile = { workspace = true }
rsleigh.workspace = true
```

(`rsleigh` is added because integration tests call `rsleigh::MemReader` methods directly. It's already a normal dep — listing it as dev too is fine and makes the intent explicit; Cargo deduplicates.)

- [ ] **Step 3: Create `common/mod.rs` skeleton**

Create `/home/mike/Desktop/strider/crates/reader/tests/common/mod.rs` with:

```rust
// Shared helpers for the reader crate's integration tests.
//
// Included by each test file via:
//     #[path = "common/mod.rs"]
//     mod common;
//
// Items use `#[allow(dead_code)]` because any given test crate only
// exercises a subset — unused items would otherwise warn.

#![allow(dead_code)]
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

pub mod elf_fixture;
pub mod reader_contract;
```

Then create empty stubs so the module compiles:

`/home/mike/Desktop/strider/crates/reader/tests/common/elf_fixture.rs`:

```rust
//! Synthetic ELF byte builders used by integration tests.
//!
//! All builders produce a complete ELF byte buffer that `object::File::parse`
//! can consume. Sections are placed at caller-chosen virtual addresses by
//! writing via `object::write::elf::Writer` (low-level API).
```

`/home/mike/Desktop/strider/crates/reader/tests/common/reader_contract.rs`:

```rust
//! Backend-agnostic assertions over the `rsleigh::MemReader` and
//! `reader::ReadOnlyMemory` traits.
//!
//! When a new backend (PE, Mach-O, raw blob, …) lands, its test file
//! builds the reader and calls these helpers in addition to its own
//! backend-specific assertions.
```

- [ ] **Step 4: Build the workspace**

Run: `cargo build --workspace`
Expected: builds cleanly with no warnings. No tests yet in `tests/common/`, so nothing to run.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/reader/Cargo.toml crates/reader/tests/common/
git commit -m "$(cat <<'EOF'
test(reader): scaffold tests/common for integration suite

Adds object/write + tempfile dev-deps and empty fixture/contract modules
that subsequent tasks will flesh out.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Implement `simple_text_elf` fixture builder

**Files:**
- Modify: `crates/reader/tests/common/elf_fixture.rs`
- Create: `crates/reader/tests/elf_reader.rs` (temporarily holds just the fixture sanity check; will grow)

- [ ] **Step 1: Write the first consumer test that proves the fixture works**

Create `/home/mike/Desktop/strider/crates/reader/tests/elf_reader.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for `reader::ElfFileMemReader` and its trait impls.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::simple_text_elf;
use reader::{ElfFileMemReader, ReadOnlyMemory};

/// Sanity check: `simple_text_elf` produces bytes that
/// `ElfFileMemReader::from_bytes` can parse, and the resulting reader
/// reflects the single `.text` section at the chosen address.
#[test]
fn simple_text_elf_fixture_round_trips_through_elf_reader() {
    let elf = simple_text_elf(0x1000, &[0xaa, 0xbb, 0xcc, 0xdd]);
    let r = ElfFileMemReader::from_bytes(&elf).expect("parse synthetic ELF");

    // reading 4 bytes at 0x1000 as a little-endian u32 = 0xddccbbaa
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
        Some(0xddccbbaa),
    );
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test --package reader --test elf_reader simple_text_elf_fixture_round_trips_through_elf_reader`
Expected: FAIL with "cannot find function `simple_text_elf` in module `common::elf_fixture`".

- [ ] **Step 3: Implement `simple_text_elf`**

Replace the contents of `/home/mike/Desktop/strider/crates/reader/tests/common/elf_fixture.rs` with:

```rust
//! Synthetic ELF byte builders used by integration tests.
//!
//! All builders produce a complete ELF byte buffer that `object::File::parse`
//! can consume. Sections are placed at caller-chosen virtual addresses by
//! writing via `object::write::elf::Writer` (low-level API); the high-level
//! `object::write::Object` API always emits `sh_addr: 0`, which is useless
//! for testing a memory reader.

#![allow(dead_code)]

use object::Endianness;
use object::elf;
use object::write::elf::{FileHeader, SectionHeader, Writer};

/// Builds a minimal 64-bit little-endian x86-64 ELF with a single
/// `.text` section of `bytes` placed at virtual address `addr`.
///
/// Flags: `SHF_ALLOC | SHF_EXECINSTR`. `sh_type` is `SHT_PROGBITS`.
pub fn simple_text_elf(addr: u64, bytes: &[u8]) -> Vec<u8> {
    build_one_section_elf(OneSectionOpts {
        addr,
        data: bytes,
        endian: Endianness::Little,
        is_64: true,
        e_machine: elf::EM_X86_64,
        name: b".text",
        sh_type: elf::SHT_PROGBITS,
        sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR),
    })
}

/// Like `simple_text_elf` but lets the caller choose endianness. Used for
/// endianness round-trip tests.
pub fn simple_text_elf_with_endian(
    addr: u64,
    bytes: &[u8],
    endian: Endianness,
) -> Vec<u8> {
    build_one_section_elf(OneSectionOpts {
        addr,
        data: bytes,
        endian,
        is_64: true,
        e_machine: elf::EM_X86_64,
        name: b".text",
        sh_type: elf::SHT_PROGBITS,
        sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR),
    })
}

struct OneSectionOpts<'a> {
    addr: u64,
    data: &'a [u8],
    endian: Endianness,
    is_64: bool,
    e_machine: u16,
    name: &'a [u8],
    sh_type: u32,
    sh_flags: u64,
}

fn build_one_section_elf(opts: OneSectionOpts<'_>) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = Writer::new(opts.endian, opts.is_64, &mut buf);

        // Reserve indices.
        let _null_idx = w.reserve_null_section_index();
        let sec_name = w.add_section_name(opts.name);
        let sec_idx = w.reserve_section_index();
        let shstrtab_idx = w.reserve_shstrtab_section_index();

        // Reserve layout: file header, then section data, then section headers.
        w.reserve_file_header();

        let sec_offset = w.reserve(opts.data.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        // Write.
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_EXEC,
            e_machine: opts.e_machine,
            e_entry: opts.addr,
            e_flags: 0,
        })
        .expect("write file header");

        w.write(opts.data);
        w.write_shstrtab();

        // Section headers: null, our section, shstrtab.
        w.write_null_section_header();

        w.write_section_header(&SectionHeader {
            name: Some(sec_name),
            sh_type: opts.sh_type,
            sh_flags: opts.sh_flags,
            sh_addr: opts.addr,
            sh_offset: sec_offset as u64,
            sh_size: opts.data.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });

        w.write_shstrtab_section_header();

        debug_assert_eq!(sec_idx.0, 1);
        debug_assert_eq!(shstrtab_idx.0, 2);
    }
    buf
}
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test --package reader --test elf_reader simple_text_elf_fixture_round_trips_through_elf_reader`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/reader/tests/common/elf_fixture.rs crates/reader/tests/elf_reader.rs
git commit -m "$(cat <<'EOF'
test(reader): add simple_text_elf synthetic fixture

First of several synthetic-ELF builders; uses object::write::elf::Writer
low-level API so sections land at caller-chosen virtual addresses.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Port `ReadOnlyMemory` tests from `src/elf.rs` → `tests/elf_reader.rs`, add pinned contract #3

**Files:**
- Modify: `crates/reader/src/elf.rs` (delete in-source `mod tests`, delete `from_parts`)
- Modify: `crates/reader/tests/elf_reader.rs` (add ported + new tests)

- [ ] **Step 1: Add the ported `ReadOnlyMemory` tests at their new home**

Open `/home/mike/Desktop/strider/crates/reader/tests/elf_reader.rs` and append:

```rust
use object::Endianness;

use common::elf_fixture::simple_text_elf_with_endian;

// ── ReadOnlyMemory: space filter ──────────────────────────────────────────

/// Only `VnSpace::RAM` produces a hit; other spaces always return `None`.
#[test]
fn ro_read_non_ram_space_returns_none() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::REGISTER, 0x1000, 4), None);
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::UNIQUE, 0x1000, 4), None);
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::CONST, 0x1000, 4), None);
}

// ── ReadOnlyMemory: size bounds ───────────────────────────────────────────

/// `size == 0` is not a legitimate load; the trait returns `None`.
#[test]
fn ro_read_size_zero_returns_none() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 0), None);
}

/// `size > 8` exceeds what a `u64` can carry; the trait returns `None`.
#[test]
fn ro_read_size_greater_than_eight_returns_none() {
    let elf = simple_text_elf(0x1000, &[0u8; 16]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 9), None);
}

// ── ReadOnlyMemory: partial read ──────────────────────────────────────────

/// When the region can only supply a prefix of the requested bytes,
/// return `None` instead of truncated data.
#[test]
fn ro_read_partial_region_returns_none() {
    // region covers 0x1000..0x1004 (4 bytes)
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    // request 4 bytes starting 2 bytes before the end → only 2 available
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1002, 4), None);
}

/// An address outside any region returns `None`.
#[test]
fn ro_read_unmapped_address_returns_none() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x9000, 4), None);
}

// ── ReadOnlyMemory: endianness ────────────────────────────────────────────

/// 4 bytes `01 02 03 04` as little-endian u32 = 0x04030201.
#[test]
fn ro_read_little_endian_u32() {
    let elf = simple_text_elf_with_endian(
        0x1000, &[0x01, 0x02, 0x03, 0x04], Endianness::Little,
    );
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
        Some(0x04030201)
    );
}

/// 4 bytes `01 02 03 04` as big-endian u32 = 0x01020304.
#[test]
fn ro_read_big_endian_u32() {
    let elf = simple_text_elf_with_endian(
        0x1000, &[0x01, 0x02, 0x03, 0x04], Endianness::Big,
    );
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
        Some(0x01020304)
    );
}

/// 8-byte read picks up the full u64 with the correct endianness.
#[test]
fn ro_read_little_endian_u64() {
    let elf = simple_text_elf_with_endian(
        0x1000,
        &[0x78, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89],
        Endianness::Little,
    );
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 8),
        Some(0x89abcdef12345678)
    );
}

/// 1-byte reads do not depend on endianness.
#[test]
fn ro_read_single_byte() {
    let elf = simple_text_elf(0x1000, &[0xab]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 1),
        Some(0xab)
    );
}
```

- [ ] **Step 2: Add pinned contract #3 — the MemReader vs ReadOnlyMemory asymmetry**

Append to `tests/elf_reader.rs`:

```rust
use rsleigh::{MemReader, VnAddr, VnSpace};

/// Pinned contract: the two traits treat short reads differently.
///  * MemReader: partial read → Ok(n) with n < buf.len()
///  * ReadOnlyMemory: cannot satisfy full `size` → None (no truncation)
///
/// This documents a deliberate design choice: ReadOnlyMemory backs the
/// LoadReadOnly optimizer pass, which must never synthesize a constant
/// from partial bytes. MemReader backs Sleigh instruction fetch, where
/// a short read at the end of a section is an expected condition.
#[test]
fn elf_reader_partial_read_asymmetry_between_traits() {
    // 4-byte region; MemReader request for 8 bytes at start → Ok(4).
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();

    let mut buf = [0u8; 8];
    let n = MemReader::read(&r, VnAddr { off: 0x1000, space: VnSpace::RAM }, &mut buf)
        .expect("MemReader read");
    assert_eq!(n, 4, "MemReader permits partial reads");
    assert_eq!(&buf[..4], &[1, 2, 3, 4]);

    // Same region, ReadOnlyMemory request for size=8 at start → None.
    assert_eq!(
        ReadOnlyMemory::read(&r, VnSpace::RAM, 0x1000, 8),
        None,
        "ReadOnlyMemory must not truncate",
    );
}
```

Check the exact `VnAddr` field layout before running — the field names used above (`off`, `space`) come from the current `rsleigh` API as shown in `crates/reader/src/elf.rs:216`. If `rsleigh::VnAddr` differs, adjust to match.

- [ ] **Step 3: Run the new tests before touching the source**

Run: `cargo test --package reader --test elf_reader`
Expected: 10 PASS (fixture sanity + 8 ported + 1 pinned contract).

- [ ] **Step 4: Delete the in-source `mod tests` in `src/elf.rs`**

Edit `/home/mike/Desktop/strider/crates/reader/src/elf.rs`. Delete the entire `// ── tests ──` divider and `#[cfg(test)] mod tests { ... }` block (lines 267 through the end of file in the current state). Also delete the `#[cfg(test)] pub(crate) fn from_parts` method on `ElfFileMemReader` (currently at `src/elf.rs:197-209`):

```rust
    /// Test-only constructor that takes already-built parts.
    ///
    /// Useful for unit-testing the trait impls without a real ELF file.
    #[cfg(test)]
    pub(crate) fn from_parts(
        regions_mem_reader: RegionsMemReader,
        endianness: object::Endianness,
    ) -> Self {
        Self {
            regions_mem_reader,
            endianness,
        }
    }
```

Use Edit with that block as `old_string` and empty `new_string` (be sure to include the preceding blank line so you don't leave a doubled blank).

- [ ] **Step 5: Run the integration tests again plus the full workspace**

Run: `cargo test --package reader --test elf_reader`
Expected: 10 PASS.

Run: `cargo build --workspace` and `cargo clippy --workspace`
Expected: clean, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/reader/src/elf.rs crates/reader/tests/elf_reader.rs
git commit -m "$(cat <<'EOF'
test(reader): move ReadOnlyMemory tests to integration suite

Ports the 9 in-source tests to tests/elf_reader.rs driven by synthetic
ELF bytes, removes the test-only from_parts constructor, and pins the
MemReader-vs-ReadOnlyMemory partial-read asymmetry as a contract test.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Port backend-agnostic tests from `src/lib.rs` → `tests/mem_region.rs`, add pinned contracts #1 and #2

**Files:**
- Modify: `crates/reader/src/lib.rs` (delete in-source `mod tests`)
- Create: `crates/reader/tests/mem_region.rs`

- [ ] **Step 1: Create the integration file with ported tests + pinned contracts**

Create `/home/mike/Desktop/strider/crates/reader/tests/mem_region.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for `MemRegion`, `MemRegionsLookupTable`, and
//! `RegionsMemReader` — the backend-agnostic layer of the reader crate.

use reader::{MemRegion, MemRegionsLookupTable, RegionsMemReader};

// ── helpers ───────────────────────────────────────────────────────────────

/// Builds a `MemRegion` at `start` with `len` bytes, each equal to its
/// offset within the region (i.e. `data[i] == i as u8 & 0xff`).
fn make_region(start: u64, len: usize) -> MemRegion {
    let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
    MemRegion::new(start, data)
}

// ── MemRegion::end_addr ───────────────────────────────────────────────────

#[test]
fn mem_region_end_addr() {
    let r = make_region(0x1000, 16);
    assert_eq!(r.end_addr(), 0x1010);
}

#[test]
fn mem_region_end_addr_empty() {
    let r = MemRegion::new(0x2000, vec![]);
    assert_eq!(r.end_addr(), 0x2000);
}

// ── MemRegion::contains ───────────────────────────────────────────────────

#[test]
fn mem_region_contains_start() {
    let r = make_region(0x1000, 16);
    assert!(r.contains(0x1000));
}

#[test]
fn mem_region_contains_last_byte() {
    let r = make_region(0x1000, 16);
    assert!(r.contains(0x100f));
}

#[test]
fn mem_region_does_not_contain_end_addr() {
    let r = make_region(0x1000, 16);
    assert!(!r.contains(0x1010));
}

#[test]
fn mem_region_does_not_contain_before_start() {
    let r = make_region(0x1000, 16);
    assert!(!r.contains(0x0fff));
}

#[test]
fn mem_region_empty_contains_nothing() {
    let r = MemRegion::new(0x1000, vec![]);
    assert!(!r.contains(0x1000));
}

// ── MemRegion::read ───────────────────────────────────────────────────────

#[test]
fn mem_region_read_full_at_start() {
    let r = make_region(0x1000, 4);
    let mut buf = [0u8; 4];
    assert_eq!(r.read(0x1000, &mut buf), Some(4));
    assert_eq!(buf, [0, 1, 2, 3]);
}

#[test]
fn mem_region_read_mid_region() {
    let r = make_region(0x1000, 8);
    let mut buf = [0u8; 3];
    assert_eq!(r.read(0x1002, &mut buf), Some(3));
    assert_eq!(buf, [2, 3, 4]);
}

#[test]
fn mem_region_read_partial_past_end() {
    let r = make_region(0x1000, 4);
    let mut buf = [0xffu8; 8];
    assert_eq!(r.read(0x1002, &mut buf), Some(2));
    assert_eq!(buf[0], 2);
    assert_eq!(buf[1], 3);
    assert_eq!(buf[2], 0xff);
}

#[test]
fn mem_region_read_zero_length_buf() {
    let r = make_region(0x1000, 4);
    let mut buf: [u8; 0] = [];
    assert_eq!(r.read(0x1000, &mut buf), Some(0));
}

#[test]
fn mem_region_read_outside_returns_none() {
    let r = make_region(0x1000, 4);
    let mut buf = [0u8; 4];
    assert_eq!(r.read(0x2000, &mut buf), None);
}

#[test]
fn mem_region_read_at_end_addr_returns_none() {
    let r = make_region(0x1000, 4);
    let mut buf = [0u8; 1];
    assert_eq!(r.read(0x1004, &mut buf), None);
}

// ── MemRegionsLookupTable ─────────────────────────────────────────────────

#[test]
fn lookup_table_finds_address_in_single_region() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 2];
    assert_eq!(table.read(0x1000, &mut buf), Some(2));
    assert_eq!(buf, [0, 1]);
}

#[test]
fn lookup_table_miss_before_all_regions() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x0fff, &mut buf), None);
}

#[test]
fn lookup_table_miss_after_all_regions() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1010, &mut buf), None);
}

#[test]
fn lookup_table_two_regions_correct_dispatch() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16), make_region(0x2000, 16)]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1005, &mut buf), Some(1));
    assert_eq!(buf[0], 5);
    assert_eq!(table.read(0x2007, &mut buf), Some(1));
    assert_eq!(buf[0], 7);
}

#[test]
fn lookup_table_same_start_last_wins() {
    let mut r1 = make_region(0x1000, 4);
    r1.data = vec![0xaa, 0xaa, 0xaa, 0xaa];
    let mut r2 = make_region(0x1000, 4);
    r2.data = vec![0xbb, 0xbb, 0xbb, 0xbb];
    let table = MemRegionsLookupTable::new([r1, r2]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "last region with same start must win");
}

#[test]
fn lookup_table_empty_returns_none() {
    let table = MemRegionsLookupTable::new([]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), None);
}

#[test]
fn lookup_table_gap_between_regions_is_none() {
    let table = MemRegionsLookupTable::new([
        make_region(0x1000, 8),
        make_region(0x1010, 8),
    ]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1008, &mut buf), None);
    assert_eq!(table.read(0x100f, &mut buf), None);
}

// ── Pinned contract #1: cross-region boundary partial read ────────────────

/// Pinned contract: reads that span two adjacent regions return only the
/// first region's bytes. The lookup table does NOT continue into the next
/// region. A caller asking for 16 bytes at 0x1008 when regions cover
/// [0x1000..0x1010) and [0x1010..0x1020) gets Some(8), not Some(16).
///
/// If this test ever starts failing, someone has changed `MemRegionsLookupTable::read`
/// to stitch reads across region boundaries. That is a meaningful
/// semantic change — audit every caller of `.read()` before updating.
#[test]
fn lookup_table_cross_boundary_read_stops_at_first_region_end() {
    let table = MemRegionsLookupTable::new([
        make_region(0x1000, 16), // bytes 0..16 at 0x1000..0x1010
        make_region(0x1010, 16), // bytes 0..16 at 0x1010..0x1020
    ]);
    let mut buf = [0xffu8; 16];
    assert_eq!(table.read(0x1008, &mut buf), Some(8));
    // First 8 bytes are from the first region's tail (bytes 8..16).
    let expected: Vec<u8> = (8..16).collect();
    assert_eq!(&buf[..8], &expected[..]);
    assert_eq!(buf[8], 0xff, "second region must not be consulted");
}

// ── Pinned contract #2: overlapping regions, later-start-wins ─────────────

/// Pinned contract: when two regions overlap but have different start
/// addresses, the region whose start_addr is the latest <= addr wins.
/// The earlier region's bytes in the overlap are shadowed.
///
/// This falls out of the BTreeMap range query but the BEHAVIOR matters to
/// callers; future backends that register overlapping regions must know.
#[test]
fn lookup_table_overlapping_regions_later_start_shadows_earlier() {
    let a = MemRegion::new(0x1000, vec![0xaa; 0x20]); // [0x1000..0x1020)
    let b = MemRegion::new(0x1010, vec![0xbb; 0x20]); // [0x1010..0x1030)
    let table = MemRegionsLookupTable::new([a, b]);
    let mut buf = [0u8; 1];

    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xaa, "pre-overlap resolves to A");

    assert_eq!(table.read(0x1010, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "overlap resolves to B (later start wins)");

    assert_eq!(table.read(0x101f, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "A's tail in overlap is shadowed");
}

// ── RegionsMemReader ──────────────────────────────────────────────────────

#[test]
fn regions_mem_reader_delegates_read() {
    let mut r = make_region(0x4000, 8);
    r.data = vec![10, 20, 30, 40, 50, 60, 70, 80];
    let table = MemRegionsLookupTable::new([r]);
    let reader = RegionsMemReader::new(table);
    let mut buf = [0u8; 3];
    assert_eq!(reader.read(0x4002, &mut buf), Some(3));
    assert_eq!(buf, [30, 40, 50]);
}

#[test]
fn regions_mem_reader_miss_returns_none() {
    let table = MemRegionsLookupTable::new([make_region(0x4000, 8)]);
    let reader = RegionsMemReader::new(table);
    let mut buf = [0u8; 1];
    assert_eq!(reader.read(0x9000, &mut buf), None);
}
```

Note: this uses `r.data = ...` on `MemRegion` which requires the `data` field to be accessible (`pub`). Confirm by reading `crates/reader/src/lib.rs:57-63` — yes, `MemRegion::data` is `pub`, so this compiles from outside the crate.

- [ ] **Step 2: Run integration tests before deletion**

Run: `cargo test --package reader --test mem_region`
Expected: 24 tests PASS (22 ported + 2 pinned contracts).

If either pinned contract fails, STOP — the behavior described is not what the code does. Report to the user and do not proceed.

- [ ] **Step 3: Delete the in-source `mod tests` in `src/lib.rs`**

Edit `/home/mike/Desktop/strider/crates/reader/src/lib.rs`. Delete everything from the `// ── tests ──` divider (currently line 170) through the closing `}` of `mod tests` at the end of the file. Also delete the `#![cfg_attr(test, allow(...))]` inner attribute at the top of the file (lines 1-9): once all tests are in integration files, the clippy allow is no longer needed because the test bodies compile in their own crate, which has its own allow list.

After deletion, line 1 of `src/lib.rs` should be:

```rust
//! Memory readers for the Strider binary analysis framework.
```

- [ ] **Step 4: Run full suite and lints**

Run: `cargo test --package reader`
Expected: mem_region (24) + elf_reader (10) + traceback (2) = 36 tests PASS.

Run: `cargo clippy --workspace`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/reader/src/lib.rs crates/reader/tests/mem_region.rs
git commit -m "$(cat <<'EOF'
test(reader): move MemRegion/LookupTable tests to integration suite

Also pins two silent-until-now contracts: cross-region partial reads
stop at the first region, and overlapping regions resolve to the one
with the latest start_addr <= addr.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Extend `elf_fixture` with `build_elf_with_sections`

**Files:**
- Modify: `crates/reader/tests/common/elf_fixture.rs`

The section-based builder is needed for `tests/elf_converters.rs` (Task 6), which exercises `elf_*_to_mem_regions` with specific flag combinations and `SHT_NOBITS` sections.

- [ ] **Step 1: Design the spec struct and builder signature**

Append to `/home/mike/Desktop/strider/crates/reader/tests/common/elf_fixture.rs`:

```rust
/// Description of one section in a fixture ELF.
#[derive(Clone, Debug)]
pub struct SectionSpec {
    pub name: &'static [u8],
    pub addr: u64,
    pub data: Vec<u8>,
    pub exec: bool,
    pub writable: bool,
    /// If true, section is `SHT_NOBITS` (no file-backed data). `data` is
    /// ignored except that its length becomes `sh_size`.
    pub nobits: bool,
}

impl SectionSpec {
    pub fn text(addr: u64, data: Vec<u8>) -> Self {
        Self { name: b".text", addr, data, exec: true, writable: false, nobits: false }
    }
    pub fn rodata(addr: u64, data: Vec<u8>) -> Self {
        Self { name: b".rodata", addr, data, exec: false, writable: false, nobits: false }
    }
    pub fn data(addr: u64, data: Vec<u8>) -> Self {
        Self { name: b".data", addr, data, exec: false, writable: true, nobits: false }
    }
    pub fn bss(addr: u64, size: usize) -> Self {
        Self {
            name: b".bss", addr,
            data: vec![0; size],
            exec: false, writable: true, nobits: true,
        }
    }
}

/// Builds a 64-bit little-endian x86-64 ELF with the given sections, in
/// order. Each section lands at its `addr`; the writer emits `SHT_PROGBITS`
/// (or `SHT_NOBITS` if `spec.nobits`) with `SHF_ALLOC` plus `SHF_EXECINSTR`
/// / `SHF_WRITE` per the spec.
///
/// Sections with `nobits == true` contribute nothing to the file on-disk
/// but still have a section header with the right `sh_size` and `sh_type`.
/// This is how `object` models `.bss`.
pub fn build_elf_with_sections(sections: &[SectionSpec]) -> Vec<u8> {
    build_sections_elf(sections, Endianness::Little, true, elf::EM_X86_64)
}

fn build_sections_elf(
    sections: &[SectionSpec],
    endian: Endianness,
    is_64: bool,
    e_machine: u16,
) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, is_64, &mut buf);

        let _null_idx = w.reserve_null_section_index();

        // Reserve one section name + index per spec, preserving order.
        let mut name_ids = Vec::with_capacity(sections.len());
        let mut sec_indices = Vec::with_capacity(sections.len());
        for spec in sections {
            name_ids.push(w.add_section_name(spec.name));
            sec_indices.push(w.reserve_section_index());
        }
        let _shstrtab_idx = w.reserve_shstrtab_section_index();

        // Reserve layout.
        w.reserve_file_header();

        // Each non-NOBITS section reserves file space equal to its data.
        let mut sec_offsets: Vec<u64> = Vec::with_capacity(sections.len());
        for spec in sections {
            if spec.nobits {
                sec_offsets.push(0);
            } else {
                sec_offsets.push(w.reserve(spec.data.len(), 1) as u64);
            }
        }
        w.reserve_shstrtab();
        w.reserve_section_headers();

        // Write file header (no program headers in this builder — sections only).
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_EXEC,
            e_machine,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");

        // Section data.
        for spec in sections {
            if !spec.nobits {
                w.write(&spec.data);
            }
        }

        w.write_shstrtab();

        // Section headers.
        w.write_null_section_header();

        for (i, spec) in sections.iter().enumerate() {
            let mut sh_flags = u64::from(elf::SHF_ALLOC);
            if spec.exec {
                sh_flags |= u64::from(elf::SHF_EXECINSTR);
            }
            if spec.writable {
                sh_flags |= u64::from(elf::SHF_WRITE);
            }
            let sh_type = if spec.nobits { elf::SHT_NOBITS } else { elf::SHT_PROGBITS };
            let sh_size = spec.data.len() as u64;
            w.write_section_header(&SectionHeader {
                name: Some(name_ids[i]),
                sh_type,
                sh_flags,
                sh_addr: spec.addr,
                sh_offset: sec_offsets[i],
                sh_size,
                sh_link: 0,
                sh_info: 0,
                sh_addralign: 1,
                sh_entsize: 0,
            });
        }

        w.write_shstrtab_section_header();
    }
    buf
}
```

- [ ] **Step 2: Refactor `simple_text_elf` to reuse `build_sections_elf` (optional — only if it reduces code without breaking anything)**

Leave `build_one_section_elf` in place. The two builders share a lot but the simple variant has fewer moving parts, keeping the failure surface small for the already-green tests from Task 2/3. Do not refactor here.

- [ ] **Step 3: Verify the new builder compiles**

Run: `cargo build --package reader --tests`
Expected: clean. No runtime verification yet — Task 6 is the first consumer.

- [ ] **Step 4: Commit**

```bash
git add crates/reader/tests/common/elf_fixture.rs
git commit -m "$(cat <<'EOF'
test(reader): add build_elf_with_sections fixture builder

Supports PROGBITS and NOBITS sections with arbitrary flag combinations
at chosen virtual addresses. Consumers arrive in the next commit.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `tests/elf_converters.rs` — section-based converter and filter tests

**Files:**
- Create: `crates/reader/tests/elf_converters.rs`

- [ ] **Step 1: Create the file with section-level tests**

Create `/home/mike/Desktop/strider/crates/reader/tests/elf_converters.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the free `elf_*_to_mem_region(s)` functions and
//! the three filter helpers in `reader::elf`.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::{SectionSpec, build_elf_with_sections};
use object::read::ObjectSection;
use reader::elf::{
    elf_get_code_and_readonly_sections_as_mem_regions,
    elf_get_executable_sections_as_mem_regions,
    elf_section_to_mem_region,
    elf_sections_to_mem_regions,
};

/// Parses the bytes as an ELF; panics with a clear message if parse fails.
fn parse(bytes: &[u8]) -> object::File<'_> {
    object::File::parse(bytes).expect("parse synthetic ELF")
}

// ── elf_section_to_mem_region (single-section round-trip) ─────────────────

#[test]
fn elf_section_to_mem_region_preserves_addr_and_data() {
    let bytes = build_elf_with_sections(&[SectionSpec::text(0x1000, vec![1, 2, 3, 4])]);
    let obj = parse(&bytes);
    let sec = obj
        .section_by_name(".text")
        .expect("find .text in synthetic ELF");

    let region = elf_section_to_mem_region(&sec).expect("convert section");
    assert_eq!(region.start_addr, 0x1000);
    assert_eq!(region.data, vec![1, 2, 3, 4]);
}

// ── elf_sections_to_mem_regions: filter is honored ────────────────────────

#[test]
fn elf_sections_to_mem_regions_filter_rejects_all() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1]),
        SectionSpec::rodata(0x2000, vec![2]),
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |_| false).unwrap();
    assert!(regions.is_empty(), "filter=false must reject all");
}

#[test]
fn elf_sections_to_mem_regions_filter_selects_subset() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1]),
        SectionSpec::rodata(0x2000, vec![2]),
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |sec| {
        sec.name().map(|n| n == ".text").unwrap_or(false)
    })
    .unwrap();

    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].start_addr, 0x1000);
    assert_eq!(regions[0].data, vec![1]);
}

// ── elf_sections_to_mem_regions: NOBITS sections are skipped ──────────────

/// `.bss` is `SHT_NOBITS` — `section.data()` returns empty bytes. The
/// helper treats empty-data sections as skippable regardless of filter.
#[test]
fn elf_sections_to_mem_regions_skips_nobits() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2, 3]),
        SectionSpec::bss(0x2000, 64),
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |_| true).unwrap();
    assert_eq!(regions.len(), 1, "NOBITS section must be skipped");
    assert_eq!(regions[0].start_addr, 0x1000);
}

// ── elf_sections_to_mem_regions: same start_addr → last wins ──────────────

/// When two sections share a start_addr, the BTreeMap keyed by start_addr
/// keeps the last one inserted (iteration order = source order).
#[test]
fn elf_sections_to_mem_regions_same_start_last_wins() {
    // Two sections at 0x1000; sections are iterated in source order, so
    // .text (first) is overwritten by .rodata (second).
    let bytes = build_elf_with_sections(&[
        SectionSpec { name: b".first", addr: 0x1000, data: vec![0xaa], exec: true,  writable: false, nobits: false },
        SectionSpec { name: b".second", addr: 0x1000, data: vec![0xbb], exec: false, writable: false, nobits: false },
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |_| true).unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].data, vec![0xbb], "later section wins on duplicate start_addr");
}

// ── elf_get_executable_sections_as_mem_regions ────────────────────────────

#[test]
fn elf_exec_sections_include_shf_execinstr_and_exclude_others() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1]),     // SHF_EXECINSTR
        SectionSpec::rodata(0x2000, vec![2]),   // not exec
        SectionSpec::data(0x3000, vec![3]),     // not exec
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_executable_sections_as_mem_regions(&obj).unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].start_addr, 0x1000);
}

// ── elf_get_code_and_readonly_sections_as_mem_regions ─────────────────────

#[test]
fn elf_code_and_readonly_sections_include_text_and_rodata_exclude_data_and_bss() {
    let bytes = build_elf_with_sections(&[
        SectionSpec::text(0x1000, vec![1, 2]),    // exec     → include
        SectionSpec::rodata(0x2000, vec![3, 4]),  // ro data  → include
        SectionSpec::data(0x3000, vec![5, 6]),    // writable → exclude
        SectionSpec::bss(0x4000, 16),             // NOBITS   → exclude (empty data)
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();

    let addrs: Vec<u64> = regions.iter().map(|r| r.start_addr).collect();
    assert!(addrs.contains(&0x1000), ".text must be included");
    assert!(addrs.contains(&0x2000), ".rodata must be included");
    assert!(!addrs.contains(&0x3000), ".data must be excluded");
    assert!(!addrs.contains(&0x4000), ".bss must be excluded");
    assert_eq!(regions.len(), 2);
}
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test --package reader --test elf_converters`
Expected: 7 PASS.

If any test fails, STOP. A failure here means either the fixture doesn't produce the flags we claim, or the reader's filter predicates disagree. Diagnose before proceeding.

- [ ] **Step 3: Commit**

```bash
git add crates/reader/tests/elf_converters.rs
git commit -m "$(cat <<'EOF'
test(reader): add section-based elf converter tests

Covers elf_section_to_mem_region, elf_sections_to_mem_regions (filter,
NOBITS skip, same-start dedup), and the two section-level filter helpers.
Segment-based helpers land in a follow-up commit.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Extend `elf_fixture` with `build_elf_with_segments`

**Files:**
- Modify: `crates/reader/tests/common/elf_fixture.rs`

Segment-based fixtures are needed for the segment helpers and `ElfFileMemReader::from_elf_segments`. They look similar to the section builder but emit program headers with `p_vaddr` / `p_flags` instead.

- [ ] **Step 1: Add the segment spec and builder**

Append to `/home/mike/Desktop/strider/crates/reader/tests/common/elf_fixture.rs`:

```rust
use object::write::elf::ProgramHeader;

/// Description of one PT_LOAD segment in a fixture ELF.
#[derive(Clone, Debug)]
pub struct SegmentSpec {
    pub addr: u64,
    pub data: Vec<u8>,
    pub exec: bool,
}

/// Builds a 64-bit little-endian x86-64 ELF with the given segments, each
/// as a PT_LOAD with `p_vaddr = addr`, `p_flags = PF_R | (PF_X if exec)`.
///
/// A single `.text`-named section is also emitted per segment so the
/// file also parses via the section view — but the typical consumer is
/// the segment-level readers.
pub fn build_elf_with_segments(segments: &[SegmentSpec]) -> Vec<u8> {
    let endian = Endianness::Little;
    let is_64 = true;

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, is_64, &mut buf);

        // Section index layout: null, [one per segment], shstrtab.
        let _null_idx = w.reserve_null_section_index();
        let mut name_ids = Vec::with_capacity(segments.len());
        let mut sec_indices = Vec::with_capacity(segments.len());
        for i in 0..segments.len() {
            // Give each a unique name so we can disambiguate.
            let owned: &'static [u8] = Box::leak(format!(".seg{i}").into_boxed_str().into_boxed_bytes());
            name_ids.push(w.add_section_name(owned));
            sec_indices.push(w.reserve_section_index());
        }
        let _shstrtab_idx = w.reserve_shstrtab_section_index();

        // Layout.
        w.reserve_file_header();
        w.reserve_program_headers(segments.len() as u32);

        let mut data_offsets: Vec<u64> = Vec::with_capacity(segments.len());
        for spec in segments {
            data_offsets.push(w.reserve(spec.data.len(), 1) as u64);
        }

        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_EXEC,
            e_machine: elf::EM_X86_64,
            e_entry: segments.first().map(|s| s.addr).unwrap_or(0),
            e_flags: 0,
        })
        .expect("write file header");

        w.write_align_program_headers();
        for (i, spec) in segments.iter().enumerate() {
            let mut p_flags = u32::from(elf::PF_R);
            if spec.exec {
                p_flags |= u32::from(elf::PF_X);
            }
            w.write_program_header(&ProgramHeader {
                p_type: elf::PT_LOAD,
                p_flags,
                p_offset: data_offsets[i],
                p_vaddr: spec.addr,
                p_paddr: spec.addr,
                p_filesz: spec.data.len() as u64,
                p_memsz: spec.data.len() as u64,
                p_align: 1,
            });
        }

        // Segment data.
        for spec in segments {
            w.write(&spec.data);
        }

        w.write_shstrtab();

        // Section headers: one SHT_PROGBITS per segment so the section
        // view is consistent with the segment view.
        w.write_null_section_header();
        for (i, spec) in segments.iter().enumerate() {
            let mut sh_flags = u64::from(elf::SHF_ALLOC);
            if spec.exec {
                sh_flags |= u64::from(elf::SHF_EXECINSTR);
            }
            w.write_section_header(&SectionHeader {
                name: Some(name_ids[i]),
                sh_type: elf::SHT_PROGBITS,
                sh_flags,
                sh_addr: spec.addr,
                sh_offset: data_offsets[i],
                sh_size: spec.data.len() as u64,
                sh_link: 0,
                sh_info: 0,
                sh_addralign: 1,
                sh_entsize: 0,
            });
        }
        w.write_shstrtab_section_header();
    }
    buf
}
```

**Note on `Box::leak`:** the `add_section_name` API takes `&'a [u8]` bound to the writer's lifetime. Per-call-leaking is ugly but acceptable inside a test fixture that runs a handful of times. If a cleaner alternative surfaces (e.g. passing owned names via a builder-scope `Vec<String>`), adopt it — but not before functional tests pass.

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --package reader --tests`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/reader/tests/common/elf_fixture.rs
git commit -m "$(cat <<'EOF'
test(reader): add build_elf_with_segments fixture builder

Emits PT_LOAD program headers plus matching SHT_PROGBITS sections so
both segment-level and section-level reader paths can be exercised.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Segment converter tests + `from_elf_segments` constructor test

**Files:**
- Modify: `crates/reader/tests/elf_converters.rs` (add segment-level tests)
- Modify: `crates/reader/tests/elf_reader.rs` (add `from_elf_segments` test)

- [ ] **Step 1: Append segment tests to `elf_converters.rs`**

Append to `/home/mike/Desktop/strider/crates/reader/tests/elf_converters.rs`:

```rust
use common::elf_fixture::{SegmentSpec, build_elf_with_segments};
use object::read::ObjectSegment;
use reader::elf::{
    elf_get_executable_segments_as_mem_regions,
    elf_segment_to_mem_region,
    elf_segments_to_mem_regions,
};

// ── elf_segment_to_mem_region ─────────────────────────────────────────────

#[test]
fn elf_segment_to_mem_region_preserves_addr_and_data() {
    let bytes = build_elf_with_segments(&[SegmentSpec {
        addr: 0x1000,
        data: vec![1, 2, 3, 4],
        exec: true,
    }]);
    let obj = parse(&bytes);
    let seg = obj.segments().next().expect("at least one segment");

    let region = elf_segment_to_mem_region(&seg).expect("convert segment");
    assert_eq!(region.start_addr, 0x1000);
    assert_eq!(region.data, vec![1, 2, 3, 4]);
}

// ── elf_segments_to_mem_regions: filter honored ───────────────────────────

#[test]
fn elf_segments_to_mem_regions_filter_rejects_all() {
    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![1], exec: true },
        SegmentSpec { addr: 0x2000, data: vec![2], exec: false },
    ]);
    let obj = parse(&bytes);
    let regions = elf_segments_to_mem_regions(&obj, |_| false).unwrap();
    assert!(regions.is_empty());
}

#[test]
fn elf_segments_to_mem_regions_filter_selects_exec_only() {
    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![1], exec: true },
        SegmentSpec { addr: 0x2000, data: vec![2], exec: false },
    ]);
    let obj = parse(&bytes);
    let regions = elf_segments_to_mem_regions(&obj, |seg| matches!(
        seg.flags(),
        object::read::SegmentFlags::Elf { p_flags }
            if p_flags & u32::from(object::elf::PF_X) != 0,
    ))
    .unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].start_addr, 0x1000);
}

// ── elf_get_executable_segments_as_mem_regions ────────────────────────────

#[test]
fn elf_exec_segments_include_pf_x_and_exclude_others() {
    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![1], exec: true },   // PF_X
        SegmentSpec { addr: 0x2000, data: vec![2], exec: false },  // no PF_X
    ]);
    let obj = parse(&bytes);
    let regions = elf_get_executable_segments_as_mem_regions(&obj).unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].start_addr, 0x1000);
}
```

- [ ] **Step 2: Add `from_elf_segments` test to `tests/elf_reader.rs`**

Append to `/home/mike/Desktop/strider/crates/reader/tests/elf_reader.rs`:

```rust
use common::elf_fixture::{SegmentSpec, build_elf_with_segments};
use object::File;

/// `from_elf_segments` picks up only the executable segment, not other
/// PT_LOADs. Addresses outside the executable segment's range are
/// unmapped.
#[test]
fn elf_reader_from_elf_segments_picks_exec_only() {
    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![0xaa, 0xbb], exec: true },
        SegmentSpec { addr: 0x2000, data: vec![0xcc, 0xdd], exec: false },
    ]);
    let obj = File::parse(&bytes[..]).unwrap();
    let r = ElfFileMemReader::from_elf_segments(&obj).unwrap();

    // exec segment is reachable
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 2),
        Some(0xbbaa),
    );
    // non-exec segment is not reachable via from_elf_segments
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x2000, 2),
        None,
    );
}
```

- [ ] **Step 3: Run both test files**

Run: `cargo test --package reader --test elf_converters`
Expected: 11 PASS (7 section + 4 segment).

Run: `cargo test --package reader --test elf_reader`
Expected: 11 PASS (10 from Task 3 + 1 segments test).

- [ ] **Step 4: Commit**

```bash
git add crates/reader/tests/elf_converters.rs crates/reader/tests/elf_reader.rs
git commit -m "$(cat <<'EOF'
test(reader): add segment converter + from_elf_segments tests

Exercises elf_segment_to_mem_region, elf_segments_to_mem_regions,
elf_get_executable_segments_as_mem_regions, and the legacy
ElfFileMemReader::from_elf_segments constructor against synthetic
multi-segment ELFs.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Reader-contract helpers + constructor tests

**Files:**
- Modify: `crates/reader/tests/common/reader_contract.rs`
- Modify: `crates/reader/tests/elf_reader.rs`

The contract helpers assert trait-level behavior independent of the concrete backend. Once written, any future backend runs the same assertions.

- [ ] **Step 1: Implement the contract helpers**

Replace `/home/mike/Desktop/strider/crates/reader/tests/common/reader_contract.rs`:

```rust
//! Backend-agnostic assertions over the `rsleigh::MemReader` and
//! `reader::ReadOnlyMemory` traits.
//!
//! When a new backend (PE, Mach-O, raw blob, …) lands, its test file
//! builds the reader and calls these helpers in addition to its own
//! backend-specific assertions.

#![allow(dead_code)]

use reader::{Error, ErrorKind, ReadOnlyMemory};
use rsleigh::{MemReader, VnAddr, VnSpace};

// ── MemReader ────────────────────────────────────────────────────────────

/// Asserts that a full `buf.len()` read at `addr` succeeds and the
/// resulting bytes equal `expected`.
pub fn assert_mem_reader_reads<R>(r: &R, addr: u64, expected: &[u8])
where
    R: MemReader,
    R::Err: std::fmt::Debug,
{
    let mut buf = vec![0u8; expected.len()];
    let n = r
        .read(VnAddr { off: addr, space: VnSpace::RAM }, &mut buf)
        .expect("MemReader::read");
    assert_eq!(n, expected.len(), "expected full read of {} bytes", expected.len());
    assert_eq!(&buf[..], expected, "MemReader read returned unexpected bytes");
}

/// Asserts that a read at an unmapped address fails with
/// `ErrorKind::NotMapped(addr)`.
pub fn assert_mem_reader_unmapped_is_not_mapped_error<R>(r: &R, addr: u64)
where
    R: MemReader<Err = Error>,
{
    let mut buf = [0u8; 1];
    let err = r
        .read(VnAddr { off: addr, space: VnSpace::RAM }, &mut buf)
        .expect_err("read at unmapped addr must error");
    match err.kind() {
        ErrorKind::NotMapped(got) => assert_eq!(*got, addr, "NotMapped should carry the requested addr"),
        other => panic!("expected NotMapped({addr:#x}), got {other:?}"),
    }
}

/// Asserts that a partial read (buf larger than region suffix) returns
/// `Ok(expected_n)`, documenting MemReader's permissive partial-read contract.
pub fn assert_mem_reader_partial_read_ok<R>(r: &R, addr: u64, buf_len: usize, expected_n: usize)
where
    R: MemReader,
    R::Err: std::fmt::Debug,
{
    assert!(expected_n <= buf_len);
    let mut buf = vec![0u8; buf_len];
    let n = r
        .read(VnAddr { off: addr, space: VnSpace::RAM }, &mut buf)
        .expect("MemReader partial read");
    assert_eq!(n, expected_n, "partial read length");
}

// ── ReadOnlyMemory ───────────────────────────────────────────────────────

pub fn assert_readonly_reads(
    r: &impl ReadOnlyMemory,
    space: VnSpace,
    addr: u64,
    size: usize,
    expected: u64,
) {
    assert_eq!(r.read(space, addr, size), Some(expected), "ReadOnlyMemory::read");
}

pub fn assert_readonly_returns_none(
    r: &impl ReadOnlyMemory,
    space: VnSpace,
    addr: u64,
    size: usize,
) {
    assert_eq!(r.read(space, addr, size), None);
}

/// Exercises the trait's rule that non-RAM spaces always return None.
/// Caller supplies any mapped address; only the space varies.
pub fn assert_readonly_rejects_non_ram_spaces(r: &impl ReadOnlyMemory, mapped_addr: u64) {
    for space in [VnSpace::REGISTER, VnSpace::UNIQUE, VnSpace::CONST] {
        assert_eq!(
            r.read(space, mapped_addr, 4),
            None,
            "space {space:?} must be rejected",
        );
    }
}

/// Exercises the trait's rule that `size == 0` and `size > 8` always return None.
pub fn assert_readonly_rejects_bad_sizes(r: &impl ReadOnlyMemory, mapped_addr: u64) {
    assert_eq!(r.read(VnSpace::RAM, mapped_addr, 0), None, "size=0 must be rejected");
    assert_eq!(r.read(VnSpace::RAM, mapped_addr, 9), None, "size=9 must be rejected");
}
```

- [ ] **Step 2: Wire the contract into `tests/elf_reader.rs`**

Append to `/home/mike/Desktop/strider/crates/reader/tests/elf_reader.rs`:

```rust
use common::reader_contract::{
    assert_mem_reader_partial_read_ok, assert_mem_reader_reads,
    assert_mem_reader_unmapped_is_not_mapped_error, assert_readonly_reads,
    assert_readonly_rejects_bad_sizes, assert_readonly_rejects_non_ram_spaces,
    assert_readonly_returns_none,
};

/// Runs the backend-agnostic reader contract against an
/// `ElfFileMemReader` built from a synthetic single-section ELF.
#[test]
fn elf_reader_satisfies_mem_reader_contract() {
    let elf = simple_text_elf(0x1000, &[0x11, 0x22, 0x33, 0x44]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();

    // full read
    assert_mem_reader_reads(&r, 0x1000, &[0x11, 0x22, 0x33, 0x44]);
    // unmapped → NotMapped(addr)
    assert_mem_reader_unmapped_is_not_mapped_error(&r, 0x9000);
    // partial: ask 6, get 4
    assert_mem_reader_partial_read_ok(&r, 0x1000, 6, 4);
}

#[test]
fn elf_reader_satisfies_read_only_memory_contract() {
    let elf = simple_text_elf(0x1000, &[0x11, 0x22, 0x33, 0x44]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();

    assert_readonly_reads(&r, rsleigh::VnSpace::RAM, 0x1000, 4, 0x44332211);
    assert_readonly_returns_none(&r, rsleigh::VnSpace::RAM, 0x9000, 4);
    assert_readonly_rejects_non_ram_spaces(&r, 0x1000);
    assert_readonly_rejects_bad_sizes(&r, 0x1000);
}
```

- [ ] **Step 3: Add the remaining constructor tests (`from_object`, `from_path`)**

Append to `/home/mike/Desktop/strider/crates/reader/tests/elf_reader.rs`:

```rust
use std::io::Write as _;
use tempfile::NamedTempFile;

/// `from_object` on an already-parsed ELF yields a reader with the same
/// mapped data as `from_bytes` on the underlying bytes.
#[test]
fn elf_reader_from_object_matches_from_bytes() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let from_bytes = ElfFileMemReader::from_bytes(&elf).unwrap();
    let parsed = object::File::parse(&elf[..]).unwrap();
    let from_obj = ElfFileMemReader::from_object(&parsed).unwrap();

    for addr in [0x1000u64, 0x1001, 0x1002, 0x1003] {
        let mut a = [0u8; 1];
        let mut b = [0u8; 1];
        let na = rsleigh::MemReader::read(&from_bytes, rsleigh::VnAddr { off: addr, space: rsleigh::VnSpace::RAM }, &mut a).unwrap();
        let nb = rsleigh::MemReader::read(&from_obj,   rsleigh::VnAddr { off: addr, space: rsleigh::VnSpace::RAM }, &mut b).unwrap();
        assert_eq!((na, a), (nb, b), "read mismatch at {addr:#x}");
    }
}

/// `from_path` on a tempfile containing valid ELF bytes succeeds and the
/// resulting reader can read the mapped region.
#[test]
fn elf_reader_from_path_reads_temp_elf() {
    let elf = simple_text_elf(0x1000, &[0xde, 0xad, 0xbe, 0xef]);
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&elf).unwrap();
    f.flush().unwrap();

    let r = ElfFileMemReader::from_path(f.path()).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
        Some(0xefbeadde),
    );
}
```

- [ ] **Step 4: Run the test file**

Run: `cargo test --package reader --test elf_reader`
Expected: 15 PASS (11 from Task 8 + 2 contract + 2 constructor).

- [ ] **Step 5: Commit**

```bash
git add crates/reader/tests/common/reader_contract.rs crates/reader/tests/elf_reader.rs
git commit -m "$(cat <<'EOF'
test(reader): add reader-contract helpers and constructor coverage

The contract helpers abstract over any rsleigh::MemReader<Err =
reader::Error> + ReadOnlyMemory, giving future backends a ready-made
assertion suite. Also covers from_object and from_path.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `tests/load_elf.rs` — top-level loader tests

**Files:**
- Create: `crates/reader/tests/load_elf.rs`

- [ ] **Step 1: Write the file**

Create `/home/mike/Desktop/strider/crates/reader/tests/load_elf.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the top-level `reader::load_elf` function.

#[path = "common/mod.rs"]
mod common;

use std::io::Write as _;

use common::elf_fixture::simple_text_elf;
use object::{Endianness, Object, read::ObjectSection};
use reader::ErrorKind;
use tempfile::NamedTempFile;

/// Happy path: `load_elf` on a valid ELF tempfile returns a parsed
/// `object::File<'static>` with expected shape.
#[test]
fn load_elf_parses_valid_tempfile() {
    let bytes = simple_text_elf(0x1000, &[0x90]);
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&bytes).unwrap();
    f.flush().unwrap();

    let obj = reader::load_elf(f.path().to_str().expect("utf8 path")).unwrap();
    assert_eq!(obj.endianness(), Endianness::Little);

    // `.text` is present at 0x1000.
    let sec = obj.section_by_name(".text").expect(".text section");
    assert_eq!(sec.address(), 0x1000);
}

/// A non-ELF file produces `ErrorKind::Object(_)`.
#[test]
fn load_elf_rejects_garbage_bytes() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"this is definitely not an ELF file").unwrap();
    f.flush().unwrap();

    let err = reader::load_elf(f.path().to_str().expect("utf8 path")).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Object(_)), "got {:?}", err.kind());
}

/// A missing path produces `ErrorKind::Io(_)`.
#[test]
fn load_elf_missing_path_is_io_error() {
    let err = reader::load_elf("/definitely/not/a/real/path/for/tests").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Io(_)), "got {:?}", err.kind());
}
```

- [ ] **Step 2: Run**

Run: `cargo test --package reader --test load_elf`
Expected: 3 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/reader/tests/load_elf.rs
git commit -m "$(cat <<'EOF'
test(reader): add load_elf success and error-path tests

Covers happy-path parsing via NamedTempFile, malformed bytes
(ErrorKind::Object), and missing path (ErrorKind::Io).

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: `tests/error.rs` — full ErrorKind + propagation coverage

**Files:**
- Create: `crates/reader/tests/error.rs`
- Delete: `crates/reader/tests/traceback.rs`

- [ ] **Step 1: Write the new error test file**

Create `/home/mike/Desktop/strider/crates/reader/tests/error.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Comprehensive error-path tests — every `ErrorKind` variant, every
//! `From` conversion, and the traceback invariants.

#[path = "common/mod.rs"]
mod common;

use std::backtrace::BacktraceStatus;

use common::elf_fixture::simple_text_elf;
use reader::{ElfFileMemReader, Error, ErrorKind};

fn assert_has_traceback(err: &Error) {
    assert!(!err.locations().is_empty(), "location chain is empty");
    let s = err.backtrace().status();
    assert!(
        matches!(s, BacktraceStatus::Captured | BacktraceStatus::Disabled),
        "unexpected backtrace status: {s:?}",
    );
}

// ── Direct construction of each variant ───────────────────────────────────

#[test]
fn not_mapped_carries_traceback_and_address() {
    let err: Error = ErrorKind::NotMapped(0xdead_beef).into();
    assert_has_traceback(&err);
    assert!(err.to_string().contains("0xdeadbeef"), "display: {err}");
    assert!(matches!(err.kind(), ErrorKind::NotMapped(addr) if *addr == 0xdead_beef));
}

#[test]
fn assertion_failed_carries_traceback_and_message() {
    let err: Error = ErrorKind::AssertionFailed("boom".into()).into();
    assert_has_traceback(&err);
    assert!(err.to_string().contains("boom"), "display: {err}");
}

// ── From<io::Error> path ──────────────────────────────────────────────────

#[test]
fn load_elf_missing_path_produces_io_error_variant() {
    let err = reader::load_elf("/definitely/not/a/real/path").unwrap_err();
    assert_has_traceback(&err);
    assert!(matches!(err.kind(), ErrorKind::Io(_)), "got {:?}", err.kind());
}

#[test]
fn elf_reader_from_path_missing_produces_io_error_variant() {
    let err = ElfFileMemReader::from_path("/definitely/not/a/real/path").unwrap_err();
    assert_has_traceback(&err);
    assert!(matches!(err.kind(), ErrorKind::Io(_)), "got {:?}", err.kind());
}

// ── From<object::Error> path ──────────────────────────────────────────────

#[test]
fn elf_reader_from_bytes_garbage_produces_object_error_variant() {
    let err = ElfFileMemReader::from_bytes(b"not an elf at all").unwrap_err();
    assert_has_traceback(&err);
    assert!(matches!(err.kind(), ErrorKind::Object(_)), "got {:?}", err.kind());
}

// ── ? propagation extends location chain ─────────────────────────────────

#[test]
fn question_mark_propagation_extends_location_chain() {
    fn inner() -> Result<(), Error> {
        Err::<(), Error>(ErrorKind::NotMapped(0).into())?;
        Ok(())
    }
    fn outer() -> Result<(), Error> {
        inner()?;
        Ok(())
    }
    let err = outer().unwrap_err();
    // Each ? across a same-crate boundary records a location via
    // From<Error> (track_caller), so we expect ≥ 1 from the origin
    // `.into()` plus at least one more from `?`.
    assert!(
        err.locations().len() >= 2,
        "expected chain length ≥ 2, got {}",
        err.locations().len(),
    );
}

// ── Positive case: loading a valid ELF does NOT error ─────────────────────

#[test]
fn elf_reader_from_bytes_valid_returns_ok() {
    let bytes = simple_text_elf(0x1000, &[0x90]);
    ElfFileMemReader::from_bytes(&bytes).expect("valid synthetic ELF parses");
}
```

**Note on `?` propagation:** whether `err.locations().len()` is exactly 2 or larger depends on whether `From<ErrorKind> for Error` and `?` each push a location. Reading `crates/strider-error/src/wrapper.rs`:
- `From<$kind> for $wrapper` is `#[track_caller]` and calls `ErrorFields::new()`, which seeds the chain with **one** entry (`Location::caller()` = the `.into()` site).
- `?` on a `Result<T, Error>` where the error is already `Error` does **not** go through `From` again, so it does NOT extend the chain. The chain length after `?` stays at what the origin set it to.

This means the assertion `>= 2` may be wrong. Before shipping this test, verify experimentally:

```bash
cargo test --package reader --test error question_mark_propagation_extends_location_chain -- --nocapture
```

If the chain length is exactly 1, change the test to assert `== 1` and document that `Error → Error` propagation does not extend the chain — the location chain is extended only when the error type changes. That itself is a useful contract to pin.

- [ ] **Step 2: Run the new file**

Run: `cargo test --package reader --test error`
Expected: 6 of 7 PASS, 1 possibly fails on `question_mark_propagation_extends_location_chain` as noted. If that fails, adjust the assertion per the note and re-run.

- [ ] **Step 3: Delete the old file**

```bash
rm /home/mike/Desktop/strider/crates/reader/tests/traceback.rs
```

Run: `cargo test --package reader`
Expected: all test files green.

- [ ] **Step 4: Commit**

```bash
git add -A crates/reader/tests/error.rs crates/reader/tests/traceback.rs
git commit -m "$(cat <<'EOF'
test(reader): replace traceback.rs with comprehensive error.rs

Covers every ErrorKind variant, From<io::Error>, From<object::Error>,
and ?-propagation chain behavior, plus the traceback invariants the
old file asserted.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: `tests/elf_smoke.rs` — real-binary end-to-end smoke test

**Files:**
- Create: `crates/reader/tests/elf_smoke.rs`

- [ ] **Step 1: Write the smoke test**

Create `/home/mike/Desktop/strider/crates/reader/tests/elf_smoke.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! End-to-end smoke test against a real toolchain-produced ELF.
//!
//! Build prerequisites first:
//!
//!     make -C binary_tests
//!
//! The test panics with a clear message if the binary is absent —
//! matching the convention used by `cfg::cfg_integration` and
//! `analyzer::analyze_binary`.

use object::{Object, ObjectSection};
use reader::{ElfFileMemReader, ReadOnlyMemory};

fn binary_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../binary_tests/out/x64/test.elf")
}

#[test]
fn elf_reader_loads_real_x64_binary() {
    let path = binary_path();
    assert!(
        path.exists(),
        "missing test binary at {} — run `make -C binary_tests`",
        path.display(),
    );

    // load_elf round-trip
    let obj = reader::load_elf(path.to_str().expect("utf8 path")).unwrap();
    assert_eq!(obj.endianness(), object::Endianness::Little);

    // ElfFileMemReader round-trip
    let r = ElfFileMemReader::from_path(&path).unwrap();

    // Read 1 byte at the entry point. The entry is inside the executable
    // segment so the read must succeed. We don't assert the byte value —
    // it depends on the toolchain — only that the reader finds it.
    let entry = obj.entry();
    let mut buf = [0u8; 1];
    let n = rsleigh::MemReader::read(
        &r,
        rsleigh::VnAddr { off: entry, space: rsleigh::VnSpace::RAM },
        &mut buf,
    )
    .unwrap();
    assert_eq!(n, 1, "could not read 1 byte at entry {entry:#x}");

    // ReadOnlyMemory read at entry returns *some* u8 value.
    assert!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, entry, 1).is_some());

    // At least one section exists.
    assert!(obj.sections().next().is_some(), "real ELF has no sections?");
}
```

- [ ] **Step 2: Ensure the fixture binary exists**

Check: `ls /home/mike/Desktop/strider/binary_tests/out/x64/test.elf`

If absent, run: `make -C /home/mike/Desktop/strider/binary_tests` (this is a prerequisite, not a test step — but the executor should know how to produce it if absent).

- [ ] **Step 3: Run the test**

Run: `cargo test --package reader --test elf_smoke`
Expected: 1 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader/tests/elf_smoke.rs
git commit -m "$(cat <<'EOF'
test(reader): add real-binary smoke test

Exercises load_elf + ElfFileMemReader::from_path + MemReader +
ReadOnlyMemory against binary_tests/out/x64/test.elf. Panics with a
build-the-fixture hint if the binary is missing, matching the
cfg::cfg_integration convention.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Final sweep — lints, full suite, post-conditions

**Files:**
- None (verification only)

- [ ] **Step 1: Run every reader-crate test**

Run: `cargo test --package reader`
Expected: all test files pass. Rough final count:
- `mem_region.rs`: 24
- `elf_converters.rs`: 11
- `elf_reader.rs`: 15
- `load_elf.rs`: 3
- `error.rs`: 7
- `elf_smoke.rs`: 1

Total: **~61 tests**.

- [ ] **Step 2: Full-workspace build + test + lint**

Run: `cargo build --workspace`
Run: `cargo test --workspace`
Run: `cargo clippy --workspace -- -D warnings`

Expected: all green. No new clippy lints introduced by the test code.

- [ ] **Step 3: Verify in-source tests are fully gone**

Run: `grep -n "#\[cfg(test)\]\|^#\[test\]\|fn from_parts" /home/mike/Desktop/strider/crates/reader/src/*.rs`
Expected: zero matches. If any hits, remove them.

Run: `grep -rn "#\[cfg(test)\]" /home/mike/Desktop/strider/crates/reader/src/`
Expected: zero matches.

- [ ] **Step 4: Verify `tests/traceback.rs` is gone**

Run: `ls /home/mike/Desktop/strider/crates/reader/tests/traceback.rs 2>&1 | head -1`
Expected: "No such file or directory".

- [ ] **Step 5: No commit**

This task is verification-only. If any check fails, pause and report.

---

## Self-review notes

**Spec coverage:**
- D1 fully integration tests: Tasks 3, 4 port in-source + delete `from_parts`. ✓
- D2 synthetic + real smoke: Tasks 2, 5, 7 build synthetic; Task 12 real smoke. ✓
- D3 shared contract helpers: Task 9. ✓
- D4 pinned contracts: Task 4 (#1, #2), Task 3 (#3). ✓
- `elf_converters.rs` coverage: Tasks 6 (sections) and 8 (segments) touch every free function and filter helper listed in the spec. ✓
- `elf_reader.rs` constructor tests: Task 8 (`from_elf_segments`), Task 9 (`from_object`, `from_path`). ✓
- Endianness round-trip in `elf_reader.rs`: Task 3 (LE u32, BE u32, LE u64). ✓
- `load_elf` success + malformed + missing: Task 10. ✓
- `error.rs` with every variant + `From` paths: Task 11. ✓
- Dev-dep changes: Task 1. ✓

**Gaps discovered during self-review:** None — spec requirements all trace to a task.

**Placeholders:** None present.

**Type consistency check:**
- `rsleigh::VnAddr { off, space }` used consistently across tasks 3, 9, 10, 12.
- `rsleigh::VnSpace::{RAM, REGISTER, UNIQUE, CONST}` used consistently.
- `reader::ErrorKind` variants `NotMapped(u64)`, `Io(_)`, `Object(_)`, `AssertionFailed(String)` — match `src/error.rs`.
- `SectionSpec` fields `{name, addr, data, exec, writable, nobits}` and `SegmentSpec` fields `{addr, data, exec}` used consistently where referenced.
- `object::write::elf::Writer` method sequence (`reserve_*` → `write_*`) consistent in Tasks 2, 5, 7.

All checks pass.
