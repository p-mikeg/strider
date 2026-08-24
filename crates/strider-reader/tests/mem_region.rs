#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_reader::{MemRegion, MemRegionsLookupTable};

/// Byte `i` of the region holds `i as u8`, so a read's contents identify the
/// offset it came from.
fn make_region(start: u64, len: usize) -> MemRegion {
    let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
    MemRegion::new(start, data).expect("test region fits in u64")
}

#[test]
fn mem_region_end_addr() {
    let r = make_region(0x1000, 16);
    assert_eq!(r.end_addr(), 0x1010);
}

#[test]
fn mem_region_end_addr_empty() {
    let r = MemRegion::new(0x2000, vec![]).expect("valid region");
    assert_eq!(r.end_addr(), 0x2000);
}

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
    let r = MemRegion::new(0x1000, vec![]).expect("valid region");
    assert!(!r.contains(0x1000));
}

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
    let r1 = MemRegion::new(0x1000, vec![0xaa; 4]).expect("valid region");
    let r2 = MemRegion::new(0x1000, vec![0xbb; 4]).expect("valid region");
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
    let table = MemRegionsLookupTable::new([make_region(0x1000, 8), make_region(0x1010, 8)]);
    let mut buf = [0u8; 1];
    assert_eq!(table.read(0x1008, &mut buf), None);
    assert_eq!(table.read(0x100f, &mut buf), None);
}

/// Pinned: a read spanning two adjacent regions returns only the first
/// region's bytes, never stitching into the next.
///
/// A failure here means someone made `read` stitch across boundaries. That is a
/// semantic change; audit every `.read()` caller before updating this.
#[test]
fn lookup_table_cross_boundary_read_stops_at_first_region_end() {
    let table = MemRegionsLookupTable::new([
        make_region(0x1000, 16), // bytes 0..16 at 0x1000..0x1010
        make_region(0x1010, 16), // bytes 0..16 at 0x1010..0x1020
    ]);
    let mut buf = [0xffu8; 16];
    assert_eq!(table.read(0x1008, &mut buf), Some(8));
    // The first region's tail, bytes 8..16.
    let expected: Vec<u8> = (8..16).collect();
    assert_eq!(&buf[..8], &expected[..]);
    assert_eq!(buf[8], 0xff, "second region must not be consulted");
}

/// Pinned: among overlapping regions at distinct starts, the latest
/// `start_addr <= addr` wins and shadows the earlier one's bytes. Falls out of
/// the BTreeMap range query, but future backends registering overlapping
/// regions depend on the behaviour.
#[test]
fn lookup_table_overlapping_regions_later_start_shadows_earlier() {
    let a = MemRegion::new(0x1000, vec![0xaa; 0x20]).expect("valid region"); // [0x1000..0x1020)
    let b = MemRegion::new(0x1010, vec![0xbb; 0x20]).expect("valid region"); // [0x1010..0x1030)
    let table = MemRegionsLookupTable::new([a, b]);
    let mut buf = [0u8; 1];

    assert_eq!(table.read(0x1000, &mut buf), Some(1));
    assert_eq!(buf[0], 0xaa, "pre-overlap resolves to A");

    assert_eq!(table.read(0x1010, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "overlap resolves to B (later start wins)");

    assert_eq!(table.read(0x101f, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb, "A's tail in overlap is shadowed");
}

/// Pinned: a shorter later-starting region that doesn't cover `addr` must fall
/// through to an earlier one that does, else overlapping regions lose data.
#[test]
fn lookup_table_shorter_inner_region_does_not_shadow_outer_tail() {
    // Outer A: [0x1000..0x1020) of 0xaa; inner B: [0x1010..0x1014) of 0xbb.
    let a = MemRegion::new(0x1000, vec![0xaa; 0x20]).expect("valid region");
    let b = MemRegion::new(0x1010, vec![0xbb; 0x04]).expect("valid region");
    let table = MemRegionsLookupTable::new([a, b]);
    let mut buf = [0u8; 1];

    // 0x1018 is in A's tail but past B's end.
    assert_eq!(table.read(0x1018, &mut buf), Some(1));
    assert_eq!(
        buf[0], 0xaa,
        "should fall through to A when B does not cover addr"
    );

    // Inside B's range, later-start-wins still applies.
    assert_eq!(table.read(0x1011, &mut buf), Some(1));
    assert_eq!(buf[0], 0xbb);
}

/// Pinned: a read starting inside a shorter inner region but running past its
/// end is satisfied by the fully-covering outer region, not truncated to the
/// inner region's tail.
///
/// Returning on the first candidate producing any `Some` lets the inner
/// region's short read shadow the outer one, and since `ReadOnlyMemory` rejects
/// partial reads, a fully-mapped address then reports as unmapped.
#[test]
fn lookup_table_multibyte_read_straddling_inner_end_uses_outer() {
    // Outer A: [0x1000..0x1100), byte i == i & 0xff.
    // Inner B: [0x1080..0x1090) of 0xbb, fully inside A.
    let a = make_region(0x1000, 0x100);
    let b = MemRegion::new(0x1080, vec![0xbb; 0x10]).expect("valid region");
    let table = MemRegionsLookupTable::new([a, b]);

    // Only 4 bytes remain inside B at 0x108C, but A covers all 8.
    let mut buf = [0u8; 8];
    assert_eq!(
        table.read(0x108C, &mut buf),
        Some(8),
        "outer region must satisfy the full request when inner can't"
    );
    // All from A (offset 0x8C..0x94), not an A/B mix.
    let expected: Vec<u8> = (0x8C..0x94).map(|i| i as u8).collect();
    assert_eq!(&buf[..], &expected[..]);

    let mut buf2 = [0u8; 4];
    assert_eq!(table.read(0x1082, &mut buf2), Some(4));
    assert_eq!(
        &buf2, &[0xbb; 4],
        "inner still wins when it covers the whole read"
    );
}

/// The error carries the offending start and length for diagnostics.
#[test]
fn mem_region_new_rejects_overflow() {
    let start = u64::MAX - 3;
    // len 4 puts the end one past u64::MAX.
    let err = MemRegion::new(start, vec![0u8; 4]).expect_err("overflowing region must be rejected");
    let msg = err.to_string();
    let expected_addr = format!("{start:#x}");
    assert!(
        msg.contains("would overflow u64")
            && msg.contains(&expected_addr)
            && msg.contains("length 4"),
        "got: {err}",
    );
}

/// An exact fit at the top of the address space is legal: `end_addr` lands on
/// `u64::MAX`, which is still representable.
#[test]
fn mem_region_new_accepts_exact_fit_at_top_of_address_space() {
    let start = u64::MAX - 3;
    let r = MemRegion::new(start, vec![1u8, 2, 3]).expect("exact-fit region is legal");
    assert_eq!(r.end_addr(), u64::MAX);
    assert!(r.contains(start));
    assert!(r.contains(u64::MAX - 1));
    assert!(!r.contains(u64::MAX), "end_addr is exclusive");
}

#[test]
fn mem_region_accessors_expose_start_and_data() {
    let r = MemRegion::new(0x1234, vec![0xaa, 0xbb, 0xcc]).expect("valid region");
    assert_eq!(r.start_addr(), 0x1234);
    let mut got = [0u8; 3];
    assert_eq!(r.read(0x1234, &mut got), Some(3));
    assert_eq!(got, [0xaa, 0xbb, 0xcc]);
}

/// Pinned: a zero-length read still discriminates mapped from unmapped. Stops a
/// future "early return if out.is_empty()" from short-circuiting the unmapped
/// arm.
#[test]
fn lookup_table_read_zero_length_buf() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut empty: [u8; 0] = [];

    assert_eq!(table.read(0x1000, &mut empty), Some(0));
    assert_eq!(table.read(0x1008, &mut empty), Some(0));

    assert_eq!(table.read(0x0fff, &mut empty), None);
    assert_eq!(table.read(0x2000, &mut empty), None);
}

// `read_exact` backs every region-backed `ReadOnlyMemory::read` impl (ELF
// reader, Python buffer), so its boundary behaviour is pinned on the table.

/// A read whose last byte is the region's last byte succeeds; the exclusive
/// `end_addr` itself is never touched.
#[test]
fn read_exact_ending_exactly_at_region_end_is_ok() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 4];
    table
        .read_exact(0x100c, &mut buf)
        .expect("read ending exactly at end_addr must succeed");
    assert_eq!(buf, [12, 13, 14, 15]);

    let mut one = [0u8; 1];
    table
        .read_exact(0x100f, &mut one)
        .expect("last byte is mapped");
    assert_eq!(one[0], 15);
}

/// A read past the region's end errors rather than partially filling, even
/// though `read` would have returned a prefix.
#[test]
fn read_exact_spanning_past_region_end_errors() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 4];
    let err = table
        .read_exact(0x100e, &mut buf)
        .expect_err("2-of-4 available must be an error, not a short fill");
    assert!(
        err.to_string().contains("spans past mapped memory"),
        "got: {err}",
    );
}

/// One past the region's end (== `end_addr`) is unmapped.
#[test]
fn read_exact_one_past_region_end_errors_not_mapped() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut buf = [0u8; 1];
    let err = table
        .read_exact(0x1010, &mut buf)
        .expect_err("end_addr is exclusive");
    assert!(err.to_string().contains("not mapped"), "got: {err}");
}

/// Zero of zero bytes counts as a full fill, so a zero-length `read_exact`
/// succeeds when mapped and still errors when not.
#[test]
fn read_exact_zero_length_mapped_ok_unmapped_errors() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut empty: [u8; 0] = [];
    table
        .read_exact(0x1000, &mut empty)
        .expect("zero-length at a mapped address is a (trivial) full fill");
    let err = table
        .read_exact(0x2000, &mut empty)
        .expect_err("zero-length at an unmapped address must still error");
    assert!(err.to_string().contains("not mapped"), "got: {err}");
}

/// Pinned: `read_exact` does not stitch across contiguous regions. `read` stops
/// at the first region's end, so a seam-spanning request is a short fill and
/// therefore an error, even though every requested byte is mapped by *some*
/// region. Making this succeed is the stitching change flagged above.
#[test]
fn read_exact_across_two_adjacent_regions_errors() {
    let table = MemRegionsLookupTable::new([
        make_region(0x1000, 16), // [0x1000..0x1010)
        make_region(0x1010, 16), // [0x1010..0x1020)
    ]);
    let mut buf = [0u8; 16];
    let err = table
        .read_exact(0x1008, &mut buf)
        .expect_err("seam-spanning read must not be stitched");
    assert!(
        err.to_string().contains("spans past mapped memory"),
        "got: {err}",
    );

    // The same-size read fully inside either region succeeds.
    table
        .read_exact(0x1000, &mut buf)
        .expect("fully inside first region");
    table
        .read_exact(0x1010, &mut buf)
        .expect("fully inside second region");
}

/// The `[start, end)` exclusion of `end_addr` holds even for a zero-byte
/// request, so a zero-length `read_exact` there must error. The sibling test
/// covers zero-length *inside* a region.
#[test]
fn mem_region_read_exact_zero_length_at_exact_end_addr() {
    let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
    let mut empty: [u8; 0] = [];
    let err = table
        .read_exact(0x1010, &mut empty)
        .expect_err("zero-length read at exactly end_addr must error (end is exclusive)");
    assert!(err.to_string().contains("not mapped"), "got: {err}");

    // The last in-range address still maps for a zero-length read.
    table
        .read_exact(0x100f, &mut empty)
        .expect("zero-length at the last in-range address is a (trivial) full fill");
}

/// Locks the all-or-most rule for two overlapping regions at distinct starts
/// that disagree on the overlap bytes.
///
/// Outer A is `[0x1000, 0x1020)` of `0xaa`, inner B is `[0x1010, 0x1018)` of
/// `0xbb`. A 16-byte read at `0x1014` gets 4 bytes from B and 12 from A;
/// neither fully covers, so covers-the-most picks A and the read resolves
/// entirely to A's bytes, not B's truncated prefix and not an A/B mix.
#[test]
fn lookup_table_overlapping_regions_differing_bytes_partial_read_is_specified() {
    let a = MemRegion::new(0x1000, vec![0xaa; 0x20]).expect("valid region");
    let b = MemRegion::new(0x1010, vec![0xbb; 0x08]).expect("valid region");
    let table = MemRegionsLookupTable::new([a, b]);

    // 16-byte read straddling B's end at 0x1018.
    let mut buf = [0u8; 16];
    assert_eq!(
        table.read(0x1014, &mut buf),
        Some(12),
        "winner is the region covering the most (A: 12 bytes), not B's 4-byte prefix",
    );
    assert_eq!(
        &buf[..12],
        &[0xaa; 12],
        "the 12 returned bytes must all come from A (no A/B mix, no B prefix)",
    );
    assert_eq!(buf[12], 0, "untouched tail of the buffer");

    // Inside B's range and fully covered by B, B still wins.
    let mut small = [0u8; 4];
    assert_eq!(table.read(0x1014, &mut small), Some(4));
    assert_eq!(
        &small, &[0xbb; 4],
        "B wins when it fully covers the request"
    );
}

#[test]
fn same_bytes_in_compares_only_the_overlap() {
    let a = MemRegion::new(0x100, vec![0xaa; 8]).unwrap();
    let b = MemRegion::new(0x104, vec![0xaa; 8]).unwrap();
    assert!(a.same_bytes_in(&b, 0x104, 0x108));

    let c = MemRegion::new(0x104, vec![0xaa, 0xaa, 0xbb, 0xaa]).unwrap();
    assert!(a.same_bytes_in(&c, 0x104, 0x106), "the equal prefix");
    assert!(!a.same_bytes_in(&c, 0x104, 0x108), "the differing byte");
    assert!(
        !a.same_bytes_in(&c, 0x104, 0x10a),
        "a range neither fully covers differs"
    );
}
