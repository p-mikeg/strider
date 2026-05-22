//! Step-through walkers used by SP-aware memory-chain analyses to
//! decide whether a single memory-side-effecting node aliases a query
//! byte range.

use strider_ir::node::{NodeId, NodeOutputId};
use strider_ir::Graph;

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

/// Decides whether walking past `node` (a `NodeKind::StackStore`) is safe
/// for a search over `[query_off, query_off + query_size)`.
pub(crate) fn step_through_stack_store(
    graph: &Graph,
    node: NodeId,
    store_offset: i64,
    query_off: i64,
    query_size: i64,
) -> AliasStep {
    // StackStore inputs: [MEM, SP, DATA].
    let inputs = graph.node_inputs(node);
    if inputs.len() < 3 {
        return AliasStep::MayAlias;
    }
    let store_size = store_value_byte_size(graph, inputs[2]);
    if ranges_disjoint(store_offset, store_size, query_off, query_size) {
        AliasStep::PassThrough { prev_mem: inputs[0] }
    } else {
        AliasStep::MayAlias
    }
}

/// Decides whether walking past `node` (a `NodeKind::StackStorePhi`) is
/// safe.  The phi disqualifies if any per-predecessor offset (stored in
/// `Graph::stack_phi_offsets`) overlaps the query range.
pub(crate) fn step_through_stack_store_phi(
    graph: &Graph,
    node: NodeId,
    query_off: i64,
    query_size: i64,
) -> AliasStep {
    // StackStorePhi inputs: [PHI, MEM, DATA].
    let inputs = graph.node_inputs(node);
    if inputs.len() < 3 {
        return AliasStep::MayAlias;
    }
    let store_size = store_value_byte_size(graph, inputs[2]);
    let offsets = graph.stack_phi_offsets(node);
    if offsets.is_empty() {
        // No per-predecessor offsets recorded — the StackStorePhi could
        // alias any stack address.  Conservative answer is MayAlias.
        // `StackStoreDetect` always populates `stack_phi_offsets` for
        // every StackStorePhi it creates, so this branch only fires for
        // graphs where another builder produced the node without
        // populating the side-table.
        return AliasStep::MayAlias;
    }
    let any_overlap = offsets
        .iter()
        .any(|&k| !ranges_disjoint(k, store_size, query_off, query_size));
    if any_overlap {
        AliasStep::MayAlias
    } else {
        AliasStep::PassThrough { prev_mem: inputs[1] }
    }
}

/// Decides whether walking past `node` (a raw `NodeKind::Store`) is safe.
/// Decomposes the store address: a non-SP-rooted address is provably
/// non-aliasing with the SP-relative query range; an SP-rooted Terminal
/// address uses the same disjointness check; an SP-rooted Phi address
/// conservatively terminates.
pub(crate) fn step_through_store(
    graph: &Graph,
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
    let mut sp_visiting: entity_utils::DenseEntitySet<NodeId> = entity_utils::DenseEntitySet::new();
    match decompose_sp(graph, inputs[1], sp_vn, sp_memo, &mut sp_visiting) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
    use strider_ir::FunctionBuilder;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// Regression: a `StackStorePhi`
    /// node with empty `stack_phi_offsets` MUST yield `MayAlias` from
    /// `step_through_stack_store_phi` — the conservative answer for
    /// "offsets unknown".  Previously it returned `PassThrough`, which
    /// would silently let `StackLoadForward` forward across a phi that
    /// could alias.
    #[test]
    fn step_through_stack_store_phi_empty_offsets_returns_may_alias() -> crate::opt::Result<()> {
        // Build a graph with a StackStorePhi but DO NOT populate
        // `stack_phi_offsets`.  We need a valid 3-input shape (PHI,
        // MEM, DATA) so the function reaches the offsets check.
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        // We'll synthesise a StackStorePhi node directly in the graph
        // by making three placeholder inputs.  Use the builder's region
        // ControlState's PhiToken slot, the builder's InitialMemory,
        // and a fresh IntConst as DATA.
        let data = b.build_int_const(0xCAFE_u64, NodeOutputType::U64)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        // Locate the ControlState (it owns the PhiToken).
        let cs = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::ControlState))
            .expect("ControlState present");
        let cs_outs = fg.node_outputs(cs).to_vec();
        let phi_token = *cs_outs
            .iter()
            .find(|&&o| matches!(fg.output_kind(o), NodeOutputKind::PhiToken))
            .expect("PhiToken slot");
        let init_mem = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::InitialMemory))
            .expect("InitialMemory present");
        let mem_out = fg.node_outputs(init_mem).iter().copied().next().unwrap();
        let phi_node = fg.create_node(
            NodeKind::StackStorePhi { space: rsleigh::VnSpace::RAM },
            [phi_token, mem_out, data],
            [NodeOutputKind::Memory],
        );
        // DELIBERATELY do NOT call set_stack_phi_offsets.
        let alias = step_through_stack_store_phi(fg.graph(), phi_node, 0, 8);
        assert!(
            matches!(alias, AliasStep::MayAlias),
            "empty stack_phi_offsets must yield MayAlias (sound default)"
        );
        Ok(())
    }
}
