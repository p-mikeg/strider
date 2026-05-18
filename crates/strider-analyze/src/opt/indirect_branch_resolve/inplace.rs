//! In-place IR edits for resolutions that don't require a CFG rebuild.
//!
//! Two variants are supported:
//!
//!   * **`LinkRegister`**: the placeholder `IndirectBranch(target_value)`
//!     is rewritten in place into a real `Return [ctrl, mem,
//!     ret_val_*]`.  The placeholder's `target_value` slot is dropped
//!     and the convention's `ret_val_regs` are appended.  The node
//!     `NodeKind` is mutated from `IndirectBranch` to `Return` so the
//!     same `NodeId` flows through.
//!   * **`Single` tail call**: replace the placeholder
//!     `IndirectBranch(target_value)` with `Call(IntConst(target)) →
//!     Return(ret_vars)`.  The placeholder is detached (becoming a
//!     zombie unreachable node) and a fresh `IntConst → Call → Return`
//!     chain is wired on the same control and memory inputs.

#![allow(clippy::module_name_repetitions)]

use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use anyhow::anyhow;

use crate::opt::error::Result;

/// Applies the `LinkRegister` resolution to a placeholder
/// `IndirectBranch(control, memory, target_value)` node, mutating it
/// into a real `Return [control, memory, ret_val_0, …]` in place.
/// The placeholder's `target_value` slot is dropped (no longer
/// meaningful — the LR-targeted branch IS the return) and
/// `ret_val_outputs` are appended as the actual return values.  The
/// node's `NodeId` is preserved so any cached handle (e.g. the
/// orchestrator's `exit_control`) remains valid.
///
/// Pre-edit: `IndirectBranch [control, memory, target_value]`
/// Post-edit: `Return [control, memory, ret_val_0, …]`
///
/// # Errors
///
/// Returns an error when `placeholder` is not a
/// [`NodeKind::IndirectBranch`], or when the IR mutation calls
/// (`Graph::add_node_input` / `Graph::remove_node_input` /
/// `Graph::set_node_kind`) fail.
pub fn apply_link_register(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    placeholder: NodeId,
    ret_val_outputs: &[NodeOutputId],
) -> Result<()> {
    let graph = ctx.graph_mut();
    let kind = *graph.node_kind(placeholder);
    if !matches!(kind, NodeKind::IndirectBranch) {
        return Err(anyhow!("expected IndirectBranch node, got {kind:?}"));
    }
    for &ret in ret_val_outputs {
        graph.add_node_input(placeholder, ret)?;
    }
    // Drop the placeholder `target_value` at slot 2 (after [control, memory]).
    // Done after `add_node_input` above so the appended ret_vals shift down
    // from slot 3+ to slot 2+ post-removal.  Removal is unconditional under
    // the contract: the matches!-guard above already pinned this as a
    // 3-input IndirectBranch [control, memory, target_value], and the loop
    // only appends, so slot 2 should always be present.  Surface a
    // violation as a typed error rather than a debug-mode panic so Python
    // users see a clean exception.
    let arity = graph.node_inputs(placeholder).len();
    if arity < 3 {
        return Err(anyhow!(
            "apply_link_register: IndirectBranch placeholder {placeholder:?} has \
             {arity} inputs, expected ≥3 (control, memory, target_value); invariant \
             violation"
        ));
    }
    graph.remove_node_input(placeholder, 2)?;
    // Mutate the kind: IndirectBranch → Return.  Same input/output
    // signature shape (control + memory + variadic value tail; no
    // outputs); both kinds are non-cacheable.
    graph.set_node_kind(placeholder, NodeKind::Return)?;
    Ok(())
}

/// Applies the `Single`-tail-call resolution by replacing the
/// placeholder `IndirectBranch(target_value)` with
/// `Call(IntConst(target)) → Return(ret_vars)`.
///
/// Pre-edit: `IndirectBranch(control, memory, target_value)`
/// Post-edit: `IntConst(target) →
///   Call(control, memory, IntConst, arg_passing_0, …) [outs:
///   Control, Memory, clob_0, …] →
///   Return(call.ctrl_out, call.mem_out, ret_val_0, …)`
///
/// The placeholder is detached (becomes a zombie unreachable from
/// `entry`).  The new Return is wired on the Call's control and memory
/// outputs.  Returns the new Return's [`NodeId`] so callers can patch
/// any cached exit-control handles.
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
/// Returns an error when `placeholder` is not a
/// [`NodeKind::IndirectBranch`] node, when its input arity isn't the
/// expected 3 (i.e. not a placeholder shape), or when IR
/// construction fails.
pub fn apply_tail_call(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    placeholder: NodeId,
    target: u64,
    arg_passing_outputs: &[NodeOutputId],
    clobbered_kinds: &[NodeOutputKind],
    ret_val_outputs: &[NodeOutputId],
) -> Result<NodeId> {
    let graph = ctx.graph_mut();
    let kind = *graph.node_kind(placeholder);
    if !matches!(kind, NodeKind::IndirectBranch) {
        return Err(anyhow!("expected IndirectBranch node, got {kind:?}"));
    }
    let inputs: smallvec::SmallVec<[NodeOutputId; 4]> =
        graph.node_inputs(placeholder).into_iter().collect();
    if inputs.len() != 3 {
        // Not a placeholder shape.  Surface as a typed error so
        // callers don't silently mis-apply.
        return Err(anyhow!(
            "expected IndirectBranch with [control, memory, target_value] (3 inputs) node, got {kind:?}"
        ));
    }
    let control_in = inputs[0];
    let memory_in = inputs[1];
    let target_value = inputs[2];

    // Surface a non-integer target type as a typed error — silently
    // defaulting to U64 would mask an upstream invariant break (every
    // BranchIndirect placeholder's target_value must be an integer
    // address).
    let target_int_ty = graph
        .output_kind(target_value)
        .as_integer_or_err()
        .map_err(|e| anyhow!(
            "apply_tail_call: expected integer target type for IndirectBranch placeholder, \
             got {:?} (node {:?}): {e}",
            graph.output_kind(target_value),
            placeholder
        ))?;

    // Snapshot the placeholder's asm-fingerprint BEFORE detaching it; we
    // absorb it into every new node spliced in below so the placeholder's
    // contributing-asm-instruction history survives the rewrite.
    let placeholder_fingerprint: Vec<u64> = graph.asm_fingerprint(placeholder).to_vec();

    // CORRECTNESS — detach BEFORE creating the new chain: removes the
    // placeholder's three inputs from their use-lists.
    graph.detach_node_inputs(placeholder);

    let masked_target = u128::from(target) & target_int_ty.bit_mask_u128();
    let int_const = graph.create_node(
        NodeKind::IntConst(masked_target),
        [],
        [NodeOutputKind::OutputType(target_int_ty)],
    );
    graph.extend_asm_fingerprint(int_const, &placeholder_fingerprint);
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
    graph.extend_asm_fingerprint(call, &placeholder_fingerprint);
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
    graph.extend_asm_fingerprint(new_return, &placeholder_fingerprint);

    Ok(new_return)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the in-place editors at the opt-crate level.
    //! Mirror the original strider tests; the strider shim's tests
    //! continue to exercise the shim path.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_ir::FunctionBuilder;
    use strider_ir::node::NodeOutputType;

    fn build_placeholder_graph() -> (strider_ir::BuiltFunctionGraph, NodeId) {
        let mut builder = FunctionBuilder::empty()
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        builder.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
        let target = builder.build_int_const(0xdeadu64, NodeOutputType::U64).unwrap();
        builder.build_indirect_branch(target).expect("build_indirect_branch");
        builder.set_lift_addr(None);
        let built = builder.build().expect("build");
        // Locate the unique IndirectBranch placeholder.
        let mut found: Option<NodeId> = None;
        for nid in built.preorder() {
            if matches!(built.graph.node_kind(nid), NodeKind::IndirectBranch) {
                assert!(found.is_none(), "more than one IndirectBranch");
                found = Some(nid);
            }
        }
        (built, found.expect("IndirectBranch"))
    }

    #[test]
    fn apply_link_register_keeps_return_node_id() {
        // Pre-edit: IndirectBranch [ctrl, mem, target_value].  Post-edit:
        // Return [ctrl, mem] — same NodeId, kind mutated in place.
        let (mut ctx, placeholder) = build_placeholder_graph();
        let inputs_before: Vec<_> =
            ctx.node_inputs(placeholder).into_iter().collect();
        assert_eq!(inputs_before.len(), 3);
        apply_link_register(&mut crate::pattern::RewriteCtx::for_built(&mut ctx), placeholder, &[]).expect("apply");
        assert!(matches!(ctx.node_kind(placeholder), NodeKind::Return));
    }

    #[test]
    fn apply_link_register_rejects_non_indirect_branch_node() {
        let (mut ctx, _placeholder) = build_placeholder_graph();
        let int_const_id = ctx.graph
            .all_node_ids()
            .find(|&nid| matches!(ctx.node_kind(nid), NodeKind::IntConst(_)))
            .expect("graph has at least one IntConst");
        let result = apply_link_register(&mut crate::pattern::RewriteCtx::for_built(&mut ctx), int_const_id, &[]);
        assert!(result.is_err(), "must reject non-IndirectBranch: {result:?}");
    }

    #[test]
    fn apply_tail_call_emits_call_then_return() {
        let (mut ctx, placeholder) = build_placeholder_graph();
        let _new_return =
            apply_tail_call(&mut crate::pattern::RewriteCtx::for_built(&mut ctx), placeholder, 0xc0de_u64, &[], &[], &[])
                .expect("apply");
        // The new Return must be reachable from entry; the placeholder
        // is detached.  Walk all node ids to confirm a Call materialised.
        let mut had_call = false;
        for nid in ctx.all_node_ids() {
            if matches!(ctx.node_kind(nid), NodeKind::Call) {
                had_call = true;
                break;
            }
        }
        assert!(had_call, "Call node must materialise");
    }

    #[test]
    fn apply_tail_call_rejects_non_indirect_branch_node() {
        // A real Return is not a placeholder; reject.  (The arity check
        // is unreachable through any non-placeholder path, since the
        // builder doesn't emit malformed IndirectBranch nodes.)
        let mut builder = FunctionBuilder::empty()
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("region");
        builder.set_entry_region(region).expect("entry");
        builder.set_region(region);
        builder.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
        builder.build_return(None, &[]).expect("return");
        builder.set_lift_addr(None);
        let mut ctx = builder.build().expect("build");
        let ret_id = ctx.graph
            .all_node_ids()
            .find(|&nid| matches!(ctx.node_kind(nid), NodeKind::Return))
            .expect("Return");
        let result = apply_tail_call(&mut crate::pattern::RewriteCtx::for_built(&mut ctx), ret_id, 0xc0de, &[], &[], &[]);
        assert!(result.is_err(), "must reject Return: {result:?}");
    }

    /// Spawns a value-typed `IntConst` and returns its single output id —
    /// a convenient stand-in for an "ABI register's IR value at the
    /// placeholder site" in unit tests that don't care which register
    /// it came from.
    fn synth_value_output(
        graph: &mut strider_ir::Graph,
        value: u128,
        ty: NodeOutputType,
    ) -> NodeOutputId {
        let nid = graph.create_node(
            NodeKind::IntConst(value),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        // Stamp sentinel asm-fingerprint so the Layer-C check passes
        // for this synthesised node (it bypasses FunctionBuilder's
        // lift_addr plumbing).
        graph.set_asm_fingerprint(nid, vec![strider_ir::test_utils::SENTINEL_LIFT_ADDR]);
        graph
            .node_outputs_exact::<1>(nid)
            .expect("IntConst has one output")[0]
    }

    // Calling-convention threading regression tests for the in-place editors.
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
        let (mut ctx, placeholder) = build_placeholder_graph();
        let inputs_before: Vec<_> = ctx.node_inputs(placeholder).into_iter().collect();
        assert_eq!(inputs_before.len(), 3);
        let r0 = synth_value_output(&mut ctx.graph, 0x42, NodeOutputType::U64);
        let r1 = synth_value_output(&mut ctx.graph, 0x43, NodeOutputType::U64);
        apply_link_register(&mut crate::pattern::RewriteCtx::for_built(&mut ctx), placeholder, &[r0, r1]).expect("apply");
        let inputs_after: Vec<_> = ctx.node_inputs(placeholder).into_iter().collect();
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
        let (mut ctx, placeholder) = build_placeholder_graph();
        let a0 = synth_value_output(&mut ctx.graph, 0x01, NodeOutputType::U64);
        let a1 = synth_value_output(&mut ctx.graph, 0x02, NodeOutputType::U64);
        let a2 = synth_value_output(&mut ctx.graph, 0x03, NodeOutputType::U64);
        let new_return =
            apply_tail_call(&mut crate::pattern::RewriteCtx::for_built(&mut ctx), placeholder, 0xc0de, &[a0, a1, a2], &[], &[])
                .expect("apply");
        // The new Return's input #0 is the Call's ctrl output.  Walk
        // back to the Call.
        let new_return_inputs: Vec<_> =
            ctx.node_inputs(new_return).into_iter().collect();
        let call_ctrl = new_return_inputs[0];
        let (call_node, _) = ctx.output_definition(call_ctrl);
        assert!(matches!(ctx.node_kind(call_node), NodeKind::Call));
        let call_inputs: Vec<_> = ctx.node_inputs(call_node).into_iter().collect();
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
        let (mut ctx, placeholder) = build_placeholder_graph();
        let clob_kinds = [
            NodeOutputKind::OutputType(NodeOutputType::U64),
            NodeOutputKind::OutputType(NodeOutputType::U32),
        ];
        let new_return = apply_tail_call(
            &mut crate::pattern::RewriteCtx::for_built(&mut ctx),
            placeholder,
            0xbeef,
            &[],
            &clob_kinds,
            &[],
        )
        .expect("apply");
        // Walk to the Call.
        let new_return_inputs: Vec<_> =
            ctx.node_inputs(new_return).into_iter().collect();
        let (call_node, _) = ctx.output_definition(new_return_inputs[0]);
        let call_outputs: Vec<_> = ctx.node_outputs(call_node).into_iter().collect();
        assert_eq!(
            call_outputs.len(),
            4,
            "Call must have [Control, Memory, clob_0, clob_1]",
        );
        assert_eq!(ctx.output_kind(call_outputs[2]), clob_kinds[0]);
        assert_eq!(ctx.output_kind(call_outputs[3]), clob_kinds[1]);
    }

    #[test]
    fn apply_tail_call_threads_ret_val_outputs_into_return() {
        // Two ret-val outputs → new Return's inputs are
        // `[call_ctrl, call_mem, ret_val_0, ret_val_1]`.
        let (mut ctx, placeholder) = build_placeholder_graph();
        let r0 = synth_value_output(&mut ctx.graph, 0x10, NodeOutputType::U64);
        let r1 = synth_value_output(&mut ctx.graph, 0x11, NodeOutputType::U64);
        let new_return =
            apply_tail_call(&mut crate::pattern::RewriteCtx::for_built(&mut ctx), placeholder, 0xface, &[], &[], &[r0, r1])
                .expect("apply");
        let inputs: Vec<_> = ctx.node_inputs(new_return).into_iter().collect();
        assert_eq!(inputs.len(), 4, "[call_ctrl, call_mem, r0, r1]");
        assert_eq!(inputs[2], r0);
        assert_eq!(inputs[3], r1);
    }

    /// Regression: `apply_tail_call` must propagate an
    /// `Err` (not silently default to `U64`) when the placeholder's
    /// `target_value` has a non-integer output type.  We construct a
    /// malformed IndirectBranch directly via `Graph::create_node` so the
    /// builder's typechecking doesn't reject it; this exercises the
    /// defensive `as_integer_or_err()?` path.
    #[test]
    fn apply_tail_call_rejects_non_integer_target_type() {
        let (mut ctx, placeholder) = build_placeholder_graph();
        // Build a Bool-typed value that we'll splice into the placeholder's
        // target_value slot.  `BoolConst` produces a single Bool output.
        let bool_const = ctx.create_node(
            NodeKind::BoolConst(true),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::Bool)],
        );
        ctx.graph
            .set_asm_fingerprint(bool_const, vec![strider_ir::test_utils::SENTINEL_LIFT_ADDR]);
        let bool_out = ctx.node_outputs(bool_const).into_iter().next().unwrap();
        // Replace the IndirectBranch's input[2] (target_value) with the Bool output.
        let target_input_id = ctx
            .graph
            .node_input_id_at(placeholder, 2)
            .expect("input slot 2 exists");
        ctx.update_input(target_input_id, bool_out);
        // Sanity: the placeholder now has a Bool target_value.
        let target_value_kind = ctx
            .graph
            .output_kind(ctx.node_inputs(placeholder)[2]);
        assert!(
            matches!(target_value_kind, NodeOutputKind::OutputType(NodeOutputType::Bool)),
            "fixture must have Bool target_value, got {target_value_kind:?}"
        );

        let result = apply_tail_call(&mut crate::pattern::RewriteCtx::for_built(&mut ctx), placeholder, 0xc0de, &[], &[], &[]);
        let err = result.expect_err("non-integer target_value must propagate as Err");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("integer") || msg.contains("Bool"),
            "Err must name the type problem; got: {msg}"
        );
    }
}
