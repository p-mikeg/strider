//! StorePat::next_mem(p) walks forward to the unique mem consumer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, int_const, store};

#[test]
fn store_next_mem_matches_following_store() {
    // Build: Store_A(addr=0x100) → Store_B(addr=0x200) → Return.
    // Both Stores advance the memory chain; Store_A.mem_out has exactly
    // one consumer (Store_B's input[0]).  Loads do NOT advance memory
    // (they read but don't write the chain), so a Store followed by a
    // Load would leave the Store's mem_out with multiple consumers
    // (Load and Return) and `.next_mem` would deterministically fail —
    // see pattern_next_mem_zero_when_multi_consumer.rs.  This test
    // exercises the on-chain successor case.
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr1");
    let v1 = b
        .build_int_const(1u64, NodeOutputType::U32)
        .expect("v1");
    b.build_store(addr1, v1, rsleigh::VnSpace::RAM)
        .expect("store_a");
    let addr2 = b
        .build_int_const(0x200u64, NodeOutputType::U64)
        .expect("addr2");
    let v2 = b
        .build_int_const(2u64, NodeOutputType::U32)
        .expect("v2");
    b.build_store(addr2, v2, rsleigh::VnSpace::RAM)
        .expect("store_b");
    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    let pat = store()
        .addr(int_const(0x100u64))
        .next_mem(store().addr(int_const(0x200u64)));
    let hits = Matcher::new(&fg).find_all(&pat.into());
    assert_eq!(hits.len(), 1);
}
