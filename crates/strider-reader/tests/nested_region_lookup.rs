//! A read costs O(log regions), not O(regions below the address).
//!
//! `MemRegionsLookupTable` walks candidates from the highest `start <= addr`
//! downward and stops once no earlier region can still reach the address. That
//! bound is a PREFIX MAXIMUM, so one region spanning the image holds it above
//! every interior address and the walk never stops early. Section count is
//! attacker-chosen -- `SHN_XINDEX` lifts the 65535 header cap -- so a crafted
//! image made every read linear in it.
//!
//! A region fully covering the request wins outright, which is every
//! instruction fetch, and that case is now answered through a max-end tree.

use std::time::Instant;
use strider_reader::{MemRegion, MemRegionsLookupTable};

/// One region spanning everything, plus `n` eight-byte regions tiling it.
fn nested(n: u64) -> MemRegionsLookupTable {
    // The outer region starts BELOW the first inner one: two regions sharing a
    // start collapse to the later, which would drop the span this is about.
    let mut regions = vec![MemRegion::new(0xFF8, vec![0x90; (n * 8 + 8) as usize]).expect("outer")];
    for i in 0..n {
        regions.push(MemRegion::new(0x1000 + i * 8, vec![0x90; 8]).expect("inner"));
    }
    MemRegionsLookupTable::new(regions)
}

fn micros_per_read(table: &MemRegionsLookupTable, addr: u64) -> f64 {
    let mut buf = [0u8; 16];
    for _ in 0..50 {
        table.read(addr, &mut buf);
    }
    let iters = 500;
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(table.read(std::hint::black_box(addr), &mut buf));
    }
    t.elapsed().as_secs_f64() * 1e6 / f64::from(iters)
}

#[test]
fn a_read_under_many_nested_regions_does_not_scale_with_their_count() {
    // Near the TOP of each table, so the number of regions below the address
    // -- what the old walk visited -- scales with the region count.
    let small = nested(1_000);
    let large = nested(64_000);
    // Far enough from the top that the outer region still fully covers the
    // request: that is the case an instruction fetch makes, and the one the
    // max-end tree answers. Everything below it used to be walked first.
    let t_small = micros_per_read(&small, 0x1000 + 990 * 8);
    let t_large = micros_per_read(&large, 0x1000 + 63_900 * 8);

    // 64x the regions. Linear would be ~64x the time; log is ~1x. A loose
    // bound keeps this from flaking on a shared machine while still failing
    // outright on a return to the linear walk.
    assert!(
        t_large < t_small * 8.0 + 5.0,
        "64x the regions cost {t_large:.2}us/read against {t_small:.2}us: \
         the lookup is scaling with region count"
    );
}

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
