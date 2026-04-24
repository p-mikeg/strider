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

use object::Endianness;

use common::elf_fixture::simple_text_elf_with_endian;

// ── ReadOnlyMemory: space filter ──────────────────────────────────────────

/// Only `VnSpace::RAM` produces a hit; other spaces always return `None`.
#[test]
fn ro_read_non_ram_space_returns_none() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::REGISTER, 0x1000, 4), None);
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::UNIQUE, 0x1000, 4), None);
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::CONST, 0x1000, 4), None);
}

// ── ReadOnlyMemory: size bounds ───────────────────────────────────────────

/// `size == 0` is not a legitimate load; the trait returns `None`.
#[test]
fn ro_read_size_zero_returns_none() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 0), None);
}

/// `size > 8` exceeds what a `u64` can carry; the trait returns `None`.
#[test]
fn ro_read_size_greater_than_eight_returns_none() {
    let elf = simple_text_elf(0x1000, &[0u8; 16]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 9), None);
}

// ── ReadOnlyMemory: partial read ──────────────────────────────────────────

/// When the region can only supply a prefix of the requested bytes,
/// return `None` instead of truncated data.
#[test]
fn ro_read_partial_region_returns_none() {
    // region covers 0x1000..0x1004 (4 bytes)
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    // request 4 bytes starting 2 bytes before the end → only 2 available
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1002, 4), None);
}

/// An address outside any region returns `None`.
#[test]
fn ro_read_unmapped_address_returns_none() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x9000, 4), None);
}

// ── ReadOnlyMemory: endianness ────────────────────────────────────────────

/// 4 bytes `01 02 03 04` as little-endian u32 = 0x04030201.
#[test]
fn ro_read_little_endian_u32() {
    let elf = simple_text_elf_with_endian(
        0x1000, &[0x01, 0x02, 0x03, 0x04], Endianness::Little,
    );
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
        Some(0x04030201)
    );
}

/// 4 bytes `01 02 03 04` as big-endian u32 = 0x01020304.
#[test]
fn ro_read_big_endian_u32() {
    let elf = simple_text_elf_with_endian(
        0x1000, &[0x01, 0x02, 0x03, 0x04], Endianness::Big,
    );
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
        Some(0x01020304)
    );
}

/// 8-byte read picks up the full u64 with the correct endianness.
#[test]
fn ro_read_little_endian_u64() {
    let elf = simple_text_elf_with_endian(
        0x1000,
        &[0x78, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89],
        Endianness::Little,
    );
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 8),
        Some(0x89abcdef12345678)
    );
}

/// 1-byte reads do not depend on endianness.
#[test]
fn ro_read_single_byte() {
    let elf = simple_text_elf(0x1000, &[0xab]);
    let r = ElfFileMemReader::from_bytes(&elf).unwrap();
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 1),
        Some(0xab)
    );
}

use rsleigh::{MemReader, VnAddr, VnSpace};

/// Pinned contract: the two traits treat short reads differently.
///  * MemReader: partial read → Ok(n) with n < buf.len()
///  * ReadOnlyMemory: cannot satisfy full `size` → None (no truncation)
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
    let n = MemReader::read(&r, VnAddr { off: 0x1000, space: VnSpace::RAM }, &mut buf)
        .expect("MemReader read");
    assert_eq!(n, 4, "MemReader permits partial reads");
    assert_eq!(&buf[..4], &[1, 2, 3, 4]);

    // Same region, ReadOnlyMemory request for size=8 at start → None.
    assert_eq!(
        ReadOnlyMemory::read(&r, VnSpace::RAM, 0x1000, 8),
        None,
        "ReadOnlyMemory must not truncate",
    );
}

use common::elf_fixture::{SegmentSpec, build_elf_with_segments};
use object::File;

/// `from_elf_segments` picks up only the executable segment, not other
/// PT_LOADs. Addresses outside the executable segment's range are
/// unmapped.
#[test]
fn elf_reader_from_elf_segments_picks_exec_only() {
    let bytes = build_elf_with_segments(&[
        SegmentSpec { addr: 0x1000, data: vec![0xaa, 0xbb], exec: true },
        SegmentSpec { addr: 0x2000, data: vec![0xcc, 0xdd], exec: false },
    ]);
    let obj = File::parse(&bytes[..]).unwrap();
    let r = ElfFileMemReader::from_elf_segments(&obj).unwrap();

    // exec segment is reachable
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 2),
        Some(0xbbaa),
    );
    // non-exec segment is not reachable via from_elf_segments
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x2000, 2),
        None,
    );
}

use common::reader_contract::{
    assert_mem_reader_partial_read_ok, assert_mem_reader_reads,
    assert_mem_reader_unmapped_is_not_mapped_error, assert_readonly_reads,
    assert_readonly_rejects_bad_sizes, assert_readonly_rejects_non_ram_spaces,
    assert_readonly_returns_none,
};

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

    assert_readonly_reads(&r, rsleigh::VnSpace::RAM, 0x1000, 4, 0x44332211);
    assert_readonly_returns_none(&r, rsleigh::VnSpace::RAM, 0x9000, 4);
    assert_readonly_rejects_non_ram_spaces(&r, 0x1000);
    assert_readonly_rejects_bad_sizes(&r, 0x1000);
}

use std::io::Write as _;
use tempfile::NamedTempFile;

/// `from_object` on an already-parsed ELF yields a reader with the same
/// mapped data as `from_bytes` on the underlying bytes.
#[test]
fn elf_reader_from_object_matches_from_bytes() {
    let elf = simple_text_elf(0x1000, &[1, 2, 3, 4]);
    let from_bytes = ElfFileMemReader::from_bytes(&elf).unwrap();
    let parsed = object::File::parse(&elf[..]).unwrap();
    let from_obj = ElfFileMemReader::from_object(&parsed).unwrap();

    for addr in [0x1000u64, 0x1001, 0x1002, 0x1003] {
        let mut a = [0u8; 1];
        let mut b = [0u8; 1];
        let na = rsleigh::MemReader::read(&from_bytes, rsleigh::VnAddr { off: addr, space: rsleigh::VnSpace::RAM }, &mut a).unwrap();
        let nb = rsleigh::MemReader::read(&from_obj,   rsleigh::VnAddr { off: addr, space: rsleigh::VnSpace::RAM }, &mut b).unwrap();
        assert_eq!((na, a), (nb, b), "read mismatch at {addr:#x}");
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
    assert_eq!(
        ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
        Some(0xefbeadde),
    );
}
