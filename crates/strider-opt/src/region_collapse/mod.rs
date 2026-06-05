//! `RegionCollapse` — collapses single-predecessor `Region` joins.
//!
//! A `Region` with exactly ONE control input is a no-op join: control
//! flows straight through it.  This pass replaces the Region's control
//! output (output index 0) with its lone control input, so every control
//! consumer connects directly to the predecessor's control producer.
//!
//! Phis layered over the Region are handled independently by
//! [`crate::PhiCollapse`]; this pass deliberately does **not** touch
//! them.  Once both of the Region's outputs have no remaining uses this
//! pass kills the now-dead Region (it is side-effecting, so the automatic
//! dead-cone cull never reaches it); until then the now-dead Region is
//! left attached (a fully-unreachable orphan is harmless — the validator
//! and pattern queries only walk from entry).

use entity_utils::{DenseEntitySet, Worklist};
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Collapses a single-control-input `Region` by rewiring its control
/// consumers to its lone predecessor.
#[derive(Clone, Copy)]
pub struct RegionCollapse;

impl Optimizer for RegionCollapse {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        _opt: &mut OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        // Snapshot the set of nodes reachable from entry ONCE per run.  The
        // detach decision below treats a phi-token consumer as "live" only if
        // it is in this set — an unreachable orphan `Phi`/`MemPhi` must not
        // pin its Region (see the comment in `try_collapse`).  Detaching only
        // ever shrinks reachability, so this once-per-run snapshot is a safe
        // over-approximation: a stale entry can only make us *more*
        // conservative (keep a Region we could have detached), never wrongly
        // detach a live one; the next fixed-point iteration recomputes it.
        // Computing it once keeps the pass O(n) per run rather than O(n²).
        let reachable: DenseEntitySet<NodeId> = ctx.walk().collect();

        // Seed with every reachable Region, then drain — re-enqueuing
        // consumers on a successful collapse so a freshly-exposed
        // single-pred Region downstream folds in the same sweep.
        let mut work: Worklist<NodeId> = ctx.walk_kind(|k| matches!(k, NodeKind::Region)).collect();
        let mut overall = OptimizationResult::NoChange;
        // Only re-enqueue `Region` consumers — `try_collapse` operates on the
        // Region output layout, so filtering here keeps the seed and the
        // re-enqueue contract identical (every dequeued node is a `Region`).
        let mut consumers: smallvec::SmallVec<[NodeId; 8]> = smallvec::SmallVec::new();
        while let Some(root) = work.dequeue() {
            consumers.clear();
            for &out in ctx.node_outputs(root) {
                for (consumer, _) in ctx.graph_ref().value_uses(out) {
                    if matches!(ctx.node_kind(consumer), NodeKind::Region) {
                        consumers.push(consumer);
                    }
                }
            }
            if self.try_collapse(ctx, root, &reachable)?.changed() {
                overall = OptimizationResult::Changed;
                for &consumer in &consumers {
                    work.enqueue(consumer);
                }
            }
        }
        Ok(overall)
    }
}

impl RegionCollapse {
    fn try_collapse(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        root: NodeId,
        reachable: &DenseEntitySet<NodeId>,
    ) -> Result<OptimizationResult> {
        // Both the seed walk and the consumer re-enqueue filter on
        // `NodeKind::Region`, so `root` is always a `Region` here and the
        // Region output layout read below is sound.
        let inputs = ctx.node_inputs(root);
        // Only a single-predecessor join is a no-op; the entry Region
        // (0 inputs) and genuine multi-way joins are left untouched.
        if inputs.len() != 1 {
            return Ok(OptimizationResult::NoChange);
        }
        let sole_ctrl_value = inputs[0];
        // Region outputs are [control, phi_token]; the control output is
        // index 0.
        let [ctrl_value, _phi_token] = ctx.node_outputs_exact::<2>(root)?;
        let result =
            OptimizationResult::NoChange.after_replace(ctx, ctrl_value, sole_ctrl_value)?;

        // After rewiring the control consumers, detach the now-dead Region's
        // own input edge — but ONLY once BOTH of its outputs (control AND
        // phi_token) have no remaining *reachable* uses.  Otherwise the Region
        // lingers as a second consumer of `sole_ctrl_value`, which breaks a
        // forward single-consumer walk (e.g. `IfPat::true_branch`) and keeps
        // the node control-reachable.
        //
        // A phi-token consumer only counts if it is reachable from entry.  An
        // orphan `Phi`/`MemPhi` (e.g. a builder-emitted single-pred VarPhi for
        // a register that's never read, or a phi `PhiCollapse` rewired past but
        // couldn't visit because it was already dead — possibly chained behind
        // other dead phis) is harmless residue in the arena, never swept (the
        // validator and pattern queries only walk from entry).  Counting such
        // orphans as live uses would pin the Region forever.
        //
        // When a *reachable* `Phi`/`MemPhi` still consumes the phi-token, leave
        // the Region attached this iteration — `PhiCollapse` will collapse that
        // phi, after which a later iteration finds both outputs free and
        // finishes the detach.
        let all_outputs_unused = ctx.node_outputs(root).iter().all(|&out| {
            ctx.graph_ref()
                .value_uses(out)
                .all(|(consumer, _)| !reachable.contains(consumer))
        });
        if all_outputs_unused && !ctx.node_inputs(root).is_empty() {
            // `Region` is side-effecting, so the automatic dead-cone cull
            // never reaches it — remove it explicitly.  `kill_node` detaches
            // its lone control-input edge (matching the former
            // `detach_node_inputs`), evicts it from the live set, and
            // auto-enqueues that now-orphaned predecessor cone for `clean`.
            ctx.kill_node(root);
        }
        Ok(result)
    }
}
