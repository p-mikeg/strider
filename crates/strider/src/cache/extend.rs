//! Predecessor-edge extension — wires a new predecessor's exit handles
//! into an existing cached region's phi nodes WITHOUT moving the phi's
//! `NodeId`s, so body refs that consume those phi outputs stay valid.

use cfg::Cfg;
use ir::node::NodeId;
use rsleigh::Vn;

use crate::error::Result;

use super::entry::{PredecessorHandles, RegionIrEntry};
use super::RegionIrCache;

/// CORRECTNESS NOTE — `extend_predecessors_into`:
///
/// When a CFG rebuild brings new predecessors into a region whose
/// cache entry already exists, the contract is:
///
///   * Add an input to the existing `entry_control_state` (NodeId
///     pinned in the cache).
///   * Add an input to the existing `entry_mem_phi`.
///   * For each `(vn, phi_node_id)` in `entry_var_phis`: add an
///     input from the predecessor's `exit_vn_to_value[vn]`
///     (or `InitialVar(vn)` if the var isn't live across the edge).
///
/// We APPEND to existing nodes — we never rewrite a `NodeOutputId`
/// or move a phi.  Body refs that pre-date this call therefore stay
/// valid: every consumer of `entry_var_phis[vn]`'s output points at
/// the same node, which now happens to have one more input slot.
///
/// In the round-1 orchestrator, full re-lift on each iteration means
/// the previous-iteration's phi nodes are gone — `extend_predecessors_into`
/// is therefore a no-op against a freshly-rebuilt graph.  The
/// granular helper [`extend_predecessors_with_handle`] is the
/// load-bearing primitive for unit tests and for future rounds that
/// persist the IR across iterations.
///
/// # Errors
///
/// Returns `Ok(())` unconditionally in this round-1 surface (no-op).
pub fn extend_predecessors_into<R: rsleigh::MemReader>(
    cache: &mut RegionIrCache,
    cfg: &Cfg<R>,
) -> Result<()> {
    // No-op: see correctness note above.  The IR arena does not
    // persist across orchestrator iterations, so we have no graph
    // handle to apply phi extensions to.  Production phi extension
    // happens via [`extend_predecessors_with_handle`] called by
    // future-round orchestrators that hold a persistent graph.
    let _ = cache;
    let _ = cfg;
    Ok(())
}

/// Append a new predecessor edge to `cache_entry`'s phi nodes inside
/// `graph`.  This is the unit-tested primitive that future-round
/// orchestrators call when a CFG rebuild brings a new predecessor
/// into a region with a still-live cache entry.
///
/// The edits performed (in order):
///   1. Append `pred.exit_control` to `entry_control_state`'s inputs.
///   2. Append `pred.exit_memory` to `entry_mem_phi`'s inputs.
///   3. For each `(vn, phi_node_id)` in `entry_var_phis`: append
///      `pred.exit_vn_to_value[vn]` (or fall back to building a fresh
///      `InitialVar(vn)` if the var isn't live across the edge).
///
/// CORRECTNESS — node id stability: every append uses
/// [`ir::Graph::add_node_input`] which mutates the existing node in
/// place.  The phi `NodeId`s pinned in the cache stay valid; body
/// refs that consume those phi outputs stay valid.
///
/// CORRECTNESS — var fallback: when `pred.exit_vn_to_value` doesn't
/// contain `vn`, we synthesise an `InitialVar(vn)` node and feed
/// that.  This mirrors how the IR builder handles vars that aren't
/// live across an edge — the phi gets the function-entry value as
/// its input on this edge, which is the consistent SSA-extension
/// semantics.  Note: `InitialVar` is cacheable, so creating one when
/// the graph already has the same `InitialVar(vn)` returns the
/// existing node id.
///
/// # Errors
///
/// Propagates IR mutation errors (e.g. `add_node_input` against a
/// cacheable node — but `ControlState` / `MemPhi` / `ControlPhi` are
/// all non-cacheable, so this should not happen).
pub fn extend_predecessors_with_handle(
    cache_entry: &mut RegionIrEntry,
    graph: &mut ir::BuiltFunctionGraph,
    pred: &PredecessorHandles,
) -> Result<()> {
    use ir::node::{NodeKind, NodeOutputKind};

    // W1 — rollback contract: every append below contributes one input to
    // a phi node.  If the per-var phi loop errors mid-iteration, the prior
    // appends (ControlState + MemPhi + already-handled var phis) would
    // leave phi arities mismatched (some have N+1 inputs, others have N)
    // — a soundness violation the orchestrator's predecessor_diffs cannot
    // detect.  We track every successful append in a stack and pop them
    // on error so the function is all-or-nothing.
    //
    // The first two steps and any per-var phi extension can fail with an
    // IR error (cacheable-node guard) or — for the var-phi fallback —
    // the unsupported-regsize dispatch.  The cacheable-node guard cannot
    // fire today (ControlState / MemPhi / ControlPhi are all non-cacheable
    // by design), but defensive hardening matters because F1 fingerprint
    // plumbing extends create_node's behavior in ways that could grow new
    // failure modes for the per-var fallback.
    let mut appended: Vec<NodeId> = Vec::new();

    // Step 1: append predecessor's exit control to the ControlState.
    // CORRECTNESS: ControlState is non-cacheable; add_node_input mutates
    // in place, so the pinned NodeId in the cache stays valid.
    graph
        .graph
        .add_node_input(cache_entry.entry_control_state, pred.exit_control)
        .map_err(|e| {
            // First append — nothing to roll back.
            crate::error::Error::from(e)
        })?;
    appended.push(cache_entry.entry_control_state);

    // Step 2: append predecessor's exit memory to the MemPhi.
    if let Err(e) = graph
        .graph
        .add_node_input(cache_entry.entry_mem_phi, pred.exit_memory)
    {
        // Roll back step 1 before propagating.
        rollback_appends(&mut graph.graph, &appended);
        return Err(e.into());
    }
    appended.push(cache_entry.entry_mem_phi);

    // Step 3: per-var phi extensions.  Same in-place mutation; non-cacheable.
    let phis_to_extend: Vec<(Vn, NodeId)> = cache_entry
        .entry_var_phis
        .iter()
        .map(|(&vn, &phi_id)| (vn, phi_id))
        .collect();
    for (vn, phi_node_id) in phis_to_extend {
        let value_for_pred = if let Some(&v) = pred.exit_vn_to_value.get(&vn) {
            v
        } else {
            // Fallback: build/dedup an InitialVar(vn).  Failures here
            // surface the unsupported-regsize dispatch — exactly the
            // partial-update window W1 closes.
            let ty: ir::node::NodeOutputType = match vn.size {
                1 => ir::node::NodeOutputType::U8,
                2 => ir::node::NodeOutputType::U16,
                4 => ir::node::NodeOutputType::U32,
                8 => ir::node::NodeOutputType::U64,
                16 => ir::node::NodeOutputType::U128,
                32 => ir::node::NodeOutputType::U256,
                other => {
                    rollback_appends(&mut graph.graph, &appended);
                    return Err(crate::error::ErrorKind::UnsupportedRegSize(other).into());
                }
            };
            let iv = graph.graph.create_node(
                NodeKind::InitialVar(vn),
                [],
                [NodeOutputKind::OutputType(ty)],
            );
            match graph.graph.node_outputs_exact::<1>(iv) {
                Ok(outs) => outs[0],
                Err(e) => {
                    rollback_appends(&mut graph.graph, &appended);
                    return Err(e.into());
                }
            }
        };
        if let Err(e) = graph.graph.add_node_input(phi_node_id, value_for_pred) {
            rollback_appends(&mut graph.graph, &appended);
            return Err(e.into());
        }
        appended.push(phi_node_id);
    }

    // Bump the cached predecessor count so a subsequent
    // predecessor_diffs call doesn't double-flag this region.  The bump
    // is the "commit" — it happens only if every append above succeeded.
    cache_entry.cached_predecessor_count += 1;
    Ok(())
}

/// Removes the most recently appended input from each node in `appended`,
/// in reverse order.  Used by [`extend_predecessors_with_handle`] to
/// roll back a partial extension when a later step fails.
///
/// CORRECTNESS — silent no-op on remove failure: if the rollback's
/// remove_node_input itself fails (only possible in pathological corruption
/// cases — the just-appended index is in range by construction), we
/// continue rolling back the rest.  The caller's error is the one that
/// gets propagated; the rollback is best-effort cleanup.
fn rollback_appends(graph: &mut ir::Graph, appended: &[NodeId]) {
    for &node_id in appended.iter().rev() {
        // Each append placed the input at index `len - 1`; popping the
        // last index undoes that single append.
        let last_idx = graph.node_inputs(node_id).len();
        if last_idx == 0 {
            continue;
        }
        let _ = graph.remove_node_input(node_id, (last_idx - 1) as u32);
    }
}
