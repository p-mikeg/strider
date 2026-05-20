//! LoadPat::bit_width(n) filters Loads by value width.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::test_utils::SENTINEL_LIFT_ADDR;
use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use pattern::{Matcher, int_const, load};

#[test]
fn bit_width_filters_load_by_value_width() {
    // Two Loads at the same address, U32 and U64.  Both must be
    // reachable (their values are aggregated as Return values), so
    // build_return takes one as primary and we feed the other through
    // a tracked variable.
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr");
    let l32 = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("u32 load");
    let l64 = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
        .expect("u64 load");
    // Make both loads reachable: store l64 at a different address
    // (so it's consumed by a Store on the mem chain), and return l32.
    let other_addr = b
        .build_int_const(0x200u64, NodeOutputType::U64)
        .expect("other_addr");
    b.build_store(other_addr, l64, rsleigh::VnSpace::RAM)
        .expect("store l64");
    b.build_return(Some(l32), &[]).expect("ret");
    b.set_lift_addr(None);
    let fg = b.build().expect("build");

    let m = Matcher::new(&fg);
    let h32 = m.find_all(&load().addr(int_const(0x100u64)).bit_width(32).into());
    let h64 = m.find_all(&load().addr(int_const(0x100u64)).bit_width(64).into());
    assert_eq!(h32.len(), 1, "bit_width(32) matches only the U32 load");
    assert_eq!(h64.len(), 1, "bit_width(64) matches only the U64 load");
}
