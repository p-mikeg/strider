//! White-box tests for [`may_clobber`].
//!
//! Construct synthetic memory chains (`InitialMemory`, `Store`,
//! `MemPhi`) and drive the walker with stub [`MemorySSAWalker`] oracles
//! whose alias verdict each test pins.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use strider_ir::node::{NodeKind, ValueKind, ValueType};
use strider_ir_test_utils::{make_empty_fn, SENTINEL_LIFT_ADDR};

/// Oracle that classifies a specific set of store memory outputs as
/// aliasing; every other def is non-aliasing.
struct AliasSet {
    aliasing: Vec<ValueId>,
}
impl MemorySSAWalker for AliasSet {
    fn def_clobbers(&mut self, function: &Function, _load: NodeId, def: NodeId) -> bool {
        let out = function
            .graph()
            .memory_output_of(def)
            .expect("a classified def has a memory output");
        self.aliasing.contains(&out)
    }
}

/// Oracle that never aliases — drives the "reaches InitialMemory clean"
/// path.
struct NeverAlias;
impl MemorySSAWalker for NeverAlias {
    fn def_clobbers(&mut self, _function: &Function, _load: NodeId, _def: NodeId) -> bool {
        false
    }
}

/// Runs [`may_clobber`] from the def that produced `start_mem` (the load
/// node is unused by these oracles, so the start node doubles as the load
/// handle).  Returns the clobber node — or the `InitialMemory` root for a
/// clean chain.
fn run<W: MemorySSAWalker>(fg: &Function, oracle: &mut W, start_mem: ValueId) -> NodeId {
    let start = fg.producer(start_mem);
    may_clobber(fg, oracle, start, start)
}

/// Asserts the walk bottomed out cleanly at the `InitialMemory` root.
fn assert_clean(fg: &Function, r: NodeId) {
    assert!(
        matches!(*fg.node_kind(r), NodeKind::InitialMemory),
        "expected the clean InitialMemory root, got {:?}",
        fg.node_kind(r),
    );
}

/// Builds `fn() -> u64 { return 7; }` and returns
/// `(function, initial_memory_output)`.
fn empty_chain() -> (Function, ValueId) {
    let fg = make_empty_fn(|b| b.build_int_const(7u64, ValueType::I64)).unwrap();
    let im = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::InitialMemory))
        .expect("InitialMemory must exist");
    let im_value = fg.node_outputs_exact::<1>(im).unwrap()[0];
    (fg, im_value)
}

/// Builds a linear chain of `depth` `Store`s and returns
/// `(function, head_memory_output, store_mem_outputs_head_to_tail)`.
fn linear_store_chain(depth: usize) -> (Function, ValueId, Vec<ValueId>) {
    let fg = make_empty_fn(|b| {
        for i in 0..depth {
            let addr = b
                .build_int_const(0x1000u64 + (i as u64) * 8, ValueType::I64)
                .unwrap();
            let v = b.build_int_const(i as u64, ValueType::I64).unwrap();
            b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
        }
        b.build_int_const(7u64, ValueType::I64)
    })
    .unwrap();
    let ret = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return must exist");
    let head = fg.node_inputs(ret)[1];
    // Collect every Store's memory output, in chain order head→tail
    // (most-recent first), by walking slot-0 from the head.
    let mut store_mems = Vec::new();
    let mut cur = head;
    loop {
        let node = fg.producer(cur);
        match *fg.node_kind(node) {
            NodeKind::Store(_) => {
                store_mems.push(cur);
                cur = fg.node_inputs(node)[0];
            }
            _ => break,
        }
    }
    (fg, head, store_mems)
}

#[test]
fn initial_memory_with_no_alias_returns_none() {
    let (fg, im_value) = empty_chain();
    let r = run(&fg, &mut NeverAlias, im_value);
    assert_clean(&fg, r);
}

#[test]
fn linear_chain_finds_nearest_aliasing_store() {
    let (fg, head, store_mems) = linear_store_chain(4);
    assert_eq!(store_mems.len(), 4, "four stores in the chain");
    // Mark the SECOND-from-head store as aliasing; the walk must return
    // it (the nearest clobber), skipping the first non-aliasing store.
    let nearest = store_mems[1];
    let mut oracle = AliasSet { aliasing: vec![nearest] };
    let r = run(&fg, &mut oracle, head);
    assert_eq!(r, fg.producer(nearest), "nearest aliasing store is the clobber");
}

#[test]
fn non_aliasing_store_is_skipped() {
    let (fg, head, store_mems) = linear_store_chain(3);
    // Mark only the FURTHEST store (closest to InitialMemory) as
    // aliasing; the walk must skip the two nearer non-aliasing stores
    // and still find it.
    let furthest = *store_mems.last().unwrap();
    let mut oracle = AliasSet { aliasing: vec![furthest] };
    let r = run(&fg, &mut oracle, head);
    assert_eq!(r, fg.producer(furthest), "walk skips non-aliasing stores");
}

#[test]
fn linear_chain_all_clean_returns_none() {
    let (fg, head, _store_mems) = linear_store_chain(5);
    let r = run(&fg, &mut NeverAlias, head);
    assert_clean(&fg, r);
}

/// Builds a function with one Store so a Region exists, then grafts a
/// `MemPhi` whose `n_arms` predecessors all route to `InitialMemory`.
/// Returns `(function, mem_phi_value)`.
fn mem_phi_all_initial(n_arms: usize) -> (Function, ValueId) {
    let mut fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x100u64, ValueType::I64)?;
        let v = b.build_int_const(0x42u64, ValueType::I64)?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, ValueType::I64)
    })
    .unwrap();
    let im_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::InitialMemory))
        .expect("InitialMemory must exist");
    let region_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("Region must exist");
    let im_value = fg.node_outputs_exact::<1>(im_node).unwrap()[0];
    let phi_token = fg.node_outputs(region_node)[1];
    let mut inputs: Vec<ValueId> = vec![phi_token];
    for _ in 0..n_arms {
        inputs.push(im_value);
    }
    let phi = fg.graph_mut().create_node(
        NodeKind::MemPhi,
        inputs.iter().copied(),
        [ValueKind::Memory],
    );
    fg.set_asm_fingerprint(phi, vec![SENTINEL_LIFT_ADDR]);
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];
    (fg, phi_value)
}

#[test]
fn mem_phi_all_arms_clean_returns_none() {
    // Every predecessor routes to InitialMemory with no alias → the phi
    // is clean → None.
    let (fg, phi_value) = mem_phi_all_initial(3);
    let r = run(&fg, &mut NeverAlias, phi_value);
    assert_clean(&fg, r);
}

#[test]
fn mem_phi_disagreeing_arms_returns_phi_boundary() {
    // Build a MemPhi with two arms: one through a Store (which the
    // oracle marks aliasing), one through InitialMemory.  Because one
    // predecessor reaches a clobber, the phi is a clobber boundary.
    let mut fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x200u64, ValueType::I64)?;
        let v = b.build_int_const(0x99u64, ValueType::I64)?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, ValueType::I64)
    })
    .unwrap();
    let im_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::InitialMemory))
        .unwrap();
    let store_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
        .unwrap();
    let region_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .unwrap();
    let im_value = fg.node_outputs_exact::<1>(im_node).unwrap()[0];
    let store_mem = fg.node_outputs_exact::<1>(store_node).unwrap()[0];
    let phi_token = fg.node_outputs(region_node)[1];
    let phi = fg.graph_mut().create_node(
        NodeKind::MemPhi,
        [phi_token, store_mem, im_value],
        [ValueKind::Memory],
    );
    fg.set_asm_fingerprint(phi, vec![SENTINEL_LIFT_ADDR]);
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];

    // Oracle marks the store-arm's store as aliasing.  One arm clobbers
    // (the store) and the other is clean (InitialMemory) → the arms
    // DISAGREE, so the MemPhi itself is the boundary clobber: the walk
    // returns the phi's own output, NOT the inner store.
    let mut oracle = AliasSet { aliasing: vec![store_mem] };
    let r = run(&fg, &mut oracle, phi_value);
    assert_eq!(
        r,
        fg.producer(phi_value),
        "a MemPhi whose arms disagree (one clobbers, one clean) is itself the boundary",
    );
}

/// Both arms of a `MemPhi` route to the SAME aliasing store (the store
/// dominates the merge, reached identically through every arm).  The
/// arms AGREE, so the walk passes the phi through transparently and
/// returns that single dominating store — NOT the phi.  This is the
/// dominator case the store-to-load forwarder forwards across.
#[test]
fn mem_phi_agreeing_arms_pass_through_to_shared_store() {
    // Build a chain: InitialMemory ← Store(dominating) ← MemPhi[both arms
    // = dominating store's mem].  The phi's two value inputs are the same
    // ValueId, so every arm resolves to the same store.
    let mut fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x300u64, ValueType::I64)?;
        let v = b.build_int_const(0x77u64, ValueType::I64)?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, ValueType::I64)
    })
    .unwrap();
    let store_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
        .unwrap();
    let region_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .unwrap();
    let store_mem = fg.node_outputs_exact::<1>(store_node).unwrap()[0];
    let phi_token = fg.node_outputs(region_node)[1];
    // Both arms carry the same (dominating) store memory token.
    let phi = fg.graph_mut().create_node(
        NodeKind::MemPhi,
        [phi_token, store_mem, store_mem],
        [ValueKind::Memory],
    );
    fg.set_asm_fingerprint(phi, vec![SENTINEL_LIFT_ADDR]);
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];

    let mut oracle = AliasSet { aliasing: vec![store_mem] };
    let r = run(&fg, &mut oracle, phi_value);
    assert_eq!(
        r,
        fg.producer(store_mem),
        "agreeing MemPhi arms pass through to the shared dominating store",
    );
}

/// A `MemPhi` whose arms reach DIFFERENT aliasing stores (per-branch
/// stores) disagrees → the phi is the boundary, NOT either inner store.
#[test]
fn mem_phi_different_clobbers_per_arm_returns_phi_boundary() {
    // Two stores, both marked aliasing; build a MemPhi whose two arms
    // each route to a DIFFERENT one.
    let (fg, _head, store_mems) = linear_store_chain(2);
    // store_mems are head→tail; both are reachable via the chain, but for
    // this test we wire a MemPhi directly to each store's mem output.
    let mut fg = fg;
    let region_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .unwrap();
    let phi_token = fg.node_outputs(region_node)[1];
    let arm_a = store_mems[0];
    let arm_b = store_mems[1];
    let phi = fg.graph_mut().create_node(
        NodeKind::MemPhi,
        [phi_token, arm_a, arm_b],
        [ValueKind::Memory],
    );
    fg.set_asm_fingerprint(phi, vec![SENTINEL_LIFT_ADDR]);
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];

    // Mark BOTH stores aliasing: each arm resolves to its own (different)
    // store → the arms disagree → boundary.
    let mut oracle = AliasSet { aliasing: vec![arm_a, arm_b] };
    let r = run(&fg, &mut oracle, phi_value);
    assert_eq!(
        r,
        fg.producer(phi_value),
        "per-arm different clobbers disagree → the MemPhi is the boundary",
    );
}

#[test]
fn long_linear_chain_is_heap_bounded() {
    // 10k-deep chain — confirms the walk is iterative (heap-bounded),
    // not call-stack-bounded.
    const DEPTH: usize = 10_000;
    let (fg, head, _store_mems) = linear_store_chain(DEPTH);
    let r = run(&fg, &mut NeverAlias, head);
    assert_clean(&fg, r);
}
