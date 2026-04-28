//! In-place IR edits for tier-2 resolutions that don't require a CFG
//! rebuild.  Two variants are supported:
//!
//!   * **`LinkRegister`**: the placeholder `Return(target_value)`
//!     already has the right control-flow shape — control reaches a
//!     `Return` node, which is exactly what the calling convention's
//!     return idiom needs.  We append the convention's `ret_val_regs`
//!     as additional value inputs to the existing Return node.  The
//!     placeholder's `target_value` slot stays as input #2 (the same
//!     `NodeOutputId`); the appended ret_vals occupy slots #3 onward.
//!     The cached `RegionIrEntry::exit_control` handle is unchanged
//!     because we're modifying a node, not replacing it.
//!
//!   * **`Single` tail call**: replace the placeholder
//!     `Return(target_vn)` with `Call(IntConst(target)) →
//!     Return(ret_vars)`.  The placeholder Return is detached
//!     (becoming a zombie unreachable node — the validator skips it
//!     via the entry-rooted reachability scope) and a fresh
//!     `IntConst → Call → Return` chain is wired on the same control
//!     and memory inputs.  The new Return's `NodeId` is returned so
//!     callers (the orchestrator / cache) can patch
//!     [`crate::RegionIrEntry::exit_control`].
//!
//! # Correctness — cache handles
//!
//! Both edits preserve cached `RegionIrEntry` entry handles because
//! the body of the region (everything before the placeholder Return)
//! is untouched.  The exit handles change semantically:
//!
//!   * `apply_link_register` keeps the same Return `NodeId`; the
//!     cache's `exit_control` need not be patched.
//!   * `apply_tail_call` produces a new Return `NodeId`; callers MUST
//!     patch `exit_control` (and `exit_memory`) using the returned id.
//!
//! Cached body refs that pre-date the edit remain valid because the
//! body itself is not touched.

#![allow(clippy::module_name_repetitions)]

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use crate::error::{ErrorKind, Result};

/// Applies the `LinkRegister` resolution to a placeholder
/// `Return(control, memory, target_value)` node by appending
/// `ret_val_outputs` as additional value inputs.
///
/// The placeholder's input layout pre-edit:
///
///   `[control, memory, target_value]`
///
/// Post-edit:
///
///   `[control, memory, target_value, ret_val_0, ret_val_1, …]`
///
/// The `target_value` slot is intentionally retained — it doesn't
/// matter for downstream consumers (it was the dispatch varnode, now
/// effectively a no-op slot in the Return) and removing it would
/// require shifting subsequent input indices.  Future rounds may
/// optimise this away; the IR's `validate` is robust to extra Return
/// value inputs.
///
/// # Correctness — node id stability
///
/// We use [`Graph::add_node_input`] which mutates the existing Return
/// node in place.  The Return's `NodeId` (and therefore any cached
/// `RegionIrEntry::exit_control` handle pointing at its control input
/// chain) is unchanged.
///
/// # Errors
///
/// * [`ErrorKind::WrongNodeKind`] if `placeholder_return` is not a
///   `NodeKind::Return` node.
/// * [`ErrorKind::IrError`] propagating any failure from
///   [`Graph::add_node_input`] (e.g. attempting to add an input to a
///   cacheable node, which `Return` is not — but the check is
///   defensive in case of future signature changes).
pub fn apply_link_register(
    fg: &mut BuiltFunctionGraph,
    placeholder_return: NodeId,
    ret_val_outputs: &[NodeOutputId],
) -> Result<()> {
    // Safety check: must be a Return node.  The orchestrator only
    // ever passes the placeholder Return's id, but defending against
    // a misuse here surfaces a typed error rather than silently
    // mangling an unrelated node.
    let kind = *fg.graph.node_kind(placeholder_return);
    if !matches!(kind, NodeKind::Return) {
        return Err(ErrorKind::WrongNodeKind {
            node: placeholder_return,
            expected: "Return",
        }
        .into());
    }
    // Append each ret-val.  add_node_input updates the use-list and
    // input-index bookkeeping; we don't re-validate after each call
    // because the validator runs at the orchestrator level after all
    // edits land.
    for &ret in ret_val_outputs {
        // Bridges ir::Error → strider::Error via the existing
        // strider_error bridge_error! impl in crate::error.
        fg.graph.add_node_input(placeholder_return, ret)?;
    }
    Ok(())
}

/// Applies the `Single`-tail-call resolution by replacing the
/// placeholder `Return(target_value)` with `Call(IntConst(target)) →
/// Return(ret_vars)`.
///
/// Pre-edit:
///
///   `Return(control, memory, target_value)`
///
/// Post-edit:
///
///   `IntConst(target) → Call(control, memory, IntConst) →
///   Return(call.ctrl_out, call.mem_out, ret_val_0, ret_val_1, …)`
///
/// The placeholder Return is detached (becomes a zombie unreachable
/// from `entry`).  The new Return is wired on the Call's control and
/// memory outputs.
///
/// # Correctness
///
/// * The placeholder's pre-Return control and memory inputs are
///   reused as the Call's control and memory inputs — no
///   `replace_all_uses` rewires are needed because we're consuming
///   them at exactly the same point in the chain.
/// * `IntConst(target)` is created with the same integer width as
///   the original `target_value`'s output kind, so the Call's
///   address operand has a sensible type.
/// * Detaching the placeholder removes its inputs from their
///   producers' use-lists (via [`Graph::detach_node_inputs`]).  The
///   placeholder node remains in the arena but is unreachable from
///   `entry`; the validator's Layer A skips it via reachability
///   scoping.
/// * The new Return's `NodeId` is returned so the orchestrator can
///   patch the cache's exit-control handle.
///
/// # Errors
///
/// * [`ErrorKind::WrongNodeKind`] if `placeholder_return` is not a
///   `NodeKind::Return` node.
/// * [`ErrorKind::IrError`] propagating IR construction errors
///   (e.g. wrong input arity, non-value output kind).
pub fn apply_tail_call(
    fg: &mut BuiltFunctionGraph,
    placeholder_return: NodeId,
    target: u64,
    ret_val_outputs: &[NodeOutputId],
) -> Result<NodeId> {
    // Safety check.
    let kind = *fg.graph.node_kind(placeholder_return);
    if !matches!(kind, NodeKind::Return) {
        return Err(ErrorKind::WrongNodeKind {
            node: placeholder_return,
            expected: "Return",
        }
        .into());
    }
    // Read the placeholder's input layout: [control, memory,
    // target_value].  We require exactly 3 inputs — the post-R1.4
    // tier-2 placeholder shape.
    let inputs: Vec<NodeOutputId> = fg
        .graph
        .node_inputs(placeholder_return)
        .into_iter()
        .collect();
    if inputs.len() != 3 {
        return Err(ErrorKind::WrongNodeKind {
            node: placeholder_return,
            expected: "Return with [control, memory, target_value] (3 inputs)",
        }
        .into());
    }
    let control_in = inputs[0];
    let memory_in = inputs[1];
    let target_value = inputs[2];

    // Determine the IntConst's output type from the original
    // target_value's output kind.  Falls back to U64 when the target
    // is not a value-typed output (defensive — production placeholder
    // Returns always have value at slot 2).
    let target_int_ty = fg
        .graph
        .output_kind(target_value)
        .as_integer_or_err()
        .unwrap_or(ir::node::NodeOutputType::U64);

    // CORRECTNESS — detach BEFORE creating the new chain: this
    // removes the placeholder's three inputs from their respective
    // use-lists.  The placeholder node id remains in the arena but
    // becomes unreachable; validate's Layer A skips unreachable
    // nodes via the entry-rooted walk.
    fg.graph.detach_node_inputs(placeholder_return);

    // Create the IntConst(target) with the same integer width as
    // the original target_value.  The mask in build_int_const-style
    // construction is applied here so the stored `u128` matches the
    // type's bit width (mirrors FunctionBuilder::build_int_const).
    let masked_target = u128::from(target) & target_int_ty.bit_mask_u128();
    let int_const = fg.graph.create_node(
        NodeKind::IntConst(masked_target),
        [],
        [NodeOutputKind::OutputType(target_int_ty)],
    );
    let int_const_out = fg.graph.node_outputs_exact::<1>(int_const)?[0];

    // Create the Call node.  Inputs: [control, memory,
    // call_address].  Outputs: [Control, Memory] — no clobbered
    // varnodes because we don't have access to the calling
    // convention's clobber list at this entry point (the orchestrator
    // would supply it via a future-rounds API).  Empty clobbers is
    // sound: the surviving Return doesn't need any of those values.
    let call = fg.graph.create_node(
        NodeKind::Call,
        [control_in, memory_in, int_const_out],
        [NodeOutputKind::Control, NodeOutputKind::Memory],
    );
    let [call_ctrl_out, call_mem_out] = fg.graph.node_outputs_exact::<2>(call)?;

    // Create the new Return.  Inputs: [call.ctrl, call.mem,
    // ...ret_val_outputs].  Returns has no outputs.
    let mut new_return_inputs: Vec<NodeOutputId> = Vec::with_capacity(2 + ret_val_outputs.len());
    new_return_inputs.push(call_ctrl_out);
    new_return_inputs.push(call_mem_out);
    new_return_inputs.extend_from_slice(ret_val_outputs);
    let new_return = fg.graph.create_node(NodeKind::Return, new_return_inputs, []);

    Ok(new_return)
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
    use ir::node::NodeOutputType;

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
            apply_tail_call(&mut graph, placeholder, 0xc0de_u64, &[]).expect("apply");
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
            apply_tail_call(&mut graph, placeholder, target, &[]).expect("apply");
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
        let _new_return = apply_tail_call(&mut graph, placeholder, 0x1234, &[])
            .expect("apply");
        ir::validate::validate(&graph.graph, graph.entry).expect("validate");
    }

    #[test]
    fn apply_tail_call_returns_new_return_node_id() {
        // The returned NodeId must point to a Return node that's
        // distinct from the original placeholder.
        let (mut graph, placeholder) = build_placeholder_graph();
        let new_return = apply_tail_call(&mut graph, placeholder, 0xface, &[])
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
        let new_return = apply_tail_call(&mut graph, placeholder, 0xbeef, &[extra])
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
        let result = apply_tail_call(&mut graph, int_const_id, 0xc0de, &[]);
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
            apply_tail_call(&mut graph, return_id, 0xfeedface_u64, &[]).expect("apply");
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
        let new_return = apply_tail_call(&mut graph, return_id, 0x1_0000_0000_u64, &[])
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
        let result = apply_tail_call(&mut graph, ret_id, 0xc0de, &[]);
        assert!(result.is_err(), "must reject 2-input Return: {result:?}");
    }
}
