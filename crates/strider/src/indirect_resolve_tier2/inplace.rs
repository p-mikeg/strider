//! In-place IR edits — F5 shim.  The canonical implementation is in
//! [`opt::indirect_branch_resolve::inplace`].  This module preserves
//! the original strider-level API (`BuiltFunctionGraph` argument,
//! strider's [`crate::error::Error`] return) for back-compat with the
//! orchestrator and existing tests.
//!
//! The opt-side functions return [`opt::Error`].  We bridge into
//! strider's error enum via the existing `IrError` route — opt errors
//! that wrap an `ir::ErrorKind` round-trip cleanly through
//! `strider::ErrorKind::IrError`.

#![allow(clippy::module_name_repetitions)]

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeOutputId};

use crate::error::Result;

/// Apply the `LinkRegister` resolution.  Delegates to
/// [`opt::apply_link_register`].
///
/// # Errors
///
/// * [`ErrorKind::WrongNodeKind`] when `placeholder_return` is not a
///   [`ir::node::NodeKind::Return`].
/// * [`ErrorKind::IrError`] propagating IR errors from the opt-side
///   `add_node_input` call.
pub fn apply_link_register(
    fg: &mut BuiltFunctionGraph,
    placeholder_return: NodeId,
    ret_val_outputs: &[NodeOutputId],
) -> Result<()> {
    opt::apply_link_register(&mut fg.graph, placeholder_return, ret_val_outputs)
        // opt is now anyhow-based; ? lifts directly via identity From.
}

/// Apply the `Single`-tail-call resolution.  Delegates to
/// [`opt::apply_tail_call`].
///
/// `arg_passing_outputs`, `clobbered_kinds`, and `ret_val_outputs`
/// thread the calling-convention's argument-passing register values,
/// clobbered output kinds, and return-value register values into the
/// freshly-spliced Call+Return.  See the opt-side function and
/// [`opt::indirect_branch_resolve::AnchorCallingContext`] for details.
///
/// # Errors
///
/// * [`ErrorKind::WrongNodeKind`] when `placeholder_return` is not a
///   [`ir::node::NodeKind::Return`] or has unexpected input arity.
/// * [`ErrorKind::IrError`] propagating IR construction errors.
pub fn apply_tail_call(
    fg: &mut BuiltFunctionGraph,
    placeholder_return: NodeId,
    target: u64,
    arg_passing_outputs: &[NodeOutputId],
    clobbered_kinds: &[ir::node::NodeOutputKind],
    ret_val_outputs: &[NodeOutputId],
) -> Result<NodeId> {
    opt::apply_tail_call(
        &mut fg.graph,
        placeholder_return,
        target,
        arg_passing_outputs,
        clobbered_kinds,
        ret_val_outputs,
    )
    // opt is now anyhow-based; ? lifts directly via identity From.
}
#[cfg(test)]
mod tests {
    //! Unit tests for [`apply_link_register`] and [`apply_tail_call`].
    //!
    //! Each test constructs a minimal `BuiltFunctionGraph` whose only
    //! Return node is shaped like the R1.4 placeholder
    //! (`[control, memory, target_value]`), invokes the editor on it,
    //! and asserts the post-edit shape.  Tests use `FunctionBuilder`'s
    //! public API where possible; raw `graph.create_node` only when
    //! the test explicitly exercises a malformed-input branch.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use ir::FunctionBuilder;
    use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

    /// Build a placeholder graph whose only Return is
    /// `Return(control, memory, IntConst(0xdead))`.  Returns the
    /// `BuiltFunctionGraph` and the Return node's id.
    fn build_placeholder_graph() -> (ir::BuiltFunctionGraph, NodeId) {
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        let target = builder.build_int_const(0xdeadu64, NodeOutputType::U64);
        builder.build_return(Some(target), &[]).expect("build_return");
        let graph = builder.build().expect("build");
        // Locate the Return: the only NodeKind::Return in the graph.
        let mut found: Option<NodeId> = None;
        for nid in graph.preorder() {
            if matches!(graph.graph.node_kind(nid), NodeKind::Return) {
                assert!(found.is_none(), "more than one Return");
                found = Some(nid);
            }
        }
        let return_id = found.expect("no Return found");
        (graph, return_id)
    }

    #[test]
    fn apply_link_register_keeps_return_node_id() {
        // The Return's NodeId must not change — that's the whole
        // point of an in-place edit.  We keep a snapshot of the id
        // before and after and assert equality.
        let (mut graph, return_id_before) = build_placeholder_graph();
        let inputs_before: Vec<_> =
            graph.graph.node_inputs(return_id_before).into_iter().collect();
        assert_eq!(inputs_before.len(), 3); // [control, memory, target_value]
        apply_link_register(&mut graph, return_id_before, &[]).expect("apply");
        // Confirm the same node id is still a Return with the same
        // arity (since ret_val_outputs is empty, no inputs added).
        assert!(matches!(
            graph.graph.node_kind(return_id_before),
            NodeKind::Return
        ));
        let inputs_after: Vec<_> =
            graph.graph.node_inputs(return_id_before).into_iter().collect();
        assert_eq!(inputs_after.len(), inputs_before.len());
    }

    #[test]
    fn apply_link_register_appends_ret_val_inputs() {
        // With one ret_val, the Return's input count grows by 1; the
        // first three inputs (ctrl, mem, target_value) are preserved.
        let (mut graph, return_id) = build_placeholder_graph();
        let inputs_before: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
        let ret_val = {
            // Add an IntConst to graph.graph directly:
            let nid = graph.graph.create_node(
                NodeKind::IntConst(0x42u128),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U64)],
            );
            let [out] = graph.graph.node_outputs_exact::<1>(nid).expect("out");
            out
        };
        apply_link_register(&mut graph, return_id, &[ret_val]).expect("apply");
        let inputs_after: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
        assert_eq!(inputs_after.len(), inputs_before.len() + 1);
        // Confirm the prefix matches (ctrl, mem, target_value).
        for (i, before) in inputs_before.iter().enumerate() {
            assert_eq!(*before, inputs_after[i], "input slot {i} changed");
        }
        // Confirm the appended slot is our ret_val.
        assert_eq!(inputs_after[inputs_before.len()], ret_val);
    }

    #[test]
    fn apply_link_register_zero_ret_vals_is_noop() {
        // ret_val_outputs is empty → no inputs added.
        let (mut graph, return_id) = build_placeholder_graph();
        let inputs_before: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
        apply_link_register(&mut graph, return_id, &[]).expect("apply");
        let inputs_after: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
        assert_eq!(inputs_after, inputs_before);
    }

    #[test]
    fn apply_link_register_rejects_non_return_node() {
        // Defensive: passing a non-Return node returns an error
        // rather than silently appending to an unrelated node.
        let (mut graph, _return_id) = build_placeholder_graph();
        // Find any IntConst node; it's not a Return, so the call
        // must fail.
        let int_const_id = graph
            .preorder()
            .find(|&nid| matches!(graph.graph.node_kind(nid), NodeKind::IntConst(_)))
            .expect("graph has at least one IntConst");
        let result = apply_link_register(&mut graph, int_const_id, &[]);
        assert!(result.is_err(), "must reject non-Return: {result:?}");
    }

    // ── apply_tail_call tests ──────────────────────────────────────────────

    #[test]
    fn apply_tail_call_emits_call_then_return() {
        // After the edit, the graph must contain a Call node whose
        // outputs feed a Return node.  The placeholder Return becomes
        // unreachable (zombie) but the new Return is reachable from
        // the entry.
        let (mut graph, placeholder) = build_placeholder_graph();
        let new_return =
            apply_tail_call(&mut graph, placeholder, 0xc0de_u64, &[], &[], &[])
                .expect("apply");
        // Walk the graph: there must be a Call + a Return reachable.
        let mut had_call = false;
        let mut had_new_return = false;
        let mut had_old_placeholder_reachable = false;
        for nid in graph.preorder() {
            if matches!(graph.graph.node_kind(nid), NodeKind::Call) {
                had_call = true;
            }
            if nid == new_return {
                had_new_return = true;
            }
            if nid == placeholder
                && matches!(graph.graph.node_kind(nid), NodeKind::Return)
            {
                had_old_placeholder_reachable = true;
            }
        }
        assert!(had_call, "Call node must be reachable from entry");
        assert!(had_new_return, "new Return must be reachable from entry");
        assert!(
            !had_old_placeholder_reachable,
            "old placeholder Return must be detached / unreachable",
        );
    }

    #[test]
    fn apply_tail_call_target_is_int_const_with_correct_value() {
        // The Call's address-input slot (input #2) must be an
        // IntConst whose value equals the requested target.  Pin
        // the contract by walking from the new Return.
        let (mut graph, placeholder) = build_placeholder_graph();
        let target = 0xc0de_u64;
        let new_return =
            apply_tail_call(&mut graph, placeholder, target, &[], &[], &[])
                .expect("apply");
        // new_return inputs: [call_ctrl, call_mem, ...ret_vals].
        // call_ctrl is produced by Call (output #0); walk to it.
        let new_return_inputs: Vec<_> =
            graph.graph.node_inputs(new_return).into_iter().collect();
        let call_ctrl = new_return_inputs[0];
        let (call_node, _idx) = graph.graph.output_definition(call_ctrl);
        assert!(matches!(graph.graph.node_kind(call_node), NodeKind::Call));
        // Call inputs: [control_in, memory_in, call_address].
        let call_inputs: Vec<_> = graph.graph.node_inputs(call_node).into_iter().collect();
        assert!(
            call_inputs.len() >= 3,
            "Call must have at least [ctrl, mem, addr]",
        );
        let call_address = call_inputs[2];
        let (addr_node, _) = graph.graph.output_definition(call_address);
        let addr_kind = graph.graph.node_kind(addr_node);
        match addr_kind {
            NodeKind::IntConst(val) => {
                let expected = u128::from(target);
                assert_eq!(*val, expected, "IntConst value must match target");
            }
            other => panic!("Call address must be IntConst, got {other:?}"),
        }
    }

    #[test]
    fn apply_tail_call_preserves_validate_invariants() {
        // ir::validate::validate must succeed on the post-edit
        // graph.  This pins use-list consistency and structural
        // soundness.
        let (mut graph, placeholder) = build_placeholder_graph();
        let _new_return = apply_tail_call(&mut graph, placeholder, 0x1234, &[], &[], &[])
            .expect("apply");
        ir::validate::validate(&graph.graph, graph.entry).expect("validate");
    }

    #[test]
    fn apply_tail_call_returns_new_return_node_id() {
        // The returned NodeId must point to a Return node that's
        // distinct from the original placeholder.
        let (mut graph, placeholder) = build_placeholder_graph();
        let new_return = apply_tail_call(&mut graph, placeholder, 0xface, &[], &[], &[])
            .expect("apply");
        assert_ne!(
            new_return, placeholder,
            "apply_tail_call must return a fresh Return id",
        );
        assert!(matches!(graph.graph.node_kind(new_return), NodeKind::Return));
    }

    #[test]
    fn apply_tail_call_with_ret_val_regs_threads_them_into_return() {
        // ret_val_outputs are appended to the new Return's input
        // list after [call_ctrl, call_mem].  Verify the slots line up.
        let (mut graph, placeholder) = build_placeholder_graph();
        // Synthesize a value-typed output to pass as a ret_val.
        let extra = {
            let nid = graph.graph.create_node(
                NodeKind::IntConst(0x42u128),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U64)],
            );
            let [out] = graph.graph.node_outputs_exact::<1>(nid).expect("out");
            out
        };
        let new_return =
            apply_tail_call(&mut graph, placeholder, 0xbeef, &[], &[], &[extra])
                .expect("apply");
        let inputs: Vec<_> = graph.graph.node_inputs(new_return).into_iter().collect();
        // Layout: [call_ctrl, call_mem, extra].
        assert_eq!(inputs.len(), 3);
        assert_eq!(
            inputs[2], extra,
            "ret_val_output must be at slot 2 of the new Return",
        );
    }

    #[test]
    fn apply_tail_call_rejects_non_return_node() {
        // Defensive: passing a non-Return node returns an error.
        let (mut graph, _return_id) = build_placeholder_graph();
        let int_const_id = graph
            .preorder()
            .find(|&nid| matches!(graph.graph.node_kind(nid), NodeKind::IntConst(_)))
            .expect("graph has at least one IntConst");
        let result = apply_tail_call(&mut graph, int_const_id, 0xc0de, &[], &[], &[]);
        assert!(result.is_err(), "must reject non-Return: {result:?}");
    }

    #[test]
    fn apply_tail_call_int_const_width_matches_target_value_width() {
        // The IntConst created by apply_tail_call must have the same
        // width as the original target_value's output type.  Pinned
        // because a width mismatch (e.g. building a U32 IntConst when
        // target_value is U64) would silently truncate the high
        // bits of the target address.
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("region");
        builder.set_entry_region(region).expect("entry");
        builder.set_region(region);
        // 32-bit target value (deliberately not U64).
        let target32 = builder.build_int_const(0xabcd_u64, NodeOutputType::U32);
        builder.build_return(Some(target32), &[]).expect("return");
        let mut graph = builder.build().expect("build");
        let return_id = graph
            .preorder()
            .find(|&nid| matches!(graph.graph.node_kind(nid), NodeKind::Return))
            .expect("Return");
        let new_return =
            apply_tail_call(&mut graph, return_id, 0xfeedface_u64, &[], &[], &[])
                .expect("apply");
        // Walk to the IntConst created by apply_tail_call and check
        // its output type.
        let inputs: Vec<_> = graph.graph.node_inputs(new_return).into_iter().collect();
        let call_ctrl = inputs[0];
        let (call_node, _) = graph.graph.output_definition(call_ctrl);
        let call_inputs: Vec<_> = graph.graph.node_inputs(call_node).into_iter().collect();
        let call_addr = call_inputs[2];
        let kind = graph.graph.output_kind(call_addr);
        assert_eq!(
            kind.as_value().expect("value"),
            NodeOutputType::U32,
            "IntConst must inherit width from target_value",
        );
    }

    #[test]
    fn apply_tail_call_target_high_bits_masked_to_width() {
        // When target exceeds the width of target_value, the high
        // bits are masked off (mirrors FunctionBuilder::build_int_const
        // semantics).  Pin the contract: low bits preserved.
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("region");
        builder.set_entry_region(region).expect("entry");
        builder.set_region(region);
        let target32 = builder.build_int_const(0u64, NodeOutputType::U32);
        builder.build_return(Some(target32), &[]).expect("return");
        let mut graph = builder.build().expect("build");
        let return_id = graph
            .preorder()
            .find(|&nid| matches!(graph.graph.node_kind(nid), NodeKind::Return))
            .expect("Return");
        // 0x1_0000_0000 > U32::MAX → masking to U32 gives 0.
        let new_return =
            apply_tail_call(&mut graph, return_id, 0x1_0000_0000_u64, &[], &[], &[])
                .expect("apply");
        let inputs: Vec<_> = graph.graph.node_inputs(new_return).into_iter().collect();
        let call_ctrl = inputs[0];
        let (call_node, _) = graph.graph.output_definition(call_ctrl);
        let call_inputs: Vec<_> = graph.graph.node_inputs(call_node).into_iter().collect();
        let call_addr = call_inputs[2];
        let (addr_node, _) = graph.graph.output_definition(call_addr);
        match graph.graph.node_kind(addr_node) {
            NodeKind::IntConst(v) => {
                // Mask to U32: 0x1_0000_0000 & 0xFFFF_FFFF == 0.
                assert_eq!(*v, 0u128);
            }
            other => panic!("expected IntConst, got {other:?}"),
        }
    }

    #[test]
    fn apply_tail_call_rejects_wrong_input_arity_return() {
        // A Return with more or fewer than 3 inputs is not a
        // placeholder; apply_tail_call must reject it.
        // Build a graph with a Return that has only [ctrl, mem]
        // (no value).
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("region");
        builder.set_entry_region(region).expect("entry");
        builder.set_region(region);
        builder.build_return(None, &[]).expect("return");
        let mut graph = builder.build().expect("build");
        let ret_id = graph
            .preorder()
            .find(|&nid| matches!(graph.graph.node_kind(nid), NodeKind::Return))
            .expect("Return");
        let result = apply_tail_call(&mut graph, ret_id, 0xc0de, &[], &[], &[]);
        assert!(result.is_err(), "must reject 2-input Return: {result:?}");
    }
}
