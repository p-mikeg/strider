//! `StackStoreDetect` — rewrites `Store` nodes whose address resolves to
//! `InitialVar(stack_ptr) + K` into dedicated `NodeKind::StackStore` /
//! `NodeKind::StackStorePhi` nodes. Configured with the calling convention's
//! stack-pointer varnode.

use ir::node::{NodeId, NodeKind, NodeOutputKind};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp};
use crate::worklist::WorkSet;

/// Rewrites one `Store` node into the matching `StackStore` / `StackStorePhi`
/// form when its address resolves to a known SP offset (or per-branch phi of
/// SP offsets).  Leaves the node untouched otherwise.
fn try_detect_stack_store(
    ctx: &mut pattern::RewriteCtx<'_>,
    node_id: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
) -> Result<OptimizationResult> {
    let NodeKind::Store(space) = *ctx.node_kind(node_id) else {
        return Ok(OptimizationResult::NoChange);
    };

    // Store inputs: [memory, addr, data].
    let [memory, addr, data] = ctx.node_inputs_exact::<3>(node_id)?;
    let [old_mem_out] = ctx.node_outputs_exact::<1>(node_id)?;

    let mut visiting: entity_utils::DenseEntitySet<ir::node::NodeId> = entity_utils::DenseEntitySet::new();
    let Some(expr) = decompose_sp(ctx.graph_ref(), addr, sp_vn, memo, &mut visiting) else {
        return Ok(OptimizationResult::NoChange);
    };

    let new_mem_out = match expr {
        SpExpr::Terminal { base, offset } => {
            let new_node = ctx.create_node(
                NodeKind::StackStore { space, offset },
                [memory, base, data],
                [NodeOutputKind::Memory],
            );
            // StackStore is non-exempt; absorb the rewritten Store's
            // fingerprint into it so the contributing machine instruction
            // survives the rewrite.
            ctx.extend_asm_fingerprint_from(new_node, node_id);
            ctx.node_outputs_exact::<1>(new_node)?[0]
        }
        SpExpr::Phi { phi_node, offsets } => {
            // The VarPhi's inputs[0] is the dispatch token from its
            // owning ControlState — the same token `StackStorePhi` will
            // consume so that `RedundantPhis` collapses it when only one
            // predecessor is live.
            let phi_inputs = ctx.node_inputs(phi_node);
            if phi_inputs.is_empty() {
                return Ok(OptimizationResult::NoChange);
            }
            let phi_token = phi_inputs[0];
            let new_node = ctx.create_node(
                NodeKind::StackStorePhi { space },
                [phi_token, memory, data],
                [NodeOutputKind::Memory],
            );
            ctx.set_stack_phi_offsets(new_node, offsets);
            // StackStorePhi is exempt (it's a phi-shaped synthesised
            // node), but we still absorb the rewritten Store's
            // fingerprint so downstream consumers can recover the
            // contributing machine instruction via the side-table.
            ctx.extend_asm_fingerprint_from(new_node, node_id);
            ctx.node_outputs_exact::<1>(new_node)?[0]
        }
    };

    // Only report `Changed` when at least one consumer was rewired.  The
    // old Store node is detached either way (no consumers means it's
    // already a zombie post-rewrite); but a no-op rewrite shouldn't
    // force a spurious extra fixed-point iteration.
    let changed = ctx.replace_all_uses(old_mem_out, new_mem_out)?;
    ctx.detach_node_inputs(node_id);
    Ok(OptimizationResult::from_changed(changed))
}

/// Rewrites `Store` nodes whose address is a compile-time-known SP offset
/// into [`NodeKind::StackStore`] / [`NodeKind::StackStorePhi`] nodes.
///
/// Runs inside the main fixed-point loop so that address arithmetic folded by
/// `ConstantFold` and SP-phi collapses produced by `RedundantPhis` feed more
/// detections on each iteration.
#[derive(Clone)]
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
        Self::new(cc.stack_ptr_vn())
    }
}

impl Optimizer for StackStoreDetect {
    fn optimize(&self, ctx: &mut pattern::RewriteCtx<'_>) -> Result<OptimizationResult> {
        // Only Store nodes can be promoted to StackStore — kind-filter at
        // the iterator level so we don't allocate a Vec sized to all
        // reachable nodes.  Mirrors the established pattern in
        // `StackLoadForward` and `CallStackArgCollect`.
        let mut work = WorkSet::seeded_kind(ctx, |k| matches!(k, NodeKind::Store(_)));
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        while let Some(node_id) = work.pop() {
            result |= try_detect_stack_store(ctx, node_id, self.stack_ptr_vn, &mut memo)?;
        }
        Ok(result)
    }
}
