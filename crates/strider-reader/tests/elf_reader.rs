#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

#[path = "common/mod.rs"]
mod common;

use std::io::Write as _;

use common::elf_fixture::simple_text_elf;
use common::reader_contract::{
    assert_mem_reader_partial_read_ok, assert_mem_reader_reads,
    assert_mem_reader_unmapped_is_not_mapped_error, assert_readonly_errors, assert_readonly_reads,
};
use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::{ElfFileMemReader, ReadOnlyMemory};
use tempfile::NamedTempFile;

fn read_raw(r: &ElfFileMemReader, addr: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    ReadOnlyMemory::read(r, addr, &mut buf).expect("ReadOnlyMemory::read");
    buf
}

/// The fixture builder produces parseable bytes, and the reader reflects the
/// single `.text` section at the chosen address, raw and unswapped.
#[test]
fn simple_text_elf_fixture_round_trips_through_elf_reader() {
    let elf = simple_text_elf(0x1000, &[0xaa, 0xbb, 0xcc, 0xdd]);
    let r = ElfFileMemReader::from_bytes(&elf).expect("parse synthetic ELF");

    assert_eq!(read_raw(&r, 0x1000, 4), &[0xaa, 0xbb, 0xcc, 0xdd]);
}

/// Bytes come back verbatim regardless of endianness.
#[test]
fn ro_read_fills_raw_bytes() {
    let elf = simple_text_elf(0x1000, &[0x01, 0x02, 0x03, 0x04]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(read_raw(&r, 0x1000, 4), &[0x01, 0x02, 0x03, 0x04]);
    assert_eq!(read_raw(&r, 0x1001, 2), &[0x02, 0x03]);
    let mut empty: [u8; 0] = [];
    ReadOnlyMemory::read(&r, 0x1000, &mut empty).unwrap();
}

/// A region supplying only a prefix must error, not truncate.
#[test]
fn ro_read_partial_region_errors() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    // 4 bytes starting 2 before the end, so only 2 are available.
    assert_readonly_errors(&r, 0x1002, 4);
}

#[test]
fn ro_read_unmapped_address_errors() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_readonly_errors(&r, 0x9000, 4);
}

#[test]
fn ro_read_spanning_past_mapped_errors() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_readonly_errors(&r, 0x1000, 8);
}

/// The two traits treat short reads differently by design: `MemReader` returns
/// `Ok(n)` with `n < buf.len()`, `ReadOnlyMemory` errors.
///
/// `ReadOnlyMemory` backs `LoadReadOnly`, which must never synthesize a
/// constant from partial bytes. `MemReader` backs Sleigh instruction fetch,
/// where a short read at a section's end is expected.
#[test]
fn elf_reader_partial_read_asymmetry_between_traits() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();

    let mut buf = [0u8; 8];
    let n = MemReader::read(
        &r,
        VnAddr {
            off: 0x1000,
            space: VnSpace::RAM,
        },
        &mut buf,
    )
    .expect("MemReader read");
    assert_eq!(n, 4, "MemReader permits partial reads");
    assert_eq!(&buf[..4], &[1, 2, 3, 4]);

    assert_readonly_errors(&r, 0x1000, 8);
}

#[test]
fn elf_reader_satisfies_mem_reader_contract() {
    let elf = simple_text_elf(0x1000, &[0x11, 0x22, 0x33, 0x44]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();

    assert_mem_reader_reads(&r, 0x1000, &[0x11, 0x22, 0x33, 0x44]);
    assert_mem_reader_unmapped_is_not_mapped_error(&r, 0x9000);
    // Partial: ask 6, get 4.
    assert_mem_reader_partial_read_ok(&r, 0x1000, 6, 4);
}

#[test]
fn elf_reader_satisfies_read_only_memory_contract() {
    let elf = simple_text_elf(0x1000, &[0x11, 0x22, 0x33, 0x44]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();

    assert_readonly_reads(&r, 0x1000, &[0x11, 0x22, 0x33, 0x44]);
    assert_readonly_errors(&r, 0x9000, 4);
}

#[test]
fn elf_reader_from_object_matches_from_bytes() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let from_bytes = ElfFileMemReader::from_bytes(&elf).unwrap();
    let parsed = object::File::parse(&elf[..]).unwrap();
    let from_obj = ElfFileMemReader::from_object(&parsed).unwrap();

    for addr in [0x1000u64, 0x1001, 0x1002, 0x1003] {
        assert_eq!(
            read_raw(&from_bytes, addr, 1),
            read_raw(&from_obj, addr, 1),
            "read mismatch at {addr:#x}",
        );
    }
}

#[test]
fn elf_reader_from_path_reads_temp_elf() {
    let elf = simple_text_elf(0x1000, &[0xde, 0xad, 0xbe, 0xef]);
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&elf).unwrap();
    f.flush().unwrap();

    let r = ElfFileMemReader::from_path(f.path()).unwrap();
    assert_eq!(read_raw(&r, 0x1000, 4), &[0xde, 0xad, 0xbe, 0xef]);
}
