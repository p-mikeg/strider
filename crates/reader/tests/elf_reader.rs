#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for `reader::ElfFileMemReader` and its trait impls.

#[path = "common/mod.rs"]
mod common;

use common::elf_fixture::simple_text_elf;
use reader::{ElfFileMemReader, ReadOnlyMemory};

/// Sanity check: `simple_text_elf` produces bytes that
/// `ElfFileMemReader::from_bytes` can parse, and the resulting reader
/// reflects the single `.text` section at the chosen address.
#[test]
fn simple_text_elf_fixture_round_trips_through_elf_reader() {
    let elf = simple_text_elf(0x1000, &[0xaa, 0xbb, 0xcc, 0xdd]);
    let r = ElfFileMemReader::from_bytes(&elf).expect("parse synthetic ELF");

    // reading 4 bytes at 0x1000 as a little-endian u32 = 0xddccbbaa
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
        Some(0xddccbbaa),
    );
}
