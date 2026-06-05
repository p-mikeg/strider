//! `load` / `store` builder matching, including memory-token chaining
//! (a `load` consuming the memory token produced by a prior `store` /
//! `mem_phi`).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::IRBuilderExt;
use strider_ir::IRViewer;
use strider_ir::FunctionBuilder;
use strider_ir::node::ValueType;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{
    Capture, Matcher, add, int_const, load, mem_phi, store,
};

// ── Load ──────────────────────────────────────────────────────────────────────

#[test]
fn load_unconstrained_matches() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x10u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = load().build();
    assert_eq!(Matcher::try_new(&function).unwrap().find_all(&pat).unwrap().len(), 1);
}

#[test]
fn load_space_matches_ram_and_rejects_unique() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x10u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let ram = load().space(rsleigh::VnSpace::RAM).build();
    assert_eq!(matcher.find_all(&ram).unwrap().len(), 1);
    let unique = load().space(rsleigh::VnSpace::UNIQUE).build();
    assert_eq!(matcher.find_all(&unique).unwrap().len(), 0);
}

#[test]
fn load_addr_matches_literal() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x100u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    assert_eq!(
        matcher.find_all(&load().addr(int_const(0x100u128)).build()).unwrap().len(),
        1
    );
    assert_eq!(
        matcher.find_all(&load().addr(int_const(0x999u128)).build()).unwrap().len(),
        0
    );
}

#[test]
fn load_with_patterned_addr() {
    // Load from `base + 8`: addr is itself a pattern.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let base = b.build_int_const(0x100u64, ValueType::I64).unwrap();
    let off = b.build_int_const(8u64, ValueType::I64).unwrap();
    let addr = b
        .build_int_binary_operation(base, off, strider_ir::IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    assert_eq!(
        matcher
            .find_all(&load().addr(add(int_const(0x100u128), int_const(8u128))).build()).unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&load().addr(add(int_const(0x100u128), int_const(9u128))).build()).unwrap()
            .len(),
        0
    );
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
    let matcher = Matcher::try_new(&function).unwrap();

    assert_eq!(matcher.find_all(&load().bit_width(32).build()).unwrap().len(), 1);
    assert_eq!(matcher.find_all(&load().bit_width(64).build()).unwrap().len(), 0);
}

#[test]
fn load_captures_value_slot() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x100u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();

    let v = Capture::new();
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&load().capture(v).build()).unwrap();
    assert_eq!(hits.len(), 1);
    let node = hits[0].node(v, function.graph()).expect("value slot capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::Load(_)
    ));
}

// ── Store ─────────────────────────────────────────────────────────────────────

#[test]
fn store_unconstrained_matches() {
    let function = store_then_load(0x100, 42);
    assert_eq!(
        Matcher::try_new(&function).unwrap().find_all(&store().build()).unwrap().len(),
        1
    );
}

#[test]
fn store_addr_and_data() {
    let function = store_then_load(0x200, 77);
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(
        matcher
            .find_all(&store().addr(int_const(0x200u128)).data(int_const(77u128)).build()).unwrap()
            .len(),
        1
    );
    // Right addr, wrong data → reject.
    assert_eq!(
        matcher
            .find_all(&store().addr(int_const(0x200u128)).data(int_const(99u128)).build()).unwrap()
            .len(),
        0
    );
}

#[test]
fn store_space_matches() {
    let function = store_then_load(0x100, 42);
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(
        matcher.find_all(&store().space(rsleigh::VnSpace::RAM).build()).unwrap().len(),
        1
    );
    assert_eq!(
        matcher.find_all(&store().space(rsleigh::VnSpace::UNIQUE).build()).unwrap().len(),
        0
    );
}

#[test]
fn store_captures_node() {
    let function = store_then_load(0x100, 42);
    let c = Capture::new();
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&store().capture(c).build()).unwrap();
    assert_eq!(hits.len(), 1);
    let node = hits[0].node(c, function.graph()).expect("store node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::Store(_)
    ));
}

// ── Memory-chain (headline) ───────────────────────────────────────────────────

/// `store(addr1, data) ; v = load(addr2) ; return v`.  The load's
/// memory input (slot 0) is wired to the store's memory token.
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
fn load_mem_in_matches_preceding_store() {
    let function = store_then_load(0x100, 42);
    // The load chains off the store's memory token: wire the store
    // pattern (its memory output) into the load's memory input slot.
    let pat = load()
        .addr(int_const(0x999u128))
        .mem_in(store().addr(int_const(0x100u128)))
        .build();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "load whose mem_in is the prior store should match exactly once"
    );
}

#[test]
fn load_mem_in_rejects_wrong_store() {
    let function = store_then_load(0x100, 42);
    // The store is at 0x100; constrain the mem_in store to a different
    // address — the chain must not match.
    let pat = load()
        .addr(int_const(0x999u128))
        .mem_in(store().addr(int_const(0xBEEFu128)))
        .build();
    assert_eq!(Matcher::try_new(&function).unwrap().find_all(&pat).unwrap().len(), 0);
}

#[test]
fn load_mem_in_matches_region_mem_phi() {
    // A freshly created region head carries a MemPhi as the region's
    // initial memory token; the first store/load in the region chains
    // off it.  Build: v = load(addr) directly off the region MemPhi.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x10u64, ValueType::I64).unwrap();
    let v = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = load().mem_in(mem_phi()).build();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "load off the region's initial MemPhi token should match"
    );
}

#[test]
fn store_mem_in_chains_off_region_mem_phi() {
    let function = store_then_load(0x100, 42);
    // The store's memory predecessor is the region's MemPhi.
    let pat = store().mem_in(mem_phi()).build();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}
