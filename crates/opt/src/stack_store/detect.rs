//! `StackStoreDetect` — rewrites `Store` nodes whose address resolves to
//! `InitialVar(stack_ptr) + K` into dedicated `NodeKind::StackStore` /
//! `NodeKind::StackStorePhi` nodes. Configured with the calling convention's
//! stack-pointer varnode.

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputKind};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp};

/// Rewrites one `Store` node into the matching `StackStore` / `StackStorePhi`
/// form when its address resolves to a known SP offset (or per-branch phi of
/// SP offsets).  Leaves the node untouched otherwise.
fn try_detect_stack_store(
    fg: &mut BuiltFunctionGraph,
    node_id: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
) -> Result<OptimizationResult> {
    let space = match *fg.graph.node_kind(node_id) {
        NodeKind::Store(space) => space,
        _ => return Ok(OptimizationResult::NoChange),
    };

    // Store inputs: [memory, addr, data].
    let [memory, addr, data] = fg.graph.node_inputs_exact::<3>(node_id)?;
    let [old_mem_out] = fg.graph.node_outputs_exact::<1>(node_id)?;

    let mut visiting = rustc_hash::FxHashSet::default();
    let Some(expr) = decompose_sp(fg, addr, sp_vn, memo, &mut visiting) else {
        return Ok(OptimizationResult::NoChange);
    };

    let new_mem_out = match expr {
        SpExpr::Terminal { base, offset } => {
            let new_node = fg.graph.create_node(
                NodeKind::StackStore { space, offset },
                [memory, base, data],
                [NodeOutputKind::Memory],
            );
            fg.graph.node_outputs_exact::<1>(new_node)?[0]
        }
        SpExpr::Phi { phi_node, offsets } => {
            // The ControlPhi's inputs[0] is the dispatch token from its
            // owning ControlState — the same token `StackStorePhi` will
            // consume so that `RedundantPhis` collapses it when only one
            // predecessor is live.
            let phi_inputs = fg.graph.node_inputs(phi_node);
            if phi_inputs.is_empty() {
                return Ok(OptimizationResult::NoChange);
            }
            let phi_token = phi_inputs[0];
            let new_node = fg.graph.create_node(
                NodeKind::StackStorePhi { space },
                [phi_token, memory, data],
                [NodeOutputKind::Memory],
            );
            fg.graph.set_stack_phi_offsets(new_node, offsets);
            fg.graph.node_outputs_exact::<1>(new_node)?[0]
        }
    };

    fg.replace_all_uses(old_mem_out, new_mem_out)?;
    fg.graph.detach_node_inputs(node_id);
    Ok(OptimizationResult::Changed)
}

/// Rewrites `Store` nodes whose address is a compile-time-known SP offset
/// into [`NodeKind::StackStore`] / [`NodeKind::StackStorePhi`] nodes.
///
/// Runs inside the main fixed-point loop so that address arithmetic folded by
/// `ConstantFold` and SP-phi collapses produced by `RedundantPhis` feed more
/// detections on each iteration.
pub struct StackStoreDetect {
    /// Varnode for the stack pointer register (e.g. `ESP`, `RSP`, `sp`).
    pub stack_ptr_vn: rsleigh::Vn,
}

impl StackStoreDetect {
    /// Creates a new pass for the given stack-pointer varnode.
    #[must_use]
    pub fn new(stack_ptr_vn: rsleigh::Vn) -> Self {
        Self { stack_ptr_vn }
    }

    /// Creates a new pass whose stack-pointer varnode is taken from the
    /// supplied calling convention.
    #[must_use]
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self::new(cc.stack_ptr_vn)
    }
}

impl Optimizer for StackStoreDetect {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let nodes: Vec<NodeId> = function.preorder().collect();
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        for node_id in nodes {
            result |= try_detect_stack_store(function, node_id, self.stack_ptr_vn, &mut memo)?;
        }
        Ok(result)
    }
}
