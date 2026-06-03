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

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};

use anyhow::anyhow;

use crate::opt::error::Result;

/// Per-placeholder data extracted by [`detach_placeholder`] — the three
/// inputs of the pre-edit `IndirectBranch [control, memory, target_value]`
/// plus the placeholder's snapshotted asm-fingerprint.  Returned to
/// each rewriter so it can build the splice tail without re-querying
/// the (now-detached) placeholder.
struct PlaceholderEdit {
    control_value: ValueId,
    memory_value: ValueId,
    target_value: ValueId,
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
/// [`NodeKind::IndirectBranch`] node.  (Once the kind is established, the
/// 3-input arity is a node-signature invariant and is asserted rather
/// than returned as an error.)
fn detach_placeholder(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    placeholder: NodeId,
) -> Result<PlaceholderEdit> {
    let kind = *ctx.node_kind(placeholder);
    if !matches!(kind, NodeKind::IndirectBranch) {
        return Err(anyhow!("expected IndirectBranch node, got {kind:?}"));
    }
    // IndirectBranch has exactly 3 inputs [control, memory, target_value]
    // (validated structural invariant).
    let [control_value, memory_value, target_value] = ctx
        .graph_ref()
        .node_inputs_exact::<3>(placeholder)
        .expect("IndirectBranch has 3 inputs per node signature");

    // Detach BEFORE creating new nodes: removes the placeholder's three
    // inputs from their use-lists so fresh nodes cleanly take ownership
    // of the control / memory edges.  The placeholder's NodeId stays a
    // valid arena slot with its asm-fingerprint intact, so callers
    // attribute every freshly-spliced node to it (via
    // `create_node_attributed(&[placeholder])`) — preserving the
    // placeholder's contributing-asm history across the rewrite.
    ctx.detach_node_inputs(placeholder);

    Ok(PlaceholderEdit { control_value, memory_value, target_value })
}

/// Applies the `LinkRegister` resolution to a placeholder
/// `IndirectBranch(control, memory, target_value)` node, replacing it
/// with a real `Return [control, memory, ret_val_0, …]`.  The
/// placeholder's `target_value` slot is dropped (no longer meaningful
/// — the LR-targeted branch IS the return) and `ret_val_values`
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
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    placeholder: NodeId,
    ret_val_values: &[ValueId],
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
            let placeholder_outs: &[ValueId] =
                ctx.graph_ref().node_outputs(placeholder);
            !ret_val_values.iter().any(|o| placeholder_outs.contains(o))
        },
        "apply_link_register: ret_val_values must not reference placeholder's own outputs",
    );
    let PlaceholderEdit { control_value, memory_value, target_value: _ } =
        detach_placeholder(ctx, placeholder)?;

    let mut return_inputs: Vec<ValueId> = Vec::with_capacity(2 + ret_val_values.len());
    return_inputs.push(control_value);
    return_inputs.push(memory_value);
    return_inputs.extend_from_slice(ret_val_values);
    // Attribute the new Return to the (now-detached) placeholder so it
    // absorbs the placeholder's asm-fingerprint history.
    let new_return =
        ctx.create_node_attributed(NodeKind::Return, return_inputs, [], &[placeholder]);
    Ok(new_return)
}

/// Applies the `Single`-tail-call resolution by replacing the
/// placeholder `IndirectBranch(target_value)` with
/// `Call(IntConst(target)) → Return(ret_vars)`.
///
/// Pre-edit: `IndirectBranch(control, memory, target_value)`
/// Post-edit: `IntConst(target) →
///   Call(control, memory, IntConst, sp, arg_passing_0, …) [outs:
///   Control, Memory, clob_0, …] →
///   Return(call.ctrl_out, call.mem_value, ret_val_0, …)`
///
/// `sp_value` is the stack-pointer value at the dispatch site, read by
/// the orchestrator via [`crate::opt::AnchorCallingContext`] and wired
/// as the Call's SP input anchor ahead of the args (mirroring
/// `FunctionBuilder::build_call`).
///
/// The placeholder is detached (becomes a zombie unreachable from
/// `entry`).  The new Return is wired on the Call's control and memory
/// outputs.  Returns the new Return's [`NodeId`] so callers can patch
/// any cached exit-control handles.
///
/// `arg_passing_values`, `ret_val_kinds`, `clobbered_kinds`, and
/// `ret_val_values` thread the calling-convention context through the
/// freshly-spliced Call+Return — see the crate-internal
/// `AnchorCallingContext` for how the opt pass and the strider
/// orchestrator populate them.  The spliced Call's value outputs are
/// `[Control, Memory] ++ ret_val_kinds ++ clobbered_kinds`, mirroring
/// [`strider_ir::FunctionBuilder::build_call`]'s two-group layout so the
/// node passes the validator's `Call` arity check (`2 + ret_val_count +
/// clobber_count`) for a real ABI.  `ret_val_kinds` is the
/// tracked-filtered ret-val list; `ret_val_values` is the *raw* declared
/// ret-val list fed to the Return (the two may differ in length).  Empty
/// slices are sound (the resulting Call/Return is degenerate but
/// well-typed); a real ABI-aware caller passes the placeholder's
/// pre-edit ABI register values.
///
/// `preserves_memory = true` suppresses the Call's memory output so
/// the new Return wires the *pre-Call* memory edge directly — required
/// for `__fentry__`-style tracing pre-ambles and any other ABI that
/// guarantees no memory side-effects through the call (e.g. the
/// `x86_64_all_preserving` preset).  Mirrors the semantics of
/// `FunctionBuilder::build_call` when its CC carries
/// `preserves_memory`.
///
/// # Errors
///
/// Returns an error when `placeholder` is not a
/// [`NodeKind::IndirectBranch`] node, when its input arity isn't the
/// expected 3 (i.e. not a placeholder shape), or when IR
/// construction fails.
// The placeholder plus the SP anchor and the ABI channels
// (args / ret-val kinds / clobber kinds / ret-vals) plus the
// preserves-memory toggle is the natural shape; bundling them into a
// struct would add boilerplate without simplifying the call site.
#[allow(clippy::too_many_arguments)]
pub fn apply_tail_call(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    placeholder: NodeId,
    target: u64,
    sp_value: ValueId,
    arg_passing_values: &[ValueId],
    ret_val_kinds: &[ValueKind],
    clobbered_kinds: &[ValueKind],
    ret_val_values: &[ValueId],
    preserves_memory: bool,
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
            let placeholder_outs: &[ValueId] =
                ctx.graph_ref().node_outputs(placeholder);
            !arg_passing_values.iter().any(|o| placeholder_outs.contains(o))
        },
        "apply_tail_call: arg_passing_values must not reference placeholder's own outputs",
    );
    debug_assert!(
        {
            let placeholder_outs: &[ValueId] =
                ctx.graph_ref().node_outputs(placeholder);
            !ret_val_values.iter().any(|o| placeholder_outs.contains(o))
        },
        "apply_tail_call: ret_val_values must not reference placeholder's own outputs",
    );
    let PlaceholderEdit { control_value, memory_value, target_value } =
        detach_placeholder(ctx, placeholder)?;

    // Surface a non-integer target type as a typed error — silently
    // defaulting to I64 would mask an upstream invariant break (every
    // BranchIndirect placeholder's target_value must be an integer
    // address).
    let target_int_ty = ctx
        .value_kind(target_value)
        .as_integer_or_err()
        .map_err(|e| anyhow!(
            "apply_tail_call: expected integer target type for IndirectBranch placeholder, \
             got {:?} (node {:?}): {e}",
            ctx.value_kind(target_value),
            placeholder
        ))?;

    let masked_target = u128::from(target) & target_int_ty.bit_mask_u128();
    // Each freshly-spliced node is attributed to the (now-detached)
    // placeholder so it absorbs the placeholder's asm-fingerprint
    // history.
    let int_const = ctx.create_node_attributed(
        NodeKind::IntConst(masked_target),
        [],
        [ValueKind::Typed(target_int_ty)],
        &[placeholder],
    );
    let [int_const_value] = ctx
        .node_outputs_exact::<1>(int_const)
        .expect("freshly created IntConst has 1 output per node signature");

    // Create the Call node.  Inputs: [control, memory, target, sp,
    // arg_passing_0, …].  Outputs: [Control, Memory, ret_val_0, …,
    // clob_0, …] — the two-group layout `FunctionBuilder::build_call`
    // emits (ret-val group ahead of the clobber group).  The
    // stack-pointer anchor (`sp_value`) is wired ahead of the args, in
    // the same slot order as `FunctionBuilder::build_call`.
    let mut call_inputs: Vec<ValueId> =
        Vec::with_capacity(4 + arg_passing_values.len());
    call_inputs.push(control_value);
    call_inputs.push(memory_value);
    call_inputs.push(int_const_value);
    call_inputs.push(sp_value);
    call_inputs.extend_from_slice(arg_passing_values);
    let mut call_outputs: Vec<ValueKind> =
        Vec::with_capacity(2 + ret_val_kinds.len() + clobbered_kinds.len());
    call_outputs.push(ValueKind::Control);
    call_outputs.push(ValueKind::Memory);
    call_outputs.extend_from_slice(ret_val_kinds);
    call_outputs.extend_from_slice(clobbered_kinds);
    let call =
        ctx.create_node_attributed(NodeKind::Call, call_inputs, call_outputs, &[placeholder]);
    // Slot 0 = Control, slot 1 = Memory.  The clobbered slots beyond
    // those are produced for downstream consumers (typically empty
    // here because the only consumer is the freshly-spliced Return).
    //
    // Memory-preserving CCs (e.g. `x86_64_all_preserving`,
    // `__fentry__`-style tracing pre-ambles) leave the Call's Memory
    // output dangling and wire the pre-Call memory edge into the new
    // Return directly, so LoadReadOnly / LoadForward chains stay
    // intact across the spliced tail call.  Mirrors
    // `FunctionBuilder::build_call`'s `preserves_memory` branch
    // (`builder/call.rs` — same Call output shape; only the
    // region-memory advance differs).
    let call_outs: Vec<_> = ctx.node_outputs(call).to_vec();
    let call_ctrl_value = call_outs[0];
    let mem_for_return = if preserves_memory {
        memory_value
    } else {
        call_outs[1]
    };

    let mut new_return_inputs: Vec<ValueId> = Vec::with_capacity(2 + ret_val_values.len());
    new_return_inputs.push(call_ctrl_value);
    new_return_inputs.push(mem_for_return);
    new_return_inputs.extend_from_slice(ret_val_values);
    let new_return =
        ctx.create_node_attributed(NodeKind::Return, new_return_inputs, [], &[placeholder]);

    Ok(new_return)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the in-place editors at the opt-crate level.
    //! Mirror the original strider tests; the strider shim's tests
    //! continue to exercise the shim path.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_pattern::GraphRewriteCtxExt;
    use strider_ir::FunctionBuilder;
    use strider_ir::node::ValueType;

    fn build_placeholder_graph() -> (strider_ir::Function, NodeId) {
        let mut builder = FunctionBuilder::empty()
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let target = builder.build_int_const(0xdeadu64, ValueType::I64).unwrap();
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
        for nid in ctx.graph().all_node_ids() {
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
            .graph().all_node_ids()
            .find(|&nid| matches!(ctx.node_kind(nid), NodeKind::IntConst(_)))
            .expect("graph has at least one IntConst");
        let result = ctx.with_rewrite_ctx(|rctx| apply_link_register(rctx, int_const_id, &[]));
        assert!(result.is_err(), "must reject non-IndirectBranch: {result:?}");
    }

    #[test]
    fn apply_tail_call_emits_call_then_return() {
        let (mut ctx, placeholder) = build_placeholder_graph();
        let sp = synth_value_output(&mut ctx, 0x7fff_0000, ValueType::I64);
        let _new_return = ctx
            .with_rewrite_ctx(|rctx| apply_tail_call(rctx, placeholder, 0xc0de_u64, sp, &[], &[], &[], &[], false))
            .expect("apply");
        // The new Return must be reachable from entry; the placeholder
        // is detached.  Walk all node ids to confirm a Call materialised.
        let mut had_call = false;
        for nid in ctx.graph().all_node_ids() {
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
            .graph().all_node_ids()
            .find(|&nid| matches!(ctx.node_kind(nid), NodeKind::Return))
            .expect("Return");
        let sp = synth_value_output(&mut ctx, 0x7fff_0000, ValueType::I64);
        let result =
            ctx.with_rewrite_ctx(|rctx| apply_tail_call(rctx, ret_id, 0xc0de, sp, &[], &[], &[], &[], false));
        assert!(result.is_err(), "must reject Return: {result:?}");
    }

    /// Spawns a value-typed `IntConst` and returns its single output id —
    /// a convenient stand-in for an "ABI register's IR value at the
    /// placeholder site" in unit tests that don't care which register
    /// it came from.
    fn synth_value_output(
        function: &mut strider_ir::Function,
        value: u128,
        ty: ValueType,
    ) -> ValueId {
        let nid = function.graph_mut().create_node(
            NodeKind::IntConst(value),
            [],
            [ValueKind::Typed(ty)],
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
    // input list (after `[ctrl, mem, target, sp]`), and clobbered output
    // kinds append to the Call's output list (after `[Control, Memory]`).

    #[test]
    fn apply_link_register_threads_ret_val_outputs_into_return() {
        // Two ret-val outputs supplied → resulting Return's inputs are
        // `[ctrl, mem, ret_val_0, ret_val_1]` (the placeholder
        // `target_value` slot is dropped so `RetPat::ret_val(idx)` stays
        // 0-indexed over the real return values).
        let (mut ctx, placeholder) = build_placeholder_graph();
        assert_eq!(ctx.node_inputs(placeholder).len(), 3);
        let r0 = synth_value_output(&mut ctx, 0x42, ValueType::I64);
        let r1 = synth_value_output(&mut ctx, 0x43, ValueType::I64);
        let new_return = ctx
            .with_rewrite_ctx(|rctx| apply_link_register(rctx, placeholder, &[r0, r1]))
            .expect("apply");
        assert!(matches!(ctx.node_kind(new_return), NodeKind::Return));
        assert_eq!(
            ctx.node_inputs(new_return).len(),
            2 + 2,
            "Return inputs are [ctrl, mem, ret_val_0, ret_val_1] after target_value removal",
        );
        assert_eq!(ctx.graph().nth_input(new_return, 2), Some(r0));
        assert_eq!(ctx.graph().nth_input(new_return, 3), Some(r1));
    }

    #[test]
    fn apply_tail_call_threads_arg_passing_into_call() {
        // Three arg-passing outputs → Call's inputs are
        // `[ctrl, mem, IntConst(target), sp, arg_0, arg_1, arg_2]`.
        let (mut ctx, placeholder) = build_placeholder_graph();
        let sp = synth_value_output(&mut ctx, 0x7fff_0000, ValueType::I64);
        let a0 = synth_value_output(&mut ctx, 0x01, ValueType::I64);
        let a1 = synth_value_output(&mut ctx, 0x02, ValueType::I64);
        let a2 = synth_value_output(&mut ctx, 0x03, ValueType::I64);
        let new_return = ctx
            .with_rewrite_ctx(|rctx| {
                apply_tail_call(rctx, placeholder, 0xc0de, sp, &[a0, a1, a2], &[], &[], &[], false)
            })
            .expect("apply");
        // The new Return's input #0 is the Call's ctrl output.  Walk
        // back to the Call.
        let call_ctrl = ctx.graph().nth_input(new_return, 0).expect("ctrl slot");
        let (call_node, _) = ctx.value_definition(call_ctrl);
        assert!(matches!(ctx.node_kind(call_node), NodeKind::Call));
        assert_eq!(
            ctx.node_inputs(call_node).len(),
            7,
            "Call must have [ctrl, mem, target, sp, a0, a1, a2]",
        );
        assert_eq!(ctx.graph().nth_input(call_node, 3), Some(sp));
        assert_eq!(ctx.graph().nth_input(call_node, 4), Some(a0));
        assert_eq!(ctx.graph().nth_input(call_node, 5), Some(a1));
        assert_eq!(ctx.graph().nth_input(call_node, 6), Some(a2));
    }

    #[test]
    fn apply_tail_call_threads_clobbered_kinds_into_call_outputs() {
        // Two clobbered output kinds → Call's outputs are
        // `[Control, Memory, clob_0, clob_1]`.
        let (mut ctx, placeholder) = build_placeholder_graph();
        let clob_kinds = [
            ValueKind::Typed(ValueType::I64),
            ValueKind::Typed(ValueType::I32),
        ];
        let sp = synth_value_output(&mut ctx, 0x7fff_0000, ValueType::I64);
        let new_return = ctx
            .with_rewrite_ctx(|rctx| {
                apply_tail_call(rctx, placeholder, 0xbeef, sp, &[], &[], &clob_kinds, &[], false)
            })
            .expect("apply");
        // Walk to the Call.
        let new_return_ctrl = ctx.graph().nth_input(new_return, 0).expect("ctrl slot");
        let (call_node, _) = ctx.value_definition(new_return_ctrl);
        let call_outputs: Vec<_> = ctx.node_outputs(call_node).to_vec();
        assert_eq!(
            call_outputs.len(),
            4,
            "Call must have [Control, Memory, clob_0, clob_1]",
        );
        assert_eq!(ctx.value_kind(call_outputs[2]), clob_kinds[0]);
        assert_eq!(ctx.value_kind(call_outputs[3]), clob_kinds[1]);
    }

    #[test]
    fn apply_tail_call_threads_ret_val_outputs_into_return() {
        // Two ret-val outputs → new Return's inputs are
        // `[call_ctrl, call_mem, ret_val_0, ret_val_1]`.
        let (mut ctx, placeholder) = build_placeholder_graph();
        let sp = synth_value_output(&mut ctx, 0x7fff_0000, ValueType::I64);
        let r0 = synth_value_output(&mut ctx, 0x10, ValueType::I64);
        let r1 = synth_value_output(&mut ctx, 0x11, ValueType::I64);
        let new_return = ctx
            .with_rewrite_ctx(|rctx| {
                apply_tail_call(rctx, placeholder, 0xface, sp, &[], &[], &[], &[r0, r1], false)
            })
            .expect("apply");
        assert_eq!(ctx.node_inputs(new_return).len(), 4, "[call_ctrl, call_mem, r0, r1]");
        assert_eq!(ctx.graph().nth_input(new_return, 2), Some(r0));
        assert_eq!(ctx.graph().nth_input(new_return, 3), Some(r1));
    }

    /// Regression: a **default-CC** tail-call splice on a function whose
    /// convention declares return-value registers must produce a `Call`
    /// whose output arity includes the ret-val group, so the result
    /// passes `validate`.  Before the fix the spliced `Call` carried only
    /// `[Control, Memory] ++ clobbers`, dropping the ret-val output group;
    /// the validator's function-default `Call` arm
    /// (`2 + ret_val_count + clobber_count`) then rejected it for any real
    /// ABI.  Every other tail-call test uses the trivial (empty) CC, which
    /// hits the validator's synthetic-test escape and so never caught this.
    #[test]
    fn apply_tail_call_default_cc_call_includes_ret_val_outputs() {
        use strider_ir::validate::validate;
        // A function whose default CC declares one ret-val reg (rax) and a
        // tracked stack pointer.  ret=[rax] ⇒ call_ret_val_regs() is
        // non-empty ⇒ the validator does NOT take its empty-CC escape.
        let rax = rsleigh::Vn {
            addr_off: 0x100,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let sp = rsleigh::Vn {
            addr_off: 0x7000,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut builder = FunctionBuilder::new_raw(
            vec![rax, sp],
            &[],
            &[sp],
            &[rax],
            Some(sp),
            0,
            strider_target::Endianness::Little,
        )
        .expect("new_raw");
        let region = builder.create_region().expect("region");
        builder.set_entry_region(region).expect("entry region");
        builder.set_region(region);
        builder.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let target = builder.build_int_const(0xdead_u64, ValueType::I64).unwrap();
        builder.build_indirect_branch(target).expect("indirect branch");
        builder.set_lift_addr(None);
        let mut function = builder.build().expect("build");
        let placeholder = function
            .walk()
            .find(|&nid| matches!(function.node_kind(nid), NodeKind::IndirectBranch))
            .expect("IndirectBranch placeholder");

        // ABI register values at the dispatch site (stand-ins).
        let sp_value = synth_value_output(&mut function, 0x7fff_0000, ValueType::I64);
        let rax_value = synth_value_output(&mut function, 0, ValueType::I64);

        // Splice a default-CC tail call: no clobbers, one ret-val (rax).
        function
            .with_rewrite_ctx(|rctx| {
                apply_tail_call(
                    rctx,
                    placeholder,
                    0xc0de_u64,
                    sp_value,
                    &[],
                    &[ValueKind::Typed(ValueType::I64)],
                    &[],
                    &[rax_value],
                    false,
                )
            })
            .expect("apply_tail_call");

        validate(&function, function.entry().expect("entry"))
            .expect("default-CC tail-call splice must produce a valid graph");
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
        let float_const = ctx.graph_mut().create_node(
            NodeKind::FloatConst(0),
            [],
            [ValueKind::Typed(ValueType::F32)],
        );
        ctx.set_asm_fingerprint(float_const, vec![strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
        let bool_value = ctx.node_outputs(float_const).iter().copied().next().unwrap();
        // Replace the IndirectBranch's input[2] (target_value) with the float output.
        let target_use_id = ctx
            .graph().node_input_id_at(placeholder, 2)
            .expect("input slot 2 exists");
        ctx.graph_mut().update_input(target_use_id, bool_value);
        // Sanity: the placeholder now has a float (non-integer) target_value.
        let target_value_kind = ctx
            .value_kind(ctx.node_inputs(placeholder)[2]);
        assert!(
            matches!(target_value_kind, ValueKind::Typed(ValueType::F32)),
            "fixture must have a non-integer (float) target_value, got {target_value_kind:?}"
        );

        let sp = synth_value_output(&mut ctx, 0x7fff_0000, ValueType::I64);
        let result =
            ctx.with_rewrite_ctx(|rctx| apply_tail_call(rctx, placeholder, 0xc0de, sp, &[], &[], &[], &[], false));
        let err = result.expect_err("non-integer target_value must propagate as Err");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("integer") || msg.contains("Bool"),
            "Err must name the type problem; got: {msg}"
        );
    }
}
