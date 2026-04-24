#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for `MemRegion`, `MemRegionsLookupTable`, and
//! `RegionsMemReader` — the backend-agnostic layer of the reader crate.

use reader::{MemRegion, MemRegionsLookupTable, RegionsMemReader};

// ── helpers ───────────────────────────────────────────────────────────────

/// Builds a `MemRegion` at `start` with `len` bytes, each equal to its
/// offset within the region (i.e. `data[i] == i as u8 & 0xff`).
fn make_region(start: u64, len: usize) -> MemRegion {
    let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
    MemRegion::new(start, data)
}

// ── MemRegion::end_addr ───────────────────────────────────────────────────

#[test]
fn mem_region_end_addr() {
    let r = make_region(0x1000, 16);
    assert_eq!(r.end_addr(), 0x1010);
}

#[test]
fn mem_region_end_addr_empty() {
    let r = MemRegion::new(0x2000, vec![]);
    assert_eq!(r.end_addr(), 0x2000);
}

// ── MemRegion::contains ───────────────────────────────────────────────────

#[test]
fn mem_region_contains_start() {
    let r = make_region(0x1000, 16);
    assert!(r.contains(0x1000));
}

#[test]
fn mem_region_contains_last_byte() {
    let r = make_region(0x1000, 16);
    assert!(r.contains(0x100f));
}

#[test]
fn mem_region_does_not_contain_end_addr() {
    let r = make_region(0x1000, 16);
    assert!(!r.contains(0x1010));
}

#[test]
fn mem_region_does_not_contain_before_start() {
    let r = make_region(0x1000, 16);
    assert!(!r.contains(0x0fff));
}

#[test]
fn mem_region_empty_contains_nothing() {
    let r = MemRegion::new(0x1000, vec![]);
    assert!(!r.contains(0x1000));
}

// ── MemRegion::read ───────────────────────────────────────────────────────

#[test]
fn mem_region_read_full_at_start() {
    let r = make_region(0x1000, 4);
    let mut buf = [0u8; 4];
    assert_eq!(r.read(0x1000, &mut buf), Some(4));
    assert_eq!(buf, [0, 1, 2, 3]);
}

#[test]
fn mem_region_read_mid_region() {
    let r = make_region(0x1000, 8);
    let mut buf = [0u8; 3];
    assert_eq!(r.read(0x1002, &mut buf), Some(3));
    assert_eq!(buf, [2, 3, 4]);
}

#[test]
fn mem_region_read_partial_past_end() {
    let r = make_region(0x1000, 4);
    let mut buf = [0xffu8; 8];
    assert_eq!(r.read(0x1002, &mut buf), Some(2));
    assert_eq!(buf[0], 2);
    assert_eq!(buf[1], 3);
    assert_eq!(buf[2], 0xff);
}

#[test]
fn mem_region_read_zero_length_buf() {
    let r = make_region(0x1000, 4);
    let mut buf: [u8; 0] = [];
    assert_eq!(r.read(0x1000, &mut buf), Some(0));
}

#[test]
fn mem_region_read_outside_returns_none() {
    let r = make_region(0x1000, 4);
    let mut buf = [0u8; 4];
    assert_eq!(r.read(0x2000, &mut buf), None);
}

#[test]
fn mem_region_read_at_end_addr_returns_none() {
    let r = make_region(0x1000, 4);
    let mut buf = [0u8; 1];
    assert_eq!(r.read(0x1004, &mut buf), None);
}

// ── MemRegionsLookupTable ─────────────────────────────────────────────────

#[test]
fn lookup_table_finds_address_in_single_region() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 2];
    assert_eq!(table.read(0x1000, &mut buf), Some(2));
    assert_eq!(buf, [0, 1]);
}

#[test]
fn lookup_table_miss_before_all_regions() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x0fff, &mut buf), None);
}

#[test]
fn lookup_table_miss_after_all_regions() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1010, &mut buf), None);
}

#[test]
fn lookup_table_two_regions_correct_dispatch() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16), make_region(0x2000, 16)]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1005, &mut buf), Some(1));
    assert_eq!(buf[0], 5);
    assert_eq!(table.read(0x2007, &mut buf), Some(1));
    assert_eq!(buf[0], 7);
}

#[test]
fn lookup_table_same_start_last_wins() {
    let mut r1 = make_region(0x1000, 4);
    r1.data = vec![0xaa, 0xaa, 0xaa, 0xaa];
    let mut r2 = make_region(0x1000, 4);
    r2.data = vec![0xbb, 0xbb, 0xbb, 0xbb];
    let table = MemRegionsLookupTable::new([r1, r2]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "last region with same start must win");
}

#[test]
fn lookup_table_empty_returns_none() {
    let table = MemRegionsLookupTable::new([]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1000, &mut buf), None);
}

#[test]
fn lookup_table_gap_between_regions_is_none() {
    let table = MemRegionsLookupTable::new([
        make_region(0x1000, 8),
        make_region(0x1010, 8),
    ]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1008, &mut buf), None);
    assert_eq!(table.read(0x100f, &mut buf), None);
}

// ── Pinned contract #1: cross-region boundary partial read ────────────────

/// Pinned contract: reads that span two adjacent regions return only the
/// first region's bytes. The lookup table does NOT continue into the next
/// region. A caller asking for 16 bytes at 0x1008 when regions cover
/// [0x1000..0x1010) and [0x1010..0x1020) gets Some(8), not Some(16).
///
/// If this test ever starts failing, someone has changed
/// `MemRegionsLookupTable::read` to stitch reads across region boundaries.
/// That is a meaningful semantic change — audit every caller of `.read()`
/// before updating.
#[test]
fn lookup_table_cross_boundary_read_stops_at_first_region_end() {
    let table = MemRegionsLookupTable::new([
        make_region(0x1000, 16), // bytes 0..16 at 0x1000..0x1010
        make_region(0x1010, 16), // bytes 0..16 at 0x1010..0x1020
    ]);
    let mut buf = [0xffu8; 16];
    assert_eq!(table.read(0x1008, &mut buf), Some(8));
    // First 8 bytes are from the first region's tail (bytes 8..16).
    let expected: Vec<u8> = (8..16).collect();
    assert_eq!(&buf[..8], &expected[..]);
    assert_eq!(buf[8], 0xff, "second region must not be consulted");
}

// ── Pinned contract #2: overlapping regions, later-start-wins ─────────────

/// Pinned contract: when two regions overlap but have different start
/// addresses, the region whose start_addr is the latest <= addr wins.
/// The earlier region's bytes in the overlap are shadowed.
///
/// This falls out of the BTreeMap range query but the BEHAVIOR matters to
/// callers; future backends that register overlapping regions must know.
#[test]
fn lookup_table_overlapping_regions_later_start_shadows_earlier() {
    let a = MemRegion::new(0x1000, vec![0xaa; 0x20]); // [0x1000..0x1020)
    let b = MemRegion::new(0x1010, vec![0xbb; 0x20]); // [0x1010..0x1030)
    let table = MemRegionsLookupTable::new([a, b]);
    let mut buf = [0u8; 1];

    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xaa, "pre-overlap resolves to A");

    assert_eq!(table.read(0x1010, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "overlap resolves to B (later start wins)");

    assert_eq!(table.read(0x101f, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "A's tail in overlap is shadowed");
}

// ── RegionsMemReader ──────────────────────────────────────────────────────

#[test]
fn regions_mem_reader_delegates_read() {
    let mut r = make_region(0x4000, 8);
    r.data = vec![10, 20, 30, 40, 50, 60, 70, 80];
    let table = MemRegionsLookupTable::new([r]);
    let reader = RegionsMemReader::new(table);
    let mut buf = [0u8; 3];
    assert_eq!(reader.read(0x4002, &mut buf), Some(3));
    assert_eq!(buf, [30, 40, 50]);
}

#[test]
fn regions_mem_reader_miss_returns_none() {
    let table = MemRegionsLookupTable::new([make_region(0x4000, 8)]);
    let reader = RegionsMemReader::new(table);
    let mut buf = [0u8; 1];
    assert_eq!(reader.read(0x9000, &mut buf), None);
}
