#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! End-to-end smoke tests against real toolchain-produced ELFs.
//!
//! One test per supported architecture. Each asserts that `load_elf`
//! parses the binary, `ElfFileMemReader::from_path` accepts it, and both
//! trait impls can read one byte at the ELF entry point.
//!
//! Build prerequisites first:
//!
//!     make -C fixtures
//!
//! Tests panic with a clear message if the binary is absent — matching
//! the convention used by `strider_lift::cfg::cfg_integration` and `strider::run`.

use object::Object;
use strider_reader::{ElfFileMemReader, ReadOnlyMemory};

fn binary_path(arch: &str) -> std::path::PathBuf {
    // The legacy single-fixture `test.elf` was split into per-category
    // fixtures by the strider-crate review.  `arithmetic.elf` stands in
    // for the smoke check — every supported arch builds it cleanly.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join("arithmetic.elf")
}

/// Loads `fixtures/out/<arch>/test.elf` and asserts the reader
/// impls all round-trip on it. Every supported arch in this workspace
/// is little-endian (x86, x64, arm/EABI5, aarch64), so that's asserted
/// uniformly here.
fn assert_smoke(arch: &str) {
    let path = binary_path(arch);
    assert!(
        path.exists(),
        "missing test binary at {} — run `make -C fixtures`",
        path.display(),
    );

    // load_elf round-trip
    let obj = strider_reader::load_elf(&path).unwrap();
    assert_eq!(
        obj.endianness(),
        object::Endianness::Little,
        "{arch}: expected little-endian binary",
    );

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
    assert_eq!(n, 1, "{arch}: could not read 1 byte at entry {entry:#x}");

    // ReadOnlyMemory read at entry returns *some* u8 value.
    assert!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, entry, 1).is_some(),
        "{arch}: ReadOnlyMemory failed at entry {entry:#x}",
    );

    // At least one section exists.
    assert!(
        obj.sections().next().is_some(),
        "{arch}: real ELF has no sections?",
    );
}

#[test]
fn elf_reader_loads_real_x86_binary() {
    assert_smoke("x86");
}

#[test]
fn elf_reader_loads_real_x64_binary() {
    assert_smoke("x64");
}

#[test]
fn elf_reader_loads_real_arm_binary() {
    assert_smoke("arm");
}

#[test]
fn elf_reader_loads_real_aarch64_binary() {
    assert_smoke("aarch64");
}
