//! `CfgDetach` — removes dead control-flow edges into `Region` joins.
//!
//! After `DeadBranchElimination` redirects a constant `If`'s live branch and
//! detaches the folded `If`, the dead branch's control producer becomes
//! unreachable from the entry. This pass visits every `Region` in the general
//! graph walk (`walk_from` — the validator's reachability notion) and drops each
//! predecessor slot whose control producer is not control-reachable
//! (`cfg_reachable`), plus the matching `Phi`/`MemPhi` value slot, via
//! `EditFunction::remove_region_predecessors`.
//!
//! It is the single home for dead-`Region`-predecessor surgery. When a dead
//! subgraph still escapes to live data (so DBE left the `If` attached), the
//! dead edge's producer stays reachable and this pass leaves it alone.

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind};
use strider_ir::walk::cfg_reachable;
use strider_ir::{IRViewer, IRWalker};

use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Removes `Region` predecessor slots whose control producer is unreachable
/// from the function entry via control edges.
///
/// Visits every `Region` in the general graph walk (`walk_from`), checks each
/// predecessor slot's control producer against `cfg_reachable`, and removes
/// every slot whose producer is absent from that control-reachable set.  The
/// matching `Phi`/`MemPhi` value slots are removed by
/// `EditFunction::remove_region_predecessors`.
///
/// Multiple dead slots on the same `Region` are removed highest-index-first
/// so earlier index-stable slots are unaffected by the removals.
#[derive(Clone, Copy)]
pub struct CfgDetach;

impl Optimizer for CfgDetach {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _ctx: &mut OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        // A `EditFunction` always wraps a built function, so the entry is
        // present by construction (`EditFunction::new` invariant).
        let entry = edit.entry();
        // Read off the function (via deref) to compute the dead-slot map;
        // the immutable borrow ends once `dead` is owned, then the slot
        // surgery runs through `edit`.
        let function: &strider_ir::Function = edit.function();
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
        // Visit every reachable `Region` in global reverse-post-order; the
        // reachable SET matches `walk()`, only the ORDER is canonicalised.
        for region in function.reverse_postorder_filter(|k| matches!(k, NodeKind::Region)) {
            for (idx, input) in function.node_inputs(region).into_iter().enumerate() {
                let producer = function.value_definition(input).0;
                if !reachable.contains(producer) {
                    dead.entry(region).or_default().push(idx as u32);
                }
            }
        }

        if dead.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // All composite rewrites route through `EditFunction`.  `dead` is fully
        // owned, so the immutable borrow used to compute it has ended —
        // perform the slot surgery through the shared `edit`.
        // Hand each region its full set of dead predecessor indices in one
        // call — `remove_region_predecessors` removes them highest-first
        // internally, so there's no per-index loop or ordering concern here.
        for (region, idxs) in dead {
            edit.remove_region_predecessors(region, &idxs)?;
        }
        Ok(OptimizationResult::Changed)
    }
}
