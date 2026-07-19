#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! The loader against a real ET_REL object, `fixtures/out/<arch>/tzcount.o`.
//!
//! ET_REL has no PT_LOAD program headers, so the loader must dispatch to the
//! section walker. Sections commonly share VMA 0 pre-link (`.text` carrying
//! `tzcount`, `.text.startup` carrying `main`), and first-wins VMA dedup is
//! what keeps a `tzcount` lookup on the intended `.text` bytes.

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

use strider_reader::{ElfFileMemReader, ReadOnlyMemory};

fn object_path(arch: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(format!("{case}.o"))
}

/// Regression: the loader used to walk PT_LOAD program headers unconditionally,
/// so `.o` files produced an empty memory map, reads at symbol addresses
/// returned `None`, and any analysis on top silently no-opped.
#[test]
fn et_rel_object_file_loads_text_at_tzcount_symbol_address() {
    let path = object_path("x64", "tzcount");
    if !path.exists() {
        // `make -C fixtures ARCH=x64 CASE=tzcount` builds this; skip cleanly
        // when fixtures aren't built.
        return;
    }

    let obj = strider_reader::load_elf(&path).expect("load_elf on .o");
    let obj = obj.file();
    assert_eq!(
        obj.kind(),
        object::ObjectKind::Relocatable,
        "tzcount.o must parse as ET_REL"
    );

    let reader = ElfFileMemReader::from_object(&obj).expect("ElfFileMemReader::from_object");

    // For an ET_REL with `sh_addr == 0` this is just the section-relative
    // offset. The specific number doesn't matter, only that a readable byte
    // sits where the symbol says it does.
    let tz = obj
        .symbol_by_name("tzcount")
        .expect("tzcount symbol present");
    let addr = tz.address();

    // The minimum contract for lifting to start at all: Sleigh's
    // `lift_one(addr)` fetches instructions through `MemReader::read`.
    assert!(
        ReadOnlyMemory::read(&reader, addr, &mut [0u8; 1]).is_ok(),
        "loader must surface .text bytes for the tzcount symbol at {addr:#x}; \
         a None here means the ET_REL section-walker dispatch is missing or \
         first-wins dedup let an empty section shadow .text"
    );
}
