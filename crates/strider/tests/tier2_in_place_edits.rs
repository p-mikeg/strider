//! Integration tests for [`strider::indirect_resolve_tier2::inplace`].
//!
//! Drives the in-place editors against full strider lifts (not the
//! unit-test scaffold in `inplace.rs::tests`).  Asserts that the
//! placeholder Return is mutated correctly, the use-list stays
//! consistent, and `ir::validate::validate` keeps passing post-edit.
//!
//! The fixtures use the existing tier-2 helper at
//! `tests/common/tier2_helpers.rs` (which lifts an x86_64 byte
//! sequence + runs the strider optimiser pipeline).  `apply_tail_call`
//! is round-2 work; tests here only pin its current "returns
//! Unimplemented" contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::tier2_helpers::build_initial_var_target_scenario_x86_64;

use ir::node::{NodeId, NodeKind};
use strider::indirect_resolve_tier2::{apply_link_register, apply_tail_call};

/// Locate the unique placeholder Return in `graph` (a Return with
/// exactly 3 inputs: ctrl, mem, target_value).  Panics if 0 or
/// multiple are found.
fn locate_placeholder_return(graph: &ir::BuiltFunctionGraph) -> NodeId {
    let mut found: Option<NodeId> = None;
    for nid in graph.preorder() {
        if !matches!(graph.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = graph.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() != 3 {
            continue;
        }
        assert!(
            found.is_none(),
            "fixture must have exactly one placeholder Return"
        );
        found = Some(nid);
    }
    found.expect("no placeholder Return found")
}

#[test]
fn apply_link_register_to_real_lift_zero_ret_vals_keeps_shape() {
    // A live x86_64 lift of `jmp rax`: the placeholder Return has 3
    // inputs ([ctrl, mem, InitialVar(rax)] after optimisation).
    // apply_link_register with no ret_vals must leave the input
    // count unchanged and keep the same NodeId.
    let (mut graph, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&graph);
    let inputs_before: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
    apply_link_register(&mut graph, return_id, &[]).expect("apply");
    let inputs_after: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
    assert_eq!(inputs_after, inputs_before);
    // ir::validate::validate must still pass on the mutated graph.
    ir::validate::validate(&graph.graph, graph.entry).expect("validate after edit");
}

#[test]
fn apply_link_register_to_real_lift_appends_one_ret_val() {
    // Append one ret_val output (the existing target_value's
    // NodeOutputId — value-typed and reachable from the entry).
    // The Return's input arity must grow by 1.
    let (mut graph, anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&graph);
    let inputs_before: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
    apply_link_register(&mut graph, return_id, &[anchor]).expect("apply");
    let inputs_after: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
    assert_eq!(inputs_after.len(), inputs_before.len() + 1);
    // The appended slot is `anchor`.
    assert_eq!(*inputs_after.last().expect("non-empty"), anchor);
    ir::validate::validate(&graph.graph, graph.entry).expect("validate after edit");
}

#[test]
fn apply_tail_call_replaces_placeholder_with_call_then_return() {
    // R3-FIXUP G2: apply_tail_call now does a real in-place edit.
    // After the edit, the IR contains a Call with the IntConst
    // target as its address, feeding a fresh Return.  The original
    // placeholder Return is detached (unreachable from entry) but
    // the rest of the body is untouched.
    let (mut graph, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&graph);
    let target = 0x1234_5678_u64;
    let new_return = apply_tail_call(&mut graph, return_id, target, &[])
        .expect("apply_tail_call");
    assert_ne!(
        new_return, return_id,
        "tail-call edit must produce a fresh Return id",
    );
    // Walk: the new Return must be reachable; the old placeholder
    // must not be.
    let mut new_seen = false;
    let mut old_seen = false;
    for nid in graph.preorder() {
        if nid == new_return {
            new_seen = true;
        }
        if nid == return_id {
            old_seen = true;
        }
    }
    assert!(new_seen, "new Return must be reachable from entry");
    assert!(!old_seen, "old placeholder must be detached / unreachable");
    // ir::validate must still pass.
    ir::validate::validate(&graph.graph, graph.entry).expect("validate after edit");
}

#[test]
fn apply_tail_call_real_lift_target_int_const_value_matches() {
    // After the edit, the Call's address-input is an IntConst with
    // the exact target value.  Pins the soundness contract — the
    // tail call dispatches to the resolved target, not some folded
    // approximation.
    use ir::node::NodeKind;
    let (mut graph, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&graph);
    let target = 0xdead_beef_u64;
    let new_return = apply_tail_call(&mut graph, return_id, target, &[])
        .expect("apply_tail_call");
    let inputs: Vec<_> = graph.graph.node_inputs(new_return).into_iter().collect();
    let call_ctrl = inputs[0];
    let (call_node, _idx) = graph.graph.output_definition(call_ctrl);
    let call_inputs: Vec<_> =
        graph.graph.node_inputs(call_node).into_iter().collect();
    let call_addr = call_inputs[2];
    let (addr_node, _) = graph.graph.output_definition(call_addr);
    match graph.graph.node_kind(addr_node) {
        NodeKind::IntConst(v) => assert_eq!(*v, u128::from(target)),
        other => panic!("expected IntConst, got {other:?}"),
    }
}
