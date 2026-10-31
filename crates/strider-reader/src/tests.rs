use super::{DESCENTS, MemRegion, MemRegionsLookupTable, RegionIndex};

/// The fallback a read takes when no region fully covers it, and the one
/// method here with no caller outside the crate.
#[test]
fn widest_at_picks_the_region_serving_the_most() {
    let regions = [
        MemRegion::new(0x1000, vec![0u8; 0x40]).unwrap(),
        MemRegion::new(0x1010, vec![0u8; 0x10]).unwrap(),
    ];
    let index = RegionIndex::new(&regions);
    assert_eq!(
        index.widest_at(0x1010),
        Some(0),
        "the higher start serves fewer bytes from here"
    );
    assert_eq!(index.widest_at(0x1030), Some(0));
    assert_eq!(index.widest_at(0x0fff), None);
    assert_eq!(index.widest_at(0x1040), None);
}

/// One region spanning everything, plus `n` eight-byte regions tiling it.
/// The outer region starts BELOW the first inner one: two regions sharing
/// a start collapse to the later, which would drop the span.
fn nested(n: u64) -> MemRegionsLookupTable {
    let mut regions = vec![MemRegion::new(0xFF8, vec![0x90; (n * 8 + 8) as usize]).unwrap()];
    for i in 0..n {
        regions.push(MemRegion::new(0x1000 + i * 8, vec![0x90; 8]).unwrap());
    }
    MemRegionsLookupTable::new(regions)
}

/// Tree nodes one 16-byte `read` at `addr` visits; `served` is the byte
/// count it must return. At least one: a read that never reaches the tree
/// is the walk this replaced. A walk that also calls the tree stays unseen.
fn descents_of_a_read(table: &MemRegionsLookupTable, addr: u64, served: usize) -> u64 {
    let mut buf = [0u8; 16];
    DESCENTS.with(|n| n.set(0));
    assert_eq!(table.read(addr, &mut buf), Some(served));
    let visits = DESCENTS.with(std::cell::Cell::get);
    assert!(visits >= 1, "the read bypassed the max-end tree");
    visits
}

/// 64,001 regions make a 17-level tree. A read walks a few root-to-leaf
/// paths, well under 16 nodes a level; the walk down every lower start
/// this replaced visited them all.
const DESCENT_BUDGET: u64 = 16 * 17;

/// Resolution used to walk candidates from the highest `start <= addr`
/// downward until no earlier region could reach the address, a bound one
/// image-spanning region held above every interior address. Near the top
/// of the table, where the outer region still covers the request: every
/// instruction fetch. Section count is attacker-chosen (`SHN_XINDEX` lifts
/// the 65535 cap), so that walk made a crafted image's every read linear.
#[test]
fn a_read_under_many_nested_regions_descends_one_path() {
    let table = nested(64_000);
    let visits = descents_of_a_read(&table, 0x1000 + 63_900 * 8, 16);
    assert!(
        visits <= DESCENT_BUDGET,
        "a covering read visited {visits} tree nodes over 64001 regions"
    );
}

/// The outer region's last eight bytes: every region starting at or below
/// reaches past them and none holds sixteen, so the covering descent
/// misses and the widest-serving fallback answers. The fallback is
/// counted on its own as well, since the miss alone meets the read's floor.
#[test]
fn a_partial_read_under_many_nested_regions_descends_one_path() {
    let table = nested(64_000);
    let addr = 0xFF8 + 64_000 * 8;
    let visits = descents_of_a_read(&table, addr, 8);
    assert!(
        visits <= DESCENT_BUDGET,
        "a partial read visited {visits} tree nodes over 64001 regions"
    );
    DESCENTS.with(|n| n.set(0));
    assert!(table.index.widest_at(addr).is_some());
    assert!(
        DESCENTS.with(std::cell::Cell::get) >= 1,
        "the widest-serving fallback bypassed the max-end tree"
    );
}

/// A read past every region's length is served by one region, so the buffer
/// is sized by the largest and filled from the one covering `addr`.
#[test]
fn read_vec_sizes_its_buffer_by_the_largest_region() {
    let table = MemRegionsLookupTable::new([
        MemRegion::new(0x1000, vec![1u8; 0x10]).unwrap(),
        MemRegion::new(0x2000, vec![2u8; 0x40]).unwrap(),
    ]);
    assert_eq!(table.read_vec(0x1008, usize::MAX), Some(vec![1u8; 8]));
    assert_eq!(table.read_vec(0x2000, usize::MAX), Some(vec![2u8; 0x40]));
    assert_eq!(table.read_vec(0x2000, 3), Some(vec![2u8; 3]));
    assert_eq!(table.read_vec(0x3000, usize::MAX), None);
}
