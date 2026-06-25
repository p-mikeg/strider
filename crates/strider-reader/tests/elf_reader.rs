#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for `strider_reader::ElfFileMemReader` and its trait impls.

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

/// Reads `expected.len()` raw bytes at `addr` into a fresh buffer.
fn read_raw(r: &ElfFileMemReader, addr: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    ReadOnlyMemory::read(r, addr, &mut buf).expect("ReadOnlyMemory::read");
    buf
}

/// Sanity check: `simple_text_elf` produces bytes that
/// `ElfFileMemReader::from_bytes` can parse, and the resulting reader
/// reflects the single `.text` section at the chosen address.  The
/// reader returns RAW bytes — no endianness swap.
#[test]
fn simple_text_elf_fixture_round_trips_through_elf_reader() {
    let elf = simple_text_elf(0x1000, &[0xaa, 0xbb, 0xcc, 0xdd]);
    let r = ElfFileMemReader::from_bytes(&elf).expect("parse synthetic ELF");

    assert_eq!(read_raw(&r, 0x1000, 4), &[0xaa, 0xbb, 0xcc, 0xdd]);
}

// ── ReadOnlyMemory: raw bytes (no decode) ─────────────────────────────────

/// The reader fills the buffer with the exact raw mapped bytes for a
/// fully-mapped range — verbatim, regardless of endianness.
#[test]
fn ro_read_fills_raw_bytes() {
    let elf = simple_text_elf(0x1000, &[0x01, 0x02, 0x03, 0x04]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(read_raw(&r, 0x1000, 4), &[0x01, 0x02, 0x03, 0x04]);
    // A sub-range from the middle of the region copies the matching bytes.
    assert_eq!(read_raw(&r, 0x1001, 2), &[0x02, 0x03]);
    // A zero-length read into a mapped address trivially succeeds.
    let mut empty: [u8; 0] = [];
    ReadOnlyMemory::read(&r, 0x1000, &mut empty).unwrap();
}

// ── ReadOnlyMemory: all-or-nothing error contract ─────────────────────────

/// When the region can only supply a prefix of the requested bytes,
/// error instead of a truncated fill.
#[test]
fn ro_read_partial_region_errors() {
    // region covers 0x1000..0x1004 (4 bytes)
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    // request 4 bytes starting 2 bytes before the end → only 2 available
    assert_readonly_errors(&r, 0x1002, 4);
}

/// An address outside any region errors.
#[test]
fn ro_read_unmapped_address_errors() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_readonly_errors(&r, 0x9000, 4);
}

/// A read straddling past the end of the mapped region errors (no
/// partial fill).
#[test]
fn ro_read_spanning_past_mapped_errors() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    // 8 bytes from the start of a 4-byte region → past the end.
    assert_readonly_errors(&r, 0x1000, 8);
}

/// Pinned contract: the two traits treat short reads differently.
///  * MemReader: partial read → Ok(n) with n < buf.len()
///  * ReadOnlyMemory: cannot satisfy the whole buffer → Err (no truncation)
///
/// This documents a deliberate design choice: ReadOnlyMemory backs the
/// LoadReadOnly optimizer pass, which must never synthesize a constant
/// from partial bytes. MemReader backs Sleigh instruction fetch, where
/// a short read at the end of a section is an expected condition.
#[test]
fn elf_reader_partial_read_asymmetry_between_traits() {
    // 4-byte region; MemReader request for 8 bytes at start → Ok(4).
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

    // Same region, ReadOnlyMemory request for 8 bytes at start → Err.
    assert_readonly_errors(&r, 0x1000, 8);
}

/// Runs the backend-agnostic reader contract against an
/// `ElfFileMemReader` built from a synthetic single-section ELF.
#[test]
fn elf_reader_satisfies_mem_reader_contract() {
    let elf = simple_text_elf(0x1000, &[0x11, 0x22, 0x33, 0x44]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();

    // full read
    assert_mem_reader_reads(&r, 0x1000, &[0x11, 0x22, 0x33, 0x44]);
    // unmapped → NotMapped(addr)
    assert_mem_reader_unmapped_is_not_mapped_error(&r, 0x9000);
    // partial: ask 6, get 4
    assert_mem_reader_partial_read_ok(&r, 0x1000, 6, 4);
}

#[test]
fn elf_reader_satisfies_read_only_memory_contract() {
    let elf = simple_text_elf(0x1000, &[0x11, 0x22, 0x33, 0x44]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();

    assert_readonly_reads(&r, 0x1000, &[0x11, 0x22, 0x33, 0x44]);
    assert_readonly_errors(&r, 0x9000, 4);
}

/// `from_object` on an already-parsed ELF yields a reader with the same
/// mapped data as `from_bytes` on the underlying bytes.
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

/// `from_path` on a tempfile containing valid ELF bytes succeeds and the
/// resulting reader can read the mapped region.
#[test]
fn elf_reader_from_path_reads_temp_elf() {
    let elf = simple_text_elf(0x1000, &[0xde, 0xad, 0xbe, 0xef]);
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(&elf).unwrap();
    f.flush().unwrap();

    let r = ElfFileMemReader::from_path(f.path()).unwrap();
    assert_eq!(read_raw(&r, 0x1000, 4), &[0xde, 0xad, 0xbe, 0xef]);
}
