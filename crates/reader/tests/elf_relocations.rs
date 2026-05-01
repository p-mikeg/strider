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

    // Post-condition: every dispatch_table slot now reads its helper.
    assert_eq!(read_u64_le(&regions, table_addr), Some(helper_a));
    assert_eq!(read_u64_le(&regions, table_addr + 8), Some(helper_b));
    assert_eq!(read_u64_le(&regions, table_addr + 16), Some(helper_c));
    assert_eq!(read_u64_le(&regions, table_addr + 24), Some(helper_d));
}

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
