//! White-box tests for [`walk_memory_ssa`].
//!
//! Construct synthetic memory chains (`InitialMemory`, `Store`,
//! `MemPhi`) and drive the walker with stub [`MemorySSAWalker`] oracles
//! whose alias verdict each test pins.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use strider_ir_test_utils::{make_empty_fn, SENTINEL_LIFT_ADDR};

/// Oracle that classifies a specific set of store NodeOutputIds as
/// aliasing; every other def is non-aliasing.
struct AliasSet {
    aliasing: Vec<NodeOutputId>,
}
impl MemorySSAWalker for AliasSet {
    fn may_alias(
        &mut self,
        _function: &Function,
        _load: NodeOutputId,
        mem_def: NodeOutputId,
    ) -> bool {
        self.aliasing.contains(&mem_def)
    }
}

/// Oracle that never aliases — drives the "reaches InitialMemory clean"
/// path.
struct NeverAlias;
impl MemorySSAWalker for NeverAlias {
    fn may_alias(
        &mut self,
        _function: &Function,
        _load: NodeOutputId,
        _mem_def: NodeOutputId,
    ) -> bool {
        false
    }
}

/// Builds `fn() -> u64 { return 7; }` and returns
/// `(function, initial_memory_output)`.
fn empty_chain() -> (Function, NodeOutputId) {
    let fg = make_empty_fn(|b| b.build_int_const(7u64, NodeOutputType::I64)).unwrap();
    let im = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::InitialMemory))
        .expect("InitialMemory must exist");
    let im_out = fg.node_outputs_exact::<1>(im).unwrap()[0];
    (fg, im_out)
}

/// Builds a linear chain of `depth` `Store`s and returns
/// `(function, head_memory_output, store_mem_outputs_head_to_tail)`.
fn linear_store_chain(depth: usize) -> (Function, NodeOutputId, Vec<NodeOutputId>) {
    let fg = make_empty_fn(|b| {
        for i in 0..depth {
            let addr = b
                .build_int_const(0x1000u64 + (i as u64) * 8, NodeOutputType::I64)
                .unwrap();
            let v = b.build_int_const(i as u64, NodeOutputType::I64).unwrap();
            b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
        }
        b.build_int_const(7u64, NodeOutputType::I64)
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
        let node = fg.node_for_output(cur);
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
    let (fg, im_out) = empty_chain();
    // load output id is irrelevant for these oracles; reuse im_out.
    let r = walk_memory_ssa(&fg, &mut NeverAlias, im_out, im_out);
    assert_eq!(r, None, "InitialMemory with no alias → None");
}

#[test]
fn linear_chain_finds_nearest_aliasing_store() {
    let (fg, head, store_mems) = linear_store_chain(4);
    assert_eq!(store_mems.len(), 4, "four stores in the chain");
    // Mark the SECOND-from-head store as aliasing; the walk must return
    // it (the nearest clobber), skipping the first non-aliasing store.
    let nearest = store_mems[1];
    let mut oracle = AliasSet { aliasing: vec![nearest] };
    let r = walk_memory_ssa(&fg, &mut oracle, head, head);
    assert_eq!(r, Some(nearest), "nearest aliasing store is the clobber");
}

#[test]
fn non_aliasing_store_is_skipped() {
    let (fg, head, store_mems) = linear_store_chain(3);
    // Mark only the FURTHEST store (closest to InitialMemory) as
    // aliasing; the walk must skip the two nearer non-aliasing stores
    // and still find it.
    let furthest = *store_mems.last().unwrap();
    let mut oracle = AliasSet { aliasing: vec![furthest] };
    let r = walk_memory_ssa(&fg, &mut oracle, head, head);
    assert_eq!(r, Some(furthest), "walk skips non-aliasing stores");
}

#[test]
fn linear_chain_all_clean_returns_none() {
    let (fg, head, _store_mems) = linear_store_chain(5);
    let r = walk_memory_ssa(&fg, &mut NeverAlias, head, head);
    assert_eq!(r, None, "no aliasing store on the chain → None");
}

/// Builds a function with one Store so a Region exists, then grafts a
/// `MemPhi` whose `n_arms` predecessors all route to `InitialMemory`.
/// Returns `(function, mem_phi_output)`.
fn mem_phi_all_initial(n_arms: usize) -> (Function, NodeOutputId) {
    let mut fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x100u64, NodeOutputType::I64)?;
        let v = b.build_int_const(0x42u64, NodeOutputType::I64)?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, NodeOutputType::I64)
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
    let im_out = fg.node_outputs_exact::<1>(im_node).unwrap()[0];
    let phi_token = fg.node_outputs(region_node)[1];
    let mut inputs: Vec<NodeOutputId> = vec![phi_token];
    for _ in 0..n_arms {
        inputs.push(im_out);
    }
    let phi = fg.create_node(
        NodeKind::MemPhi,
        inputs.iter().copied(),
        [NodeOutputKind::Memory],
    );
    fg.set_asm_fingerprint(phi, vec![SENTINEL_LIFT_ADDR]);
    let phi_out = fg.node_outputs_exact::<1>(phi).unwrap()[0];
    (fg, phi_out)
}

#[test]
fn mem_phi_all_arms_clean_returns_none() {
    // Every predecessor routes to InitialMemory with no alias → the phi
    // is clean → None.
    let (fg, phi_out) = mem_phi_all_initial(3);
    let r = walk_memory_ssa(&fg, &mut NeverAlias, phi_out, phi_out);
    assert_eq!(r, None, "all-clean MemPhi arms → None");
}

#[test]
fn mem_phi_disagreeing_arms_returns_phi_boundary() {
    // Build a MemPhi with two arms: one through a Store (which the
    // oracle marks aliasing), one through InitialMemory.  Because one
    // predecessor reaches a clobber, the phi is a clobber boundary.
    let mut fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x200u64, NodeOutputType::I64)?;
        let v = b.build_int_const(0x99u64, NodeOutputType::I64)?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, NodeOutputType::I64)
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
    let im_out = fg.node_outputs_exact::<1>(im_node).unwrap()[0];
    let store_mem = fg.node_outputs_exact::<1>(store_node).unwrap()[0];
    let phi_token = fg.node_outputs(region_node)[1];
    let phi = fg.create_node(
        NodeKind::MemPhi,
        [phi_token, store_mem, im_out],
        [NodeOutputKind::Memory],
    );
    fg.set_asm_fingerprint(phi, vec![SENTINEL_LIFT_ADDR]);
    let phi_out = fg.node_outputs_exact::<1>(phi).unwrap()[0];

    // Oracle marks the store-arm's store as aliasing.
    let mut oracle = AliasSet { aliasing: vec![store_mem] };
    let r = walk_memory_ssa(&fg, &mut oracle, phi_out, phi_out);
    assert_eq!(
        r,
        Some(store_mem),
        "a MemPhi with one clobbering arm returns that clobber (phi boundary)"
    );
}

#[test]
fn long_linear_chain_is_heap_bounded() {
    // 10k-deep chain — confirms the walk is iterative (heap-bounded),
    // not call-stack-bounded.
    const DEPTH: usize = 10_000;
    let (fg, head, _store_mems) = linear_store_chain(DEPTH);
    let r = walk_memory_ssa(&fg, &mut NeverAlias, head, head);
    assert_eq!(r, None, "deep clean chain terminates at InitialMemory");
}
