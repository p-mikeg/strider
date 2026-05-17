//! LoadPat::mem_in(p) constrains inputs[0] (memory predecessor).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::test_utils::SENTINEL_LIFT_ADDR;
use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, int_const, load, store};

#[test]
fn load_mem_in_matches_preceding_store() {
    // Build: Store(addr=0x100, data=42) ; v = Load(addr=0x200) ; ret v
    // The Load's value is consumed by Return so the Load is reachable
    // from the entry; its inputs[0] (memory) is the Store's mem_out.
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr1");
    let val = b
        .build_int_const(42u64, NodeOutputType::U32)
        .expect("val");
    b.build_store(addr1, val, rsleigh::VnSpace::RAM)
        .expect("store");
    let addr2 = b
        .build_int_const(0x200u64, NodeOutputType::U64)
        .expect("addr2");
    let load_val = b
        .build_load(addr2, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_return(Some(load_val), &[]).expect("ret");
    b.set_lift_addr(None);
    let fg = b.build().expect("build");

    let pat = load()
        .addr(int_const(0x200u64))
        .mem_in(store().addr(int_const(0x100u64)));
    let hits = Matcher::new(&fg).find_all(&pat.into());
    assert_eq!(
        hits.len(),
        1,
        "load whose mem_in is the prior store should match exactly once"
    );
}
