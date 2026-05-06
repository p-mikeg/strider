//! Canonical use case for the new mem-walk pattern surface:
//! "find an atomic Store inside a LOCK ... UNLOCK pair".

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, call_other, int_const, store};

#[test]
fn matches_store_bracketed_by_lock_and_unlock() {
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);

    // LOCK (PURE_WITH_MEM_EDGE — produces a memory output we must
    // promote to cur_region_memory before the Store).
    let (lock, _, _) = b
        .build_call_other_modeled(1, "LOCK", &[], None, &[], &[], &[])
        .expect("LOCK");
    let lock_mem_out = b.body().graph.node_outputs(lock)[1];
    b.advance_cur_region_memory(lock_mem_out)
        .expect("advance to LOCK mem_out");

    // Store inside the bracket
    let addr = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr");
    let val = b
        .build_int_const(42u64, NodeOutputType::U32)
        .expect("val");
    b.build_store(addr, val, rsleigh::VnSpace::RAM)
        .expect("store");

    // UNLOCK (also PURE_WITH_MEM_EDGE).  build_call_other_modeled reads
    // cur_region_memory as the new op's mem_in (which is now the
    // Store's mem_out), so the Store's mem_out's unique consumer is
    // UNLOCK's mem input.
    let (unlock, _, _) = b
        .build_call_other_modeled(2, "UNLOCK", &[], None, &[], &[], &[])
        .expect("UNLOCK");
    let unlock_mem_out = b.body().graph.node_outputs(unlock)[1];
    b.advance_cur_region_memory(unlock_mem_out)
        .expect("advance to UNLOCK mem_out");

    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    // Pattern: a Store whose mem_in is LOCK and whose next_mem is UNLOCK.
    let pat = store()
        .addr(int_const(0x100u64))
        .mem_in(call_other().name("LOCK"))
        .next_mem(call_other().name("UNLOCK"));
    let hits = Matcher::new(&fg).find_all(&pat.into());
    assert_eq!(
        hits.len(),
        1,
        "atomic Store inside LOCK ... UNLOCK should match exactly once"
    );
}
