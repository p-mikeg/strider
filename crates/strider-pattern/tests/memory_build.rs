use strider_ir::node::ValueType;
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer};
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{
    Capture, CaptureExt, Matcher, call, int_const, load, mem_phi, one_of, store,
};

#[test]
fn load_space_matches_ram_and_rejects_unique() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x10u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::new(&function);

    let ram = load().space(rsleigh::VnSpace::RAM).build();
    assert_eq!(matcher.find_all(&ram).unwrap().len(), 1);
    let unique = load().space(rsleigh::VnSpace::UNIQUE).build();
    assert_eq!(matcher.find_all(&unique).unwrap().len(), 0);
}

#[test]
fn load_bit_width_filters_value_output() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x10u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::new(&function);

    assert_eq!(
        matcher
            .find_all(&load().bit_width(32).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&load().bit_width(64).build())
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn store_addr_and_data() {
    let function = store_then_load(0x200, 77);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(
                &store()
                    .addr(int_const(0x200u128))
                    .data(int_const(77u128))
                    .build()
            )
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(
                &store()
                    .addr(int_const(0x200u128))
                    .data(int_const(99u128))
                    .build()
            )
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn store_captures_node() {
    let function = store_then_load(0x100, 42);
    let c = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(&store().capture(c).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    let node = hits[0]
        .node(c, function.graph())
        .expect("store node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::Store(_)
    ));
}

/// `store(addr1, data); v = load(addr2); return v`. The load's memory input
/// (slot 0) is wired to the store's memory token.
fn store_then_load(store_addr: u64, data: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let a1 = b.build_int_const(store_addr, ValueType::I64).unwrap();
    let d = b.build_int_const(data, ValueType::I32).unwrap();
    b.build_store(a1, d, rsleigh::VnSpace::RAM).unwrap();
    let a2 = b.build_int_const(0x999u64, ValueType::I64).unwrap();
    let v = b
        .build_load(a2, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    b.build_return(Some(v), &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn load_mem_rejects_wrong_store() {
    let function = store_then_load(0x100, 42);
    // Store is at 0x100, so a mem constrained to another address must
    // break the chain.
    let pat = load()
        .addr(int_const(0x999u128))
        .mem(store().addr(int_const(0xBEEFu128)))
        .build();
    assert_eq!(Matcher::new(&function).find_all(&pat).unwrap().len(), 0);
}

#[test]
fn load_mem_matches_region_mem_phi() {
    // A fresh region head carries a MemPhi as its initial memory token, so
    // the region's first load chains straight off it.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x10u64, ValueType::I64).unwrap();
    let v = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = load().mem(mem_phi()).build();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "load off the region's initial MemPhi token should match"
    );
}

#[test]
fn store_mem_chains_off_region_mem_phi() {
    let function = store_then_load(0x100, 42);
    let pat = store().mem(mem_phi()).build();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

/// A slot a fixed operand pinned is not a candidate for an existential.
#[test]
fn any_input_skips_a_slot_a_fixed_operand_pinned() {
    let function = store_then_load(0x200, 7);
    let matcher = Matcher::new(&function);

    // The only 7 is the data operand, which `.data()` already claims.
    assert_eq!(
        matcher
            .find_all(
                &store()
                    .data(int_const(7u128))
                    .any_input(int_const(7u128))
                    .build()
            )
            .unwrap()
            .len(),
        0
    );

    // A second 7 in the address slot satisfies both.
    let both = store_then_load(7, 7);
    assert_eq!(
        Matcher::new(&both)
            .find_all(
                &store()
                    .data(int_const(7u128))
                    .any_input(int_const(7u128))
                    .build()
            )
            .unwrap()
            .len(),
        1
    );
}

/// A `Call` whose memory token the following `Store` chains off.
fn call_then_store() -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let tgt = b.build_int_const(0x1234u64, ValueType::I64).unwrap();
    b.build_call_cc(tgt, None).unwrap();
    let a = b.build_int_const(0x10u64, ValueType::I64).unwrap();
    let d = b.build_int_const(7u64, ValueType::I32).unwrap();
    b.build_store(a, d, rsleigh::VnSpace::RAM).unwrap();
    b.build_return(None, &[]).unwrap();
    b.build().unwrap()
}

/// `.capture()` on an alternation arm must not turn the memory slot back into
/// a value slot: `call()` anchors on its value output there and never matches.
#[test]
fn captured_alternation_arm_stays_in_the_memory_slot() {
    let function = call_then_store();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&store().mem(one_of![call()]).build())
            .unwrap()
            .len(),
        1
    );

    let c = Capture::new();
    let hits = matcher
        .find_all(&store().mem(one_of![one_of![call()].capture(c)]).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(c).is_some());
}
