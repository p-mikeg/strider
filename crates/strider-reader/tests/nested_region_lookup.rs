//! Which region a read resolves to where regions nest; the descent count is
//! pinned by the lookup table's own unit tests.

use strider_reader::{MemRegion, MemRegionsLookupTable};

/// The fast path must not change which bytes come back.
#[test]
fn the_covering_region_still_wins_over_a_shorter_inner_one() {
    let outer = MemRegion::new(0x1000, vec![0xAA; 64]).expect("outer");
    let inner = MemRegion::new(0x1010, vec![0xBB; 8]).expect("inner");
    let table = MemRegionsLookupTable::new(vec![outer, inner]);

    // Fits inside the inner region: the higher start wins.
    let mut buf = [0u8; 8];
    assert_eq!(table.read(0x1010, &mut buf), Some(8));
    assert_eq!(buf, [0xBB; 8]);

    // Straddles the inner region's end, so it falls through to the outer one
    // rather than returning a truncated prefix.
    let mut buf = [0u8; 16];
    assert_eq!(table.read(0x1010, &mut buf), Some(16));
    assert_eq!(buf, [0xAA; 16]);

    // Unmapped stays unmapped.
    assert_eq!(table.read(0x900, &mut [0u8; 4]), None);
}
