//! `CfgDetach` — removes dead control-flow edges into `Region` joins.
//!
//! After `DeadBranchElimination` redirects a constant `If`'s live branch and
//! detaches the folded `If`, the dead branch's control producer becomes
//! unreachable from the entry. This pass visits every `Region` in the general
//! graph walk (`walk_from` — the validator's reachability notion) and drops each
//! predecessor slot whose control producer is not control-reachable
//! (`cfg_reachable`), plus the matching `Phi`/`MemPhi` value slot, via
//! `RewriteCtx::remove_region_predecessors`.
//!
//! It is the single home for dead-`Region`-predecessor surgery. When a dead
//! subgraph still escapes to live data (so DBE left the `If` attached), the
//! dead edge's producer stays reachable and this pass leaves it alone.

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind};
use strider_ir::walk::cfg_reachable;

use crate::opt::OptRewrite;
use crate::opt::error::Result;
use crate::opt::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Removes `Region` predecessor slots whose control producer is unreachable
/// from the function entry via control edges.
///
/// Visits every `Region` in the general graph walk (`walk_from`), checks each
/// predecessor slot's control producer against `cfg_reachable`, and removes
/// every slot whose producer is absent from that control-reachable set.  The
/// matching `Phi`/`MemPhi` value slots are removed by
/// `RewriteCtx::remove_region_predecessors`.
///
/// Multiple dead slots on the same `Region` are removed highest-index-first
/// so earlier index-stable slots are unaffected by the removals.
#[derive(Clone, Copy)]
pub struct CfgDetach;

impl Optimizer for CfgDetach {
    fn apply(
        &self,
        rctx: &mut strider_pattern::RewriteCtx<'_>,
        _ctx: &OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        // Read off the function (via deref) to compute the dead-slot map;
        // the immutable borrow ends once `dead` is owned, then the slot
        // surgery runs through `rctx`.
        let function: &strider_ir::Function = rctx;
        let entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("CfgDetach: function must be built (entry not set)"))?;
        // Control-reachability is the liveness oracle for a predecessor: a
        // predecessor edge is dead iff its control producer can't be reached
        // from entry by following control. The *iteration* set, however, is the
        // general graph walk (`walk_from`, the same reachability the validator
        // uses) — a join that is only data-reachable (e.g. an escaping dead
        // branch whose value still feeds a live phi) is still validator-visible
        // and may carry a dead control slot, so we must visit it too. A region
        // reachable only via the control-only set would miss those.
        let reachable = cfg_reachable(function.graph(), entry);

        // Group dead predecessor slots by region (ascending `enumerate` order);
        // the removal runs in a second pass through the rewrite ctx.
        let mut dead: FxHashMap<NodeId, Vec<u32>> = FxHashMap::default();
        for region in function
            .walk()
            .filter(|&n| matches!(function.node_kind(n), NodeKind::Region))
        {
            for (idx, input) in function.node_inputs(region).into_iter().enumerate() {
                let producer = function.output_definition(input).0;
                if !reachable.contains(producer) {
                    dead.entry(region).or_default().push(idx as u32);
                }
            }
        }

        if dead.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // All composite rewrites route through `RewriteCtx`.  `dead` is fully
        // owned, so the immutable borrow used to compute it has ended —
        // perform the slot surgery through the shared `rctx`.
        // Hand each region its full set of dead predecessor indices in one
        // call — `remove_region_predecessors` removes them highest-first
        // internally, so there's no per-index loop or ordering concern here.
        for (region, idxs) in dead {
            rctx.remove_region_predecessors(region, &idxs)?;
        }
        Ok(OptimizationResult::Changed)
    }
}
