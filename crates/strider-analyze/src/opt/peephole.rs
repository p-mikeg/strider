//! `PeepholePass` trait + generic driver for the kind-filtered, per-node
//! rewrite shape shared by most opt passes in this crate.
//!
//! A `PeepholePass` impl declares (a) which `NodeKind`s it cares about and
//! (b) how to attempt one rewrite at a given root.  The driver
//! ([`run_peephole`]) handles the worklist, kind-filtered seeding,
//! and (optionally) consumer re-enqueue on a successful rewrite.  Passes
//! that don't need cascading re-enqueue can override
//! [`PeepholePass::propagate_to_consumers`] to return `false`.
//!
//! `PeepholePass` is *below* the existing [`crate::opt::pipeline::Optimizer`]
//! trait — concrete passes implement `PeepholePass` and provide a thin
//! `Optimizer` impl whose body is just `run_peephole(self, ctx)`.  The
//! pipeline still consumes `dyn Optimizer` exactly as before.
//!
//! Passes that don't fit this shape (analytic passes, multi-stage passes
//! with a per-pass memo, etc.) keep their hand-written `Optimizer` impl.

use entity_utils::Worklist;
use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::pipeline::OptimizationResult;

/// A kind-filtered, per-node rewrite pass.  See module docs.
pub(crate) trait PeepholePass {
    /// Concrete pass name, for debug / tracing only.  Held in the trait
    /// surface (not just on the concrete pass type) so `dyn`-erased
    /// drivers can attribute failures to the originating pass.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    /// Which `NodeKind`s does this pass care about?  Seeded into the
    /// worklist by [`run_peephole`] via `ctx.preorder_kind`.
    fn matches_kind(&self, kind: &NodeKind) -> bool;

    /// Attempt to rewrite at `root`.  Returns `Changed` if a rewrite
    /// fired (the driver will re-enqueue consumers iff
    /// [`Self::propagate_to_consumers`] is `true`).
    ///
    /// # Errors
    /// Propagates the first error from the underlying rewrite.
    fn try_rewrite(
        &self,
        ctx: &mut crate::pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult>;

    /// When `true`, the driver re-enqueues every consumer of `root`'s
    /// outputs after a successful rewrite, so cascading folds can
    /// re-fire in the same sweep (no need for a fresh fixed-point
    /// iteration).  Default `true` matches the `ConstantFold` shape.
    fn propagate_to_consumers(&self) -> bool {
        true
    }
}

/// Drive a [`PeepholePass`] over the reachable graph.
///
/// Seeds the worklist with every kind-matching reachable root, then
/// drains the worklist by calling `pass.try_rewrite` on each root.  On
/// `Changed`, consumers of `root`'s outputs are re-enqueued (subject to
/// [`PeepholePass::propagate_to_consumers`]) so cascading folds can fire
/// in the same sweep.
///
/// Consumers are snapshotted **before** `try_rewrite` runs because the
/// rewrite typically rewires uses to a replacement, leaving
/// `output_uses(old_out)` empty afterwards.  A `SmallVec<[NodeId; 8]>`
/// inlines the common case (~95% of IR nodes fan out to <=8 consumers)
/// to avoid heap allocation on the hot worklist path.
///
/// # Errors
/// Propagates the first error from `try_rewrite`.
pub(crate) fn run_peephole<P: PeepholePass>(
    pass: &P,
    ctx: &mut crate::pattern::RewriteCtx<'_>,
) -> Result<OptimizationResult> {
    let mut work: Worklist<NodeId> =
        ctx.preorder_kind(|k| pass.matches_kind(k)).collect();
    let mut overall = OptimizationResult::NoChange;
    let propagate = pass.propagate_to_consumers();
    // Reused per iteration to snapshot consumer NodeIds BEFORE running
    // the pass body.  After a rewrite, `output_uses(old_out)` is empty
    // (uses were rewired to the replacement), so capture consumers ahead.
    let mut consumers: smallvec::SmallVec<[NodeId; 8]> = smallvec::SmallVec::new();
    while let Some(root) = work.dequeue() {
        if propagate {
            consumers.clear();
            for &out in ctx.node_outputs(root) {
                for (consumer, _) in ctx.output_uses(out) {
                    consumers.push(consumer);
                }
            }
        }
        let r = pass.try_rewrite(ctx, root)?;
        if r.changed() {
            overall = OptimizationResult::Changed;
            if propagate {
                for &consumer in &consumers {
                    work.enqueue(consumer);
                }
            }
        }
    }
    Ok(overall)
}
