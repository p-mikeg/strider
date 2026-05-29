//! In-place IR edits for resolutions that don't require a CFG rebuild.
//!
//! Two variants are supported:
//!
//!   * **`LinkRegister`**: the placeholder `IndirectBranch(target_value)`
//!     is replaced with a real `Return [ctrl, mem, ret_val_*]`.  A
//!     fresh `Return` node is created on the same control and memory
//!     inputs, with the convention's `ret_val_regs` appended.  The
//!     placeholder is detached and becomes a zombie unreachable node.
//!   * **`Single` tail call**: replace the placeholder
//!     `IndirectBranch(target_value)` with `Call(IntConst(target)) →
//!     Return(ret_vars)`.  The placeholder is detached (becoming a
//!     zombie unreachable node) and a fresh `IntConst → Call → Return`
//!     chain is wired on the same control and memory inputs.

#![allow(clippy::module_name_repetitions)]

use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use anyhow::anyhow;

use crate::opt::error::Result;

/// Per-placeholder data extracted by [`detach_placeholder`] — the three
/// inputs of the pre-edit `IndirectBranch [control, memory, target_value]`
/// plus the placeholder's snapshotted asm-fingerprint.  Returned to
/// each rewriter so it can build the splice tail without re-querying
/// the (now-detached) placeholder.
struct PlaceholderEdit {
    control_in: NodeOutputId,
    memory_in: NodeOutputId,
    target_value: NodeOutputId,
    fingerprint: Vec<u64>,
}

/// Validates that `placeholder` is a `NodeKind::IndirectBranch` with the
/// expected `[control, memory, target_value]` shape, snapshots its
/// asm-fingerprint, and detaches its inputs so the caller can build a
/// fresh replacement chain on top of the same control / memory edges.
///
/// Shared prelude for [`apply_link_register`] and [`apply_tail_call`].
///
/// # Errors
///
/// Returns an error when `placeholder` is not a
/// [`NodeKind::IndirectBranch`] node, or when its input arity isn't the
/// expected 3.
fn detach_placeholder(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    placeholder: NodeId,
) -> Result<PlaceholderEdit> {
    let function = ctx.function_mut();
    let kind = *function.node_kind(placeholder);
    if !matches!(kind, NodeKind::IndirectBranch) {
        return Err(anyhow!("expected IndirectBranch node, got {kind:?}"));
    }
    let [control_in, memory_in, target_value] =
        function.node_inputs_exact::<3>(placeholder).map_err(|_| {
            anyhow!(
                "expected IndirectBranch with [control, memory, target_value] (3 inputs) node, \
                 got {kind:?}"
            )
        })?;

    // Snapshot the placeholder's asm-fingerprint BEFORE detaching it;
    // every freshly-spliced node absorbs it so the placeholder's
    // contributing-asm-instruction history survives the rewrite.
    let fingerprint: Vec<u64> = function.asm_fingerprint(placeholder).to_vec();

    // Detach BEFORE creating new nodes: removes the placeholder's three
    // inputs from their use-lists so fresh nodes cleanly take ownership
    // of the control / memory edges.
    function.detach_node_inputs(placeholder);

    Ok(PlaceholderEdit { control_in, memory_in, target_value, fingerprint })
}

/// Applies the `LinkRegister` resolution to a placeholder
/// `IndirectBranch(control, memory, target_value)` node, replacing it
/// with a real `Return [control, memory, ret_val_0, …]`.  The
/// placeholder's `target_value` slot is dropped (no longer meaningful
/// — the LR-targeted branch IS the return) and `ret_val_outputs`
/// are appended as the actual return values.  The placeholder is
/// detached (becomes a zombie unreachable from `entry`); the new
/// Return is wired on the placeholder's pre-edit control and memory
/// inputs.  The orchestrator's next region-walk picks up the new
/// terminator via the freshly-computed exit-control mapping.
///
/// Pre-edit: `IndirectBranch [control, memory, target_value]`
/// Post-edit: `Return [control, memory, ret_val_0, …]`
///
/// # Errors
///
/// Returns an error when `placeholder` is not a
/// [`NodeKind::IndirectBranch`], or when the IR mutation fails.
pub fn apply_link_register(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    placeholder: NodeId,
    ret_val_outputs: &[NodeOutputId],
) -> Result<NodeId> {
    // Defensive: ret-val outputs must reference ABI register values
    // produced upstream of the placeholder, never the placeholder's
    // own outputs.  Including a placeholder output would create a
    // self-referential edge after detach_placeholder runs (the new
    // Return node would consume a value produced by the soon-to-be-
    // zombie placeholder).  Sleigh's contract + the orchestrator's
    // construction site guarantee this; the assert pins the invariant.
    debug_assert!(
        {
            let placeholder_outs: &[NodeOutputId] =
                ctx.graph_ref().node_outputs(placeholder);
            !ret_val_outputs.iter().any(|o| placeholder_outs.contains(o))
        },
        "apply_link_register: ret_val_outputs must not reference placeholder's own outputs",
    );
    let PlaceholderEdit { control_in, memory_in, target_value: _, fingerprint } =
        detach_placeholder(ctx, placeholder)?;
    let function = ctx.function_mut();

    let mut return_inputs: Vec<NodeOutputId> = Vec::with_capacity(2 + ret_val_outputs.len());
    return_inputs.push(control_in);
    return_inputs.push(memory_in);
    return_inputs.extend_from_slice(ret_val_outputs);
    let new_return = function.create_node(NodeKind::Return, return_inputs, []);
    function.extend_asm_fingerprint(new_return, &fingerprint);
    Ok(new_return)
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
/// Call+Return — see the crate-internal `AnchorCallingContext` for how
/// the opt pass and the strider orchestrator populate them.  Empty
/// slices are sound (the resulting Call/Return is degenerate but
/// well-typed); a real ABI-aware caller passes the placeholder's
/// pre-edit ABI register values.
///
/// `no_memory_clobber = true` suppresses the Call's memory output so
/// the new Return wires the *pre-Call* memory edge directly — required
/// for `__fentry__`-style tracing pre-ambles and any other ABI that
/// guarantees no memory side-effects through the call (e.g. the
/// `x86_64_all_preserving` preset).  Mirrors the semantics of
/// `FunctionBuilder::build_call_with_cc` when its CC carries
/// `no_memory_clobber`.
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
    no_memory_clobber: bool,
) -> Result<NodeId> {
    // Defensive: arg-passing and ret-val outputs must reference ABI
    // register values produced upstream of the placeholder, never the
    // placeholder's own outputs.  Including a placeholder output would
    // create a self-referential edge after `detach_placeholder` runs
    // (the new Call / Return would consume a value produced by the
    // soon-to-be-zombie placeholder).  Sleigh's contract + the
    // orchestrator's construction site guarantee this; the asserts pin
    // the invariant.
    debug_assert!(
        {
            let placeholder_outs: &[NodeOutputId] =
                ctx.graph_ref().node_outputs(placeholder);
            !arg_passing_outputs.iter().any(|o| placeholder_outs.contains(o))
        },
        "apply_tail_call: arg_passing_outputs must not reference placeholder's own outputs",
    );
    debug_assert!(
        {
            let placeholder_outs: &[NodeOutputId] =
                ctx.graph_ref().node_outputs(placeholder);
            !ret_val_outputs.iter().any(|o| placeholder_outs.contains(o))
        },
        "apply_tail_call: ret_val_outputs must not reference placeholder's own outputs",
    );
    let PlaceholderEdit { control_in, memory_in, target_value, fingerprint } =
        detach_placeholder(ctx, placeholder)?;
    let function = ctx.function_mut();

    // Surface a non-integer target type as a typed error — silently
    // defaulting to I64 would mask an upstream invariant break (every
    // BranchIndirect placeholder's target_value must be an integer
    // address).
    let target_int_ty = function
        .output_kind(target_value)
        .as_integer_or_err()
        .map_err(|e| anyhow!(
            "apply_tail_call: expected integer target type for IndirectBranch placeholder, \
             got {:?} (node {:?}): {e}",
            function.output_kind(target_value),
            placeholder
        ))?;

    let masked_target = u128::from(target) & target_int_ty.bit_mask_u128();
    let int_const = function.create_node(
        NodeKind::IntConst(masked_target),
        [],
        [NodeOutputKind::OutputType(target_int_ty)],
    );
    function.extend_asm_fingerprint(int_const, &fingerprint);
    let int_const_out = function.node_outputs_exact::<1>(int_const)?[0];

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
    let call = function.create_node(NodeKind::Call, call_inputs, call_outputs);
    function.extend_asm_fingerprint(call, &fingerprint);
    // Slot 0 = Control, slot 1 = Memory.  The clobbered slots beyond
    // those are produced for downstream consumers (typically empty
    // here because the only consumer is the freshly-spliced Return).
    //
    // Memory-preserving CCs (e.g. `x86_64_all_preserving`,
    // `__fentry__`-style tracing pre-ambles) leave the Call's Memory
    // output dangling and wire the pre-Call memory edge into the new
    // Return directly, so LoadReadOnly / LoadForward chains stay
    // intact across the spliced tail call.  Mirrors
    // `FunctionBuilder::build_call_with_cc`'s `no_memory_clobber` branch
    // (`builder/call.rs` — same Call output shape; only the
    // region-memory advance differs).
    let call_outs: Vec<_> = function.node_outputs(call).to_vec();
    let call_ctrl_out = call_outs[0];
    let mem_for_return = if no_memory_clobber {
        memory_in
    } else {
        call_outs[1]
    };

    let mut new_return_inputs: Vec<NodeOutputId> = Vec::with_capacity(2 + ret_val_outputs.len());
    new_return_inputs.push(call_ctrl_out);
    new_return_inputs.push(mem_for_return);
    new_return_inputs.extend_from_slice(ret_val_outputs);
    let new_return = function.create_node(NodeKind::Return, new_return_inputs, []);
    function.extend_asm_fingerprint(new_return, &fingerprint);

    Ok(new_return)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the in-place editors at the opt-crate level.
    //! Mirror the original strider tests; the strider shim's tests
    //! continue to exercise the shim path.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::pattern::GraphRewriteCtxExt;
    use strider_ir::FunctionBuilder;
    use strider_ir::node::NodeOutputType;

    fn build_placeholder_graph() -> (strider_ir::Function, NodeId) {
        let mut builder = FunctionBuilder::empty()
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let target = builder.build_int_const(0xdeadu64, NodeOutputType::I64).unwrap();
        builder.build_indirect_branch(target).expect("build_indirect_branch");
        builder.set_lift_addr(None);
        let built = builder.build().expect("build");
        // Locate the unique IndirectBranch placeholder.
        let mut found: Option<NodeId> = None;
        for nid in built.walk() {
            if matches!(built.node_kind(nid), NodeKind::IndirectBranch) {
                assert!(found.is_none(), "more than one IndirectBranch");
                found = Some(nid);
            }
        }
        (built, found.expect("IndirectBranch"))
    }

    #[test]
    fn apply_link_register_emits_return_and_detaches_placeholder() {
        // Pre-edit: IndirectBranch [ctrl, mem, target_value].  Post-edit:
        // a freshly-created Return [ctrl, mem] node materialises; the
        // placeholder is detached (zero inputs, still reachable by id
        // but no longer wired into the graph).
        let (mut ctx, placeholder) = build_placeholder_graph();
        assert_eq!(ctx.node_inputs(placeholder).len(), 3);
        ctx.with_rewrite_ctx(|rctx| apply_link_register(rctx, placeholder, &[])).expect("apply");
        // Placeholder is detached: its inputs are gone, and its kind
        // remains IndirectBranch (the orchestrator filters by kind
        // via find_indirect_branch_placeholder, so leaving the kind
        // as IndirectBranch is fine — what matters is detachment so
        // it's no longer reachable via the use-list walk).
        assert_eq!(ctx.node_inputs(placeholder).len(), 0);
        // A fresh Return materialised.
        let mut had_return = false;
        for nid in ctx.all_node_ids() {
            if matches!(ctx.node_kind(nid), NodeKind::Return) && !ctx.node_inputs(nid).is_empty() {
                had_return = true;
                break;
            }
        }
        assert!(had_return, "Return node must materialise");
    }

    #[test]
    fn apply_link_register_rejects_non_indirect_branch_node() {
        let (mut ctx, _placeholder) = build_placeholder_graph();
        let int_const_id = ctx
            .all_node_ids()
            .find(|&nid| matches!(ctx.node_kind(nid), NodeKind::IntConst(_)))
            .expect("graph has at least one IntConst");
        let result = ctx.with_rewrite_ctx(|rctx| apply_link_register(rctx, int_const_id, &[]));
        assert!(result.is_err(), "must reject non-IndirectBranch: {result:?}");
    }

    #[test]
    fn apply_tail_call_emits_call_then_return() {
        let (mut ctx, placeholder) = build_placeholder_graph();
        let _new_return = ctx
            .with_rewrite_ctx(|rctx| apply_tail_call(rctx, placeholder, 0xc0de_u64, &[], &[], &[], false))
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
        builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        builder.build_return(None, &[]).expect("return");
        builder.set_lift_addr(None);
        let mut ctx = builder.build().expect("build");
        let ret_id = ctx
            .all_node_ids()
            .find(|&nid| matches!(ctx.node_kind(nid), NodeKind::Return))
            .expect("Return");
        let result =
            ctx.with_rewrite_ctx(|rctx| apply_tail_call(rctx, ret_id, 0xc0de, &[], &[], &[], false));
        assert!(result.is_err(), "must reject Return: {result:?}");
    }

    /// Spawns a value-typed `IntConst` and returns its single output id —
    /// a convenient stand-in for an "ABI register's IR value at the
    /// placeholder site" in unit tests that don't care which register
    /// it came from.
    fn synth_value_output(
        function: &mut strider_ir::Function,
        value: u128,
        ty: NodeOutputType,
    ) -> NodeOutputId {
        let nid = function.create_node(
            NodeKind::IntConst(value),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        // Stamp sentinel asm-fingerprint so the Layer-C check passes
        // for this synthesised node (it bypasses FunctionBuilder's
        // lift_addr plumbing).
        function.set_asm_fingerprint(nid, vec![strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
        function
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
        assert_eq!(ctx.node_inputs(placeholder).len(), 3);
        let r0 = synth_value_output(&mut ctx, 0x42, NodeOutputType::I64);
        let r1 = synth_value_output(&mut ctx, 0x43, NodeOutputType::I64);
        let new_return = ctx
            .with_rewrite_ctx(|rctx| apply_link_register(rctx, placeholder, &[r0, r1]))
            .expect("apply");
        assert!(matches!(ctx.node_kind(new_return), NodeKind::Return));
        assert_eq!(
            ctx.node_inputs(new_return).len(),
            2 + 2,
            "Return inputs are [ctrl, mem, ret_val_0, ret_val_1] after target_value removal",
        );
        assert_eq!(ctx.nth_input(new_return, 2), Some(r0));
        assert_eq!(ctx.nth_input(new_return, 3), Some(r1));
    }

    #[test]
    fn apply_tail_call_threads_arg_passing_into_call() {
        // Three arg-passing outputs → Call's inputs are
        // `[ctrl, mem, IntConst(target), arg_0, arg_1, arg_2]`.
        let (mut ctx, placeholder) = build_placeholder_graph();
        let a0 = synth_value_output(&mut ctx, 0x01, NodeOutputType::I64);
        let a1 = synth_value_output(&mut ctx, 0x02, NodeOutputType::I64);
        let a2 = synth_value_output(&mut ctx, 0x03, NodeOutputType::I64);
        let new_return = ctx
            .with_rewrite_ctx(|rctx| {
                apply_tail_call(rctx, placeholder, 0xc0de, &[a0, a1, a2], &[], &[], false)
            })
            .expect("apply");
        // The new Return's input #0 is the Call's ctrl output.  Walk
        // back to the Call.
        let call_ctrl = ctx.nth_input(new_return, 0).expect("ctrl slot");
        let (call_node, _) = ctx.output_definition(call_ctrl);
        assert!(matches!(ctx.node_kind(call_node), NodeKind::Call));
        assert_eq!(
            ctx.node_inputs(call_node).len(),
            6,
            "Call must have [ctrl, mem, target, a0, a1, a2]",
        );
        assert_eq!(ctx.nth_input(call_node, 3), Some(a0));
        assert_eq!(ctx.nth_input(call_node, 4), Some(a1));
        assert_eq!(ctx.nth_input(call_node, 5), Some(a2));
    }

    #[test]
    fn apply_tail_call_threads_clobbered_kinds_into_call_outputs() {
        // Two clobbered output kinds → Call's outputs are
        // `[Control, Memory, clob_0, clob_1]`.
        let (mut ctx, placeholder) = build_placeholder_graph();
        let clob_kinds = [
            NodeOutputKind::OutputType(NodeOutputType::I64),
            NodeOutputKind::OutputType(NodeOutputType::I32),
        ];
        let new_return = ctx
            .with_rewrite_ctx(|rctx| {
                apply_tail_call(rctx, placeholder, 0xbeef, &[], &clob_kinds, &[], false)
            })
            .expect("apply");
        // Walk to the Call.
        let new_return_ctrl = ctx.nth_input(new_return, 0).expect("ctrl slot");
        let (call_node, _) = ctx.output_definition(new_return_ctrl);
        let call_outputs: Vec<_> = ctx.node_outputs(call_node).to_vec();
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
        let r0 = synth_value_output(&mut ctx, 0x10, NodeOutputType::I64);
        let r1 = synth_value_output(&mut ctx, 0x11, NodeOutputType::I64);
        let new_return = ctx
            .with_rewrite_ctx(|rctx| {
                apply_tail_call(rctx, placeholder, 0xface, &[], &[], &[r0, r1], false)
            })
            .expect("apply");
        assert_eq!(ctx.node_inputs(new_return).len(), 4, "[call_ctrl, call_mem, r0, r1]");
        assert_eq!(ctx.nth_input(new_return, 2), Some(r0));
        assert_eq!(ctx.nth_input(new_return, 3), Some(r1));
    }

    /// Regression: `apply_tail_call` must propagate an
    /// `Err` (not silently default to `I64`) when the placeholder's
    /// `target_value` has a non-integer output type.  We construct a
    /// malformed IndirectBranch directly via `Graph::create_node` so the
    /// builder's typechecking doesn't reject it; this exercises the
    /// defensive `as_integer_or_err()?` path.
    #[test]
    fn apply_tail_call_rejects_non_integer_target_type() {
        let (mut ctx, placeholder) = build_placeholder_graph();
        // Build a float-typed value that we'll splice into the placeholder's
        // target_value slot.  Booleans are now 1-bit *integers*, so a float
        // is the only non-integer value type that exercises the
        // `as_integer_or_err()?` rejection path.
        let float_const = ctx.create_node(
            NodeKind::FloatConst(0),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::F32)],
        );
        ctx.set_asm_fingerprint(float_const, vec![strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
        let bool_out = ctx.node_outputs(float_const).iter().copied().next().unwrap();
        // Replace the IndirectBranch's input[2] (target_value) with the float output.
        let target_input_id = ctx
            .node_input_id_at(placeholder, 2)
            .expect("input slot 2 exists");
        ctx.update_input(target_input_id, bool_out);
        // Sanity: the placeholder now has a float (non-integer) target_value.
        let target_value_kind = ctx
            .output_kind(ctx.node_inputs(placeholder)[2]);
        assert!(
            matches!(target_value_kind, NodeOutputKind::OutputType(NodeOutputType::F32)),
            "fixture must have a non-integer (float) target_value, got {target_value_kind:?}"
        );

        let result =
            ctx.with_rewrite_ctx(|rctx| apply_tail_call(rctx, placeholder, 0xc0de, &[], &[], &[], false));
        let err = result.expect_err("non-integer target_value must propagate as Err");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("integer") || msg.contains("Bool"),
            "Err must name the type problem; got: {msg}"
        );
    }
}
