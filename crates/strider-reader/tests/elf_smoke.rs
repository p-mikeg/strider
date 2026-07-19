#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! End-to-end against real toolchain-produced ELFs, one test per architecture.
//!
//! Requires `make -C fixtures`. Unlike most fixture-backed tests here, these
//! panic rather than skip when the binary is absent.

use object::Object;
use strider_reader::{ElfFileMemReader, ReadOnlyMemory};

fn binary_path(arch: &str) -> std::path::PathBuf {
    // `arithmetic.elf` stands in for the smoke check: every supported arch
    // builds it cleanly.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join("arithmetic.elf")
}

/// Every arch in this workspace is little-endian (x86, x64, arm/EABI5,
/// aarch64), so endianness is asserted uniformly.
fn assert_smoke(arch: &str) {
    let path = binary_path(arch);
    assert!(
        path.exists(),
        "missing test binary at {} — run `make -C fixtures`",
        path.display(),
    );

    let obj = strider_reader::load_elf(&path).unwrap();
    let obj = obj.file();
    assert_eq!(
        obj.endianness(),
        object::Endianness::Little,
        "{arch}: expected little-endian binary",
    );

    let r = ElfFileMemReader::from_path(&path).unwrap();

    // Some toolchains (x86 in this fixture set) emit `e_entry == 0` when no
    // entry symbol reached the linker. Fall back to the first executable
    // section, which is guaranteed to be in the reader's region table.
    use object::{ObjectSection, SectionKind};
    let exec_addr = if obj.entry() != 0 {
        obj.entry()
    } else {
        obj.sections()
            .find_map(|sec| {
                let addr = sec.address();
                (matches!(sec.kind(), SectionKind::Text) && addr != 0).then_some(addr)
            })
            .expect("real ELF has at least one .text section")
    };
    let mut buf = [0u8; 1];
    let n = rsleigh::MemReader::read(
        &r,
        rsleigh::VnAddr {
            off: exec_addr,
            space: rsleigh::VnSpace::RAM,
        },
        &mut buf,
    )
    .unwrap();
    assert_eq!(n, 1, "{arch}: could not read 1 byte at {exec_addr:#x}");

    assert!(
        ReadOnlyMemory::read(&r, exec_addr, &mut [0u8; 1]).is_ok(),
        "{arch}: ReadOnlyMemory failed at {exec_addr:#x}",
    );

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
