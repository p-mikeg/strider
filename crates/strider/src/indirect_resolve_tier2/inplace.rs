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
//!   * **`Single` tail call** *(round-1 stub)*: replace the
//!     placeholder `Return(target_vn)` with `Call(IntConst(target)) →
//!     Return(ret_vars)`.  Round-1 implementation is deferred — see
//!     the function-level docs of [`apply_tail_call`].  The orchestrator
//!     handles tail calls by rebuilding the CFG with the resolved
//!     target wired as a `RegionTerminator::TailCall`, which produces
//!     a clean Call+Return at lift time.  The in-place path exists
//!     for symmetry with the spec and will be filled in once the
//!     `RegionIrCache`'s per-region IR-handle plumbing lands.
//!
//! # Correctness — cache handles
//!
//! Both edits preserve cached `RegionIrEntry::exit_control` /
//! `exit_memory` handles by either:
//!   * keeping the same `Return` node id (LinkRegister), or
//!   * (future) updating the cache entry to point at the new Return.
//!
//! Cached body refs that pre-date the edit remain valid because the
//! body itself is not touched.

#![allow(clippy::module_name_repetitions)]

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

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
/// * [`ErrorKind::IrError`] if `placeholder_return` is not a
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
/// **Round-1 status: NOT IMPLEMENTED.**  The orchestrator handles
/// tail-call resolutions by rebuilding the CFG with the resolved
/// target threaded through `Builder::with_known_targets` (see R3.6),
/// which produces a `RegionTerminator::TailCall`.  At lift time the
/// strider `handle_call`+`handle_return` pair emits the same
/// Call+Return shape this in-place editor would produce, so the
/// rebuild path is functionally correct.  The in-place editor is a
/// future-rounds optimisation that lets the orchestrator skip the
/// rebuild for tail-call-only resolutions.
///
/// # Errors
///
/// Returns [`ErrorKind::Unimplemented`] in round 1.
pub fn apply_tail_call(
    _fg: &mut BuiltFunctionGraph,
    _placeholder_return: NodeId,
    _target: u64,
    _ret_val_outputs: &[NodeOutputId],
) -> Result<()> {
    // Round-1 stub.  The orchestrator routes Single-tail-call
    // resolutions through the CFG rebuild path; the in-place editor
    // is a future optimisation.
    Err(ErrorKind::Unimplemented(
        "indirect_resolve_tier2::inplace::apply_tail_call (round 2+)".to_string(),
    )
    .into())
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
        let inputs_before: Vec<_> = graph.graph.node_inputs(return_id_before).into_iter().collect();
        assert_eq!(inputs_before.len(), 3); // [control, memory, target_value]
        apply_link_register(&mut graph, return_id_before, &[]).expect("apply");
        // Confirm the same node id is still a Return with the same
        // arity (since ret_val_outputs is empty, no inputs added).
        assert!(matches!(graph.graph.node_kind(return_id_before), NodeKind::Return));
        let inputs_after: Vec<_> = graph.graph.node_inputs(return_id_before).into_iter().collect();
        assert_eq!(inputs_after.len(), inputs_before.len());
    }

    #[test]
    fn apply_link_register_appends_ret_val_inputs() {
        // With one ret_val, the Return's input count grows by 1; the
        // first three inputs (ctrl, mem, target_value) are preserved.
        let (mut graph, return_id) = build_placeholder_graph();
        let inputs_before: Vec<_> = graph.graph.node_inputs(return_id).into_iter().collect();
        let ret_val = {
            // Synthesise a fresh IntConst output to use as a ret-val.
            let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
                .expect("FunctionBuilder::new_raw");
            let r = builder.create_region().expect("region");
            builder.set_entry_region(r).expect("entry");
            builder.set_region(r);
            // We can't add the new IntConst to the *existing* graph
            // via FunctionBuilder (no api), so we add via raw
            // graph.create_node below instead.
            drop(builder);
            // Add an IntConst to graph.graph directly:
            use ir::node::NodeOutputKind;
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

    #[test]
    fn apply_tail_call_returns_unimplemented_in_round_1() {
        // Pin the round-1 contract: apply_tail_call returns a typed
        // error.  Round 2+ will replace this with a real
        // implementation; the test will then be replaced.
        let (mut graph, return_id) = build_placeholder_graph();
        let result = apply_tail_call(&mut graph, return_id, 0xc0de, &[]);
        assert!(result.is_err(), "round 1: apply_tail_call must error");
    }
}
