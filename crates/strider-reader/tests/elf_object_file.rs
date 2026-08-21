#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! The loader against a real ET_REL object, `fixtures/out/<arch>/tzcount.o`.
//!
//! ET_REL has no PT_LOAD program headers, so the loader must dispatch to the
//! section walker. Sections commonly share VMA 0 pre-link (`.text` carrying
//! `tzcount`, `.text.startup` carrying `main`), and `ElfSectionLayout` is what
//! puts each at an address of its own, so a `tzcount` lookup lands on `.text`'s
//! bytes and `main` on `.text.startup`'s. Per-section coverage of that lives in
//! `elf_rel_layout.rs`.

use object::Object;
use std::path::PathBuf;

use strider_reader::elf::ElfSectionLayout;
use strider_reader::{ElfFileMemReader, ReadOnlyMemory};

fn object_path(arch: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(format!("{case}.o"))
}

/// A `.o` must load through the section walk. Walking PT_LOAD program headers
/// unconditionally yields an empty memory map here, reads at symbol addresses
/// returning `None` and any analysis on top silently no-opping.
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

    // An ET_REL symbol's `st_value` is an offset into its section, so the
    // address only means anything once rebased through the layout.
    let layout = ElfSectionLayout::new(&obj);
    let tz = obj
        .symbol_by_name("tzcount")
        .expect("tzcount symbol present");
    let addr = layout.symbol_address(&tz);

    // The minimum contract for lifting to start at all: Sleigh's
    // `lift_one(addr)` fetches instructions through `MemReader::read`.
    assert!(
        ReadOnlyMemory::read(&reader, addr, &mut [0u8; 1]).is_ok(),
        "loader must surface .text bytes for the tzcount symbol at {addr:#x}; \
         an error here means the ET_REL section-walker dispatch is missing"
    );
}
