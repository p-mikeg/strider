//! In-place IR edits for tier-2 resolutions that don't require a CFG
//! rebuild.  Two variants are supported:
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
//!
//! ## Origin
//!
//! Originally implemented in `strider::indirect_resolve_tier2::inplace`
//! and using strider's error type.  F5 relocates the logic into the
//! opt crate; the error type switches to [`crate::Error`] so opt-pass
//! impls don't depend on strider.

#![allow(clippy::module_name_repetitions)]

use ir::Graph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use crate::error::{ErrorKind, Result};

/// Applies the `LinkRegister` resolution to a placeholder
/// `Return(control, memory, target_value)` node by appending
/// `ret_val_outputs` as additional value inputs.
///
/// Pre-edit: `[control, memory, target_value]`
/// Post-edit: `[control, memory, target_value, ret_val_0, …]`
///
/// The `target_value` slot is intentionally retained — removing it
/// would require shifting subsequent input indices.  The IR's
/// `validate` is robust to extra Return value inputs.
///
/// # Errors
///
/// * [`ErrorKind::ExpectedNodeNotFound`] if `placeholder_return` is
///   not a [`NodeKind::Return`].
/// * [`ErrorKind::IrError`] propagating any failure from
///   [`Graph::add_node_input`].
pub fn apply_link_register(
    graph: &mut Graph,
    placeholder_return: NodeId,
    ret_val_outputs: &[NodeOutputId],
) -> Result<()> {
    let kind = *graph.node_kind(placeholder_return);
    if !matches!(kind, NodeKind::Return) {
        return Err(ErrorKind::ExpectedNodeNotFound("Return", kind).into());
    }
    for &ret in ret_val_outputs {
        graph.add_node_input(placeholder_return, ret)?;
    }
    Ok(())
}

/// Applies the `Single`-tail-call resolution by replacing the
/// placeholder `Return(target_value)` with `Call(IntConst(target)) →
/// Return(ret_vars)`.
///
/// Pre-edit: `Return(control, memory, target_value)`
/// Post-edit: `IntConst(target) → Call(control, memory, IntConst) →
///   Return(call.ctrl_out, call.mem_out, ret_val_0, …)`
///
/// The placeholder Return is detached (becomes a zombie unreachable
/// from `entry`).  The new Return is wired on the Call's control and
/// memory outputs.  Returns the new Return's [`NodeId`] so callers
/// can patch any cached exit-control handles.
///
/// # Errors
///
/// * [`ErrorKind::ExpectedNodeNotFound`] if `placeholder_return` is
///   not a [`NodeKind::Return`] node, or if its input arity isn't
///   the expected 3 (i.e. not a placeholder shape).
/// * [`ErrorKind::IrError`] propagating IR construction errors.
pub fn apply_tail_call(
    graph: &mut Graph,
    placeholder_return: NodeId,
    target: u64,
    ret_val_outputs: &[NodeOutputId],
) -> Result<NodeId> {
    let kind = *graph.node_kind(placeholder_return);
    if !matches!(kind, NodeKind::Return) {
        return Err(ErrorKind::ExpectedNodeNotFound("Return", kind).into());
    }
    let inputs: Vec<NodeOutputId> = graph.node_inputs(placeholder_return).into_iter().collect();
    if inputs.len() != 3 {
        // Not a placeholder Return.  Surface as a typed error so
        // callers don't silently mis-apply.
        return Err(ErrorKind::ExpectedNodeNotFound(
            "Return with [control, memory, target_value] (3 inputs)",
            kind,
        )
        .into());
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

    // Create the Call node.  Outputs: [Control, Memory] — no
    // clobbered varnodes because we don't have access to the calling
    // convention's clobber list at this entry point.  Empty clobbers
    // is sound: the surviving Return doesn't need any of those values.
    let call = graph.create_node(
        NodeKind::Call,
        [control_in, memory_in, int_const_out],
        [NodeOutputKind::Control, NodeOutputKind::Memory],
    );
    let [call_ctrl_out, call_mem_out] = graph.node_outputs_exact::<2>(call)?;

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

    fn build_placeholder_graph() -> (ir::Graph, NodeId) {
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
        (built.graph, found.expect("Return"))
    }

    #[test]
    fn apply_link_register_keeps_return_node_id() {
        let (mut graph, return_id_before) = build_placeholder_graph();
        let inputs_before: Vec<_> =
            graph.node_inputs(return_id_before).into_iter().collect();
        assert_eq!(inputs_before.len(), 3);
        apply_link_register(&mut graph, return_id_before, &[]).expect("apply");
        assert!(matches!(graph.node_kind(return_id_before), NodeKind::Return));
    }

    #[test]
    fn apply_link_register_rejects_non_return_node() {
        let (mut graph, _return_id) = build_placeholder_graph();
        let int_const_id = graph
            .all_node_ids()
            .find(|&nid| matches!(graph.node_kind(nid), NodeKind::IntConst(_)))
            .expect("graph has at least one IntConst");
        let result = apply_link_register(&mut graph, int_const_id, &[]);
        assert!(result.is_err(), "must reject non-Return: {result:?}");
    }

    #[test]
    fn apply_tail_call_emits_call_then_return() {
        let (mut graph, placeholder) = build_placeholder_graph();
        let _new_return =
            apply_tail_call(&mut graph, placeholder, 0xc0de_u64, &[]).expect("apply");
        // The new Return must be reachable from entry; the placeholder
        // is detached.  Walk all node ids to confirm a Call materialised.
        let mut had_call = false;
        for nid in graph.all_node_ids() {
            if matches!(graph.node_kind(nid), NodeKind::Call) {
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
        let built = builder.build().expect("build");
        let mut graph = built.graph;
        let ret_id = graph
            .all_node_ids()
            .find(|&nid| matches!(graph.node_kind(nid), NodeKind::Return))
            .expect("Return");
        let result = apply_tail_call(&mut graph, ret_id, 0xc0de, &[]);
        assert!(result.is_err(), "must reject 2-input Return: {result:?}");
    }
}
