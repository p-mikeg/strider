# Reader Crate Review — Round 6 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the last few small remaining items in the `reader` crate after rounds 1–5:
  1. **Doc gap (real correctness):** every ELF→`MemRegion` converter (single-item, batch, and the three filter presets) plus `ElfFileMemReader::from_object` calls `MemRegion::new`, which can return `ErrorKind::RegionOverflow` when `addr + len` overflows `u64`. Today every `# Errors` section only mentions `object::Error` — `RegionOverflow` is a hidden return path. Pin it with both prose and a converter-level integration test.
  2. **Readability:** `ReadOnlyMemory::read` reads `self.is_little_endian` twice. Bind it to a local once to flatten the function.
  3. **Style:** `MemRegion::new` uses `if checked_add(...).is_none() { return Err(...) }`. The idiomatic `.ok_or(...)?` is shorter and matches the rest of the crate's `?`-style error propagation.

**Architecture:** Three independently-committable changes. None alters any happy-path behavior, public API shape, or error variant set; (1) only updates rustdoc and adds one pinned-contract test, (2) and (3) are in-function refactors with no semantic change.

**Tech Stack:** Rust, `object` crate, `rsleigh`, `strider-error`.

---

## Baseline (verified 2026-04-25)

- `cargo test -p reader` → 27 `mem_region` tests pass plus full `elf_reader`, `elf_converters`, `elf_smoke`, `error`, `load_elf` suites.
- `cargo clippy -p reader --all-targets --no-deps -- -D warnings` → clean.
- Round 5 fully landed: filter-before-data (`1f6133c`) + endianness-cached-as-bool (already in tree). Latest reader commit at planning time: `1f6133c`.

---

## Open questions for the reviewer before execution

**Q1 — `RegionOverflow` doc gap (Task 1).** Every batch and single-item ELF converter calls `MemRegion::new`. `MemRegion::new` returns `ErrorKind::RegionOverflow` when `addr + data.len()` would exceed `u64::MAX` (rare, but possible for adversarial ELFs whose section places near the top of the address space). The current `# Errors` blocks are silent on this:

```rust
/// # Errors
///
/// Returns an error wrapping the underlying `object::Error` if a
/// segment accepted by `filter` has file-backed data that cannot be
/// read. Segments rejected by `filter` are never read, …
```

Two options:

  - **(A)** Mention `ErrorKind::RegionOverflow` in the `# Errors` block on every affected function, and add ONE pinned converter-level test that constructs a section whose `sh_addr + sh_size` overflows and asserts the error propagates as `ErrorKind::RegionOverflow`. The single test is sufficient — the propagation rule is identical for every converter (they all bottom out in `MemRegion::new`). **Default choice.**
  - **(B)** Leave the doc gap; document the omission in the crate-level rustdoc instead. Cheaper but leaves `# Errors` rustdoc lying about its return set.

  Assume **(A)**. Rationale: `# Errors` is a contract — callers may match on `ErrorKind` and a missing variant in the doc is a real wart.

**Q2 — `is_little_endian` local binding (Task 2).** Today:

```rust
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
```

Two reads of `self.is_little_endian`. Bind once:

```rust
let is_little = self.is_little_endian;
let slot = if is_little { … } else { … };
…
Some(if is_little { u64::from_le_bytes(buf) } else { u64::from_be_bytes(buf) })
```

  - **(A)** Bind to a local. **Default choice.**
  - **(B)** Leave as-is. The compiler generates identical code; the change is purely cosmetic.

  Assume **(A)**. Rationale: marginal but reads more naturally; one line cost.

**Q3 — `.ok_or(...)?` in `MemRegion::new` (Task 3).** Today:

```rust
pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
    let len = data.len() as u64;
    if start_addr.checked_add(len).is_none() {
        return Err(error::ErrorKind::RegionOverflow { start_addr, len }.into());
    }
    Ok(Self { start_addr, data })
}
```

Tightened:

```rust
pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
    let len = data.len() as u64;
    start_addr
        .checked_add(len)
        .ok_or(error::ErrorKind::RegionOverflow { start_addr, len })?;
    Ok(Self { start_addr, data })
}
```

  - **(A)** Switch to `.ok_or(...)?`. Two lines saved. Consistent with the rest of the crate's `?`-style. **Default choice.**
  - **(B)** Leave the `if … is_none()` form. It's the only place in the crate where overflow is checked; no harm done.

  Assume **(A)**. The two forms compile identically (`?` adds an `Into::into` call but `ErrorKind: Into<Error>` is what the original `.into()` already does). Pure churn-vs-cleanup tradeoff.

**Reviewer choices locked in:** Q1 → **TBD**. Q2 → **TBD**. Q3 → **TBD**. Filled in once the user approves; defaults apply otherwise.

---

## Task 1: Document `ErrorKind::RegionOverflow` and pin its propagation

**Files:**
- Modify: [crates/reader/src/elf.rs](crates/reader/src/elf.rs) — every `# Errors` block on a converter / `from_object`.
- Modify: [crates/reader/tests/elf_converters.rs](crates/reader/tests/elf_converters.rs) — add one pinned-contract test.

### Step-by-step

- [ ] **Step 1: Write the failing pinned-contract test**

Append to `crates/reader/tests/elf_converters.rs`:

```rust
// ── Pinned contract: RegionOverflow from MemRegion::new propagates ──────

/// Pinned contract: when an accepted section's `sh_addr + sh_size` would
/// overflow `u64`, `MemRegion::new` returns `ErrorKind::RegionOverflow`,
/// and the converter must propagate that — *not* silently drop it and not
/// rewrap it as `ErrorKind::Object(_)`.
///
/// This complements `elf_sections_to_mem_regions_propagates_data_error`:
/// that test pins the `object::Error` arm of the converter's error set;
/// this test pins the `RegionOverflow` arm. Together they enumerate
/// every error variant the converters can return.
///
/// We synthesize the failure by building a section whose `sh_addr` is one
/// less than `u64::MAX` and whose `sh_size` is 4. The data block fits in
/// the file (no `object::Error`), but `addr + len` overflows by 3 bytes,
/// so `MemRegion::new` must reject it.
#[test]
fn elf_sections_to_mem_regions_propagates_region_overflow() {
    use object::Endianness;
    use object::elf;
    use object::write::elf::{FileHeader, SectionHeader, Writer};

    let payload = [0u8, 0, 0, 0]; // 4 bytes of data on disk

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let name = w.add_section_name(b".overflow");
        let _sec = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        let data_off = w.reserve(payload.len(), 1);
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
        w.write(&payload);
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags: u64::from(elf::SHF_ALLOC),
            // sh_addr near top of address space; sh_addr + sh_size > u64::MAX
            sh_addr: u64::MAX - 1,
            sh_offset: data_off as u64,
            sh_size: payload.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }
    let obj = parse(&buf);
    let err = elf_sections_to_mem_regions(&obj, |_| true)
        .expect_err("addr+len overflow must surface as RegionOverflow");
    assert!(
        matches!(
            err.kind(),
            reader::ErrorKind::RegionOverflow { start_addr, len }
                if *start_addr == u64::MAX - 1 && *len == 4
        ),
        "got {:?}",
        err.kind(),
    );
}
```

- [ ] **Step 2: Run the new test to verify it passes on the current code**

Run: `cargo test -p reader --test elf_converters elf_sections_to_mem_regions_propagates_region_overflow`

Expected: PASS. The test verifies an *existing*, undocumented behavior (the doc fix in Step 3 is a doc-only change). The test is the artifact that pins it; if it ever fails, the contract has changed.

- [ ] **Step 3: Update `# Errors` on every affected converter**

In `crates/reader/src/elf.rs`, replace each `# Errors` block listed below with the corresponding new wording.

**`elf_segment_to_mem_region`:**

```rust
/// # Errors
///
/// Returns:
/// - `ErrorKind::Object` wrapping the underlying `object::Error` if the
///   segment's file-backed data cannot be read.
/// - `ErrorKind::RegionOverflow` if `segment.address() + data.len()` would
///   exceed `u64::MAX`.
```

**`elf_section_to_mem_region`:** same shape, "section" instead of "segment".

```rust
/// # Errors
///
/// Returns:
/// - `ErrorKind::Object` wrapping the underlying `object::Error` if the
///   section's file-backed data cannot be read.
/// - `ErrorKind::RegionOverflow` if `section.address() + data.len()` would
///   exceed `u64::MAX`.
```

**`elf_segments_to_mem_regions`:**

```rust
/// # Errors
///
/// Returns:
/// - `ErrorKind::Object` wrapping the underlying `object::Error` if a
///   segment accepted by `filter` has file-backed data that cannot be
///   read. Segments rejected by `filter` are never read, so malformed
///   rejected segments do not surface as errors.
/// - `ErrorKind::RegionOverflow` if an accepted segment's
///   `address() + data.len()` would exceed `u64::MAX`.
///
/// Accepted empty-data segments (e.g. `p_filesz == 0`) are skipped rather
/// than reported.
```

**`elf_sections_to_mem_regions`:** mirror with "section" / "SHT_NOBITS-equivalents".

```rust
/// # Errors
///
/// Returns:
/// - `ErrorKind::Object` wrapping the underlying `object::Error` if a
///   section accepted by `filter` has file-backed data that cannot be
///   read. Sections rejected by `filter` are never read, so malformed
///   rejected sections do not surface as errors.
/// - `ErrorKind::RegionOverflow` if an accepted section's
///   `address() + data.len()` would exceed `u64::MAX`.
///
/// Accepted empty-data sections (e.g. `SHT_NOBITS`-equivalents) are
/// skipped rather than reported.
```

**`elf_get_executable_segments_as_mem_regions`:**

```rust
/// # Errors
///
/// Propagates any error from the underlying segment iteration; see
/// [`elf_segments_to_mem_regions`] for the full error set
/// (`Object` + `RegionOverflow`).
```

**`elf_get_executable_sections_as_mem_regions`:**

```rust
/// # Errors
///
/// Propagates any error from the underlying section iteration; see
/// [`elf_sections_to_mem_regions`] for the full error set
/// (`Object` + `RegionOverflow`).
```

**`elf_get_code_and_readonly_sections_as_mem_regions`:**

```rust
/// # Errors
///
/// Propagates any error from the underlying section iteration; see
/// [`elf_sections_to_mem_regions`] for the full error set
/// (`Object` + `RegionOverflow`).
```

**`ElfFileMemReader::from_object`:**

```rust
/// # Errors
///
/// Propagates any error from
/// [`elf_get_code_and_readonly_sections_as_mem_regions`]: `Object` for
/// unreadable section data and `RegionOverflow` if any included
/// section's `address() + data.len()` would exceed `u64::MAX`.
```

(`from_bytes` and `from_path` already say "any error produced by [the inner constructor]", so they cover this transitively. No edit needed there. `load_elf` does NOT call `MemRegion::new`, so it's unaffected.)

- [ ] **Step 4: Run the full reader test suite**

Run: `cargo test -p reader`
Expected: PASS, including the new `elf_sections_to_mem_regions_propagates_region_overflow`.

- [ ] **Step 5: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. Doc-only + new test, no API change.

- [ ] **Step 7: Commit**

```bash
git add crates/reader/src/elf.rs crates/reader/tests/elf_converters.rs
git commit -m "docs(reader): document RegionOverflow in converter error sections; pin propagation"
```

---

## Task 2: Bind `is_little_endian` to a local in `ReadOnlyMemory::read`

**Files:**
- Modify: [crates/reader/src/elf.rs:265-291](crates/reader/src/elf.rs#L265-L291).

### Step-by-step

- [ ] **Step 1: Replace the `ReadOnlyMemory` impl body**

In `crates/reader/src/elf.rs`, replace:

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

with:

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
        let is_little = self.is_little_endian;
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

- [ ] **Step 2: Run the existing endianness tests**

Run: `cargo test -p reader --test elf_reader`
Expected: PASS — `ro_read_little_endian_u32`, `ro_read_big_endian_u32`, `ro_read_little_endian_u64`, `ro_read_single_byte`, and `elf_reader_satisfies_read_only_memory_contract` still pass unchanged.

- [ ] **Step 3: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/reader/src/elf.rs
git commit -m "refactor(reader): bind is_little_endian to a local in ReadOnlyMemory::read"
```

---

## Task 3: Tighten `MemRegion::new` overflow check with `.ok_or(...)?`

**Files:**
- Modify: [crates/reader/src/lib.rs:66-72](crates/reader/src/lib.rs#L66-L72).

### Step-by-step

- [ ] **Step 1: Replace the body of `MemRegion::new`**

In `crates/reader/src/lib.rs`, replace:

```rust
pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
    let len = data.len() as u64;
    if start_addr.checked_add(len).is_none() {
        return Err(error::ErrorKind::RegionOverflow { start_addr, len }.into());
    }
    Ok(Self { start_addr, data })
}
```

with:

```rust
pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
    let len = data.len() as u64;
    start_addr
        .checked_add(len)
        .ok_or(error::ErrorKind::RegionOverflow { start_addr, len })?;
    Ok(Self { start_addr, data })
}
```

The behavior is identical: same error variant, same error fields, same `Into::into` happens at the `?`. Only the surface form changes.

- [ ] **Step 2: Run the existing overflow tests**

Run: `cargo test -p reader --test mem_region mem_region_new_rejects_overflow mem_region_new_accepts_exact_fit_at_top_of_address_space`
Expected: PASS — both overflow tests still pass unchanged.

- [ ] **Step 3: Run the full reader test suite**

Run: `cargo test -p reader`
Expected: PASS.

- [ ] **Step 4: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/reader/src/lib.rs
git commit -m "refactor(reader): tighten MemRegion::new overflow check with ok_or"
```

---

## Task 4: Final sanity sweep

**Files:** run-only, no edits.

- [ ] **Step 1: Full reader test suite**

Run: `cargo test -p reader`
Expected: PASS. `elf_converters` tests grow by 1 (Task 1's new pin); all other counts unchanged.

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

- **Add a `VnSpace` filter to `MemReader::read` on `ElfFileMemReader`.** The trait is documented as "reads memory from the given address" without a space restriction, and Sleigh in practice only invokes it with `VnSpace::RAM` for instruction fetch. Adding a filter risks breaking callers that rely on permissive lookup-by-offset and changes a long-stable API. Worth flagging here so a future round can revisit if a real bug ever materializes.
- **Cache `MemRegion::end_addr` instead of recomputing on each call.** `end_addr` is on the cold path — called externally for diagnostics and `contains`; never inside the read fast path. One `u64` saved per region, churns the struct, breaks the "fields are `start_addr` + `data` only" mental model.
- **Replace the `for { map.insert(...) }` loop in `MemRegionsLookupTable::new` with `regions.into_iter().map(|r| (r.start_addr(), r)).collect()`.** Rejected in round 5; same allocations, same big-O, pure cosmetic.
- **Generic-over-section-vs-segment helper to collapse `elf_*_to_mem_region(s)` duplication.** Rejected in round 1; `object` provides no common trait, and a macro is heavier than the 12-line duplicate bodies.
- **Rename `load_elf` to make the leak opt-in.** Rejected in round 5; the leak is documented and call sites are short-lived tools/tests.
- **Switch `MemRegion::data` from `Vec<u8>` to `Box<[u8]>`.** Rejected in rounds 2/3/4; saves one word per region, churns API, no caller benefits.
- **Enable `clippy::pedantic` workspace-wide.** Deferred until/unless the workspace opts in globally.
- **Re-align `section_is_code_or_readonly`'s column-aligned `let` block.** It's the only place in the crate that uses column alignment, but the alignment is locally readable. Not worth churning.
