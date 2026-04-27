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
fn apply_tail_call_round_1_returns_typed_error() {
    // Pins the round-1 contract: the in-place tail-call editor is
    // not yet implemented.  Round-2 work replaces this with a real
    // implementation; the test will then change to assert the
    // resulting Call+Return shape.  Until then the orchestrator
    // routes Single-tail-call resolutions through the CFG rebuild
    // path.
    let (mut graph, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&graph);
    let result = apply_tail_call(&mut graph, return_id, 0x1234_5678, &[]);
    assert!(result.is_err(), "round 1: must return Unimplemented");
}
