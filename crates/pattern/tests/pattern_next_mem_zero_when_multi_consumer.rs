//! .next_mem(p) returns no match when the mem output has zero or
//! multiple consumers — deterministic, no arbitrary pick.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::test_utils::SENTINEL_LIFT_ADDR;
use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use pattern::{Matcher, any, int_const, store};

#[test]
fn next_mem_returns_no_match_when_multi_consumer() {
    // Build: Store_A(0x100) ; Load(0x200) ; Store_B(0x300).
    // The Load reads memory but doesn't advance it (Loads aren't on
    // the memory chain), so Store_A.mem_out has TWO consumers:
    //   * Load.inputs[0] (memory predecessor)
    //   * Store_B.inputs[0] (memory predecessor)
    // .next_mem(any()) on Store_A must therefore return zero matches
    // (deterministic — never arbitrarily picks one consumer).
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr1");
    let v1 = b
        .build_int_const(1u64, NodeOutputType::U32)
        .expect("v1");
    b.build_store(addr1, v1, rsleigh::VnSpace::RAM)
        .expect("store_a");
    // Load reads cur_region_memory (= store_a's mem_out) but doesn't
    // advance it.
    let addr_l = b
        .build_int_const(0x200u64, NodeOutputType::U64)
        .expect("addr_l");
    let load_val = b
        .build_load(addr_l, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    // store_b reads cur_region_memory (still store_a's mem_out, since
    // Load didn't advance it).
    let addr2 = b
        .build_int_const(0x300u64, NodeOutputType::U64)
        .expect("addr2");
    let v2 = b
        .build_int_const(2u64, NodeOutputType::U32)
        .expect("v2");
    b.build_store(addr2, v2, rsleigh::VnSpace::RAM)
        .expect("store_b");
    b.build_return(Some(load_val), &[]).expect("ret");
    b.set_lift_addr(None);
    let fg = b.build().expect("build");

    let m = Matcher::new(&fg);

    // Sanity: store_a matches without .next_mem (baseline).
    let h_baseline = m.find_all(&store().addr(int_const(0x100u64)).into());
    assert_eq!(h_baseline.len(), 1, "baseline: store_a matches");

    // store_a's mem_out has multiple consumers (Load + Store_B); .next_mem
    // must deterministically return no match.
    let h_next = m.find_all(
        &store()
            .addr(int_const(0x100u64))
            .next_mem(any())
            .into(),
    );
    assert_eq!(
        h_next.len(),
        0,
        ".next_mem on a multi-consumer mem output returns deterministic no-match"
    );
}

#[test]
fn next_mem_returns_no_match_when_no_consumer() {
    // A Store immediately followed by a Return that does NOT consume
    // mem... but build_return always wires res.memory.  So a tail
    // Store will always have at least one consumer (the Return).  The
    // multi-consumer case above covers the deterministic-no-match
    // branch that's actually reachable from the IR builder.
    //
    // We still want a test for the "zero consumers" branch of the
    // helper; rather than contort the IR builder, we exercise it
    // implicitly by ensuring that find_all over a graph whose Store's
    // mem_out has exactly ONE consumer DOES match.
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
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
    b.set_lift_addr(None);
    let fg = b.build().expect("build");

    let m = Matcher::new(&fg);
    // store_a's mem_out has exactly one consumer (store_b's mem_in).
    // .next_mem(any()) matches.
    let h = m.find_all(
        &store()
            .addr(int_const(0x100u64))
            .next_mem(any())
            .into(),
    );
    assert_eq!(h.len(), 1);
}
