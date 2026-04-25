# Reader Crate Review — Round 5 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two remaining small items in the `reader` crate that survived rounds 1–4:
  1. The batch ELF converters call `seg.data()` / `sec.data()` **before** the caller-supplied filter, so a filter that rejects a segment/section still pays for reading its bytes — and, worse, a malformed rejected item surfaces as an error even though the caller didn't want it in the first place. Call the filter first.
  2. `ElfFileMemReader` stores the full `object::Endianness` enum and then derives a single `is_little` bit from it on every `ReadOnlyMemory::read` call. Cache the bit at construction.

**Architecture:** Two self-contained, independently-committable changes. Neither alters the public API or any observable behavior of the well-formed happy path. Task 1 narrows the error surface (fewer spurious errors from malformed rejected items) and skips a `data()` bounds-check on filtered-out items. Task 2 is a zero-user-visible internal state refactor.

**Tech Stack:** Rust, `object` crate, `rsleigh`, `strider-error`.

---

## Baseline (verified 2026-04-25)

- `cargo test -p reader` → passes (27 mem_region tests plus full `elf_reader`, `elf_converters`, `elf_smoke`, `error`, `load_elf` suites).
- `cargo clippy -p reader --all-targets --no-deps -- -D warnings` → clean.
- Rounds 1–4 (`2026-04-24-reader-crate-review{,-round-2,-round-3}.md`, `2026-04-25-reader-crate-review-round-4.md`) are fully landed. Latest reader-crate commit: `b031fd8`.

---

## Open questions for the reviewer before execution

**Q1 — filter-before-data() in batch converters (Task 1).** Today both `elf_segments_to_mem_regions` and `elf_sections_to_mem_regions` do:

```rust
for seg in obj.segments() {
    let data = seg.data()?;              // (a) read data (propagates Err)
    if data.is_empty() || !filter(&seg) {  // (b) then decide whether we want it
        continue;
    }
    out.push(MemRegion::new(seg.address(), data.to_vec())?);
}
```

Two real effects:
  - A filter that rejects 80% of segments still reads 100% of their data blocks. Wasted work.
  - A malformed rejected segment surfaces as `Err(ErrorKind::Object(_))` instead of being silently skipped. Since the caller's whole point in supplying a filter is "I don't care about these," propagating errors from them is surprising.

  - **(A)** Swap order: `if !filter(&seg) { continue; }` first, then `seg.data()?`. Drops both effects. The single pinned-contract test (`elf_sections_to_mem_regions_propagates_data_error`) uses `filter: |_| true`, so it is unaffected. Also add a new pinned test covering the new rule: "a malformed segment that the filter rejects does NOT surface an error." **Default choice.**
  - **(B)** Keep the current order and document that filters observe unusable data. Cheap; leaves the behavioral wart in place.

  Assume **(A)**.

**Q2 — endianness field type in `ElfFileMemReader` (Task 2).** Today the struct stores `endianness: object::Endianness` and `ReadOnlyMemory::read` does `let is_little = matches!(self.endianness, object::Endianness::Little);` every call. The ELF is parsed once at construction, the endianness is known once, and nothing else reads the full-enum form.

  - **(A)** Store `is_little_endian: bool`, compute once in `from_object`, use directly in `ReadOnlyMemory::read`. One fewer match, one fewer import of `object::Endianness` in `elf.rs`'s hot path. **Default choice.**
  - **(B)** Leave the enum. The compiler already folds the match; storing the full type is future-proof if we ever need to distinguish big-endian variants we don't today.

  Assume **(A)**. The future-proofing argument is weak: `object::Endianness` is a plain two-variant enum (`Little` | `Big`), so "big-endian variant" doesn't exist to distinguish.

**Reviewer choices locked in (2026-04-25):** Q1 → (A) swap lines. Q2 → (A) store boolean. No CodeRabbit pass this round.

---

## Task 1: Call the filter before reading data in batch ELF converters

**Files:**
- Modify: [crates/reader/src/elf.rs:69-110](crates/reader/src/elf.rs#L69-L110) — both batch converters.
- Modify: [crates/reader/tests/elf_converters.rs](crates/reader/tests/elf_converters.rs) — add a pinned test for "rejected malformed section is silent."

### Step-by-step

- [ ] **Step 1: Write the new failing pinned-contract test**

Append to `crates/reader/tests/elf_converters.rs`:

```rust
// ── Task 1: a FILTER-REJECTED malformed section is silent, not an error ──

/// Pinned contract: when the filter rejects a section, the converter must
/// NOT call `section.data()` on it — so a malformed rejected section
/// cannot spuriously surface as an `ErrorKind::Object(_)`.
///
/// This is the complement of `elf_sections_to_mem_regions_propagates_data_error`:
/// that test uses `filter: |_| true` and pins the "accepted-and-malformed ⇒
/// error" rule; this test uses `filter: |_| false` and pins the
/// "rejected-and-malformed ⇒ empty Ok" rule. Together they lock in
/// filter-before-data semantics.
#[test]
fn elf_sections_to_mem_regions_skips_rejected_malformed_section() {
    use object::Endianness;
    use object::elf;
    use object::write::elf::{FileHeader, SectionHeader, Writer};

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
            sh_offset: 0xdead_beef, // past EOF → data() would fail
            sh_size: 4,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);

    // filter rejects everything, including the broken section.
    // The converter must NOT call `section.data()` on a rejected section,
    // so no Object(_) error surfaces.
    let regions = elf_sections_to_mem_regions(&obj, |_| false)
        .expect("filter-rejected malformed section must not surface an error");
    assert!(regions.is_empty(), "nothing was accepted");
}
```

- [ ] **Step 2: Run the new test to verify it fails on the current code**

Run: `cargo test -p reader --test elf_converters elf_sections_to_mem_regions_skips_rejected_malformed_section`
Expected: FAIL — current code calls `sec.data()?` before the filter, so the malformed-but-rejected section still propagates `ErrorKind::Object(_)`.

- [ ] **Step 3: Reorder filter-before-data in both batch converters**

In `crates/reader/src/elf.rs`, replace the two batch converter bodies with:

```rust
pub fn elf_segments_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Segment<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for seg in obj.segments() {
        if !filter(&seg) {
            continue;
        }
        let data = seg.data()?;
        if data.is_empty() {
            continue;
        }
        out.push(MemRegion::new(seg.address(), data.to_vec())?);
    }
    Ok(out)
}

pub fn elf_sections_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Section<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for sec in obj.sections() {
        if !filter(&sec) {
            continue;
        }
        let data = sec.data()?;
        if data.is_empty() {
            continue;
        }
        out.push(MemRegion::new(sec.address(), data.to_vec())?);
    }
    Ok(out)
}
```

- [ ] **Step 4: Update the `# Errors` phrasing on both converters**

Both currently say "Returns an error wrapping the underlying `object::Error` if any segment's file-backed data cannot be read." Tighten to reflect that only *accepted* (filter-true) items can produce errors:

Replace the `# Errors` block on `elf_segments_to_mem_regions` with:

```rust
/// # Errors
///
/// Returns an error wrapping the underlying `object::Error` if a
/// segment accepted by `filter` has file-backed data that cannot be
/// read. Segments rejected by `filter` are never read, so malformed
/// rejected segments do not surface as errors. Accepted empty-data
/// segments (e.g. `SHT_NOBITS`-equivalents) are skipped rather than
/// reported.
```

Mirror the same wording on `elf_sections_to_mem_regions`, substituting "section" for "segment".

- [ ] **Step 5: Run the full reader test suite**

Run: `cargo test -p reader`
Expected: PASS, including the new `elf_sections_to_mem_regions_skips_rejected_malformed_section` and the unchanged `elf_sections_to_mem_regions_propagates_data_error` (which uses `filter: |_| true` so it still propagates).

- [ ] **Step 6: Strict clippy on reader**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. No external caller changes — the filter/data ordering is an internal implementation detail.

- [ ] **Step 8: Commit**

```bash
git add crates/reader/src/elf.rs crates/reader/tests/elf_converters.rs
git commit -m "refactor(reader): apply segment/section filter before reading data"
```

---

## Task 2: Cache endianness as `is_little_endian: bool` in `ElfFileMemReader`

**Files:**
- Modify: [crates/reader/src/elf.rs:196-281](crates/reader/src/elf.rs#L196-L281) — struct + constructor + `ReadOnlyMemory` impl.

### Step-by-step

- [ ] **Step 1: Change the struct and constructor**

In `crates/reader/src/elf.rs`, replace the `ElfFileMemReader` struct declaration and the `from_object` body with:

```rust
/// An rsleigh [`rsleigh::MemReader`] backed by an ELF file's sections.
///
/// The reader owns its backing bytes (copied into [`MemRegion`]s at
/// construction) so no lifetime borrow on the source `object::File` or its
/// byte buffer is required. Both the executable sections (for instruction
/// fetch) and the read-only data sections (for compile-time-constant loads)
/// are loaded from the same ELF.
#[derive(Debug)]
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
    is_little_endian: bool,
}

impl ElfFileMemReader {
    /// Builds a reader from an already-parsed [`object::File`].
    ///
    /// Loads every executable section and every non-writable section with
    /// file-backed data. The parsed object is not retained — the returned
    /// reader is self-owning.
    ///
    /// # Errors
    ///
    /// Propagates any `object::Error` from reading the selected sections.
    pub fn from_object(obj: &object::File<'_>) -> Result<Self> {
        let regions = elf_get_code_and_readonly_sections_as_mem_regions(obj)?;
        Ok(Self {
            lookup: MemRegionsLookupTable::new(regions),
            is_little_endian: matches!(obj.endianness(), object::Endianness::Little),
        })
    }
```

Leave `from_bytes` and `from_path` (they delegate to `from_object`).

- [ ] **Step 2: Simplify the `ReadOnlyMemory` impl**

Replace the `impl crate::ReadOnlyMemory for ElfFileMemReader { ... }` block in `crates/reader/src/elf.rs` with:

```rust
impl crate::ReadOnlyMemory for ElfFileMemReader {
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        if space != rsleigh::VnSpace::RAM {
            return None;
        }
        if size == 0 || size > 8 {
            return None;
        }
        // Place the read bytes at the endianness-appropriate end of an 8-byte
        // buffer so the final from_{le,be}_bytes produces the same numeric
        // value for an N-byte load as the target machine would.
        let mut buf = [0u8; 8];
        let slot = if self.is_little_endian {
            &mut buf[..size]
        } else {
            &mut buf[8 - size..]
        };
        if self.lookup.read(addr, slot)? != size {
            return None;
        }
        Some(if self.is_little_endian {
            u64::from_le_bytes(buf)
        } else {
            u64::from_be_bytes(buf)
        })
    }
}
```

- [ ] **Step 3: Run the existing endianness tests**

Run: `cargo test -p reader --test elf_reader`
Expected: PASS, including `ro_read_little_endian_u32`, `ro_read_big_endian_u32`, `ro_read_little_endian_u64`, `ro_read_single_byte`, and the full `elf_reader_satisfies_read_only_memory_contract` contract.

- [ ] **Step 4: Full reader suite + strict clippy**

Run: `cargo test -p reader && cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. The struct's fields are private (landed in round 1 Task 5 / round 3 Task 1) so no external crate sees the type change.

- [ ] **Step 6: Commit**

```bash
git add crates/reader/src/elf.rs
git commit -m "refactor(reader): cache endianness as is_little_endian bool in ElfFileMemReader"
```

---

## Task 3: Final sanity sweep

**Files:** run-only, no edits.

- [ ] **Step 1: Full reader test suite**

Run: `cargo test -p reader`
Expected: PASS. Total `elf_converters` tests grows by 1 (Task 1's new pinned test). All other counts unchanged.

- [ ] **Step 2: Reader-only strict clippy**

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

- **Rename `load_elf` to make the leak opt-in (e.g. `load_elf_leaking` or a `LeakedElf` newtype).** The leak is documented and every caller's call site is a local tool or test that runs once. Renaming is pure churn across 10+ call sites for no correctness gain.
- **Replace the `BTreeMap::new` + loop in `MemRegionsLookupTable::new` with `collect()`.** Purely cosmetic; same allocations, same big-O.
- **Enable `clippy::pedantic` workspace-wide** to surface the ~40 remaining stylistic warnings in the reader crate. Deferred until/unless the workspace opts in globally.
- **Switch `MemRegion::data` from `Vec<u8>` to `Box<[u8]>`.** Saves one word per region, churns API. Rejected in rounds 2, 3, 4.
- **Share the copied section bytes between the two `ElfFileMemReader::from_object(&obj)` calls in `crates/analyzer/examples/analyzer.rs`.** Analyzer-side refactor (`Arc`-wrap the regions or restructure `Sleigh::new` + `LoadReadOnly` plumbing). Belongs to an analyzer review, not this one.
- **Generic-over-section-vs-segment helper to collapse `elf_segments_to_mem_regions` and `elf_sections_to_mem_regions`.** `object` provides no common trait across both; a macro would be heavier than the 12-line duplicated bodies. Rejected in round 1.
