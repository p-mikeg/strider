//! Step-through walkers used by SP-aware memory-chain analyses to
//! decide whether a single memory-side-effecting node aliases a query
//! byte range.

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};
use strider_ir::Function;

use crate::opt::AliasMode;

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

/// Decides whether walking past `node` (a raw `NodeKind::Store`) is
/// safe for an SP-rooted query range.  See [`AliasMode`] for the
/// soundness/coverage trade-off the `mode` parameter controls.
pub(crate) fn step_through_store(
    graph: &Function,
    node: NodeId,
    stack_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    query_off: i64,
    query_size: i64,
    mode: AliasMode,
) -> AliasStep {
    // Store inputs: [MEM, ADDR, DATA].
    let inputs = graph.node_inputs(node);
    if inputs.len() < 3 {
        return AliasStep::MayAlias;
    }
    match decompose_sp(graph, inputs[1], stack_vn, sp_memo) {
        None => match mode {
            // Strict: cannot prove disjoint from an SP-rooted query
            // without a memory-layout assumption.  Bail.
            AliasMode::Strict => AliasStep::MayAlias,
            // Permissive: stack region and constant-address region are
            // assumed disjoint.  A Store whose address is a literal
            // `IntConst` therefore cannot alias the SP-rooted query;
            // step through.  Anchor addresses (anything else) still
            // bail — closing that gap requires escape analysis.
            AliasMode::AssumeStackConstDisjoint => {
                let store_addr_node = graph.get_node_from_output(inputs[1]);
                if matches!(graph.node_kind(store_addr_node), NodeKind::IntConst(_)) {
                    AliasStep::PassThrough { prev_mem: inputs[0] }
                } else {
                    AliasStep::MayAlias
                }
            }
        },
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

