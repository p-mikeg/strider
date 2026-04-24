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
