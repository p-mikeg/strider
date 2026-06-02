#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Loader tests against an ET_REL object file produced by the
//! toolchain — `fixtures/out/<arch>/tzcount.o`.
//!
//! ET_REL files have no PT_LOAD program headers, so the loader must
//! dispatch to the section-walker.  Several sections commonly share
//! VMA 0 pre-link (`.text` carrying `tzcount`, `.text.startup`
//! carrying `main`); first-wins VMA dedup is what makes a `tzcount`
//! lookup land on the intended `.text` bytes instead of being
//! silently swapped by a later iteration-order shadow.

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

use strider_reader::{ElfFileMemReader, ReadOnlyMemory};

fn object_path(arch: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(format!("{case}.o"))
}

/// `strider.load`'s loader path: parse the `.o`, build an
/// `ElfFileMemReader`, then verify a function symbol from the `.o`
/// (`tzcount`) resolves to bytes that can be read back at its
/// symbol address.  Pre-fix, this loader produced an empty memory
/// map for `.o` files because it walked PT_LOAD program headers
/// (which an `.o` doesn't have); the read at the symbol address
/// returned `None` and any analysis built on top was a no-op.
#[test]
fn et_rel_object_file_loads_text_at_tzcount_symbol_address() {
    let path = object_path("x64", "tzcount");
    if !path.exists() {
        // `make -C fixtures ARCH=x64 CASE=tzcount` builds this.
        // Tests skip cleanly when fixtures aren't built — matches the
        // existing `apply_elf_relocations_patches_dispatch_table_x86_64`
        // skip convention.
        return;
    }

    let obj = strider_reader::load_elf(&path).expect("load_elf on .o");
    assert_eq!(
        obj.kind(),
        object::ObjectKind::Relocatable,
        "tzcount.o must parse as ET_REL"
    );

    let reader = ElfFileMemReader::from_object(&obj).expect("ElfFileMemReader::from_object");

    // `tzcount`'s symbol resolves to an absolute address in the loaded
    // image (for an ET_REL with `sh_addr == 0`, that's just the
    // section-relative offset, which is 0 because `.text` starts at
    // VMA 0 and `tzcount` is the first function).  The point of the
    // test isn't the specific number, just that *some* readable byte
    // sits where the symbol says it does.
    let tz = obj
        .symbol_by_name("tzcount")
        .expect("tzcount symbol present");
    let addr = tz.address();

    // Reading a single byte at the symbol address must succeed.  This
    // is the minimum contract Strider needs to even start lifting:
    // Sleigh's `lift_one(addr)` will call `MemReader::read(addr, _)`
    // for instruction fetch, and a failed read here means "I couldn't
    // load this function from the binary at all".
    assert!(
        ReadOnlyMemory::read(&reader, addr, &mut [0u8; 1]).is_ok(),
        "loader must surface .text bytes for the tzcount symbol at {addr:#x}; \
         a None here means the ET_REL section-walker dispatch is missing or \
         first-wins dedup let an empty section shadow .text"
    );
}
