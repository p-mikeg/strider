//! `CfgDetach` — removes dead control-flow edges into `Region` joins.
//!
//! After `DeadBranchElimination` redirects a constant `If`'s live branch and
//! detaches the folded `If`, the dead branch's control producer becomes
//! unreachable from the entry. This pass walks `cfg_reachable(entry)` and, for
//! every reachable `Region`, drops each predecessor slot whose control producer
//! is unreachable (plus the matching `Phi`/`MemPhi` value slot) via
//! `RewriteCtx::remove_region_predecessors`.
//!
//! It is the single home for dead-`Region`-predecessor surgery. When a dead
//! subgraph still escapes to live data (so DBE left the `If` attached), the
//! dead edge's producer stays reachable and this pass leaves it alone.

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind};
use strider_ir::walk::cfg_reachable;

use crate::opt::error::Result;
use crate::opt::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Removes `Region` predecessor slots whose control producer is unreachable
/// from the function entry via CFG edges.
///
/// Iterates all `Region` nodes reachable from the entry, checks each
/// predecessor slot's control producer against `cfg_reachable`, and removes
/// every slot whose producer is absent from the reachable set.  The matching
/// `Phi`/`MemPhi` value slots are removed by `RewriteCtx::remove_region_predecessors`.
///
/// Multiple dead slots on the same `Region` are removed highest-index-first
/// so earlier index-stable slots are unaffected by the removals.
#[derive(Clone, Copy)]
pub struct CfgDetach;

impl Optimizer for CfgDetach {
    fn optimize(
        &self,
        function: &mut strider_ir::Function,
        _ctx: &OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("CfgDetach: function must be built (entry not set)"))?;
        let reachable = cfg_reachable(function.graph(), entry);

        // Scan ONLY the live (entry-reachable) Region nodes — a whole-dead
        // region needs no surgery (orphan cleanup handles it), so there's no
        // reason to walk the full node arena. Group the dead predecessor slots
        // by region: each value is the list of dead `pred_index`es for that
        // region, in ascending order (the `enumerate` order). The removal runs
        // in a second pass so we can drop each region's slots high-index-first.
        let mut dead: FxHashMap<NodeId, Vec<u32>> = FxHashMap::default();
        for region in reachable
            .iter()
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
        // owned, so the immutable borrow of `function` used to compute it has
        // ended — build the rewrite ctx and perform the slot surgery through it.
        let mut ctx = strider_pattern::RewriteCtx::try_for_built(function)?;
        // Hand each region its full set of dead predecessor indices in one
        // call — `remove_region_predecessors` removes them highest-first
        // internally, so there's no per-index loop or ordering concern here.
        for (region, idxs) in dead {
            ctx.remove_region_predecessors(region, &idxs)?;
        }
        Ok(OptimizationResult::Changed)
    }
}
