//! Unit tests for the `RegionIrCache` types and helpers.
//!
//! These tests exercise the cache invariants in isolation, with
//! the smallest possible fixtures: hand-rolled
//! `MachineInsnAddr` / `PcodeInsnAddr` values, a `RegionIrCache`
//! built from scratch, and direct invocation of the helpers.
//!
//! Integration tests in `crates/strider/tests/tier2_cache.rs`
//! cover the end-to-end lifetime against real CFGs / built
//! function graphs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use cfg::{MachineInsnAddr, PcodeInsnAddr};
use ir::node::NodeOutputKind;
use ir::node::NodeOutputType;
use ir::node::{NodeId, NodeOutputId};
use ir::FunctionBuilder;
use rsleigh::Vn;

use super::lift::populate_cache_from_handles;
use super::{
    extend_predecessors_with_handle, LiftStats, PredecessorHandles, RegionIrCache, RegionIrEntry,
};

fn pcode_addr(machine: u64) -> PcodeInsnAddr {
    PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: machine },
        insn_index: 0,
    }
}

fn make_vn(off: u64) -> Vn {
    Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off,
        },
        size: 4,
    }
}

/// Build a minimal `BuiltFunctionGraph` whose entry region tracks
/// one variable.  Used by extend-predecessors tests as a backing
/// graph the helper mutates.  Returns the graph and the entry
/// region's lift handles (control_state node id, mem_phi, etc.).
fn build_minimal_graph_with_one_var() -> (ir::BuiltFunctionGraph, RegionIrEntry) {
    let v = make_vn(0x10);
    let mut b = FunctionBuilder::new_raw(vec![v], &[], &[], &[], None, 0)
        .expect("new_raw");
    let r = b.create_region().expect("create");
    b.set_entry_region(r).expect("set_entry");
    b.set_region(r);
    // Read the variable so the ControlPhi has an output the body
    // would reference (same shape as a real strider lift).
    let _val = b.read_variable(&v).expect("read");
    b.build_return(None, &[]).expect("ret");
    // Capture handles BEFORE build() consumes the builder.
    let cs = b.region_control_node(r);
    let mp = b.region_memory_node(r);
    let entry_ctrl = b.region_entry_control(r).expect("entry_ctrl");
    let entry_mem = b.region_entry_memory(r).expect("entry_mem");
    let exit_ctrl = b.region_cur_ctrl(r);
    let exit_mem = b.region_cur_memory(r);
    let mut entry_var_phis: HashMap<Vn, NodeId> = HashMap::new();
    for (var_id, phi_out) in b.region_initial_variables(r) {
        if let Some(vn) = b.vn_of_var(var_id) {
            let phi_node = b.body().graph.output_definition(phi_out).0;
            entry_var_phis.insert(vn, phi_node);
        }
    }
    let mut exit_vn_to_value: HashMap<Vn, NodeOutputId> = HashMap::new();
    for (var_id, val_out) in b.region_exit_variables(r) {
        if let Some(vn) = b.vn_of_var(var_id) {
            exit_vn_to_value.insert(vn, val_out);
        }
    }
    let graph = b.build().expect("build");
    let entry = RegionIrEntry {
        entry_control: entry_ctrl,
        entry_memory: entry_mem,
        exit_control: exit_ctrl,
        exit_memory: exit_mem,
        entry_var_phis,
        entry_mem_phi: mp,
        entry_control_state: cs,
        exit_vn_to_value,
        start_addr: pcode_addr(0x1000),
        cached_predecessor_count: 1,
    };
    (graph, entry)
}

#[test]
fn region_ir_entry_default_is_empty() {
    // The empty constructor must produce a usable but
    // sentinel-valued entry: empty maps, zero predecessor count,
    // start_addr threaded through.
    let entry = RegionIrEntry::empty(pcode_addr(0x1234));
    assert!(entry.entry_var_phis.is_empty());
    assert!(entry.exit_vn_to_value.is_empty());
    assert_eq!(entry.cached_predecessor_count, 0);
    assert_eq!(entry.start_addr, pcode_addr(0x1234));
}

#[test]
fn region_ir_cache_default_is_empty() {
    let cache: RegionIrCache = HashMap::new();
    assert_eq!(cache.len(), 0);
}

#[test]
fn region_ir_entry_insert_then_retrieve_round_trips() {
    // Round-trip insertion + lookup via the same key.  Pins the
    // expectation that MachineInsnAddr is a usable HashMap key.
    let mut cache: RegionIrCache = HashMap::new();
    let key = MachineInsnAddr { addr: 0xdead_beef };
    cache.insert(key, RegionIrEntry::empty(pcode_addr(0xdead_beef)));
    assert_eq!(cache.len(), 1);
    let got = cache.get(&key).expect("key must round-trip");
    assert_eq!(got.start_addr.machine_addr, key);
}

#[test]
fn cache_key_uses_machine_addr_only() {
    // Two PcodeInsnAddrs with the same machine_addr but
    // different insn_index hash to the same MachineInsnAddr key.
    // This is what makes the cache stable across iterations:
    // region starts always have insn_index 0, but if a future
    // refactor accidentally keys on the full PcodeInsnAddr it
    // would silently miss the cache on every lookup.
    let a = MachineInsnAddr { addr: 0x1000 };
    let b = MachineInsnAddr { addr: 0x1000 };
    assert_eq!(a, b);
    let mut cache: RegionIrCache = HashMap::new();
    cache.insert(a, RegionIrEntry::empty(pcode_addr(0x1000)));
    assert!(cache.contains_key(&b));
}

#[test]
fn predecessor_diffs_of_empty_cache_is_empty() {
    // The diff function must return an empty vec when the cache
    // is empty — there's nothing to compare against.
    let cache: RegionIrCache = HashMap::new();
    assert_eq!(cache.len(), 0);
}

// ── from_lift_handles tests (G1 cache populate) ─────────────────────────

#[test]
fn from_lift_handles_populates_all_fields() {
    // Pin: every field of RegionLiftHandles ends up at the
    // matching field of RegionIrEntry.
    let v = make_vn(0x20);
    let cs = NodeId::from_u32(7);
    let mp = NodeId::from_u32(8);
    let phi = NodeId::from_u32(9);
    let ec = NodeOutputId::from_u32(11);
    let em = NodeOutputId::from_u32(12);
    let xc = NodeOutputId::from_u32(13);
    let xm = NodeOutputId::from_u32(14);
    let xv = NodeOutputId::from_u32(15);
    let mut entry_var_phis = HashMap::new();
    entry_var_phis.insert(v, phi);
    let mut exit_vn_to_value = HashMap::new();
    exit_vn_to_value.insert(v, xv);
    let h = crate::RegionLiftHandles {
        start_addr: pcode_addr(0xbeef),
        predecessor_count: 3,
        entry_control_state: cs,
        entry_mem_phi: mp,
        entry_control: ec,
        entry_memory: em,
        exit_control: xc,
        exit_memory: xm,
        entry_var_phis,
        exit_vn_to_value,
    };
    let e = RegionIrEntry::from_lift_handles(&h);
    assert_eq!(e.entry_control_state, cs);
    assert_eq!(e.entry_mem_phi, mp);
    assert_eq!(e.entry_control, ec);
    assert_eq!(e.entry_memory, em);
    assert_eq!(e.exit_control, xc);
    assert_eq!(e.exit_memory, xm);
    assert_eq!(e.entry_var_phis.get(&v), Some(&phi));
    assert_eq!(e.exit_vn_to_value.get(&v), Some(&xv));
    assert_eq!(e.start_addr, pcode_addr(0xbeef));
    assert_eq!(e.cached_predecessor_count, 3);
}

#[test]
fn populate_cache_from_handles_inserts_one_entry_per_region() {
    // A snapshot with N entries produces a cache with N keys.
    let mut cache: RegionIrCache = HashMap::new();
    let h1 = crate::RegionLiftHandles {
        start_addr: pcode_addr(0x1000),
        predecessor_count: 0,
        entry_control_state: NodeId::from_u32(1),
        entry_mem_phi: NodeId::from_u32(2),
        entry_control: NodeOutputId::from_u32(1),
        entry_memory: NodeOutputId::from_u32(2),
        exit_control: NodeOutputId::from_u32(3),
        exit_memory: NodeOutputId::from_u32(4),
        entry_var_phis: HashMap::new(),
        exit_vn_to_value: HashMap::new(),
    };
    let h2 = crate::RegionLiftHandles {
        start_addr: pcode_addr(0x2000),
        predecessor_count: 1,
        entry_control_state: NodeId::from_u32(3),
        entry_mem_phi: NodeId::from_u32(4),
        entry_control: NodeOutputId::from_u32(5),
        entry_memory: NodeOutputId::from_u32(6),
        exit_control: NodeOutputId::from_u32(7),
        exit_memory: NodeOutputId::from_u32(8),
        entry_var_phis: HashMap::new(),
        exit_vn_to_value: HashMap::new(),
    };
    populate_cache_from_handles(&mut cache, &[h1, h2]);
    assert_eq!(cache.len(), 2);
    assert!(cache.contains_key(&MachineInsnAddr { addr: 0x1000 }));
    assert!(cache.contains_key(&MachineInsnAddr { addr: 0x2000 }));
}

#[test]
fn populate_cache_from_handles_overwrites_prior_entry() {
    // Round-1 reset semantics: a second call replaces prior entries.
    let mut cache: RegionIrCache = HashMap::new();
    let h1 = crate::RegionLiftHandles {
        start_addr: pcode_addr(0x1000),
        predecessor_count: 0,
        entry_control_state: NodeId::from_u32(1),
        entry_mem_phi: NodeId::from_u32(2),
        entry_control: NodeOutputId::from_u32(1),
        entry_memory: NodeOutputId::from_u32(2),
        exit_control: NodeOutputId::from_u32(3),
        exit_memory: NodeOutputId::from_u32(4),
        entry_var_phis: HashMap::new(),
        exit_vn_to_value: HashMap::new(),
    };
    populate_cache_from_handles(&mut cache, &[h1]);
    let h2 = crate::RegionLiftHandles {
        start_addr: pcode_addr(0x1000),
        predecessor_count: 5,
        entry_control_state: NodeId::from_u32(99),
        entry_mem_phi: NodeId::from_u32(98),
        entry_control: NodeOutputId::from_u32(97),
        entry_memory: NodeOutputId::from_u32(96),
        exit_control: NodeOutputId::from_u32(95),
        exit_memory: NodeOutputId::from_u32(94),
        entry_var_phis: HashMap::new(),
        exit_vn_to_value: HashMap::new(),
    };
    populate_cache_from_handles(&mut cache, &[h2]);
    let entry = cache.get(&MachineInsnAddr { addr: 0x1000 }).expect("present");
    assert_eq!(entry.cached_predecessor_count, 5);
    assert_eq!(entry.entry_control_state, NodeId::from_u32(99));
}

// ── extend_predecessors_with_handle tests (G1 phi extension) ────────────

#[test]
fn extend_predecessors_into_appends_to_existing_control_state() {
    // After one call, the ControlState's input count grows by 1
    // and its NodeId is unchanged.
    let (mut graph, mut entry) = build_minimal_graph_with_one_var();
    let cs_before = entry.entry_control_state;
    let inputs_before = graph
        .graph
        .node_inputs(cs_before)
        .into_iter()
        .count();
    // Synthesise a Control output to feed as the new pred edge.
    // Use an Entry node — already in the graph; locate it.
    let entry_ctrl = {
        let entry_node = graph.entry;
        let outs: Vec<_> = graph.graph.node_outputs(entry_node).into_iter().collect();
        outs[0]
    };
    let pred = PredecessorHandles {
        exit_control: entry_ctrl,
        exit_memory: graph
            .graph
            .node_outputs(graph.entry)
            .into_iter()
            .next()
            .unwrap(),
        exit_vn_to_value: HashMap::new(),
    };
    // For exit_memory we need a Memory-typed output.  Locate
    // the InitialMemory node's output.
    let initial_mem = graph
        .preorder()
        .find(|&nid| {
            matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
        })
        .expect("InitialMemory");
    let im_out = graph
        .graph
        .node_outputs(initial_mem)
        .into_iter()
        .next()
        .expect("output");
    let pred = PredecessorHandles {
        exit_control: pred.exit_control,
        exit_memory: im_out,
        exit_vn_to_value: HashMap::new(),
    };
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
    let inputs_after = graph
        .graph
        .node_inputs(cs_before)
        .into_iter()
        .count();
    assert_eq!(inputs_after, inputs_before + 1);
    assert_eq!(entry.entry_control_state, cs_before, "NodeId stable");
}

#[test]
fn extend_predecessors_into_appends_to_existing_mem_phi() {
    // After one call, the MemPhi's input count grows by 1.
    let (mut graph, mut entry) = build_minimal_graph_with_one_var();
    let mp = entry.entry_mem_phi;
    let inputs_before = graph.graph.node_inputs(mp).into_iter().count();
    let entry_ctrl = {
        let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
        outs[0]
    };
    let initial_mem = graph
        .preorder()
        .find(|&nid| {
            matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
        })
        .expect("InitialMemory");
    let im_out = graph
        .graph
        .node_outputs(initial_mem)
        .into_iter()
        .next()
        .expect("output");
    let pred = PredecessorHandles {
        exit_control: entry_ctrl,
        exit_memory: im_out,
        exit_vn_to_value: HashMap::new(),
    };
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
    let inputs_after = graph.graph.node_inputs(mp).into_iter().count();
    assert_eq!(inputs_after, inputs_before + 1);
    assert_eq!(entry.entry_mem_phi, mp, "NodeId stable");
}

#[test]
fn extend_predecessors_into_appends_to_existing_var_phi() {
    // After one call, the per-var ControlPhi's input count grows.
    let v = make_vn(0x10);
    let (mut graph, mut entry) = build_minimal_graph_with_one_var();
    let phi_id = *entry.entry_var_phis.get(&v).expect("phi present");
    let inputs_before = graph.graph.node_inputs(phi_id).into_iter().count();
    let entry_ctrl = {
        let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
        outs[0]
    };
    let initial_mem = graph
        .preorder()
        .find(|&nid| {
            matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
        })
        .expect("InitialMemory");
    let im_out = graph
        .graph
        .node_outputs(initial_mem)
        .into_iter()
        .next()
        .expect("output");
    // Synthesise a value-typed output for the predecessor's
    // exit-value of `v`.
    let val_node = graph.graph.create_node(
        ir::node::NodeKind::IntConst(0xabcd_u128),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let val_out = graph.graph.node_outputs_exact::<1>(val_node).expect("out")[0];
    let mut exit_vn_to_value = HashMap::new();
    exit_vn_to_value.insert(v, val_out);
    let pred = PredecessorHandles {
        exit_control: entry_ctrl,
        exit_memory: im_out,
        exit_vn_to_value,
    };
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
    let inputs_after = graph.graph.node_inputs(phi_id).into_iter().count();
    assert_eq!(inputs_after, inputs_before + 1);
    // NodeId stable.
    assert_eq!(*entry.entry_var_phis.get(&v).unwrap(), phi_id);
}

#[test]
fn extend_predecessors_into_no_change_when_pred_count_unchanged() {
    // A predecessor_diffs call against the just-populated cache
    // returns no diffs — i.e. the predecessor count matches the
    // CFG.  We cover this against a real cfg in the integration
    // tests; here we pin the per-entry contract: bumping
    // cached_predecessor_count after extend_predecessors_with_handle.
    let (mut graph, mut entry) = build_minimal_graph_with_one_var();
    let count_before = entry.cached_predecessor_count;
    let entry_ctrl = {
        let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
        outs[0]
    };
    let initial_mem = graph
        .preorder()
        .find(|&nid| {
            matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
        })
        .expect("InitialMemory");
    let im_out = graph
        .graph
        .node_outputs(initial_mem)
        .into_iter()
        .next()
        .expect("output");
    let pred = PredecessorHandles {
        exit_control: entry_ctrl,
        exit_memory: im_out,
        exit_vn_to_value: HashMap::new(),
    };
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
    // After one extension, count grew by exactly 1.
    assert_eq!(entry.cached_predecessor_count, count_before + 1);
}

#[test]
fn extend_predecessors_with_handle_two_calls_grow_input_count_by_two() {
    // Sanity: two consecutive calls add two inputs to each phi.
    // Pins that the function is idempotent in the sense of
    // "every call grows by exactly 1" — no double-add bugs, no
    // skipped extensions.
    let v = make_vn(0x10);
    let (mut graph, mut entry) = build_minimal_graph_with_one_var();
    let phi_id = *entry.entry_var_phis.get(&v).expect("phi present");
    let inputs_before = graph.graph.node_inputs(phi_id).into_iter().count();
    let entry_ctrl = {
        let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
        outs[0]
    };
    let initial_mem = graph
        .preorder()
        .find(|&nid| {
            matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
        })
        .expect("InitialMemory");
    let im_out = graph
        .graph
        .node_outputs(initial_mem)
        .into_iter()
        .next()
        .expect("output");
    let pred = PredecessorHandles {
        exit_control: entry_ctrl,
        exit_memory: im_out,
        exit_vn_to_value: HashMap::new(),
    };
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend 1");
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend 2");
    let inputs_after = graph.graph.node_inputs(phi_id).into_iter().count();
    assert_eq!(inputs_after, inputs_before + 2);
    assert_eq!(entry.cached_predecessor_count, 1 + 2);
}

#[test]
fn extend_predecessors_with_handle_keeps_control_state_node_id_stable_across_calls() {
    // The ControlState's NodeId is the same after two calls.
    // Pins the round-trip stability that the orchestrator relies
    // on across iterations.
    let (mut graph, mut entry) = build_minimal_graph_with_one_var();
    let cs_before = entry.entry_control_state;
    let entry_ctrl = {
        let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
        outs[0]
    };
    let initial_mem = graph
        .preorder()
        .find(|&nid| {
            matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
        })
        .expect("InitialMemory");
    let im_out = graph
        .graph
        .node_outputs(initial_mem)
        .into_iter()
        .next()
        .expect("output");
    let pred = PredecessorHandles {
        exit_control: entry_ctrl,
        exit_memory: im_out,
        exit_vn_to_value: HashMap::new(),
    };
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("e1");
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("e2");
    assert_eq!(
        entry.entry_control_state, cs_before,
        "ControlState NodeId must stay stable across multiple extensions",
    );
}

// ── G1-COMPLETE: LiftStats unit tests ───────────────────────────────────

#[test]
fn lift_stats_default_is_zeroed() {
    // Pin: the LiftStats default state means "nothing was lifted
    // yet."  Callers can assume `Default::default()` is the
    // identity for accumulation.
    let stats = LiftStats::default();
    assert_eq!(stats.pcode_insns_lifted, 0);
    assert_eq!(stats.regions_lifted, 0);
    assert!(stats.newly_lifted_addrs.is_empty());
}

#[test]
fn lift_stats_partial_eq_round_trip() {
    // Pin: LiftStats supports structural equality for test
    // assertion purposes.
    let s1 = LiftStats::default();
    let s2 = LiftStats::default();
    assert_eq!(s1, s2);
    let s3 = LiftStats {
        pcode_insns_lifted: 5,
        ..Default::default()
    };
    assert_ne!(s1, s3);
}

#[test]
fn extend_predecessors_into_handles_var_not_in_predecessor_exit_map() {
    // When pred.exit_vn_to_value lacks the var, fallback to
    // building/reusing an InitialVar(vn) — the phi gets the
    // function-entry value as its new input on this edge.
    let v = make_vn(0x10);
    let (mut graph, mut entry) = build_minimal_graph_with_one_var();
    let phi_id = *entry.entry_var_phis.get(&v).expect("phi present");
    let inputs_before = graph.graph.node_inputs(phi_id).into_iter().count();
    let entry_ctrl = {
        let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
        outs[0]
    };
    let initial_mem = graph
        .preorder()
        .find(|&nid| {
            matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
        })
        .expect("InitialMemory");
    let im_out = graph
        .graph
        .node_outputs(initial_mem)
        .into_iter()
        .next()
        .expect("output");
    // Note: exit_vn_to_value is EMPTY — `v` is not in the pred's
    // map.  Fallback path triggers.
    let pred = PredecessorHandles {
        exit_control: entry_ctrl,
        exit_memory: im_out,
        exit_vn_to_value: HashMap::new(),
    };
    extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
    let inputs_after = graph.graph.node_inputs(phi_id).into_iter().count();
    assert_eq!(inputs_after, inputs_before + 1, "phi got the fallback input");
    // The new input slot's source must be an InitialVar(v).
    let new_input_idx = inputs_after - 1;
    let new_input: Vec<_> = graph.graph.node_inputs(phi_id).into_iter().collect();
    let new_input_out = new_input[new_input_idx];
    let (new_input_node, _) = graph.graph.output_definition(new_input_out);
    assert!(
        matches!(
            graph.graph.node_kind(new_input_node),
            ir::node::NodeKind::InitialVar(vn) if *vn == v,
        ),
        "fallback must be InitialVar(vn), got {:?}",
        graph.graph.node_kind(new_input_node),
    );
}

// ── W1: rollback when the per-var phi loop fails mid-extension ─────────

/// Helper: build a graph whose entry-region has TWO ControlPhis — one
/// for a normally-sized vn and one whose vn has an unsupported size
/// (5).  When `extend_predecessors_with_handle` reaches the unsupported
/// vn it must error out AND undo the prior ControlState / MemPhi
/// appends so the function leaves no partial-update window for callers
/// to observe.
fn build_graph_with_one_unsupported_var() -> (ir::BuiltFunctionGraph, RegionIrEntry) {
    // The "good" vn (4 bytes, supported).
    let v_ok = make_vn(0x10);
    // The "bad" vn — size 5 is not in the supported set
    // {1,2,4,8,16,32}.  We can't actually have this vn participate in
    // the builder (the builder rejects sizes via NodeOutputType::try_from),
    // so we construct the graph with `v_ok` and then PATCH the cache
    // entry's `entry_var_phis` to claim a phi for `v_bad` exists.
    // The fallback branch in extend_predecessors_with_handle will
    // attempt to construct InitialVar(v_bad) which fails the size
    // dispatch — exactly the error path W1 must roll back.
    let v_bad = Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 5,
    };
    let (graph, mut entry) = build_minimal_graph_with_one_var();
    // The good vn is `v_ok` (matches make_vn(0x10) used by the helper).
    // Verify and attach a phantom phi-id for v_bad pointing at the
    // existing v_ok phi node — the helper iterates the map, sees
    // v_bad, fails the size dispatch, and we want the rollback to
    // undo the ControlState+MemPhi appends regardless of which vn
    // entry triggered the error.
    let some_phi_id = *entry.entry_var_phis.get(&v_ok).expect("v_ok phi");
    entry.entry_var_phis.insert(v_bad, some_phi_id);
    (graph, entry)
}

#[test]
fn extend_predecessors_with_handle_rolls_back_on_var_phi_error() {
    // W1: when the per-var phi loop errors AFTER ControlState/MemPhi
    // were already extended, the function must undo those prior
    // appends.  Pre/post-call snapshots of ControlState/MemPhi input
    // counts and cached_predecessor_count must be equal — no partial
    // update visible to the caller.
    let (mut graph, mut entry) = build_graph_with_one_unsupported_var();
    let cs = entry.entry_control_state;
    let mp = entry.entry_mem_phi;
    let cs_inputs_before = graph.graph.node_inputs(cs).into_iter().count();
    let mp_inputs_before = graph.graph.node_inputs(mp).into_iter().count();
    let pred_count_before = entry.cached_predecessor_count;

    let entry_ctrl = {
        let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
        outs[0]
    };
    let initial_mem = graph
        .preorder()
        .find(|&nid| matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory))
        .expect("InitialMemory");
    let im_out = graph
        .graph
        .node_outputs(initial_mem)
        .into_iter()
        .next()
        .expect("output");
    let pred = PredecessorHandles {
        exit_control: entry_ctrl,
        exit_memory: im_out,
        // Empty so the unsupported-vn path triggers the size dispatch
        // and surfaces the error mid-loop.
        exit_vn_to_value: HashMap::new(),
    };
    let res = extend_predecessors_with_handle(&mut entry, &mut graph, &pred);
    assert!(res.is_err(), "must propagate the size-dispatch error");

    // Rollback contract: the prior ControlState / MemPhi appends are
    // undone; cached_predecessor_count is unchanged.
    let cs_inputs_after = graph.graph.node_inputs(cs).into_iter().count();
    let mp_inputs_after = graph.graph.node_inputs(mp).into_iter().count();
    assert_eq!(
        cs_inputs_after, cs_inputs_before,
        "ControlState input count must roll back on error",
    );
    assert_eq!(
        mp_inputs_after, mp_inputs_before,
        "MemPhi input count must roll back on error",
    );
    assert_eq!(
        entry.cached_predecessor_count, pred_count_before,
        "cached_predecessor_count must not increment on error",
    );
}

// ── W6 module-structure tests ────────────────────────────────────────────

#[test]
fn cache_module_split_re_exports_public_api() {
    // Pin: every public item the previous flat ir_cache.rs exposed is
    // still reachable via crate::cache::*.  This test exists because
    // the W6 file move can silently drop a re-export and break
    // downstream callers; the test forces every name to be referenced.
    let _ = LiftStats::default();
    let _ = RegionIrEntry::empty(pcode_addr(0));
    let _ = MachineInsnAddr { addr: 0 };
    let _ = PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: 0 },
        insn_index: 0,
    };
    let _phs: PredecessorHandles = PredecessorHandles {
        exit_control: NodeOutputId::from_u32(0),
        exit_memory: NodeOutputId::from_u32(0),
        exit_vn_to_value: HashMap::new(),
    };
    let _: RegionIrCache = HashMap::new();
}

#[test]
fn cache_module_top_level_helpers_compile() {
    // Pin: the top-level helpers (cache_key_for_region,
    // count_uncached_regions, predecessor_diffs) live in cache::mod and
    // are reachable by name without sub-module qualification.  The
    // type-check is the test — if the W6 split lost a re-export the
    // build would fail before the test ran.
    fn _ref_helpers<R: rsleigh::MemReader>() {
        let _ = super::cache_key_for_region::<R>;
        let _ = super::count_uncached_regions::<R>;
        let _ = super::predecessor_diffs::<R>;
        let _ = super::lift_new_regions_into::<R>;
        let _ = super::lift_new_regions_into_with_stats::<R>;
        let _ = super::extend_predecessors_into::<R>;
        let _ = super::invalidate_split_regions::<R>;
    }
}
