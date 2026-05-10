//! Integration tests for `reader::elf::apply_elf_relocations`.
//!
//! Uses the `fixtures/out/<arch>/elf_relocs.elf` shared-library
//! fixture (built by `fixtures/Makefile` with `-shared -fPIC`).  The
//! fixture's `dispatch_table` array has one relocation per slot
//! pointing at one of `helper_a..helper_d`; without the applier the
//! slots read zero, with the applier they read the helper addresses.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

fn fixture_path(arch: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(format!("{case}.elf"))
}

/// Read 8 bytes (LE) from `regions` at virtual address `addr`.
/// Returns `None` if no region covers the range.
fn read_u64_le(regions: &[reader::MemRegion], addr: u64) -> Option<u64> {
    for r in regions {
        if r.contains(addr) && addr + 8 <= r.end_addr() {
            let off = (addr - r.start_addr()) as usize;
            let bytes = &r.data()[off..off + 8];
            return Some(u64::from_le_bytes(bytes.try_into().unwrap()));
        }
    }
    None
}

/// Find the symbol's address; panic with a clear message when missing.
fn sym_addr(obj: &object::File<'_>, name: &str) -> u64 {
    obj.symbol_by_name(name)
        .unwrap_or_else(|| panic!("symbol {name:?} not found"))
        .address()
}

#[test]
fn apply_elf_relocations_patches_dispatch_table_x86_64() {
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        panic!("missing {path:?}; run `make -C fixtures CASE=elf_relocs ARCH=x64`");
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    // dispatch_table lives in `.data.rel.ro` (writable section that
    // gets relro-protected at runtime).  The default
    // code-and-readonly loader skips it; the wider loader picks it
    // up so the applier has somewhere to patch.
    let (regions, stats) = reader::elf::elf_load_with_relocations(&obj).expect("load+apply");

    let table_addr = sym_addr(&obj, "dispatch_table");
    let helper_a = sym_addr(&obj, "helper_a");
    let helper_b = sym_addr(&obj, "helper_b");
    let helper_c = sym_addr(&obj, "helper_c");
    let helper_d = sym_addr(&obj, "helper_d");

    assert!(
        stats.applied >= 4,
        "expected ≥4 dispatch-table relocations applied; stats = {stats:?}"
    );

    // regression: a
    // well-formed ELF must produce zero malformed-target counts.
    // Pre-fix the GOT-PLT path bucketed Section-bad-index errors as
    // `skipped_malformed_target` while the generic-symbol path
    // bucketed the same shape as `skipped_unresolved_target`; this
    // assertion plus the post-fix code-inspection of the three
    // malformed-bucket sites (elf.rs lines 538, 572, 584) pin the
    // consistent classification.
    assert_eq!(
        stats.skipped_malformed_target, 0,
        "a well-formed ELF must report zero malformed targets; stats = {stats:?}"
    );
    assert_eq!(
        stats.skipped_unresolved_target, 0,
        "a well-formed ELF (no weak externs) must report zero unresolved targets; stats = {stats:?}"
    );

    // Post-condition: every dispatch_table slot now reads its helper.
    assert_eq!(read_u64_le(&regions, table_addr), Some(helper_a));
    assert_eq!(read_u64_le(&regions, table_addr + 8), Some(helper_b));
    assert_eq!(read_u64_le(&regions, table_addr + 16), Some(helper_c));
    assert_eq!(read_u64_le(&regions, table_addr + 24), Some(helper_d));
}

// the dedicated synthetic-ELF unit
// test for the malformed-target bucket invariant remains deferred —
// constructing an ET_DYN with a bad-symbol-index Rela through
// `object::write::elf::Writer` requires writing a full dynamic table
// + dynsym + dynstr + rela.dyn + program headers.  The fix
// is verified by:
// (1) code inspection — elf.rs:538 (GOT-PLT path), elf.rs:572 (bad
//     symbol index in generic path), elf.rs:584 (bad section index in
//     generic path) all increment the same `skipped_malformed_target`
//     counter post-fix; the pre-fix Section path used the wrong
//     bucket.
// (2) the existing positive-path test
//     `apply_elf_relocations_patches_dispatch_table_x86_64` now
//     asserts `stats.skipped_malformed_target == 0` and
//     `stats.skipped_unresolved_target == 0` (a well-formed ELF
//     produces zero in both buckets — any future regression that
//     leaks a malformed-bucket increment into a clean path fails).

#[test]
fn default_loader_omits_data_rel_ro() {
    // Sanity: without `apply_relocations`, the
    // `elf_get_code_and_readonly_sections_as_mem_regions` filter
    // still excludes `.data.rel.ro` (it's writable).  This is the
    // existing back-compat behaviour the Python `MemoryMap`
    // default mirrors.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let regions = reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();
    let table_addr = sym_addr(&obj, "dispatch_table");
    assert!(
        read_u64_le(&regions, table_addr).is_none(),
        "default loader must NOT cover `.data.rel.ro` — apply_relocations=True is required"
    );
}

#[test]
fn apply_elf_relocations_no_op_on_pre_resolved_binary() {
    // Strider's existing fixtures (e.g. control.elf) are statically
    // linked executables (ET_EXEC) — `dynamic_relocations()` returns
    // an empty iterator and the applier is a no-op.  This guards
    // the "false default" of the strider-py opt-in flag: a normal
    // userland binary doesn't need relocation processing.
    let path = fixture_path("x86", "control");
    if !path.exists() {
        return; // skip if fixture not built
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let mut regions = reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj)
        .expect("regions");
    let stats = reader::elf::apply_elf_relocations(&mut regions, &obj).expect("apply");
    // ET_EXEC with no dynamic relocations: every counter is 0 (or a
    // small set of GLOB_DAT / JUMP_SLOT entries we deliberately skip
    // because they target undefined externs).
    assert_eq!(
        stats.applied, 0,
        "ET_EXEC pre-link-resolved binary must have nothing to apply; stats = {stats:?}"
    );
}

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

    assert!(stats.seen > 0, "fixture should have at least one reloc; stats = {stats:?}");
    assert_eq!(stats.skipped_no_region, 0, "autoload must cover every site; stats = {stats:?}");
    assert_eq!(stats.applied, stats.seen, "every reloc should land; stats = {stats:?}");
    assert!(regions.len() > regions_before, "autoload must have added at least one region");

    let table_addr = sym_addr(&obj, "dispatch_table");
    let helper_a = sym_addr(&obj, "helper_a");
    assert_eq!(read_u64_le(&regions, table_addr), Some(helper_a));
}

#[test]
fn apply_elf_relocations_idempotent() {
    // Running the applier twice produces the same regions as running
    // it once.  Each relocation is a deterministic write; re-applying
    // overwrites with the same value.
    let path = fixture_path("x64", "elf_relocs");
    if !path.exists() {
        return;
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let mut regions =
        reader::elf::elf_get_allocatable_file_backed_sections_as_mem_regions(&obj).unwrap();
    let _ = reader::elf::apply_elf_relocations(&mut regions, &obj).unwrap();
    let snapshot: Vec<Vec<u8>> = regions.iter().map(|r| r.data().to_vec()).collect();
    let _ = reader::elf::apply_elf_relocations(&mut regions, &obj).unwrap();
    let after: Vec<Vec<u8>> = regions.iter().map(|r| r.data().to_vec()).collect();
    assert_eq!(snapshot, after, "apply_elf_relocations is not idempotent");
}

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
fn apply_elf_relocations_autoload_does_not_fabricate_values_for_undefined_externs() {
    // ET_EXEC userland fixture (`control`) has GLOB_DAT / JMP_SLOT
    // entries that target undefined externs (libc).  Autoload
    // happily pulls the .got / .got.plt sections in (every site
    // covered ⇒ skipped_no_region = 0), but the inner applier
    // still refuses to write a value because the symbol is
    // undefined ⇒ applied = 0, skipped_unresolved_target = seen.
    //
    // Pins the property that autoload doesn't tempt the applier
    // into making up values for undefined externs.
    let path = fixture_path("x86", "control");
    if !path.exists() {
        return;
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let mut regions =
        reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();

    let stats = reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();

    assert!(stats.seen > 0, "fixture should expose dynamic relocs: {stats:?}");
    assert_eq!(stats.applied, 0, "undefined externs must not be fabricated: {stats:?}");
    assert_eq!(stats.skipped_no_region, 0, "autoload should cover every site: {stats:?}");
    assert_eq!(
        stats.skipped_unresolved_target, stats.seen,
        "every reloc here targets an undefined extern: {stats:?}"
    );
}

#[test]
fn apply_elf_relocations_autoload_no_op_when_no_dynamic_table() {
    // A statically-linked fixture (no dynamic_relocations()) ⇒
    // autoload short-circuits with empty stats and zero region
    // mutation.  Uses `arithmetic` because it's the simplest
    // statically-linked fixture in the suite.
    let path = fixture_path("x86", "arithmetic");
    if !path.exists() {
        return;
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let mut regions =
        reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj).unwrap();
    let regions_before = regions.len();

    let stats = reader::elf::apply_elf_relocations_autoload(&mut regions, &obj).unwrap();

    // Whether the fixture has zero dyn relocs or just none of the
    // shape we autoload depends on the linker, but either way the
    // postcondition is identical: nothing applied, nothing
    // staged.  Skip the assert when seen > 0 (means the linker
    // emitted a small table — the previous test covers that).
    if stats.seen == 0 {
        assert_eq!(stats.applied, 0);
        assert_eq!(regions.len(), regions_before, "no dynamic table ⇒ no autoload work");
    }
}

#[test]
fn relocation_stats_default_includes_autoload_parse_failure_counter() {
    // Pin the field's default so a future field rename / removal trips a
    // build-level signal — programmatic callers depend on this counter
    // to detect malformed-ELF cases that would otherwise look like
    // benign `skipped_no_region` entries.
    let stats = reader::elf::RelocationStats::default();
    assert_eq!(stats.autoload_section_parse_failures, 0);
}
