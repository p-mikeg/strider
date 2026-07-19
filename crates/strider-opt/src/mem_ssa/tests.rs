#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use strider_ir::node::{NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{IRBuilderExt, IRWalker};
use strider_ir_test_utils::make_empty_fn;

/// Aliases exactly the listed memory outputs; every other def is disjoint.
struct AliasSet {
    aliasing: Vec<ValueId>,
}
impl MemorySSAWalker for AliasSet {
    fn def_clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        let out = function
            .memory_output_of(def)
            .expect("a classified def has a memory output");
        self.aliasing.contains(&out)
    }
}

/// Drives the "chain reaches InitialMemory clean" path.
struct NeverAlias;
impl MemorySSAWalker for NeverAlias {
    fn def_clobbers(&mut self, _function: &Function, _def: NodeId) -> bool {
        false
    }
}

/// Walk from the def that produced `start_mem`, no narrowing.
fn run<W: MemorySSAWalker>(fg: &mut Function, walker: &mut W, start_mem: ValueId) -> NodeId {
    let start = fg.producer(start_mem);
    walker.find_nearest_clobber(fg, start)
}

fn assert_clean(fg: &Function, r: NodeId) {
    assert!(
        matches!(*fg.node_kind(r), NodeKind::InitialMemory),
        "expected the clean InitialMemory root, got {:?}",
        fg.node_kind(r),
    );
}

fn empty_chain() -> (Function, ValueId) {
    let fg = make_empty_fn(|b| b.build_int_const(7u64, ValueType::I64)).unwrap();
    let im = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::InitialMemory))
        .expect("InitialMemory must exist");
    let im_value = fg.node_outputs_exact::<1>(im).unwrap()[0];
    (fg, im_value)
}

/// Returns `(function, head_memory_output, store_mems_head_to_tail)`.
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
    // Head to tail, i.e. most recent store first.
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

/// Returns `(function, load_node, load_memory_input,
/// store_mems_head_to_tail)`.  The loaded value is returned by the function,
/// so the `Load` stays reachable.
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

/// Walk plus the caller-side narrowing step, so the rewrite is exercised.
fn run_load<W: MemorySSAWalker>(fg: &mut Function, walker: &mut W, load: NodeId) -> NodeId {
    let mem = fg.node_inputs(load)[0];
    let mem_node = fg.producer(mem);
    let clobber = walker.find_nearest_clobber(fg, mem_node);
    let mut ctx = crate::EditFunction::new(fg);
    super::narrow_load_to(&mut ctx, load, clobber);
    clobber
}

#[test]
fn narrows_load_past_disjoint_prefix() {
    // load -> store0(head) -> store1 -> store2 -> InitialMemory, with only
    // store2 (nearest InitialMemory) aliasing.
    let (mut fg, load, head, store_mems) = linear_chain_with_load(3);
    assert_eq!(store_mems.len(), 3, "three stores in the chain");
    assert_eq!(
        fg.node_inputs(load)[0],
        head,
        "load starts at the chain head"
    );
    let furthest = *store_mems.last().unwrap();

    let mut walker = AliasSet {
        aliasing: vec![furthest],
    };
    let r = run_load(&mut fg, &mut walker, load);
    assert_eq!(
        r,
        fg.producer(furthest),
        "nearest clobber is the furthest store"
    );

    // The load's memory input is repointed past the two disjoint stores.
    assert_eq!(
        fg.node_inputs(load)[0],
        furthest,
        "load memory edge narrowed onto the nearest clobber",
    );
}

#[test]
fn narrowing_is_idempotent() {
    // Once narrowed, a second walk finds the same clobber and moves nothing.
    let (mut fg, load, _head, store_mems) = linear_chain_with_load(3);
    let furthest = *store_mems.last().unwrap();
    let mut walker = AliasSet {
        aliasing: vec![furthest],
    };

    let r1 = run_load(&mut fg, &mut walker, load);
    assert_eq!(fg.node_inputs(load)[0], furthest, "narrowed on first walk");
    let r2 = run_load(&mut fg, &mut walker, load);
    assert_eq!(r1, r2, "same nearest clobber on the second walk");
    assert_eq!(fg.node_inputs(load)[0], furthest, "no further movement");
}

/// Scaffold the grafted-phi tests grow a `MemPhi` and a load onto.  Returns
/// `(function, initial_memory_output, store_memory_output, region_phi_token)`.
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

fn mk_const(fg: &mut Function, v: u64) -> ValueId {
    let const_id = fg.intern_int_const(u128::from(v), ValueType::I64);
    let n = strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::IntConst(const_id),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    fg.node_outputs_exact::<1>(n).unwrap()[0]
}

fn mk_store(fg: &mut Function, mem: ValueId, addr: ValueId, data: ValueId) -> ValueId {
    let n = strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::Store(rsleigh::VnSpace::RAM),
        [mem, addr, data],
        [ValueKind::Memory],
    );
    fg.node_outputs_exact::<1>(n).unwrap()[0]
}

/// Returns the load NODE, not its value output.
fn mk_load(fg: &mut Function, mem: ValueId, addr: ValueId) -> NodeId {
    strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::Load(rsleigh::VnSpace::RAM),
        [mem, addr],
        [ValueKind::Typed(ValueType::I64)],
    )
}

/// Returns the CallOther's memory output (slot 1).
fn mk_call_other(fg: &mut Function, control: ValueId, mem: ValueId) -> ValueId {
    let n = strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::CallOther { user_op_id: 0 },
        [control, mem],
        [ValueKind::Control, ValueKind::Memory],
    );
    fg.memory_output_of(n)
        .expect("CallOther has a memory output")
}

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
    // load -> store_outer(disjoint) -> MemPhi[store_dom, store_dom].  Agreeing
    // arms make the phi transparent, so the load jumps past both it and the
    // disjoint outer store onto store_dom.
    let (mut fg, _im, store_dom_mem, phi_token) = base_with_store();
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[store_dom_mem, store_dom_mem]);
    let a2 = mk_const(&mut fg, 0x20);
    let d2 = mk_const(&mut fg, 0xaa);
    let store_outer_mem = mk_store(&mut fg, phi_mem, a2, d2);
    let a3 = mk_const(&mut fg, 0x30);
    let load = mk_load(&mut fg, store_outer_mem, a3);

    // store_dom aliases, store_outer is disjoint.
    let mut walker = AliasSet {
        aliasing: vec![store_dom_mem],
    };
    let r = run_load(&mut fg, &mut walker, load);
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
    // load -> store_outer(disjoint) -> MemPhi[store_inner(aliasing), im].
    // One arm clobbers and one is clean, so the phi is the boundary: the load
    // skips the disjoint outer store but never crosses the merge.
    let (mut fg, im, store_inner_mem, phi_token) = base_with_store();
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[store_inner_mem, im]);
    let a2 = mk_const(&mut fg, 0x20);
    let d2 = mk_const(&mut fg, 0xbb);
    let store_outer_mem = mk_store(&mut fg, phi_mem, a2, d2);
    let a3 = mk_const(&mut fg, 0x30);
    let load = mk_load(&mut fg, store_outer_mem, a3);

    // Only the inner (phi-arm) store aliases.
    let mut walker = AliasSet {
        aliasing: vec![store_inner_mem],
    };
    let r = run_load(&mut fg, &mut walker, load);
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
    // Second-from-head aliases, so the walk must skip the first store.
    let nearest = store_mems[1];
    let mut walker = AliasSet {
        aliasing: vec![nearest],
    };
    let r = run(&mut fg, &mut walker, head);
    assert_eq!(
        r,
        fg.producer(nearest),
        "nearest aliasing store is the clobber"
    );
}

#[test]
fn non_aliasing_store_is_skipped() {
    let (mut fg, head, store_mems) = linear_store_chain(3);
    // Only the furthest store aliases, so two nearer stores get skipped.
    let furthest = *store_mems.last().unwrap();
    let mut walker = AliasSet {
        aliasing: vec![furthest],
    };
    let r = run(&mut fg, &mut walker, head);
    assert_eq!(r, fg.producer(furthest), "walk skips non-aliasing stores");
}

#[test]
fn linear_chain_all_clean_returns_none() {
    let (mut fg, head, _store_mems) = linear_store_chain(5);
    let r = run(&mut fg, &mut NeverAlias, head);
    assert_clean(&fg, r);
}

/// One Store, so a Region exists to hang a `MemPhi` on; the phi's `n_arms`
/// predecessors all route straight to `InitialMemory`.
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
    let (mut fg, phi_value) = mem_phi_all_initial(3);
    let r = run(&mut fg, &mut NeverAlias, phi_value);
    assert_clean(&fg, r);
}

#[test]
fn mem_phi_disagreeing_arms_returns_phi_boundary() {
    // Two arms: one through an aliasing Store, one through InitialMemory.
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

    // The arms disagree, so the walk returns the phi's own output rather
    // than the inner store.
    let mut walker = AliasSet {
        aliasing: vec![store_mem],
    };
    let r = run(&mut fg, &mut walker, phi_value);
    assert_eq!(
        r,
        fg.producer(phi_value),
        "a MemPhi whose arms disagree (one clobbers, one clean) is itself the boundary",
    );
}

/// Both arms route to the same aliasing store, so the phi is transparent and
/// the walk returns that store, not the phi.
#[test]
fn mem_phi_agreeing_arms_pass_through_to_shared_store() {
    // Both phi arms are the same ValueId, so they resolve to the same store.
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
    let phi = strider_ir_test_utils::sentinel_node(
        &mut fg,
        NodeKind::MemPhi,
        [phi_token, store_mem, store_mem],
        [ValueKind::Memory],
    );
    let phi_value = fg.node_outputs_exact::<1>(phi).unwrap()[0];

    let mut walker = AliasSet {
        aliasing: vec![store_mem],
    };
    let r = run(&mut fg, &mut walker, phi_value);
    assert_eq!(
        r,
        fg.producer(store_mem),
        "agreeing MemPhi arms pass through to the shared dominating store",
    );
}

/// Arms reaching different aliasing stores disagree, so the phi is the
/// boundary rather than either inner store.
#[test]
fn mem_phi_different_clobbers_per_arm_returns_phi_boundary() {
    // Wire a MemPhi directly to each store's memory output, one per arm.
    let (fg, _head, store_mems) = linear_store_chain(2);
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

    // Both stores alias, so each arm resolves to a different one.
    let mut walker = AliasSet {
        aliasing: vec![arm_a, arm_b],
    };
    let r = run(&mut fg, &mut walker, phi_value);
    assert_eq!(
        r,
        fg.producer(phi_value),
        "per-arm different clobbers disagree → the MemPhi is the boundary",
    );
}

/// A clobbering `CallOther` terminates the walk exactly as a `Store` does.
#[test]
fn call_on_chain_is_the_nearest_clobber() {
    // InitialMemory <- Store(disjoint) <- CallOther(clobbering) <- load.
    let (mut fg, _im, store_mem, _phi_token) = base_with_store();
    let region_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("Region must exist");
    let control = fg.node_outputs(region_node)[0];
    let call_mem = mk_call_other(&mut fg, control, store_mem);
    let a = mk_const(&mut fg, 0x40);
    let load = mk_load(&mut fg, call_mem, a);

    // Only the CallOther clobbers; the store underneath is disjoint.
    let mut walker = AliasSet {
        aliasing: vec![call_mem],
    };
    let r = run_load(&mut fg, &mut walker, load);
    assert_eq!(
        r,
        fg.producer(call_mem),
        "the clobbering CallOther on the chain is the nearest clobber",
    );
    assert!(
        matches!(fg.node_kind(r), NodeKind::CallOther { .. }),
        "nearest clobber must be the CallOther node, got {:?}",
        fg.node_kind(r),
    );
}

/// The Call-on-an-arm case of the disagree rule.
#[test]
fn mem_phi_call_arm_disagrees_returns_phi_boundary() {
    let (mut fg, im, _store_mem, phi_token) = base_with_store();
    let region_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("Region must exist");
    let control = fg.node_outputs(region_node)[0];
    // arm 0: a clobbering CallOther rooted at InitialMemory.  arm 1: clean.
    let call_mem = mk_call_other(&mut fg, control, im);
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[call_mem, im]);

    let mut walker = AliasSet {
        aliasing: vec![call_mem],
    };
    let r = run(&mut fg, &mut walker, phi_mem);
    assert_eq!(
        r,
        fg.producer(phi_mem),
        "a MemPhi whose arms disagree (a clobbering Call vs a clean arm) is the boundary",
    );
}

/// Loop-carried memory chain; returns
/// `(function, entry_store_mem, load_node, phi_value)`.
///
/// ```text
///   InitialMemory ← MemPhi[ im, back_store ]      (loop-header phi)
///                        ↑              │
///                   entry_store ←───────┘ (back_store consumes the phi output)
///   load -> entry_store -> MemPhi -> ... (entry_store's mem input is the phi)
/// ```
/// The phi's second arm is a `Store` consuming the phi's own memory output, a
/// genuine back-edge, so resolving the phi re-encounters it.
fn cyclic_loop_chain() -> (Function, ValueId, NodeId, ValueId) {
    use strider_ir::IRViewer;
    let (mut fg, im, _store_mem, phi_token) = base_with_store();
    // arm0 = im (entry edge), arm1 = placeholder, rewired below.
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[im, im]);
    let phi_node = fg.producer(phi_mem);
    // Sits on the entry arm's live path, reading the phi output.
    let ea = mk_const(&mut fg, 0x10);
    let ed = mk_const(&mut fg, 0x42);
    let entry_store_mem = mk_store(&mut fg, phi_mem, ea, ed);
    // Closes the loop: consumes the phi's output, feeds its second arm.
    let ba = mk_const(&mut fg, 0x77);
    let bd = mk_const(&mut fg, 0x88);
    let back_store_mem = mk_store(&mut fg, phi_mem, ba, bd);
    // arm1 is input slot 2: [phi_token, arm0, arm1].
    let use_id = fg.node_input_id_at(phi_node, 2).unwrap();
    fg.graph_mut().update_input(use_id, back_store_mem);
    let la = mk_const(&mut fg, 0x20);
    let load = mk_load(&mut fg, entry_store_mem, la);
    (fg, entry_store_mem, load, phi_mem)
}

/// A loop-header `MemPhi` feeding back to its own output must not diverge the
/// walk, with or without a real clobber present.
#[test]
fn cyclic_loop_header_phi_terminates() {
    // All-clean: cut the cycle and reach InitialMemory.
    let (mut fg, _entry_store_mem, _load, phi_value) = cyclic_loop_chain();
    let r_clean = run(&mut fg, &mut NeverAlias, phi_value);
    assert_clean(&fg, r_clean);

    // One aliasing store on the entry arm: terminates with a clobber too.
    let (mut fg, entry_store_mem, load, _phi_value) = cyclic_loop_chain();
    let mut walker = AliasSet {
        aliasing: vec![entry_store_mem],
    };
    let r = run_load(&mut fg, &mut walker, load);
    assert_eq!(
        r,
        fg.producer(entry_store_mem),
        "the aliasing store on the non-back arm is the nearest clobber",
    );
    assert!(
        matches!(fg.node_kind(r), NodeKind::Store(_)),
        "nearest clobber must be the entry Store, got {:?}",
        fg.node_kind(r),
    );
}
