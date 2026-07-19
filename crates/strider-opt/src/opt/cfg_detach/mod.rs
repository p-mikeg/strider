//! Removes dead control-flow edges into `Region` joins, plus the matching
//! `Phi`/`MemPhi` value slots. The single home for dead-`Region`-predecessor
//! surgery; `DeadBranchElimination` detaches a folded `If` but never strips the
//! predecessor slot it orphans.
//!
//! A dead subgraph that still escapes to live data keeps its `If` attached, so
//! its producer stays control-reachable and this pass leaves it alone.

use rustc_hash::FxHashMap;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};
use strider_ir::walk::cfg_reachable;

use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct CfgDetach;

impl Optimizer for CfgDetach {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _ctx: &mut OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let entry = edit.entry();
        // Two different reachability notions, deliberately. A predecessor edge
        // is dead iff its control producer is control-unreachable from entry,
        // but the set we ITERATE is the general graph walk: a join that is only
        // data-reachable (an escaping dead branch whose value still feeds a live
        // phi) is still validator-visible and can carry a dead control slot.
        let reachable = cfg_reachable(edit.function().graph(), entry);

        let mut dead: FxHashMap<NodeId, Vec<u32>> = FxHashMap::default();
        // The cached live set covers the same nodes as a fresh walk, so reuse
        // its RPO rather than re-walking.
        let regions: Vec<NodeId> = edit
            .reverse_postorder_filter(|k| matches!(k, NodeKind::Region))
            .collect();
        for region in regions {
            for (idx, input) in edit.node_inputs(region).into_iter().enumerate() {
                let producer = edit.value_definition(input).0;
                if !reachable.contains(producer) {
                    dead.entry(region).or_default().push(idx as u32);
                }
            }
        }

        if dead.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // Pass each region all its dead indices at once; the removal orders them
        // highest-index-first internally so earlier slots stay index-stable.
        //
        // Severing a Region's LAST predecessor makes its whole control-dominated
        // subgraph unreachable, but the incremental live-set bookkeeping tracks
        // DATA orphaning only, so a `MemPhi` there survives in the cached live
        // set (its value still feeds a consumer that is itself control-dead but
        // not yet known to be). The memory-SSA walk would then hit a zero-arm
        // `MemPhi` and trip its "clean chain bottoms out at InitialMemory"
        // invariant, so resync from a fresh entry walk whenever a strip empties
        // a region.
        let mut emptied_a_region = false;
        for (region, idxs) in dead {
            edit.remove_region_predecessors(region, &idxs)?;
            emptied_a_region |= edit.node_inputs(region).is_empty();
        }
        if emptied_a_region {
            edit.resync_live_set();
        }
        Ok(OptimizationResult::Changed)
    }
}
