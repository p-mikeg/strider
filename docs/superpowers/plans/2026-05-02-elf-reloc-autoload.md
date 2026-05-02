# ELF reloc autoload Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `MemoryMap.apply_elf_relocations(path)` self-healing — when a relocation site falls outside every currently-loaded region, lazily pull the containing section from the supplied ELF and patch into it, instead of silently incrementing `skipped_no_region`.

**Architecture:** Add a `reader::elf::apply_elf_relocations_autoload(&mut Vec<MemRegion>, &File) -> Result<RelocationStats>` helper that pre-walks the dynamic relocation table, finds sites with no covering region, looks up the containing ELF section, and appends a `MemRegion` for each unique missing section before delegating to the existing pure `apply_elf_relocations`. Keep the existing pure function unchanged so the bundled `elf_load_with_relocations` path and the Rust-only callers continue to behave identically. Wire `PyMemoryMap::apply_elf_relocations` to the new autoload variant.

**Tech Stack:** Rust (`reader` crate, `object` crate 0.38), PyO3 wrapper (`strider-py`), pytest + cargo test.

---

## Background

Bug: on i386 FreeBSD kernels (≥12.x), the user's call sequence

```python
mem.add_region_from_elf(path)              # default: code+ro only
mem.apply_elf_relocations(path)
```

reports `applied=0, skipped_no_region=N` even though every relocation kind is recognised. The site addresses for `R_386_IRELATIVE` entries fall inside `.got.plt` (writable, hence excluded by the default loader). The bundled `add_region_from_elf(path, apply_relocations=True)` works because it widens to all `SHF_ALLOC` file-backed sections; the split call path is the footgun.

Fix: lift the widening into `apply_elf_relocations` itself — but lazily, only for sections that actually contain reloc sites. That keeps the loaded region set small for callers that don't need anything else, and it eliminates the "applied=0 with no error" silent-failure mode.

## File Structure

- **Modify** `crates/reader/src/elf.rs` — add `apply_elf_relocations_autoload` helper + private `find_section_containing` lookup
- **Modify** `crates/reader/tests/elf_relocations.rs` — add Rust integration tests for the autoload helper
- **Modify** `crates/strider-py/src/reader.rs` — switch `PyMemoryMap::apply_elf_relocations` from the pure helper to the autoload variant
- **Modify** `crates/strider-py/tests/python/test_elf_relocations.py` — flip the previously-pinned "skipped_no_region" assertion (now stale) to assert the autoload behaviour, plus add a fresh test pinning `skipped_no_region == 0` after autoload

---

## Task 1: Rust autoload helper — failing integration test first

**Files:**
- Test: `crates/reader/tests/elf_relocations.rs`

- [ ] **Step 1: Add a failing integration test for `apply_elf_relocations_autoload`**

Append to `crates/reader/tests/elf_relocations.rs`:

```rust
#[test]
fn apply_elf_relocations_autoload_pulls_in_missing_site_sections() {
    // Reproduces the i386 kernel scenario at the Rust level: load only
    // code-and-readonly (so `.data.rel.ro` is excluded), then call the
    // autoload variant.  It must lazily pull the missing section so
    // every relocation lands and `skipped_no_region` is zero.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let mut regions =
        reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();
    let regions_before = regions.len();

    let stats = reader::elf::apply_elf_relocations_autoload(&mut regions, &obj)
        .expect("autoload apply");

    // Every reloc must be applied — autoload's whole job is to make
    // skipped_no_region unreachable for sites the ELF itself defines.
    assert!(stats.seen > 0, "fixture should have at least one reloc; stats = {stats:?}");
    assert_eq!(stats.skipped_no_region, 0, "autoload must cover every site; stats = {stats:?}");
    assert_eq!(stats.applied, stats.seen, "every reloc should land; stats = {stats:?}");
    assert!(regions.len() > regions_before, "autoload must have added at least one region");

    // Post-condition: dispatch_table now reads the helper addresses.
    let table_addr = sym_addr(&obj, "dispatch_table");
    let helper_a = sym_addr(&obj, "helper_a");
    assert_eq!(read_u64_le(&regions, table_addr), Some(helper_a));
}
```

- [ ] **Step 2: Run the test, confirm it fails with "no method named `apply_elf_relocations_autoload`"**

Run:
```
cargo test -p reader --test elf_relocations apply_elf_relocations_autoload_pulls_in_missing_site_sections 2>&1 | tail -20
```
Expected: compile error — `apply_elf_relocations_autoload` does not exist.

---

## Task 2: Implement the autoload helper

**Files:**
- Modify: `crates/reader/src/elf.rs`

- [ ] **Step 1: Add the autoload helper after the existing `apply_elf_relocations`**

Append immediately after the closing `}` of `pub fn apply_elf_relocations` (~line 641 in the current file):

```rust
/// Like [`apply_elf_relocations`], but pre-walks the dynamic
/// relocation table and lazily extends `regions` with any
/// SHF_ALLOC file-backed section from `obj` that contains a
/// relocation site not yet covered by an existing region.  Then
/// delegates to the pure [`apply_elf_relocations`].
///
/// Use this when the caller has already loaded a curated subset
/// of the ELF (e.g. only code+rodata) but wants relocation
/// application to "just work" without needing to know upfront
/// which writable sections (`.got.plt`, `.data.rel.ro`, …) the
/// dynamic relocs target.  Avoids the silent-failure mode of
/// the pure variant where every relocation is counted as
/// `skipped_no_region` because the caller didn't pre-load the
/// right sections.
///
/// Sections are added in iteration order of `obj.sections()`,
/// each appended once even when multiple relocs target the same
/// section.  An ELF section that has no file-backed bytes
/// (`SHT_NOBITS`, e.g. `.bss`) is *not* added — there's nothing
/// to patch — and the corresponding relocs still increment
/// `skipped_no_region` from inside the inner call.
///
/// # Errors
///
/// Same as [`apply_elf_relocations`].  The lazy-load step itself
/// only fails on a malformed ELF (a section whose `data()` can't
/// be read or whose `address() + len()` overflows `u64`).
pub fn apply_elf_relocations_autoload(
    regions: &mut Vec<MemRegion>,
    obj: &object::File<'_>,
) -> Result<RelocationStats> {
    let Some(dyn_relocs) = obj.dynamic_relocations() else {
        // No dynamic-reloc table → nothing to autoload, nothing
        // to apply.  Same return value as the pure helper.
        return Ok(RelocationStats::default());
    };

    // Pass 1 — collect site addresses not yet covered, look up
    // their owning section, and stage one MemRegion per unique
    // missing section.  We never mutate `regions` here so an
    // error mid-pass leaves it untouched.
    let mut staged: Vec<MemRegion> = Vec::new();
    for (site_addr, _reloc) in dyn_relocs {
        let already_covered = regions
            .iter()
            .chain(staged.iter())
            .any(|r| r.contains(site_addr));
        if already_covered {
            continue;
        }
        // Find the SHF_ALLOC PROGBITS section that contains
        // `site_addr`.  Anything else (SHT_NOBITS, non-alloc) is
        // skipped — it has no patchable bytes and the inner
        // applier will count the reloc under `skipped_no_region`.
        let Some(sec) = find_loadable_section_containing(obj, site_addr) else {
            continue;
        };
        let data = sec.data().context("failed to parse ELF")?;
        if data.is_empty() {
            continue;
        }
        staged.push(MemRegion::new(sec.address(), data.to_vec())?);
    }

    regions.extend(staged);
    apply_elf_relocations(regions, obj)
}

/// Returns the first section in `obj` that contains `addr` and is
/// safe to materialise as a `MemRegion`: `SHF_ALLOC` set, file-
/// backed (i.e. *not* `SHT_NOBITS`).  Returns `None` when no
/// section matches — caller treats that as "leave the reloc as
/// skipped_no_region".
fn find_loadable_section_containing<'a>(
    obj: &'a object::File<'_>,
    addr: u64,
) -> Option<object::read::Section<'a, '_>> {
    obj.sections().find(|sec| {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            return false;
        };
        if sh_flags & u64::from(object::elf::SHF_ALLOC) == 0 {
            return false;
        }
        // SHT_NOBITS sections have no file-backed bytes.  Detect
        // via empty data() rather than the raw section type so we
        // also skip exotic types that happen to be data-less.
        if sec.data().map(|d| d.is_empty()).unwrap_or(true) {
            return false;
        }
        let lo = sec.address();
        let hi = lo.saturating_add(sec.size());
        addr >= lo && addr < hi
    })
}
```

- [ ] **Step 2: Run the failing test, confirm it now passes**

Run:
```
cargo test -p reader --test elf_relocations apply_elf_relocations_autoload_pulls_in_missing_site_sections 2>&1 | tail -15
```
Expected: test passes.

- [ ] **Step 3: Run the entire reader test suite to confirm nothing else broke**

Run:
```
cargo test -p reader 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/reader/src/elf.rs crates/reader/tests/elf_relocations.rs
git commit -m "reader: add apply_elf_relocations_autoload — lazy section extension"
```

---

## Task 3: Idempotency + no-op-on-empty-relocs Rust tests

**Files:**
- Modify: `crates/reader/tests/elf_relocations.rs`

- [ ] **Step 1: Add an idempotency test for the autoload variant**

Append to `crates/reader/tests/elf_relocations.rs`:

```rust
#[test]
fn apply_elf_relocations_autoload_is_idempotent() {
    // Running autoload twice must produce the same regions as
    // running it once: the second call sees the previously-staged
    // sections in `regions` and skips re-staging.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let mut regions =
        reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();

    let _ = reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();
    let snapshot: Vec<(u64, Vec<u8>)> =
        regions.iter().map(|r| (r.start_addr(), r.data().to_vec())).collect();

    let _ = reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();
    let after: Vec<(u64, Vec<u8>)> =
        regions.iter().map(|r| (r.start_addr(), r.data().to_vec())).collect();

    assert_eq!(snapshot, after, "autoload must be idempotent");
}

#[test]
fn apply_elf_relocations_autoload_no_op_on_pre_resolved_binary() {
    // Pre-link-resolved ET_EXEC fixture (`control`) has no dynamic
    // relocations → autoload returns the empty stats with no
    // region mutation, same as the pure variant on this case.
    let path = fixture_path("x86", "control");
    if !path.exists() {
        return;
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let mut regions =
        reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();
    let regions_before = regions.len();

    let stats = reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();

    assert_eq!(stats.seen, 0, "ET_EXEC has no dynamic relocs");
    assert_eq!(stats.applied, 0);
    assert_eq!(regions.len(), regions_before, "no relocs ⇒ no autoload work");
}
```

- [ ] **Step 2: Run both new tests**

Run:
```
cargo test -p reader --test elf_relocations apply_elf_relocations_autoload 2>&1 | tail -20
```
Expected: all three `apply_elf_relocations_autoload*` tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/reader/tests/elf_relocations.rs
git commit -m "reader: cover autoload idempotency + no-op-on-static-binary"
```

---

## Task 4: Wire `PyMemoryMap::apply_elf_relocations` to the autoload variant

**Files:**
- Modify: `crates/strider-py/src/reader.rs:257-274`

- [ ] **Step 1: Update the existing `apply_elf_relocations` PyMethod**

Replace the body of `fn apply_elf_relocations` (currently at `crates/strider-py/src/reader.rs:257`) with the autoload-backed version. The whole method becomes:

```rust
    /// Apply the ELF at `path`'s dynamic relocations to the regions
    /// already loaded into this MemoryMap.  Returns the
    /// `RelocationStats` breakdown so callers can sanity-check the
    /// outcome.
    ///
    /// **Auto-loads missing site sections.**  When a relocation
    /// site falls outside every region currently in the MemoryMap,
    /// the supplied ELF's containing section is appended on the
    /// fly and the relocation is then applied to it.  This is what
    /// makes the common pattern
    ///
    /// ```python
    /// mem.add_region_from_elf(path)              # code + rodata
    /// mem.apply_elf_relocations(path)            # autoloads .got.plt etc.
    /// ```
    ///
    /// produce the same patched-region set as the bundled
    /// `add_region_from_elf(path, apply_relocations=True)` form,
    /// instead of silently reporting `applied = 0` with every
    /// reloc counted under `skipped_no_region`.  See
    /// `crates/reader/src/elf.rs::apply_elf_relocations_autoload`
    /// for the lazy-load contract (file-backed SHF_ALLOC sections
    /// only — SHT_NOBITS like `.bss` is never autoloaded).
    fn apply_elf_relocations(&self, path: &str) -> PyResult<PyRelocationStats> {
        let obj = reader::load_elf(path).map_err(into_reader_err)?;
        let mut regions = self
            .inner
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap regions lock poisoned")))?;
        let stats = reader::elf::apply_elf_relocations_autoload(&mut regions, &obj)
            .map_err(into_reader_err)?;
        // Invalidate the lookup table — both the autoload step
        // (which appends new regions) and the in-place patches
        // require a rebuild before the next read.
        let mut slot = self
            .table
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap table lock poisoned")))?;
        *slot = None;
        Ok(stats.into())
    }
```

- [ ] **Step 2: Rebuild the wheel into the venv**

Run:
```
cd crates/strider-py && source .venv/bin/activate && maturin develop --release 2>&1 | tail -5
```
Expected: build succeeds, wheel installed.

- [ ] **Step 3: Run the existing strider-py reloc tests — confirm one fails**

Run:
```
cd crates/strider-py && source .venv/bin/activate && python -m pytest tests/python/test_elf_relocations.py -v 2>&1 | tail -30
```
Expected: `test_apply_elf_relocations_reports_skipped_no_region` FAILS — its assertion `stats.skipped_no_region >= 4` is now stale because autoload makes that scenario applied=seen.

This is the expected breakage; Task 5 fixes the test to match the new behaviour.

---

## Task 5: Update the now-stale Python test + add an explicit autoload-pinning test

**Files:**
- Modify: `crates/strider-py/tests/python/test_elf_relocations.py`

- [ ] **Step 1: Replace `test_apply_elf_relocations_reports_skipped_no_region` with the new contract**

Find the existing test (at line 127) and replace the whole function with:

```python
def test_apply_elf_relocations_autoloads_missing_site_sections():
    """When the load step omits `.data.rel.ro` (default behaviour),
    the standalone applier now lazily pulls the section in from the
    same ELF rather than silently reporting `skipped_no_region`.

    This is the strider-py-side guarantee for the i386-kernel bug
    (see `crates/reader/src/elf.rs::apply_elf_relocations_autoload`):
    `add_region_from_elf(path)` followed by
    `apply_elf_relocations(path)` produces the same patched-region
    state as the bundled `add_region_from_elf(path,
    apply_relocations=True)` call.
    """
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))  # default: no `.data.rel.ro`
    table_addr = mem.symbol("dispatch_table")
    # Pre-condition: dispatch_table is unmapped before the apply call.
    assert mem.read(table_addr, 8) is None

    stats = mem.apply_elf_relocations(str(elf))

    # Autoload kicks in: every reloc lands.
    assert stats.seen > 0, f"fixture should expose dynamic relocs: {stats!r}"
    assert stats.skipped_no_region == 0, (
        f"autoload should leave nothing skipped_no_region: {stats!r}"
    )
    assert stats.applied == stats.seen, (
        f"every reloc should be applied after autoload: {stats!r}"
    )
    # And the autoloaded section is now readable through the MemoryMap.
    helper_a = mem.symbol("helper_a")
    assert _read_u64_le(mem, table_addr) == helper_a
```

- [ ] **Step 2: Add a sibling test pinning the equivalence with the bundled path**

Append directly after the test from Step 1:

```python
def test_split_call_path_matches_bundled_after_autoload():
    """Autoload makes `add_region_from_elf(path)` +
    `apply_elf_relocations(path)` observationally equivalent to
    the bundled `add_region_from_elf(path, apply_relocations=True)`
    for every dispatch_table slot.  Pins the "no footgun" property
    for users who don't know about the bundled flag."""
    elf = X64_RELOCS()

    bundled = strider.MemoryMap()
    bundled.add_region_from_elf(str(elf), apply_relocations=True)

    split = strider.MemoryMap()
    split.add_region_from_elf(str(elf))
    split.apply_elf_relocations(str(elf))

    table_addr = bundled.symbol("dispatch_table")
    for slot in range(4):
        addr = table_addr + 8 * slot
        assert _read_u64_le(bundled, addr) == _read_u64_le(split, addr), (
            f"slot {slot} differs: bundled={_read_u64_le(bundled, addr):#x} "
            f"split={_read_u64_le(split, addr):#x}"
        )
```

- [ ] **Step 3: Run the full strider-py reloc test suite — confirm all green**

Run:
```
cd crates/strider-py && source .venv/bin/activate && python -m pytest tests/python/test_elf_relocations.py -v 2>&1 | tail -25
```
Expected: every test passes, including the two new ones.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/src/reader.rs crates/strider-py/tests/python/test_elf_relocations.py
git commit -m "strider-py: MemoryMap.apply_elf_relocations now autoloads missing site sections"
```

---

## Task 6: End-to-end verification on the actual i386 kernel

**Files:** none (verification only)

- [ ] **Step 1: Run the original i386-kernel scenario from the bug report**

Run:
```
cd crates/strider-py && source .venv/bin/activate && python -c "
import strider
for ver in ['10.0', '12.0', '13.0', '14.0']:
    path = f'/home/mike/Desktop/bsdfinder/kernels/i386/{ver}/kernel'
    mem = strider.MemoryMap()
    mem.add_region_from_elf(path)
    stats = mem.apply_elf_relocations(path)
    print(f'i386 {ver}: {stats!r}')
"
```
Expected output:
```
i386 10.0: RelocationStats(seen=0, applied=0, ..., skipped_no_region=0, ...)
i386 12.0: RelocationStats(seen=3, applied=3, ..., skipped_no_region=0, ...)
i386 13.0: RelocationStats(seen=3, applied=3, ..., skipped_no_region=0, ...)
i386 14.0: RelocationStats(seen=5, applied=5, ..., skipped_no_region=0, ...)
```

The `seen=0` for 10.0 is correct — that kernel has no dynamic relocs at all.
12.0 / 13.0 / 14.0 must show `applied == seen` and `skipped_no_region == 0`.

- [ ] **Step 2: Run full workspace test suite as a final check**

Run:
```
cargo test --workspace 2>&1 | tail -15
```
Expected: all tests pass.

Run:
```
cd crates/strider-py && source .venv/bin/activate && python -m pytest tests/python/ 2>&1 | tail -10
```
Expected: full pytest suite passes.

- [ ] **Step 3: Run clippy**

Run:
```
cargo clippy --workspace --tests 2>&1 | tail -20
```
Expected: no new warnings.

---

## Self-Review Checklist

- **Spec coverage:** Bug report → Task 2 implements the autoload, Task 4 wires it into the Python wrapper, Tasks 1/3/5 cover it with tests, Task 6 verifies on the original i386-kernel scenario.
- **Placeholders:** None — every step has either complete code, an exact command, or a literal expected output snippet.
- **Type consistency:** `apply_elf_relocations_autoload(&mut Vec<MemRegion>, &object::File<'_>) -> Result<RelocationStats>` is used consistently across Tasks 1, 2, 3, and 4. The Python wrapper's `regions` field is already `Arc<RwLock<Vec<MemRegion>>>`, so the `&mut Vec<...>` signature drops in cleanly.
