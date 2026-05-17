//! CallOtherPat::next_mem(p) and .next_ctrl(p) — forward walk to the
//! unique consumer of the matched CallOther's mem / ctrl output.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::test_utils::SENTINEL_LIFT_ADDR;
use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, call_other, int_const, store};

#[test]
fn callother_next_mem_matches_following_store() {
    // Build: LOCK ; Store(addr=0x100, data=42).  LOCK is classified as
    // PURE_WITH_MEM_EDGE in target::call_other_abi, so it sits on the
    // memory chain and its mem_out's unique consumer is the Store.
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let (lock, _lock_val, _lock_clobbers) = b
        .build_call_other_modeled(1, "LOCK", &[], None, &[], &[], &[])
        .expect("LOCK ok");
    // CallOther outputs = [ctrl(0), mem(1), ...].  Advance the region's
    // memory cursor to LOCK's mem_out so the next Store's mem_in is
    // LOCK's mem_out.
    let lock_mem_out = b.body().graph.node_outputs(lock)[1];
    b.advance_cur_region_memory(lock_mem_out)
        .expect("advance mem to LOCK mem_out");
    let addr = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr");
    let val = b
        .build_int_const(42u64, NodeOutputType::U32)
        .expect("val");
    b.build_store(addr, val, rsleigh::VnSpace::RAM)
        .expect("store");
    b.build_return(None, &[]).expect("ret");
    b.set_lift_addr(None);
    let fg = b.build().expect("build");

    let pat = call_other()
        .name("LOCK")
        .next_mem(store().addr(int_const(0x100u64)));
    let hits = Matcher::new(&fg).find_all(&pat.into());
    assert_eq!(
        hits.len(),
        1,
        "LOCK whose mem_out's unique consumer is the Store should match"
    );
}
