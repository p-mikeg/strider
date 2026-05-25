//! Step-through walkers used by SP-aware memory-chain analyses to
//! decide whether a single memory-side-effecting node aliases a query
//! byte range.

use strider_ir::node::{NodeId, NodeOutputId};
use strider_ir::Function;

use super::decompose::{decompose_sp, SpExpr, SpExprMemo};
use super::ranges::{ranges_disjoint, store_value_byte_size};

/// Outcome of inspecting a memory-chain node for the byte range
/// `[query_off, query_off + query_size)`: either the node may alias and
/// further walking must terminate, or the prior memory output is safe to
/// recurse on.
pub(crate) enum AliasStep {
    /// The node is provably non-aliasing with the query range — walk to
    /// `prev_mem` to keep searching.
    PassThrough { prev_mem: NodeOutputId },
    /// The node may alias the query range (overlapping byte ranges, an
    /// SP-rooted Phi address, or malformed inputs).  Caller must terminate.
    MayAlias,
}

/// Decides whether walking past `node` (a raw `NodeKind::Store`) is safe.
/// Decomposes the store address: a non-SP-rooted address is provably
/// non-aliasing with the SP-relative query range; an SP-rooted Terminal
/// address uses the same disjointness check; an SP-rooted Phi address
/// conservatively terminates.
pub(crate) fn step_through_store(
    graph: &Function,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    query_off: i64,
    query_size: i64,
) -> AliasStep {
    // Store inputs: [MEM, ADDR, DATA].
    let inputs = graph.node_inputs(node);
    if inputs.len() < 3 {
        return AliasStep::MayAlias;
    }
    match decompose_sp(graph, inputs[1], sp_vn, sp_memo) {
        // Non-SP-rooted address provably cannot alias the stack-arg byte
        // range — walk through.
        None => AliasStep::PassThrough { prev_mem: inputs[0] },
        Some(SpExpr::Terminal { base: _, offset: store_off }) => {
            let store_size = store_value_byte_size(graph, inputs[2]);
            if ranges_disjoint(store_off, store_size, query_off, query_size) {
                AliasStep::PassThrough { prev_mem: inputs[0] }
            } else {
                AliasStep::MayAlias
            }
        }
        // SP-rooted Phi: per-predecessor range analysis would be needed to
        // prove disjointness; conservatively terminate.
        Some(SpExpr::Phi { .. }) => AliasStep::MayAlias,
    }
}

