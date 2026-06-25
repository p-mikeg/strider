//! Memory-chain matchers — `LoadPat::mem_in`, `StorePat::mem_in` — exercise
//! the backward walk along the per-region memory chain.

use strider_ir::node::ValueType;
use strider_ir::{FunctionBuilder, IRBuilderExt};
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{Matcher, int_const, load, store};

// ── LoadPat::mem_in ──────────────────────────────────────────────────────────

#[test]
fn load_mem_in_matches_preceding_store() {
    // Build: Store(addr=0x100, data=42) ; v = Load(addr=0x200) ; ret v
    // The Load's value is consumed by Return so the Load is reachable;
    // its inputs[0] (memory) is the Store's mem_value.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr1 = b.build_int_const(0x100u64, ValueType::I64).expect("addr1");
    let val = b.build_int_const(42u64, ValueType::I32).expect("val");
    b.build_store(addr1, val, rsleigh::VnSpace::RAM)
        .expect("store");
    let addr2 = b.build_int_const(0x200u64, ValueType::I64).expect("addr2");
    let load_val = b
        .build_load(addr2, rsleigh::VnSpace::RAM, ValueType::I32)
        .expect("load");
    b.build_return(Some(load_val), &[]).expect("ret");
    let function = b.build().expect("build");

    let pat = load()
        .addr(int_const(0x200u128))
        .mem_in(store().addr(int_const(0x100u128)))
        .build();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "load whose mem_in is the prior store should match exactly once"
    );
}
