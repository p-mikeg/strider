# Reader Crate Review — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clean up the `reader` crate — fix one latent correctness issue, delete dead and legacy code, collapse duplicated constructor plumbing, and tighten API surface, without changing the behavior external callers rely on.

**Architecture:** Each task is a self-contained, independently-committable change. The only external callers of the crate are (a) `crates/opt/src/load_readonly.rs` (uses `ReadOnlyMemory` trait only), (b) `crates/analyzer/examples/analyzer.rs` (uses `load_elf` + `ElfFileMemReader::from_object`). No other crate touches anything else, so the public surface can be narrowed aggressively.

**Tech Stack:** Rust, `object` crate, `rsleigh`, `strider-error`.

---

## Open questions for the reviewer before execution

Each of these changes your mind about a task below. Pick one per group.

**Q1 — overlapping regions (Task 1):** When `MemRegionsLookupTable` is handed two overlapping regions (not same-start), what's the right behavior?
  - **(A)** Fix the read lookup to fall back to earlier regions when the latest-start candidate doesn't cover `addr`. More forgiving; O(n) worst case but only on miss. **Default choice.**
  - **(B)** Validate at construction time and return an error / panic when regions overlap. Stricter; changes the `new()` signature to return `Result`.
  - **(C)** Document the current "last-start wins, no fallthrough" behavior as the contract and add a test pinning it, accepting the sharp edge.

**Q2 — `load_elf` (Task 6): RESOLVED — keep the function, change signature to `impl AsRef<Path>`.** The leak-by-design is the whole reason it returns `'static`; there is no non-leaking way to return `object::File<'static>` from a read-a-file call without a self-referential type (ouroboros/yoke-level complexity). Keeping the function as the "give me a pre-parsed, long-lived `object::File`" helper; just fixing the `&str` → `impl AsRef<Path>` inconsistency with `from_path`.

**Q3 — `RegionsMemReader` (Task 4):** Single-method wrapper over `MemRegionsLookupTable`.
  - **(A)** Delete it. `ElfFileMemReader` stores the lookup table directly. **Default choice.**
  - **(B)** Keep it. Mark `pub(crate)` at minimum.

Assume defaults unless the reviewer says otherwise. The tasks below reflect defaults.

---

## Task 1: Fix overlapping-region fallthrough in `MemRegionsLookupTable::read`

**Files:**
- Modify: [crates/reader/src/lib.rs:126-131](crates/reader/src/lib.rs#L126-L131)
- Modify: [crates/reader/tests/mem_region.rs](crates/reader/tests/mem_region.rs)

- [ ] **Step 1: Write a failing test**

Append to `crates/reader/tests/mem_region.rs`:

```rust
/// Pinned contract: when a later-starting region is *shorter* and does not
/// cover `addr`, lookup must fall through to an earlier region that does.
/// Without this, overlapping regions silently lose data.
#[test]
fn lookup_table_shorter_inner_region_does_not_shadow_outer_tail() {
    // Outer A: [0x1000..0x1020), all 0xaa
    // Inner B: [0x1010..0x1014), all 0xbb  (shorter, starts inside A)
    let a = MemRegion::new(0x1000, vec![0xaa; 0x20]);
    let b = MemRegion::new(0x1010, vec![0xbb; 0x04]);
    let table = MemRegionsLookupTable::new([a, b]);
    let mut buf = [0u8; 1];

    // 0x1018 is in A's tail but past B's end.
    assert_eq!(table.read(0x1018, &mut buf), Some(1));
    assert_eq!(buf[0], 0xaa, "should fall through to A when B does not cover addr");

    // Inside B's range, B still wins (existing "later start wins" rule).
    assert_eq!(table.read(0x1011, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p reader --test mem_region lookup_table_shorter_inner_region_does_not_shadow_outer_tail`
Expected: FAIL (returns `None` because B is picked but doesn't contain the addr).

- [ ] **Step 3: Fix the lookup**

Replace the body of `MemRegionsLookupTable::read` with:

```rust
pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
    // Walk candidates from highest start_addr <= addr downward. The usual
    // case hits on the first candidate (no overlaps); fallthrough only
    // matters when a later, shorter region sits inside an earlier one.
    for (_, region) in self.regions.range(..=addr).rev() {
        if let Some(n) = region.read(addr, out) {
            return Some(n);
        }
    }
    None
}
```

- [ ] **Step 4: Run the full reader test suite**

Run: `cargo test -p reader`
Expected: all pass, including the new test and the existing "later start wins" / "cross-boundary" pinned-contract tests.

- [ ] **Step 5: Commit**

```bash
git add crates/reader/src/lib.rs crates/reader/tests/mem_region.rs
git commit -m "fix(reader): fall through to earlier region when later-start region doesn't cover addr"
```

---

## Task 2: Remove `ErrorKind::AssertionFailed`

Unused outside its own existence test.

**Files:**
- Modify: [crates/reader/src/error.rs](crates/reader/src/error.rs)
- Modify: [crates/reader/tests/error.rs](crates/reader/tests/error.rs)

- [ ] **Step 1: Delete the variant**

Remove from `ErrorKind` in `crates/reader/src/error.rs`:

```rust
/// A test assertion failed. Exists so tests can return `Result<(), Error>`
/// instead of using `panic!`.
#[error("assertion failed: {0}")]
AssertionFailed(String),
```

- [ ] **Step 2: Delete the now-dead test**

Remove `fn assertion_failed_carries_traceback_and_message` from `crates/reader/tests/error.rs`.

- [ ] **Step 3: Verify no other uses**

Run: `grep -rn AssertionFailed crates/reader`
Expected: empty output.

- [ ] **Step 4: Run tests**

Run: `cargo test -p reader`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/reader
git commit -m "refactor(reader): remove unused ErrorKind::AssertionFailed"
```

---

## Task 3: Remove legacy `from_elf_segments` / `from_elf_sections`

Only their own tests consume them. The unified `from_object` covers the real use case.

**Files:**
- Modify: [crates/reader/src/elf.rs:174-198](crates/reader/src/elf.rs#L174-L198)
- Modify: [crates/reader/tests/elf_reader.rs](crates/reader/tests/elf_reader.rs)

- [ ] **Step 1: Delete the two methods**

Remove `from_elf_segments` and `from_elf_sections` from `impl ElfFileMemReader` in `crates/reader/src/elf.rs`.

- [ ] **Step 2: Update the one test that used `from_elf_segments`**

In `crates/reader/tests/elf_reader.rs`, replace the body of `elf_reader_from_elf_segments_picks_exec_only` (at [elf_reader.rs:172-190](crates/reader/tests/elf_reader.rs#L172-L190)) with a test of `elf_get_executable_segments_as_mem_regions` against a `MemRegionsLookupTable`:

```rust
/// Executable segments are picked up; non-exec segments are skipped.
#[test]
fn elf_exec_segments_only_yield_mapped_regions() {
    use reader::{MemRegion, MemRegionsLookupTable};
    use reader::elf::elf_get_executable_segments_as_mem_regions;

    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![0xaa, 0xbb], exec: true },
        SegmentSpec { addr: 0x2000, data: vec![0xcc, 0xdd], exec: false },
    ]);
    let obj = File::parse(&bytes[..]).unwrap();
    let regions: Vec<MemRegion> = elf_get_executable_segments_as_mem_regions(&obj).unwrap();
    let table = MemRegionsLookupTable::new(regions);

    let mut buf = [0u8; 2];
    assert_eq!(table.read(0x1000, &mut buf), Some(2));
    assert_eq!(buf, [0xaa, 0xbb]);
    assert_eq!(table.read(0x2000, &mut buf), None);
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p reader`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader
git commit -m "refactor(reader): drop legacy from_elf_segments/from_elf_sections constructors"
```

---

## Task 4: Delete `RegionsMemReader`; inline lookup into `ElfFileMemReader`

Single-method wrapper; adds no value.

**Files:**
- Modify: [crates/reader/src/lib.rs](crates/reader/src/lib.rs)
- Modify: [crates/reader/src/elf.rs](crates/reader/src/elf.rs)
- Modify: [crates/reader/tests/mem_region.rs](crates/reader/tests/mem_region.rs)

- [ ] **Step 1: In `crates/reader/src/lib.rs`**

Remove the entire `RegionsMemReader` struct and impl (the `── RegionsMemReader ──` section). Remove the re-export from the module-level docs if mentioned.

- [ ] **Step 2: In `crates/reader/src/elf.rs`**

Change the `ElfFileMemReader` struct to:

```rust
#[derive(Debug)]
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
    endianness: object::Endianness,
}
```

Replace every `self.regions_mem_reader.read(addr, out_buf)` with `self.lookup.read(addr, out_buf)`. Update `from_object` accordingly.

Drop the `RegionsMemReader` import from the `use crate::{...}` line.

- [ ] **Step 3: Delete the two now-dead tests**

Remove `regions_mem_reader_delegates_read` and `regions_mem_reader_miss_returns_none` from `crates/reader/tests/mem_region.rs`. The `MemRegionsLookupTable` tests already cover the same behavior.

- [ ] **Step 4: Run tests**

Run: `cargo test -p reader && cargo check --workspace`
Expected: PASS (workspace check ensures no external caller referenced `RegionsMemReader`).

- [ ] **Step 5: Commit**

```bash
git add crates/reader
git commit -m "refactor(reader): delete RegionsMemReader wrapper; use MemRegionsLookupTable directly"
```

---

## Task 5: Privatize `ElfFileMemReader` fields

No external code reads them.

**Files:**
- Modify: [crates/reader/src/elf.rs](crates/reader/src/elf.rs)

- [ ] **Step 1: Drop `pub` on the two fields**

In `crates/reader/src/elf.rs`, change:

```rust
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
    endianness: object::Endianness,
}
```

(i.e. no `pub` qualifier on either field — struct stays `pub`).

Drop the ASCII-box doc comments above each field that describe them as public API; the struct-level doc already covers the story.

- [ ] **Step 2: Verify workspace still builds**

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 3: Run tests**

Run: `cargo test -p reader`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader
git commit -m "refactor(reader): privatize ElfFileMemReader internal fields"
```

---

## Task 6: Broaden `load_elf` to accept `impl AsRef<Path>`

Keeps the function (user wants it preserved for later use); only fixes the `&str` vs `AsRef<Path>` inconsistency with `from_path`.

**Files:**
- Modify: [crates/reader/src/elf.rs:250](crates/reader/src/elf.rs#L250)
- Modify: [crates/reader/tests/load_elf.rs](crates/reader/tests/load_elf.rs) (drop the `.to_str().expect(...)` dance)
- Modify: [crates/reader/tests/elf_smoke.rs](crates/reader/tests/elf_smoke.rs) (same)

- [ ] **Step 1: Change the signature**

In `crates/reader/src/elf.rs`, replace:

```rust
pub fn load_elf(path: &str) -> Result<object::File<'static>> {
    let data = std::fs::read(path)?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    Ok(object::File::parse(leaked)?)
}
```

with:

```rust
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<object::File<'static>> {
    let data = std::fs::read(path)?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    Ok(object::File::parse(leaked)?)
}
```

Keep the doc comment (the leak-by-design note is still accurate).

- [ ] **Step 2: Drop `.to_str().expect(...)` in call sites that now have a `Path`**

In `crates/reader/tests/load_elf.rs`, change each `reader::load_elf(f.path().to_str().expect("utf8 path"))` to `reader::load_elf(f.path())`.

In `crates/reader/tests/elf_smoke.rs`, change `reader::load_elf(path.to_str().expect("utf8 path"))` to `reader::load_elf(&path)`.

Leave `crates/analyzer/examples/analyzer.rs` alone — `&str` coerces to `AsRef<Path>` so the call compiles unchanged.

- [ ] **Step 3: Run tests**

Run: `cargo test -p reader && cargo build --example analyzer`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader
git commit -m "refactor(reader): accept AsRef<Path> in load_elf to match from_path"
```

---

## Task 7: Simplify helpers — drop the internal BTreeMap dedup

`MemRegionsLookupTable::new` already dedups by start address. The internal BTreeMap in `elf_{sections,segments}_to_mem_regions` duplicates that work.

**Files:**
- Modify: [crates/reader/src/elf.rs:32-72](crates/reader/src/elf.rs#L32-L72)

- [ ] **Step 1: Collapse both helpers to plain `Vec` accumulators**

Replace the two helpers with:

```rust
pub fn elf_segments_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Segment<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for seg in obj.segments() {
        let Ok(data) = seg.data() else { continue };
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
        let Ok(data) = sec.data() else { continue };
        if data.is_empty() || !filter(&sec) {
            continue;
        }
        out.push(MemRegion::new(sec.address(), data.to_vec()));
    }
    Ok(out)
}
```

Drop the `use std::collections::BTreeMap;` at the top of `elf.rs`.

Drop the doc line "If two sections/segments share the same start address, the last one encountered is kept" — this is now owned by `MemRegionsLookupTable::new`'s doc. Replace with a forwarding note: "Preserves iteration order; duplicate `start_addr`s are resolved later by `MemRegionsLookupTable`."

- [ ] **Step 2: Re-check the `elf_sections_to_mem_regions_same_start_last_wins` test**

Run: `cargo test -p reader --test elf_converters elf_sections_to_mem_regions_same_start_last_wins`

This test asserts the post-conversion vec has one entry at 0x1000 with `data == [0xbb]`. With the new helpers, the vec has **two** entries at 0x1000. Rewrite the test to feed the vec through a `MemRegionsLookupTable` and assert the final read yields 0xbb (which is the behavior users actually depend on):

```rust
#[test]
fn elf_sections_same_start_last_wins_via_lookup_table() {
    let bytes = build_elf_with_sections(&[
        SectionSpec { name: b".first",  addr: 0x1000, data: vec![0xaa], exec: true,  writable: false, nobits: false },
        SectionSpec { name: b".second", addr: 0x1000, data: vec![0xbb], exec: false, writable: false, nobits: false },
    ]);
    let obj = parse(&bytes);
    let regions = elf_sections_to_mem_regions(&obj, |_| true).unwrap();
    let table = reader::MemRegionsLookupTable::new(regions);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "later section wins on duplicate start_addr");
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p reader`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/reader
git commit -m "refactor(reader): drop redundant BTreeMap dedup inside elf helpers"
```

---

## Task 8: Extract named filter predicates in `elf_get_*` helpers (readability)

**Files:**
- Modify: [crates/reader/src/elf.rs:76-125](crates/reader/src/elf.rs#L76-L125)

- [ ] **Step 1: Add three small private predicates**

Above the public helpers in `crates/reader/src/elf.rs`:

```rust
fn segment_is_executable(seg: &object::read::Segment<'_, '_>) -> bool {
    matches!(
        seg.flags(),
        object::read::SegmentFlags::Elf { p_flags }
            if p_flags & object::elf::PF_X != 0
    )
}

fn section_is_executable(sec: &object::read::Section<'_, '_>) -> bool {
    matches!(
        sec.flags(),
        object::read::SectionFlags::Elf { sh_flags }
            if sh_flags & object::elf::SHF_EXECINSTR as u64 != 0
    )
}

fn section_is_code_or_readonly(sec: &object::read::Section<'_, '_>) -> bool {
    let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
        return false;
    };
    let is_alloc    = sh_flags & object::elf::SHF_ALLOC     as u64 != 0;
    let is_exec     = sh_flags & object::elf::SHF_EXECINSTR as u64 != 0;
    let is_writable = sh_flags & object::elf::SHF_WRITE     as u64 != 0;
    is_alloc && (is_exec || !is_writable)
}
```

Rewrite the three `elf_get_*` helpers to delegate:

```rust
pub fn elf_get_executable_segments_as_mem_regions(...) -> Result<Vec<MemRegion>> {
    elf_segments_to_mem_regions(obj, segment_is_executable)
}
pub fn elf_get_executable_sections_as_mem_regions(...) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, section_is_executable)
}
pub fn elf_get_code_and_readonly_sections_as_mem_regions(...) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, section_is_code_or_readonly)
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p reader`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/reader
git commit -m "refactor(reader): extract named predicates for exec/readonly filters"
```

---

## Task 9: Fix all clippy lints on the reader crate

Baseline `cargo clippy -p reader --all-targets --no-deps -- -D warnings` currently reports (before Tasks 3/4/6 prune some):
- 13 x `clippy::missing_errors_doc` on `Result`-returning public functions in `src/elf.rs` (plus the generic error type).
- 2 x `clippy::must_use_candidate` on `MemRegion::new` and `RegionsMemReader::new` in `src/lib.rs`. The `RegionsMemReader` case goes away with Task 4.

**Q4 for the reviewer:** how should we satisfy `missing_errors_doc`?
  - **(A)** Add a `# Errors` section to each public `Result`-returning fn describing the variants it can produce (e.g. `Io` for fs calls, `Object` for parse, `NotMapped` for reads). Verbose but matches the lint's intent. **Default choice.**
  - **(B)** Add a single `#![allow(clippy::missing_errors_doc)]` at the crate root since errors go through one narrow `ErrorKind` anyway.

Assume (A) unless the reviewer picks (B).

**Files:**
- Modify: [crates/reader/src/lib.rs](crates/reader/src/lib.rs)
- Modify: [crates/reader/src/elf.rs](crates/reader/src/elf.rs)

- [ ] **Step 1: Add `#[must_use]` to `MemRegion::new`**

In `crates/reader/src/lib.rs`, directly above `pub fn new(start_addr: u64, data: Vec<u8>) -> Self`, add `#[must_use]`. (The `RegionsMemReader::new` case is already removed by Task 4.)

- [ ] **Step 2: Add `# Errors` to each remaining Result-returning fn**

For each fn below, append a `# Errors` paragraph to its doc comment. Use the same phrasing for the shared `Io` / `Object` cases to avoid drift.

In `crates/reader/src/elf.rs`:
- `elf_segment_to_mem_region` — `# Errors: returns Err(Error) if the segment's file-backed data cannot be read (propagated from object::Error).`
- `elf_section_to_mem_region` — same wording, s/segment/section/.
- `elf_segments_to_mem_regions` — same wording, applied to any one segment.
- `elf_sections_to_mem_regions` — same wording, applied to any one section.
- `elf_get_executable_segments_as_mem_regions` — `# Errors: propagates any object::Error encountered while reading segment data.`
- `elf_get_executable_sections_as_mem_regions` — same.
- `elf_get_code_and_readonly_sections_as_mem_regions` — same.
- `ElfFileMemReader::from_object` — `# Errors: propagates any object::Error from reading the selected sections.`
- `ElfFileMemReader::from_bytes` — `# Errors: returns ErrorKind::Object for a parse failure or any error from from_object.`
- `ElfFileMemReader::from_path` — `# Errors: returns ErrorKind::Io for an fs::read failure or any error from from_bytes.`

(`from_elf_segments`, `from_elf_sections`, and `load_elf` are deleted by Tasks 3 and 6 — no doc to add.)

- [ ] **Step 3: Verify clippy passes on reader**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: `Finished ... dev ...` with no lint errors.

- [ ] **Step 4: Verify test suite still passes**

Run: `cargo test -p reader`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/reader
git commit -m "docs(reader): satisfy clippy::missing_errors_doc / must_use_candidate on reader surface"
```

### Note on `strider-error` lint failures

`cargo clippy -p reader --all-targets` (without `--no-deps`) surfaces 2 `clippy::must_use_candidate` errors in [crates/strider-error/src/wrapper.rs:33](crates/strider-error/src/wrapper.rs#L33) (`ErrorFields::new` and `ErrorFields::push_caller`). These are outside the reader crate and therefore out of scope for "fix lints on the reader crate." They will still block a `cargo clippy --workspace` run in Task 10. Flag with the reviewer: fix here (2-line addition) or defer to a follow-up.

---

## Task 10: Final workspace sanity sweep

**Files:**
- Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS, no warnings.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 3: Reader-only lint (strict)**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Workspace lint (may require the `strider-error` follow-up noted in Task 9)**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS if Q5 is resolved; otherwise the 2 `strider-error` lints remain, which is acceptable if the reviewer deferred them.

- [ ] **Step 5: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced (this is the existing behavior; we're confirming nothing regressed).

---

## Out of scope (considered, rejected or deferred)

- **`MemRegion::end_addr` overflow**: could overflow in theory with data at top of u64; in practice unreachable for real ELFs. Adding `checked_add` changes the return type to `Option<u64>` and cascades into `contains` / `read`. Cost > benefit. If paranoia matters later, fold into the new `MemRegionsLookupTable::new` as a construction-time guard.
- **Making `MemRegionsLookupTable::new` return `Result`** (overlap detection): rejected in favor of Task 1's lookup-time fallthrough.
- **Moving endianness assembly into `MemRegionsLookupTable`**: endianness is an ELF concern, not a generic-regions concern. Kept in the ELF layer.
- **Generic-over-section-vs-segment helper**: `object` provides no common trait across both; a macro would be heavier than the duplicated 10-line bodies after Task 7. Not worth it.
