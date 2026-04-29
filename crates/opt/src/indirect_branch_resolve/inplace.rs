//! In-place IR edits for resolutions that don't require a CFG rebuild.
//!
//! Two variants are supported:
//!
//!   * **`LinkRegister`**: the placeholder `Return(target_value)`
//!     already has the right control-flow shape.  We append the
//!     convention's `ret_val_regs` as additional value inputs to the
//!     existing Return node.
//!   * **`Single` tail call**: replace the placeholder
//!     `Return(target_vn)` with `Call(IntConst(target)) →
//!     Return(ret_vars)`.  The placeholder Return is detached
//!     (becoming a zombie unreachable node) and a fresh
//!     `IntConst → Call → Return` chain is wired on the same control
//!     and memory inputs.

#![allow(clippy::module_name_repetitions)]

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use anyhow::anyhow;

use crate::error::Result;

/// Applies the `LinkRegister` resolution to a placeholder
/// `Return(control, memory, target_value)` node, dropping the
/// `target_value` slot (no longer meaningful — the LR-targeted branch
/// IS the return) and appending `ret_val_outputs` as the actual return
/// values.
///
/// Pre-edit: `[control, memory, target_value]`
/// Post-edit: `[control, memory, ret_val_0, …]`
///
/// Removing the `target_value` slot keeps `RetPat::ret_val(idx)` 0-indexed
/// over actual return values, matching the pattern crate's documented
/// contract.  Without this, downstream pattern queries that look at
/// `ret_val(0)` would hit the dead anchor placeholder.
///
/// # Errors
///
/// Returns an error when `placeholder_return` is not a
/// [`NodeKind::Return`], or when the IR mutation calls
/// ([`Graph::add_node_input`] / [`Graph::remove_node_input`]) fail.
pub fn apply_link_register(
    fg: &mut BuiltFunctionGraph,
    placeholder_return: NodeId,
    ret_val_outputs: &[NodeOutputId],
) -> Result<()> {
    let graph = &mut fg.graph;
    let kind = *graph.node_kind(placeholder_return);
    if !matches!(kind, NodeKind::Return) {
        return Err(anyhow!("expected Return node, got {kind:?}"));
    }
    for &ret in ret_val_outputs {
        graph.add_node_input(placeholder_return, ret)?;
    }
    // Drop the placeholder `target_value` at slot 2 (after [control, memory]).
    // Done last so `add_node_input` above appended after the placeholder; the
    // shift moves ret_val_0 from slot 3 to slot 2 and so on.
    let inputs = graph.node_inputs(placeholder_return);
    if inputs.len() > 2 && inputs.len() > ret_val_outputs.len() + 2 {
        graph.remove_node_input(placeholder_return, 2)?;
    }
    Ok(())
}

/// Applies the `Single`-tail-call resolution by replacing the
/// placeholder `Return(target_value)` with `Call(IntConst(target)) →
/// Return(ret_vars)`.
///
/// Pre-edit: `Return(control, memory, target_value)`
/// Post-edit: `IntConst(target) →
///   Call(control, memory, IntConst, arg_passing_0, …) [outs:
///   Control, Memory, clob_0, …] →
///   Return(call.ctrl_out, call.mem_out, ret_val_0, …)`
///
/// The placeholder Return is detached (becomes a zombie unreachable
/// from `entry`).  The new Return is wired on the Call's control and
/// memory outputs.  Returns the new Return's [`NodeId`] so callers
/// can patch any cached exit-control handles.
///
/// `arg_passing_outputs`, `clobbered_kinds`, and `ret_val_outputs`
/// thread the calling-convention context through the freshly-spliced
/// Call+Return — see [`super::AnchorCallingContext`] for how the opt
/// pass and the strider orchestrator populate them.  Empty slices are
/// sound (the resulting Call/Return is degenerate but well-typed); a
/// real ABI-aware caller passes the placeholder's pre-edit ABI
/// register values.
///
/// # Errors
///
/// Returns an error when `placeholder_return` is not a
/// [`NodeKind::Return`] node, when its input arity isn't the
/// expected 3 (i.e. not a placeholder shape), or when IR
/// construction fails.
pub fn apply_tail_call(
    fg: &mut BuiltFunctionGraph,
    placeholder_return: NodeId,
    target: u64,
    arg_passing_outputs: &[NodeOutputId],
    clobbered_kinds: &[NodeOutputKind],
    ret_val_outputs: &[NodeOutputId],
) -> Result<NodeId> {
    let graph = &mut fg.graph;
    let kind = *graph.node_kind(placeholder_return);
    if !matches!(kind, NodeKind::Return) {
        return Err(anyhow!("expected Return node, got {kind:?}"));
    }
    let inputs: Vec<NodeOutputId> = graph.node_inputs(placeholder_return).into_iter().collect();
    if inputs.len() != 3 {
        // Not a placeholder Return.  Surface as a typed error so
        // callers don't silently mis-apply.
        return Err(anyhow!(
            "expected Return with [control, memory, target_value] (3 inputs) node, got {kind:?}"
        ));
    }
    let control_in = inputs[0];
    let memory_in = inputs[1];
    let target_value = inputs[2];

    let target_int_ty = graph
        .output_kind(target_value)
        .as_integer_or_err()
        .unwrap_or(ir::node::NodeOutputType::U64);

    // CORRECTNESS — detach BEFORE creating the new chain: removes the
    // placeholder's three inputs from their use-lists.
    graph.detach_node_inputs(placeholder_return);

    let masked_target = u128::from(target) & target_int_ty.bit_mask_u128();
    let int_const = graph.create_node(
        NodeKind::IntConst(masked_target),
        [],
        [NodeOutputKind::OutputType(target_int_ty)],
    );
    let int_const_out = graph.node_outputs_exact::<1>(int_const)?[0];

    // Create the Call node.  Inputs: [control, memory, target,
    // arg_passing_0, …].  Outputs: [Control, Memory, clob_0, …].
    let mut call_inputs: Vec<NodeOutputId> =
        Vec::with_capacity(3 + arg_passing_outputs.len());
    call_inputs.push(control_in);
    call_inputs.push(memory_in);
    call_inputs.push(int_const_out);
    call_inputs.extend_from_slice(arg_passing_outputs);
    let mut call_outputs: Vec<NodeOutputKind> =
        Vec::with_capacity(2 + clobbered_kinds.len());
    call_outputs.push(NodeOutputKind::Control);
    call_outputs.push(NodeOutputKind::Memory);
    call_outputs.extend_from_slice(clobbered_kinds);
    let call = graph.create_node(NodeKind::Call, call_inputs, call_outputs);
    // Slot 0 = Control, slot 1 = Memory.  The clobbered slots beyond
    // those are produced for downstream consumers (typically empty
    // here because the only consumer is the freshly-spliced Return).
    let call_outs: Vec<_> = graph.node_outputs(call).into_iter().collect();
    let call_ctrl_out = call_outs[0];
    let call_mem_out = call_outs[1];

    let mut new_return_inputs: Vec<NodeOutputId> = Vec::with_capacity(2 + ret_val_outputs.len());
    new_return_inputs.push(call_ctrl_out);
    new_return_inputs.push(call_mem_out);
    new_return_inputs.extend_from_slice(ret_val_outputs);
    let new_return = graph.create_node(NodeKind::Return, new_return_inputs, []);

    Ok(new_return)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the in-place editors at the opt-crate level.
    //! Mirror the original strider tests; the strider shim's tests
    //! continue to exercise the shim path.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use ir::FunctionBuilder;
    use ir::node::NodeOutputType;

    fn build_placeholder_graph() -> (ir::BuiltFunctionGraph, NodeId) {
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        let target = builder.build_int_const(0xdeadu64, NodeOutputType::U64);
        builder.build_return(Some(target), &[]).expect("build_return");
        let built = builder.build().expect("build");
        // Locate the unique Return.
        let mut found: Option<NodeId> = None;
        for nid in built.preorder() {
            if matches!(built.graph.node_kind(nid), NodeKind::Return) {
                assert!(found.is_none(), "more than one Return");
                found = Some(nid);
            }
        }
        (built, found.expect("Return"))
    }

    #[test]
    fn apply_link_register_keeps_return_node_id() {
        let (mut fg, return_id_before) = build_placeholder_graph();
        let inputs_before: Vec<_> =
            fg.graph.node_inputs(return_id_before).into_iter().collect();
        assert_eq!(inputs_before.len(), 3);
        apply_link_register(&mut fg, return_id_before, &[]).expect("apply");
        assert!(matches!(fg.graph.node_kind(return_id_before), NodeKind::Return));
    }

    #[test]
    fn apply_link_register_rejects_non_return_node() {
        let (mut fg, _return_id) = build_placeholder_graph();
        let int_const_id = fg.graph
            .all_node_ids()
            .find(|&nid| matches!(fg.graph.node_kind(nid), NodeKind::IntConst(_)))
            .expect("graph has at least one IntConst");
        let result = apply_link_register(&mut fg, int_const_id, &[]);
        assert!(result.is_err(), "must reject non-Return: {result:?}");
    }

    #[test]
    fn apply_tail_call_emits_call_then_return() {
        let (mut fg, placeholder) = build_placeholder_graph();
        let _new_return =
            apply_tail_call(&mut fg, placeholder, 0xc0de_u64, &[], &[], &[])
                .expect("apply");
        // The new Return must be reachable from entry; the placeholder
        // is detached.  Walk all node ids to confirm a Call materialised.
        let mut had_call = false;
        for nid in fg.graph.all_node_ids() {
            if matches!(fg.graph.node_kind(nid), NodeKind::Call) {
                had_call = true;
                break;
            }
        }
        assert!(had_call, "Call node must materialise");
    }

    #[test]
    fn apply_tail_call_rejects_wrong_arity_return() {
        // A Return with 2 inputs (no value) is not a placeholder; reject.
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("region");
        builder.set_entry_region(region).expect("entry");
        builder.set_region(region);
        builder.build_return(None, &[]).expect("return");
        let mut fg = builder.build().expect("build");
        let ret_id = fg.graph
            .all_node_ids()
            .find(|&nid| matches!(fg.graph.node_kind(nid), NodeKind::Return))
            .expect("Return");
        let result = apply_tail_call(&mut fg, ret_id, 0xc0de, &[], &[], &[]);
        assert!(result.is_err(), "must reject 2-input Return: {result:?}");
    }

    /// Spawns a value-typed `IntConst` and returns its single output id —
    /// a convenient stand-in for an "ABI register's IR value at the
    /// placeholder site" in unit tests that don't care which register
    /// it came from.
    fn synth_value_output(
        graph: &mut ir::Graph,
        value: u128,
        ty: NodeOutputType,
    ) -> NodeOutputId {
        let nid = graph.create_node(
            NodeKind::IntConst(value),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        graph
            .node_outputs_exact::<1>(nid)
            .expect("IntConst has one output")[0]
    }

    // H0 — calling-convention threading tests for the in-place editors.
    //
    // Pre-fix: `apply_link_register` and `apply_tail_call` produced a
    // Return / Call with no ABI ret-val / arg-passing / clobbered slots,
    // so pattern queries that walk those slots failed silently.  These
    // tests pin the post-fix shape: ret-val outputs append to the
    // Return's input list, arg-passing outputs append to the Call's
    // input list (after `[ctrl, mem, target]`), and clobbered output
    // kinds append to the Call's output list (after `[Control, Memory]`).

    #[test]
    fn apply_link_register_threads_ret_val_outputs_into_return() {
        // Two ret-val outputs supplied → resulting Return's inputs are
        // `[ctrl, mem, ret_val_0, ret_val_1]` (the placeholder
        // `target_value` slot is dropped so `RetPat::ret_val(idx)` stays
        // 0-indexed over the real return values).
        let (mut fg, placeholder) = build_placeholder_graph();
        let inputs_before: Vec<_> = fg.graph.node_inputs(placeholder).into_iter().collect();
        assert_eq!(inputs_before.len(), 3);
        let r0 = synth_value_output(&mut fg.graph, 0x42, NodeOutputType::U64);
        let r1 = synth_value_output(&mut fg.graph, 0x43, NodeOutputType::U64);
        apply_link_register(&mut fg, placeholder, &[r0, r1]).expect("apply");
        let inputs_after: Vec<_> = fg.graph.node_inputs(placeholder).into_iter().collect();
        assert_eq!(
            inputs_after.len(),
            2 + 2,
            "Return inputs are [ctrl, mem, ret_val_0, ret_val_1] after target_value removal",
        );
        assert_eq!(inputs_after[2], r0);
        assert_eq!(inputs_after[3], r1);
    }

    #[test]
    fn apply_tail_call_threads_arg_passing_into_call() {
        // Three arg-passing outputs → Call's inputs are
        // `[ctrl, mem, IntConst(target), arg_0, arg_1, arg_2]`.
        let (mut fg, placeholder) = build_placeholder_graph();
        let a0 = synth_value_output(&mut fg.graph, 0x01, NodeOutputType::U64);
        let a1 = synth_value_output(&mut fg.graph, 0x02, NodeOutputType::U64);
        let a2 = synth_value_output(&mut fg.graph, 0x03, NodeOutputType::U64);
        let new_return =
            apply_tail_call(&mut fg, placeholder, 0xc0de, &[a0, a1, a2], &[], &[])
                .expect("apply");
        // The new Return's input #0 is the Call's ctrl output.  Walk
        // back to the Call.
        let new_return_inputs: Vec<_> =
            fg.graph.node_inputs(new_return).into_iter().collect();
        let call_ctrl = new_return_inputs[0];
        let (call_node, _) = fg.graph.output_definition(call_ctrl);
        assert!(matches!(fg.graph.node_kind(call_node), NodeKind::Call));
        let call_inputs: Vec<_> = fg.graph.node_inputs(call_node).into_iter().collect();
        assert_eq!(
            call_inputs.len(),
            6,
            "Call must have [ctrl, mem, target, a0, a1, a2]",
        );
        assert_eq!(call_inputs[3], a0);
        assert_eq!(call_inputs[4], a1);
        assert_eq!(call_inputs[5], a2);
    }

    #[test]
    fn apply_tail_call_threads_clobbered_kinds_into_call_outputs() {
        // Two clobbered output kinds → Call's outputs are
        // `[Control, Memory, clob_0, clob_1]`.
        let (mut fg, placeholder) = build_placeholder_graph();
        let clob_kinds = [
            NodeOutputKind::OutputType(NodeOutputType::U64),
            NodeOutputKind::OutputType(NodeOutputType::U32),
        ];
        let new_return = apply_tail_call(
            &mut fg,
            placeholder,
            0xbeef,
            &[],
            &clob_kinds,
            &[],
        )
        .expect("apply");
        // Walk to the Call.
        let new_return_inputs: Vec<_> =
            fg.graph.node_inputs(new_return).into_iter().collect();
        let (call_node, _) = fg.graph.output_definition(new_return_inputs[0]);
        let call_outputs: Vec<_> = fg.graph.node_outputs(call_node).into_iter().collect();
        assert_eq!(
            call_outputs.len(),
            4,
            "Call must have [Control, Memory, clob_0, clob_1]",
        );
        assert_eq!(fg.graph.output_kind(call_outputs[2]), clob_kinds[0]);
        assert_eq!(fg.graph.output_kind(call_outputs[3]), clob_kinds[1]);
    }

    #[test]
    fn apply_tail_call_threads_ret_val_outputs_into_return() {
        // Two ret-val outputs → new Return's inputs are
        // `[call_ctrl, call_mem, ret_val_0, ret_val_1]`.
        let (mut fg, placeholder) = build_placeholder_graph();
        let r0 = synth_value_output(&mut fg.graph, 0x10, NodeOutputType::U64);
        let r1 = synth_value_output(&mut fg.graph, 0x11, NodeOutputType::U64);
        let new_return =
            apply_tail_call(&mut fg, placeholder, 0xface, &[], &[], &[r0, r1])
                .expect("apply");
        let inputs: Vec<_> = fg.graph.node_inputs(new_return).into_iter().collect();
        assert_eq!(inputs.len(), 4, "[call_ctrl, call_mem, r0, r1]");
        assert_eq!(inputs[2], r0);
        assert_eq!(inputs[3], r1);
    }
}
