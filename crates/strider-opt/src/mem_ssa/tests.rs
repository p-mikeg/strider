use super::*;
use strider_ir::node::{NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{Function, IRBuilderExt, IRWalker};
use strider_ir_test_utils::make_empty_fn;

/// Aliases exactly the listed memory outputs; every other def is disjoint.
fn alias_set(aliasing: Vec<ValueId>) -> impl FnMut(&Function, NodeId) -> bool {
    move |function: &Function, def: NodeId| {
        let out = function
            .memory_output_of(def)
            .expect("a classified def has a memory output");
        aliasing.contains(&out)
    }
}

fn never_alias() -> impl FnMut(&Function, NodeId) -> bool {
    |_function: &Function, _def: NodeId| false
}

/// Every def a candidate: the climb's answer with no index to lean on.
struct EveryDef<'a>(&'a mut dyn FnMut(&Function, NodeId) -> bool);

impl ClobberProbe for EveryDef<'_> {
    fn clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        (self.0)(function, def)
    }

    fn candidate(&mut self, _layout: &MemLayout, lo: u32, hi: u32) -> Option<u32> {
        (lo <= hi).then_some(hi)
    }
}

/// The dominator climb from the def that produced `start_mem`, asked the way
/// `MemAnalyzer::nearest` asks it: a start the layout does not cover bounds
/// itself.
fn climb(
    fg: &Function,
    clobbers: &mut dyn FnMut(&Function, NodeId) -> bool,
    start_mem: ValueId,
) -> NodeId {
    let layout = MemLayout::build(fg);
    assert!(
        layout.positions().is_some(),
        "a reducible graph is laid out"
    );
    let start = fg.producer(start_mem);
    layout
        .nearest_clobber(fg, start, &mut EveryDef(clobbers), &mut Answers::default())
        .unwrap_or(start)
}

fn assert_clean(fg: &Function, r: NodeId) {
    assert!(
        matches!(*fg.node_kind(r), NodeKind::InitialMemory),
        "expected the clean InitialMemory root, got {:?}",
        fg.node_kind(r),
    );
}

fn empty_chain() -> (Function, ValueId) {
    let fg = make_empty_fn(|b| b.build_int_const(7u64, ValueType::int(64))).unwrap();
    let im = fg
        .walk_kind(|k| matches!(k, NodeKind::InitialMemory))
        .next()
        .expect("InitialMemory must exist");
    let im_value = fg.node_outputs_exact::<1>(im).unwrap()[0];
    (fg, im_value)
}

/// Returns `(function, head_memory_output, store_mems_head_to_tail)`.
fn linear_store_chain(depth: usize) -> (Function, ValueId, Vec<ValueId>) {
    let fg = make_empty_fn(|b| {
        for i in 0..depth {
            let addr = b
                .build_int_const(0x1000u64 + (i as u64) * 8, ValueType::int(64))
                .unwrap();
            let v = b.build_int_const(i as u64, ValueType::int(64)).unwrap();
            b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
        }
        b.build_int_const(7u64, ValueType::int(64))
    })
    .unwrap();
    let ret = fg
        .walk_kind(|k| matches!(k, NodeKind::Return))
        .next()
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
/// store_mems_head_to_tail)`.
fn linear_chain_with_load(depth: usize) -> (Function, NodeId, ValueId, Vec<ValueId>) {
    let fg = make_empty_fn(|b| {
        for i in 0..depth {
            let addr = b.build_int_const(0x1000u64 + (i as u64) * 8, ValueType::int(64))?;
            let v = b.build_int_const(i as u64, ValueType::int(64))?;
            b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        }
        let laddr = b.build_int_const(0x9000u64, ValueType::int(64))?;
        b.build_load(laddr, rsleigh::VnSpace::RAM, ValueType::int(64))
    })
    .unwrap();
    let load = fg
        .walk_kind(|k| matches!(k, NodeKind::Load(_)))
        .next()
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

/// The climb plus the caller-side `narrow_load_to` rewrite.
fn run_load(
    fg: &mut Function,
    clobbers: &mut dyn FnMut(&Function, NodeId) -> bool,
    load: NodeId,
) -> NodeId {
    let mem = fg.node_inputs(load)[0];
    let clobber = climb(fg, clobbers, mem);
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

    let mut walker = alias_set(vec![furthest]);
    let r = run_load(&mut fg, &mut walker, load);
    assert_eq!(
        r,
        fg.producer(furthest),
        "nearest clobber is the furthest store"
    );

    assert_eq!(
        fg.node_inputs(load)[0],
        furthest,
        "load memory edge narrowed onto the nearest clobber",
    );
}

#[test]
fn narrowing_is_idempotent() {
    let (mut fg, load, _head, store_mems) = linear_chain_with_load(3);
    let furthest = *store_mems.last().unwrap();
    let mut walker = alias_set(vec![furthest]);

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
        let addr = b.build_int_const(0x10u64, ValueType::int(64))?;
        let v = b.build_int_const(0x42u64, ValueType::int(64))?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, ValueType::int(64))
    })
    .unwrap();
    let im_node = fg
        .walk_kind(|k| matches!(k, NodeKind::InitialMemory))
        .next()
        .expect("InitialMemory must exist");
    let store_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Store(_)))
        .next()
        .expect("Store must exist");
    let region_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Region))
        .next()
        .expect("Region must exist");
    let im = fg.node_outputs_exact::<1>(im_node).unwrap()[0];
    let store_mem = fg.node_outputs_exact::<1>(store_node).unwrap()[0];
    let phi_token = fg.node_outputs(region_node)[1];
    (fg, im, store_mem, phi_token)
}

fn mk_const(fg: &mut Function, v: u64) -> ValueId {
    let const_id = fg.intern_int_const(u128::from(v), ValueType::int(64));
    let n = strider_ir_test_utils::sentinel_node(
        fg,
        NodeKind::IntConst(const_id),
        [],
        [ValueKind::Typed(ValueType::int(64))],
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
        [ValueKind::Typed(ValueType::int(64))],
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

    let mut walker = alias_set(vec![store_dom_mem]);
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

    let mut walker = alias_set(vec![store_inner_mem]);
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
    let (fg, im_value) = empty_chain();
    let r = climb(&fg, &mut never_alias(), im_value);
    assert_clean(&fg, r);
}

#[test]
fn linear_chain_finds_nearest_aliasing_store() {
    let (fg, head, store_mems) = linear_store_chain(4);
    assert_eq!(store_mems.len(), 4, "four stores in the chain");
    // Second-from-head aliases, so the walk must skip the first store.
    let nearest = store_mems[1];
    let mut walker = alias_set(vec![nearest]);
    let r = climb(&fg, &mut walker, head);
    assert_eq!(
        r,
        fg.producer(nearest),
        "nearest aliasing store is the clobber"
    );
}

#[test]
fn linear_chain_all_clean_returns_none() {
    let (fg, head, _store_mems) = linear_store_chain(5);
    let r = climb(&fg, &mut never_alias(), head);
    assert_clean(&fg, r);
}

/// One Store, so a Region exists to hang a `MemPhi` on; the phi's `n_arms`
/// predecessors all route straight to `InitialMemory`.
fn mem_phi_all_initial(n_arms: usize) -> (Function, ValueId) {
    let mut fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x100u64, ValueType::int(64))?;
        let v = b.build_int_const(0x42u64, ValueType::int(64))?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, ValueType::int(64))
    })
    .unwrap();
    let im_node = fg
        .walk_kind(|k| matches!(k, NodeKind::InitialMemory))
        .next()
        .expect("InitialMemory must exist");
    let region_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Region))
        .next()
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

/// An armless `MemPhi` joins nothing, so no path under it reaches
/// `InitialMemory` and the climb has no clean bottom to name.  It must answer
/// conservatively instead of panicking.
#[test]
fn armless_mem_phi_answers_at_the_chain_start() {
    let (fg, phi_value) = mem_phi_all_initial(0);
    let r = climb(&fg, &mut never_alias(), phi_value);
    assert_eq!(
        r,
        fg.producer(phi_value),
        "with nothing proven the query stops where it started",
    );
}

/// A chain hanging off an armless `MemPhi` bounds itself at its start even
/// though a clobber sits above it: conservative, and unreachable from valid
/// IR, so the query asserts the cone is clean rather than leaning on it.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "holds a clobber")]
fn a_clobber_above_an_uncovered_start_is_asserted() {
    let (mut fg, phi_value) = mem_phi_all_initial(0);
    let (a, d) = (mk_const(&mut fg, 0x30), mk_const(&mut fg, 0x31));
    let store_a = mk_store(&mut fg, phi_value, a, d);
    let store_b = mk_store(&mut fg, store_a, a, d);
    climb(&fg, &mut alias_set(vec![store_a]), store_b);
}

#[test]
fn mem_phi_all_arms_clean_returns_none() {
    let (fg, phi_value) = mem_phi_all_initial(3);
    let r = climb(&fg, &mut never_alias(), phi_value);
    assert_clean(&fg, r);
}

#[test]
fn mem_phi_disagreeing_arms_returns_phi_boundary() {
    // Two arms: one through an aliasing Store, one through InitialMemory.
    let mut fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x200u64, ValueType::int(64))?;
        let v = b.build_int_const(0x99u64, ValueType::int(64))?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, ValueType::int(64))
    })
    .unwrap();
    let im_node = fg
        .walk_kind(|k| matches!(k, NodeKind::InitialMemory))
        .next()
        .unwrap();
    let store_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Store(_)))
        .next()
        .unwrap();
    let region_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Region))
        .next()
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

    let mut walker = alias_set(vec![store_mem]);
    let r = climb(&fg, &mut walker, phi_value);
    assert_eq!(
        r,
        fg.producer(phi_value),
        "a MemPhi whose arms disagree (one clobbers, one clean) is itself the boundary",
    );
}

#[test]
fn mem_phi_agreeing_arms_pass_through_to_shared_store() {
    let mut fg = make_empty_fn(|b| {
        let addr = b.build_int_const(0x300u64, ValueType::int(64))?;
        let v = b.build_int_const(0x77u64, ValueType::int(64))?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
        b.build_int_const(7u64, ValueType::int(64))
    })
    .unwrap();
    let store_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Store(_)))
        .next()
        .unwrap();
    let region_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Region))
        .next()
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

    let mut walker = alias_set(vec![store_mem]);
    let r = climb(&fg, &mut walker, phi_value);
    assert_eq!(
        r,
        fg.producer(store_mem),
        "agreeing MemPhi arms pass through to the shared dominating store",
    );
}

#[test]
fn mem_phi_different_clobbers_per_arm_returns_phi_boundary() {
    let (fg, _head, store_mems) = linear_store_chain(2);
    let mut fg = fg;
    let region_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Region))
        .next()
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

    let mut walker = alias_set(vec![arm_a, arm_b]);
    let r = climb(&fg, &mut walker, phi_value);
    assert_eq!(
        r,
        fg.producer(phi_value),
        "per-arm different clobbers disagree -> the MemPhi is the boundary",
    );
}

#[test]
fn call_on_chain_is_the_nearest_clobber() {
    // InitialMemory <- Store(disjoint) <- CallOther(clobbering) <- load.
    let (mut fg, _im, store_mem, _phi_token) = base_with_store();
    let region_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Region))
        .next()
        .expect("Region must exist");
    let control = fg.node_outputs(region_node)[0];
    let call_mem = mk_call_other(&mut fg, control, store_mem);
    let a = mk_const(&mut fg, 0x40);
    let load = mk_load(&mut fg, call_mem, a);

    let mut walker = alias_set(vec![call_mem]);
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

#[test]
fn mem_phi_call_arm_disagrees_returns_phi_boundary() {
    let (mut fg, im, _store_mem, phi_token) = base_with_store();
    let region_node = fg
        .walk_kind(|k| matches!(k, NodeKind::Region))
        .next()
        .expect("Region must exist");
    let control = fg.node_outputs(region_node)[0];
    // arm 0: a clobbering CallOther rooted at InitialMemory.  arm 1: clean.
    let call_mem = mk_call_other(&mut fg, control, im);
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[call_mem, im]);

    let mut walker = alias_set(vec![call_mem]);
    let r = climb(&fg, &mut walker, phi_mem);
    assert_eq!(
        r,
        fg.producer(phi_mem),
        "a MemPhi whose arms disagree (a clobbering Call vs a clean arm) is the boundary",
    );
}

/// Loop-carried memory chain; returns
/// `(function, entry_store_mem, load_node, phi_value)`.
///
/// The phi's arms are `InitialMemory` and a `Store` consuming the phi's own
/// memory output, a genuine back-edge, so resolving the phi re-encounters it.
/// `entry_store_mem` sits below the merge, on the load's chain.
fn cyclic_loop_chain() -> (Function, ValueId, NodeId, ValueId) {
    use strider_ir::IRViewer;
    let (mut fg, im, _store_mem, phi_token) = base_with_store();
    // arm0 = im (entry edge), arm1 = placeholder, rewired below.
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[im, im]);
    let phi_node = fg.producer(phi_mem);
    // Below the merge, on the load's chain: consumes the phi output.
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

/// A load after a loop whose body writes only a disjoint slot forwards through
/// the loop-header `MemPhi` to the dominating store: the back-edge arm resolves
/// to `Cycle`, a don't-care.
#[test]
fn loop_header_phi_back_edge_is_dropped_not_a_disagreement() {
    use strider_ir::IRViewer;
    // MemPhi[dom_store (entry), back_store (loop body, disjoint)]; load reads
    // the phi output, i.e. sits after the loop.
    let (mut fg, _im, store_dom_mem, phi_token) = base_with_store();
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[store_dom_mem, store_dom_mem]);
    let phi_node = fg.producer(phi_mem);
    let ba = mk_const(&mut fg, 0x77);
    let bd = mk_const(&mut fg, 0x88);
    // The loop body consumes the phi output and feeds its own back-edge arm.
    let back_store_mem = mk_store(&mut fg, phi_mem, ba, bd);
    let use_id = fg.node_input_id_at(phi_node, 2).unwrap();
    fg.graph_mut().update_input(use_id, back_store_mem);
    let la = mk_const(&mut fg, 0x20);
    let load = mk_load(&mut fg, phi_mem, la);

    let mut walker = alias_set(vec![store_dom_mem]);
    let r = run_load(&mut fg, &mut walker, load);
    assert_eq!(
        r,
        fg.producer(store_dom_mem),
        "the load must forward through the loop-header phi to the dominating \
         store (back-edge is a don't-care), got {:?}",
        fg.node_kind(r),
    );
}

/// A loop-header `MemPhi` feeding back to its own output must not diverge the
/// climb, with or without a real clobber present.
#[test]
fn cyclic_loop_header_phi_terminates() {
    // All-clean: cut the cycle and reach InitialMemory.
    let (fg, _entry_store_mem, _load, phi_value) = cyclic_loop_chain();
    let r_clean = climb(&fg, &mut never_alias(), phi_value);
    assert_clean(&fg, r_clean);

    // One aliasing store below the merge: terminates with a clobber too.
    let (mut fg, entry_store_mem, load, _phi_value) = cyclic_loop_chain();
    let mut walker = alias_set(vec![entry_store_mem]);
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

/// A loop header `outer` whose arms are an exit edge out of an inner loop's
/// body `inner_body`, the inner header `inner`, and its own back-edge.  The
/// inner loop is closed by the time the exit edge is read, and `inner`'s entry
/// arm reaches `InitialMemory`.  Returns `(function, outer)`.
fn loop_exit_from_a_closed_inner_loop() -> (Function, ValueId) {
    let (mut fg, im, _store_mem, phi_token) = base_with_store();
    let inner = mk_mem_phi(&mut fg, phi_token, &[im, im]);
    let (a, d) = (mk_const(&mut fg, 0x77), mk_const(&mut fg, 0x88));
    let inner_body = mk_store(&mut fg, inner, a, d);
    let use_id = fg.node_input_id_at(fg.producer(inner), 2).unwrap();
    fg.graph_mut().update_input(use_id, inner_body);
    let outer = mk_mem_phi(&mut fg, phi_token, &[inner_body, inner, im]);
    let outer_body = mk_store(&mut fg, outer, a, d);
    let use_id = fg.node_input_id_at(fg.producer(outer), 3).unwrap();
    fg.graph_mut().update_input(use_id, outer_body);
    (fg, outer)
}

/// The climb has no walk history to degrade `inner_body` with: every path out
/// of the closed inner loop is clean.
#[test]
fn the_climb_does_not_degrade_a_closed_inner_loop() {
    let (fg, outer) = loop_exit_from_a_closed_inner_loop();
    let r = climb(&fg, &mut never_alias(), outer);
    assert_clean(&fg, r);
}

#[test]
fn the_climb_steps_through_a_loop_back_edge_to_the_dominating_store() {
    let (mut fg, _im, store_dom_mem, phi_token) = base_with_store();
    let phi_mem = mk_mem_phi(&mut fg, phi_token, &[store_dom_mem, store_dom_mem]);
    let (ba, bd) = (mk_const(&mut fg, 0x77), mk_const(&mut fg, 0x88));
    let back_store_mem = mk_store(&mut fg, phi_mem, ba, bd);
    let use_id = fg.node_input_id_at(fg.producer(phi_mem), 2).unwrap();
    fg.graph_mut().update_input(use_id, back_store_mem);
    let r = climb(&fg, &mut alias_set(vec![store_dom_mem]), phi_mem);
    assert_eq!(r, fg.producer(store_dom_mem));
}

/// An inner loop header whose entry arm leads to the outer header: the outer
/// back edge reaches the inner header again, which is a cycle, not a value.
#[test]
fn the_climb_resolves_a_query_at_an_inner_loop_header_through_the_outer_loop() {
    let (mut fg, im, _store, phi_token) = base_with_store();
    let (a, d) = (mk_const(&mut fg, 0x20), mk_const(&mut fg, 0x21));
    let entry_store = mk_store(&mut fg, im, a, d);
    let outer = mk_mem_phi(&mut fg, phi_token, &[entry_store, entry_store]);
    let pre_inner = mk_store(&mut fg, outer, a, d);
    let inner = mk_mem_phi(&mut fg, phi_token, &[pre_inner, pre_inner]);
    let inner_body = mk_store(&mut fg, inner, a, d);
    let use_id = fg.node_input_id_at(fg.producer(inner), 2).unwrap();
    fg.graph_mut().update_input(use_id, inner_body);
    let outer_latch = mk_store(&mut fg, inner, a, d);
    let use_id = fg.node_input_id_at(fg.producer(outer), 2).unwrap();
    fg.graph_mut().update_input(use_id, outer_latch);
    let r = climb(&fg, &mut alias_set(vec![entry_store]), inner);
    assert_eq!(r, fg.producer(entry_store), "got {:?}", fg.node_kind(r));
}

/// Two loops entered at each other's bodies: no header dominates the cycle.
#[test]
fn an_irreducible_loop_is_not_laid_out() {
    let (mut fg, im, _store, phi_token) = base_with_store();
    let (a, d) = (mk_const(&mut fg, 0x20), mk_const(&mut fg, 0x21));
    let left_in = mk_store(&mut fg, im, a, d);
    let right_in = mk_store(&mut fg, im, a, d);
    let left = mk_mem_phi(&mut fg, phi_token, &[left_in, left_in]);
    let right = mk_mem_phi(&mut fg, phi_token, &[right_in, left]);
    let use_id = fg.node_input_id_at(fg.producer(left), 2).unwrap();
    fg.graph_mut().update_input(use_id, right);
    assert!(MemLayout::build(&fg).positions().is_none());
}

/// Random structured memory graphs: sequences of stores, merges of two or
/// three arms, top-tested and bottom-tested loops, and loops left from the
/// middle of their body.
struct ShapeGen {
    fg: Function,
    token: ValueId,
    rng: crate::test_support::Rng,
    slots: Vec<ValueId>,
    allow_loops: bool,
    values: Vec<ValueId>,
}

impl ShapeGen {
    fn new(seed: u64, allow_loops: bool) -> (Self, ValueId) {
        let (mut fg, im, _store, token) = base_with_store();
        let slots = (0..3).map(|k| mk_const(&mut fg, 0x100 + k)).collect();
        let gen_ = Self {
            fg,
            token,
            rng: crate::test_support::Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
            slots,
            allow_loops,
            values: vec![im],
        };
        (gen_, im)
    }

    fn next(&mut self, bound: u64) -> u64 {
        self.rng.next_u64() % bound
    }

    fn phi(&mut self, arms: &[ValueId]) -> ValueId {
        let v = mk_mem_phi(&mut self.fg, self.token, arms);
        self.values.push(v);
        v
    }

    /// A loop header whose back arm is filled in later.
    fn open_loop(&mut self, entry: ValueId) -> ValueId {
        self.phi(&[entry, entry])
    }

    fn close_loop(&mut self, header: ValueId, latch: ValueId) {
        let use_id = self
            .fg
            .node_input_id_at(self.fg.producer(header), 2)
            .unwrap();
        self.fg.graph_mut().update_input(use_id, latch);
    }

    fn body(&mut self, mem: ValueId, depth: u32) -> ValueId {
        let mut m = mem;
        for _ in 0..=self.next(3) {
            let nested = depth < 4;
            m = match self.next(8) {
                2 if nested => {
                    let (a, b) = (self.body(m, depth + 1), self.body(m, depth + 1));
                    self.phi(&[a, b])
                }
                3 if nested => {
                    let (a, b, c) = (
                        self.body(m, depth + 1),
                        self.body(m, depth + 1),
                        self.body(m, depth + 1),
                    );
                    self.phi(&[a, b, c])
                }
                4 if nested && self.allow_loops => {
                    let header = self.open_loop(m);
                    let latch = self.body(header, depth + 1);
                    self.close_loop(header, latch);
                    header
                }
                5 if nested && self.allow_loops => {
                    let header = self.open_loop(m);
                    let latch = self.body(header, depth + 1);
                    self.close_loop(header, latch);
                    latch
                }
                6 if nested && self.allow_loops => {
                    let header = self.open_loop(m);
                    let middle = self.body(header, depth + 1);
                    let latch = self.body(middle, depth + 1);
                    self.close_loop(header, latch);
                    self.phi(&[header, middle])
                }
                _ => {
                    let pick = self.next(3) as usize;
                    let slot = self.slots[pick];
                    let value = self.next(4);
                    let data = mk_const(&mut self.fg, value);
                    let v = mk_store(&mut self.fg, m, slot, data);
                    self.values.push(v);
                    v
                }
            };
        }
        m
    }

    /// Every third merge's LAST arm becomes an edge to an arbitrary def, which
    /// may enter a loop away from its header. The first arm is left alone, so
    /// every def stays reachable from `InitialMemory`.
    fn rewire_every_third_merge(&mut self) {
        let phis: Vec<ValueId> = self
            .values
            .iter()
            .copied()
            .filter(|&v| matches!(self.fg.node_kind(self.fg.producer(v)), NodeKind::MemPhi))
            .collect();
        for (i, &p) in phis.iter().enumerate().filter(|(i, _)| i % 3 == 0) {
            let node = self.fg.producer(p);
            let arms = self.fg.phi_data_inputs(node).count();
            let target = self.values[(i * 13 + 7) % self.values.len()];
            let use_id = self
                .fg
                .node_input_id_at(node, arms)
                .expect("a merge has an arm at its own arity");
            self.fg.graph_mut().update_input(use_id, target);
        }
    }
}

/// The documented answer, computed the long way: the nearest dominator of the
/// start that clobbers or is a `MemPhi` with a clobber in its region, over
/// dominators found by the textbook iteration.
struct ByDefinition {
    preds: rustc_hash::FxHashMap<NodeId, Vec<NodeId>>,
    idom: rustc_hash::FxHashMap<NodeId, NodeId>,
}

impl ByDefinition {
    fn new(fg: &Function, values: &[ValueId]) -> Self {
        let nodes: Vec<NodeId> = values.iter().map(|&v| fg.producer(v)).collect();
        let preds: rustc_hash::FxHashMap<NodeId, Vec<NodeId>> = nodes
            .iter()
            .map(|&n| {
                let ps = match fg.node_kind(n) {
                    NodeKind::MemPhi => fg.phi_data_inputs(n).map(|v| fg.producer(v)).collect(),
                    NodeKind::InitialMemory => Vec::new(),
                    _ => fg
                        .memory_input_of(n)
                        .into_iter()
                        .map(|v| fg.producer(v))
                        .collect(),
                };
                (n, ps)
            })
            .collect();
        let root = nodes[0];
        // Reverse postorder from the root over consumers.
        let mut succs: rustc_hash::FxHashMap<NodeId, Vec<NodeId>> = Default::default();
        for (&n, ps) in &preds {
            for &p in ps {
                succs.entry(p).or_default().push(n);
            }
        }
        let mut post = Vec::new();
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack = vec![(root, 0usize)];
        seen.insert(root);
        while let Some((n, i)) = stack.last().copied() {
            let next = succs.get(&n).and_then(|s| s.get(i)).copied();
            stack.last_mut().expect("non-empty").1 += 1;
            match next {
                Some(m) if seen.insert(m) => stack.push((m, 0)),
                Some(_) => {}
                None => {
                    post.push(n);
                    stack.pop();
                }
            }
        }
        let order: rustc_hash::FxHashMap<NodeId, usize> =
            post.iter().enumerate().map(|(i, &n)| (n, i)).collect();
        let mut idom: rustc_hash::FxHashMap<NodeId, NodeId> = Default::default();
        idom.insert(root, root);
        let mut changed = true;
        while changed {
            changed = false;
            for &n in post.iter().rev().skip(1) {
                let mut new: Option<NodeId> = None;
                for &p in &preds[&n] {
                    if !idom.contains_key(&p) {
                        continue;
                    }
                    new = Some(match new {
                        None => p,
                        Some(mut q) => {
                            let mut p = p;
                            while p != q {
                                while order[&p] < order[&q] {
                                    p = idom[&p];
                                }
                                while order[&q] < order[&p] {
                                    q = idom[&q];
                                }
                            }
                            p
                        }
                    });
                }
                let new = new.expect("a reachable node has a processed predecessor");
                if idom.get(&n) != Some(&new) {
                    idom.insert(n, new);
                    changed = true;
                }
            }
        }
        Self { preds, idom }
    }

    fn nearest(
        &self,
        fg: &Function,
        start: NodeId,
        clobbers: &mut dyn FnMut(&Function, NodeId) -> bool,
    ) -> NodeId {
        let mut x = start;
        loop {
            match fg.node_kind(x) {
                NodeKind::InitialMemory => return x,
                NodeKind::MemPhi => {
                    let stop = self.idom[&x];
                    let mut region = vec![x];
                    let mut seen = rustc_hash::FxHashSet::default();
                    let mut dirty = false;
                    while let Some(y) = region.pop() {
                        for &p in &self.preds[&y] {
                            if p != stop && p != x && seen.insert(p) {
                                region.push(p);
                                let def = !matches!(
                                    fg.node_kind(p),
                                    NodeKind::MemPhi | NodeKind::InitialMemory
                                );
                                dirty |= def && clobbers(fg, p);
                            }
                        }
                    }
                    if dirty {
                        return x;
                    }
                    x = stop;
                }
                _ => {
                    if clobbers(fg, x) {
                        return x;
                    }
                    x = self.idom[&x];
                }
            }
        }
    }
}

fn store_at(slot: ValueId) -> impl FnMut(&Function, NodeId) -> bool {
    move |f: &Function, def: NodeId| {
        matches!(f.node_kind(def), NodeKind::Store(_)) && f.store_addr(def) == slot
    }
}

/// The memory predecessors of one def, the direction the walk takes.
fn mem_preds(fg: &Function, n: NodeId) -> Vec<NodeId> {
    match fg.node_kind(n) {
        NodeKind::MemPhi => fg.phi_data_inputs(n).map(|v| fg.producer(v)).collect(),
        NodeKind::InitialMemory => Vec::new(),
        _ => fg
            .memory_input_of(n)
            .into_iter()
            .map(|v| fg.producer(v))
            .collect(),
    }
}

/// The generated defs and their memory successors by index, the form the
/// brute-force oracles take.  The fixture's own store, left out of `values`,
/// is a sink off `InitialMemory` and closes no cycle.
fn succ_graph(fg: &Function, values: &[ValueId]) -> (Vec<NodeId>, Vec<Vec<u32>>) {
    let nodes: Vec<NodeId> = values.iter().map(|&v| fg.producer(v)).collect();
    let mut succs = vec![Vec::new(); nodes.len()];
    for (i, &n) in nodes.iter().enumerate() {
        for p in mem_preds(fg, n) {
            let at = nodes.iter().position(|&m| m == p).expect("a generated def");
            succs[at].push(i as u32);
        }
    }
    (nodes, succs)
}

/// One nearest-clobber answer is sound iff no def that clobbers the slot and no
/// `InitialMemory` but `answer` itself reaches `start` without passing it.
fn assert_sound(
    fg: &Function,
    start: NodeId,
    answer: NodeId,
    clobbers: &mut dyn FnMut(&Function, NodeId) -> bool,
    seed: u64,
) {
    let mut stack = vec![start];
    let mut seen = rustc_hash::FxHashSet::default();
    while let Some(y) = stack.pop() {
        if y == answer || !seen.insert(y) {
            continue;
        }
        match fg.node_kind(y) {
            NodeKind::InitialMemory => {
                panic!("seed {seed}: InitialMemory {y:?} reaches {start:?} past {answer:?}")
            }
            NodeKind::MemPhi => {}
            _ => assert!(
                !clobbers(fg, y),
                "seed {seed}: clobber {y:?} reaches {start:?} past {answer:?}"
            ),
        }
        stack.extend(mem_preds(fg, y));
    }
}

/// The climb names what its definition names on the graphs that have no
/// position index, from every def and for every slot, asked through the
/// [`MemLayout::nearest_clobber`] production calls rather than the slow climb
/// itself.  [`ByDefinition`]'s dominators are the textbook iteration, which
/// converges on an irreducible graph too; every equality here rests on that,
/// so it is checked against the brute-force ones rather than assumed.
#[test]
fn the_climb_matches_its_definition_on_random_irreducible_graphs() {
    use strider_ir_test_utils::graph_oracle;
    let mut irreducible = 0;
    for seed in 0..300 {
        let (mut shape, im) = ShapeGen::new(seed, true);
        shape.body(im, 0);
        shape.rewire_every_third_merge();
        let ShapeGen {
            fg, slots, values, ..
        } = shape;
        let layout = MemLayout::build(&fg);
        if layout.positions().is_some() {
            continue;
        }
        irreducible += 1;
        let definition = ByDefinition::new(&fg, &values);
        let (nodes, succs) = succ_graph(&fg, &values);
        let oracle = graph_oracle::idoms(&graph_oracle::dom_matrix(&succs, 0), 0);
        for (i, &n) in nodes.iter().enumerate() {
            let want = oracle[i].map_or(nodes[0], |d| nodes[d as usize]);
            assert_eq!(definition.idom[&n], want, "seed {seed}: the idom of {n:?}");
        }
        for &slot in &slots {
            let mut answers = Answers::default();
            for &v in &values {
                let start = fg.producer(v);
                let mut clobbers = store_at(slot);
                let got = layout
                    .nearest_clobber(&fg, start, &mut EveryDef(&mut clobbers), &mut answers)
                    .expect("every generated def is laid out");
                let want = definition.nearest(&fg, start, &mut store_at(slot));
                assert_eq!(got, want, "seed {seed}: from {start:?}");
                assert_sound(&fg, start, got, &mut store_at(slot), seed);
            }
        }
    }
    assert!(
        irreducible > 150,
        "{irreducible} of 300 graphs have no position index, too few to pin the slow climb"
    );
}

/// The climb names what its definition names, from every def and for every
/// slot, over every random graph a layout covers, rewired shapes included.
#[test]
fn the_climb_matches_its_definition_on_random_graphs() {
    let (mut laid_out, mut rewired_laid_out) = (0, 0);
    for seed in 0..300 {
        let allow_loops = seed % 3 != 0;
        let rewired = seed % 2 == 0;
        let (mut shape, im) = ShapeGen::new(seed, allow_loops);
        shape.body(im, 0);
        if rewired {
            shape.rewire_every_third_merge();
        }
        let ShapeGen {
            fg, slots, values, ..
        } = shape;
        // A rewired merge can enter a loop away from its header, which has no
        // position index; the irreducible corpus covers those.
        let layout = MemLayout::build(&fg);
        if layout.positions().is_none() {
            assert!(rewired, "seed {seed}: a structured graph is reducible");
            continue;
        }
        laid_out += 1;
        rewired_laid_out += u32::from(rewired);
        let definition = ByDefinition::new(&fg, &values);
        for &slot in &slots {
            let mut answers = Answers::default();
            for &v in &values {
                let start = fg.producer(v);
                let mut clobbers = store_at(slot);
                let got = layout
                    .nearest_clobber(&fg, start, &mut EveryDef(&mut clobbers), &mut answers)
                    .expect("every generated def is laid out");
                let want = definition.nearest(&fg, start, &mut store_at(slot));
                assert_eq!(got, want, "seed {seed}: from {start:?}");
                assert_sound(&fg, start, got, &mut store_at(slot), seed);
            }
        }
    }
    assert!(
        laid_out > 200 && rewired_laid_out > 50,
        "{laid_out} graphs laid out, {rewired_laid_out} of them rewired"
    );
}

/// `build` lays out exactly the reducible memory graphs. The edges come from
/// `memory_input_of` and `phi_data_inputs`, as in production; reducibility
/// over them is by definition.
#[test]
fn a_layout_exists_iff_the_memory_graph_is_reducible() {
    use strider_ir_test_utils::graph_oracle;
    let seeds = if cfg!(debug_assertions) { 60 } else { 2_000 };
    let (mut irreducible, mut rewired_reducible_loops, mut reducible_loops, mut acyclic) =
        (0, 0, 0, 0);
    for seed in 0..seeds {
        let (mut shape, im) = ShapeGen::new(seed, true);
        shape.body(im, 0);
        if seed % 2 == 0 {
            shape.rewire_every_third_merge();
        }
        let (nodes, succs) = succ_graph(&shape.fg, &shape.values);
        let want = graph_oracle::is_reducible(&succs, 0);
        assert_eq!(
            MemLayout::build(&shape.fg).positions().is_some(),
            want,
            "seed {seed}"
        );
        let cyclic = (0..nodes.len() as u32).any(|v| graph_oracle::reaches(&succs, v, v));
        match (want, seed % 2 == 0) {
            (false, _) => irreducible += 1,
            (true, true) if cyclic => rewired_reducible_loops += 1,
            (true, _) if cyclic => reducible_loops += 1,
            (true, _) => acyclic += 1,
        }
    }
    let floor = seeds / 30;
    assert!(
        [
            irreducible,
            rewired_reducible_loops,
            reducible_loops,
            acyclic
        ]
        .iter()
        .all(|&c| c >= floor),
        "floor {floor}: {irreducible} irreducible, {rewired_reducible_loops} rewired \
         reducible with a loop, {reducible_loops} other reducible with a loop, \
         {acyclic} acyclic"
    );
}
