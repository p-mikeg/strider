//! White-box tests for [`may_clobber`].
//!
//! Construct synthetic memory chains (`InitialMemory`, `Store`,
//! `MemPhi`) and drive the walker with stub [`MemorySSAWalker`] oracles
//! whose alias verdict each test pins.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::IRBuilderExt;
use super::*;
use strider_ir::Function;
use strider_ir::node::{NodeKind, ValueKind, ValueType};
use strider_ir_test_utils::make_empty_fn;

/// Oracle that classifies a specific set of store memory outputs as
/// aliasing; every other def is non-aliasing.
struct AliasSet {
    aliasing: Vec<ValueId>,
}
impl MemorySSAWalker for AliasSet {
    fn def_clobbers<B: IRBuilder>(&mut self, builder: &B, _load: NodeId, def: NodeId) -> bool {
        let out = builder
            .function()
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
    fn def_clobbers<B: IRBuilder>(&mut self, _builder: &B, _load: NodeId, _def: NodeId) -> bool {
        false
    }
}

/// Runs [`may_clobber`] from the def that produced `start_mem`.  The load
/// handle is the start node itself — a `Store` / `MemPhi` / `InitialMemory`
/// producer, never a `Load` — so the narrowing rewrite never fires and only
/// the returned clobber node is exercised.  Returns the clobber node — or
/// the `InitialMemory` root for a clean chain.
fn run<W: MemorySSAWalker>(fg: &mut Function, oracle: &mut W, start_mem: ValueId) -> NodeId {
    let start = fg.producer(start_mem);
    let mut ctx = crate::EditFunction::try_for_built(fg).unwrap();
    may_clobber(&mut ctx, oracle, start, start)
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

/// Builds a linear chain of `depth` `Store`s followed by a `Load` that
/// reads the head of the chain, and returns `(function, load_node,
/// load_memory_input, store_mem_outputs_head_to_tail)`.  The loaded value is
/// the function's return value, so the `Load` is reachable.
fn linear_chain_with_load(depth: usize) -> (Function, NodeId, ValueId, Vec<ValueId>) {
    let fg = make_empty_fn(|b| {
        for i in 0..depth {
            let addr = b.build_int_const(0x1000u64 + (i as u64) * 8, ValueType::I64)?;
            let v = b.build_int_const(i as u64, ValueType::I64)?;
            b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        }
        let laddr = b.build_int_const(0x9000u64, ValueType::I64)?;
        b.build_load(laddr, rsleigh::VnSpace::RAM, ValueType::I64)
    })
    .unwrap();
    let load = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .expect("Load must exist");
    let head = fg.node_inputs(load)[0];
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
    (fg, load, head, store_mems)
}

/// Runs [`may_clobber`] with a real `Load` node so the narrowing rewrite is
/// exercised: `mem` is the load's own memory input.  Returns the clobber.
fn run_load<W: MemorySSAWalker>(fg: &mut Function, oracle: &mut W, load: NodeId) -> NodeId {
    let mem = fg.node_inputs(load)[0];
    let mem_node = fg.producer(mem);
    let mut ctx = crate::EditFunction::try_for_built(fg).unwrap();
    may_clobber(&mut ctx, oracle, load, mem_node)
}

#[test]
fn narrows_load_past_disjoint_prefix() {
    // load → store0(head) → store1 → store2 → InitialMemory.  Only the
    // furthest store (store2, nearest InitialMemory) aliases the load.
    let (mut fg, load, head, store_mems) = linear_chain_with_load(3);
    assert_eq!(store_mems.len(), 3, "three stores in the chain");
    assert_eq!(
        fg.node_inputs(load)[0],
        head,
        "load starts at the chain head"
    );
    let furthest = *store_mems.last().unwrap();

    let mut oracle = AliasSet {
        aliasing: vec![furthest],
    };
    let r = run_load(&mut fg, &mut oracle, load);
    assert_eq!(
        r,
        fg.producer(furthest),
        "nearest clobber is the furthest store"
    );

    // Narrowing: the load's memory input is repointed directly at that
    // store, skipping the two disjoint stores in between.
    assert_eq!(
        fg.node_inputs(load)[0],
        furthest,
        "load memory edge narrowed onto the nearest clobber",
    );
}

#[test]
fn narrowing_is_idempotent() {
    // After the first narrowing the load points at its nearest clobber, so a
    // second walk returns the same clobber and moves nothing.
    let (mut fg, load, _head, store_mems) = linear_chain_with_load(3);
    let furthest = *store_mems.last().unwrap();
    let mut oracle = AliasSet {
        aliasing: vec![furthest],
    };

    let r1 = run_load(&mut fg, &mut oracle, load);
    assert_eq!(fg.node_inputs(load)[0], furthest, "narrowed on first walk");
    let r2 = run_load(&mut fg, &mut oracle, load);
    assert_eq!(r1, r2, "same nearest clobber on the second walk");
    assert_eq!(fg.node_inputs(load)[0], furthest, "no further movement");
}

/// Builds `fn { *0x10 = 0x42; return 7; }` and returns
/// `(function, initial_memory_output, store_memory_output, region_phi_token)`
/// — the scaffold every grafted-phi test grows a `MemPhi` and load onto.
fn base_with_store() -> (Function, ValueId, ValueId, ValueId) {
    let fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x10u64, ValueType::I64)?;
        let v = b.build_int_const(0x42u64, ValueType::I64)?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, ValueType::I64)
    })
    .unwrap();
    let im_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::InitialMemory))
        .expect("InitialMemory must exist");
    let store_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
        .expect("Store must exist");
    let region_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("Region must exist");
    let im = fg.node_outputs_exact::<1>(im_node).unwrap()[0];
    let store_mem = fg.node_outputs_exact::<1>(store_node).unwrap()[0];
    let phi_token = fg.node_outputs(region_node)[1];
    (fg, im, store_mem, phi_token)
}

/// Grafts an `IntConst` and returns its value output.
fn mk_const(fg: &mut Function, v: u128) -> ValueId {
    let n = strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::IntConst(v),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    fg.node_outputs_exact::<1>(n).unwrap()[0]
}

/// Grafts a `Store(mem, addr, data)` and returns its memory output.
fn mk_store(fg: &mut Function, mem: ValueId, addr: ValueId, data: ValueId) -> ValueId {
    let n = strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::Store(rsleigh::VnSpace::RAM),
        [mem, addr, data],
        [ValueKind::Memory],
    );
    fg.node_outputs_exact::<1>(n).unwrap()[0]
}

/// Grafts a `Load(mem, addr)` and returns the load NODE.
fn mk_load(fg: &mut Function, mem: ValueId, addr: ValueId) -> NodeId {
    strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::Load(rsleigh::VnSpace::RAM),
        [mem, addr],
        [ValueKind::Typed(ValueType::I64)],
    )
}

/// Grafts a `MemPhi` over `arms` (with `phi_token`) and returns its memory
/// output.
fn mk_mem_phi(fg: &mut Function, phi_token: ValueId, arms: &[ValueId]) -> ValueId {
    let inputs: Vec<ValueId> = core::iter::once(phi_token)
        .chain(arms.iter().copied())
        .collect();
    let n = strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::MemPhi,
        inputs.iter().copied(),
        [ValueKind::Memory],
    );
    fg.node_outputs_exact::<1>(n).unwrap()[0]
}

#[test]
fn narrowing_jumps_past_transparent_phi_with_disjoint_prefix() {
    // load → store_outer(disjoint) → MemPhi[store_dom, store_dom] → store_dom.
    // Both phi arms agree on the dominating store, so the walk passes through
    // the phi to it; the load is repointed straight onto store_dom, jumping
    // both the disjoint outer store AND the transparent phi.
    let (mut fg, _im, store_dom_mem, phi_token) = base_with_store();
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[store_dom_mem, store_dom_mem]);
    let a2 = mk_const(&mut fg, 0x20);
    let d2 = mk_const(&mut fg, 0xaa);
    let store_outer_mem = mk_store(&mut fg, phi_mem, a2, d2);
    let a3 = mk_const(&mut fg, 0x30);
    let load = mk_load(&mut fg, store_outer_mem, a3);

    // store_dom aliases; store_outer is disjoint.
    let mut oracle = AliasSet {
        aliasing: vec![store_dom_mem],
    };
    let r = run_load(&mut fg, &mut oracle, load);
    assert_eq!(
        r,
        fg.producer(store_dom_mem),
        "passes through to the dominating store"
    );
    assert_eq!(
        fg.node_inputs(load)[0],
        store_dom_mem,
        "load jumps past the disjoint store and the transparent phi onto store_dom",
    );
}

#[test]
fn narrowing_stops_at_disagreeing_phi_skipping_disjoint_prefix() {
    // load → store_outer(disjoint) → MemPhi[store_inner(aliasing), InitialMemory].
    // The arms disagree (one clobbers, one clean), so the phi is the boundary:
    // the load is repointed onto the phi, skipping the disjoint outer store
    // but NEVER past the merge.
    let (mut fg, im, store_inner_mem, phi_token) = base_with_store();
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[store_inner_mem, im]);
    let a2 = mk_const(&mut fg, 0x20);
    let d2 = mk_const(&mut fg, 0xbb);
    let store_outer_mem = mk_store(&mut fg, phi_mem, a2, d2);
    let a3 = mk_const(&mut fg, 0x30);
    let load = mk_load(&mut fg, store_outer_mem, a3);

    // Only the inner (phi-arm) store aliases; the outer store is disjoint.
    let mut oracle = AliasSet {
        aliasing: vec![store_inner_mem],
    };
    let r = run_load(&mut fg, &mut oracle, load);
    assert_eq!(
        r,
        fg.producer(phi_mem),
        "the disagreeing MemPhi is the boundary"
    );
    assert_eq!(
        fg.node_inputs(load)[0],
        phi_mem,
        "load is repointed onto the phi, skipping the disjoint store, never past the merge",
    );
}

#[test]
fn initial_memory_with_no_alias_returns_none() {
    let (mut fg, im_value) = empty_chain();
    let r = run(&mut fg, &mut NeverAlias, im_value);
    assert_clean(&fg, r);
}

#[test]
fn linear_chain_finds_nearest_aliasing_store() {
    let (mut fg, head, store_mems) = linear_store_chain(4);
    assert_eq!(store_mems.len(), 4, "four stores in the chain");
    // Mark the SECOND-from-head store as aliasing; the walk must return
    // it (the nearest clobber), skipping the first non-aliasing store.
    let nearest = store_mems[1];
    let mut oracle = AliasSet {
        aliasing: vec![nearest],
    };
    let r = run(&mut fg, &mut oracle, head);
    assert_eq!(
        r,
        fg.producer(nearest),
        "nearest aliasing store is the clobber"
    );
}

#[test]
fn non_aliasing_store_is_skipped() {
    let (mut fg, head, store_mems) = linear_store_chain(3);
    // Mark only the FURTHEST store (closest to InitialMemory) as
    // aliasing; the walk must skip the two nearer non-aliasing stores
    // and still find it.
    let furthest = *store_mems.last().unwrap();
    let mut oracle = AliasSet {
        aliasing: vec![furthest],
    };
    let r = run(&mut fg, &mut oracle, head);
    assert_eq!(r, fg.producer(furthest), "walk skips non-aliasing stores");
}

#[test]
fn linear_chain_all_clean_returns_none() {
    let (mut fg, head, _store_mems) = linear_store_chain(5);
    let r = run(&mut fg, &mut NeverAlias, head);
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
    let phi = strider_ir_test_utils::sentinel_node(
        &mut fg,
        NodeKind::MemPhi,
        inputs.iter().copied(),
        [ValueKind::Memory],
    );
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];
    (fg, phi_value)
}

#[test]
fn mem_phi_all_arms_clean_returns_none() {
    // Every predecessor routes to InitialMemory with no alias → the phi
    // is clean → None.
    let (mut fg, phi_value) = mem_phi_all_initial(3);
    let r = run(&mut fg, &mut NeverAlias, phi_value);
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
    let phi = strider_ir_test_utils::sentinel_node(
        &mut fg,
        NodeKind::MemPhi,
        [phi_token, store_mem, im_value],
        [ValueKind::Memory],
    );
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];

    // Oracle marks the store-arm's store as aliasing.  One arm clobbers
    // (the store) and the other is clean (InitialMemory) → the arms
    // DISAGREE, so the MemPhi itself is the boundary clobber: the walk
    // returns the phi's own output, NOT the inner store.
    let mut oracle = AliasSet {
        aliasing: vec![store_mem],
    };
    let r = run(&mut fg, &mut oracle, phi_value);
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
    let phi = strider_ir_test_utils::sentinel_node(
        &mut fg,
        NodeKind::MemPhi,
        [phi_token, store_mem, store_mem],
        [ValueKind::Memory],
    );
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];

    let mut oracle = AliasSet {
        aliasing: vec![store_mem],
    };
    let r = run(&mut fg, &mut oracle, phi_value);
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
    let phi = strider_ir_test_utils::sentinel_node(
        &mut fg,
        NodeKind::MemPhi,
        [phi_token, arm_a, arm_b],
        [ValueKind::Memory],
    );
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];

    // Mark BOTH stores aliasing: each arm resolves to its own (different)
    // store → the arms disagree → boundary.
    let mut oracle = AliasSet {
        aliasing: vec![arm_a, arm_b],
    };
    let r = run(&mut fg, &mut oracle, phi_value);
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
    let (mut fg, head, _store_mems) = linear_store_chain(DEPTH);
    let r = run(&mut fg, &mut NeverAlias, head);
    assert_clean(&fg, r);
}
