//! `DetachUnreachable` — whole-graph orphan-cleanup sweep.
//!
//! After the destructive peepholes ([`crate::opt::PhiCollapse`],
//! [`crate::opt::RegionCollapse`], [`crate::opt::DeadBranchElimination`],
//! [`crate::opt::CfgDetach`]) rewire consumers past collapsed nodes, the
//! displaced producers can become unreachable from the function entry.
//! This pass detaches their inputs so the arena holds no dangling
//! input edges into a still-attached subgraph.
//!
//! It is an **analytic** pass, not a [`crate::opt::peephole::PeepholePass`]:
//! the sweep is one whole-graph reachability walk
//! ([`strider_pattern::RewriteCtx::detach_unreachable_nodes`]), not a
//! per-node kind-filtered rewrite.
//!
//! Like the equivalent sweep the former `RedundantPhis` pass ran, this
//! pass reports [`OptimizationResult::NoChange`] **even when it detaches**:
//! an unreachable node can't be a consumer of a reachable producer, so no
//! other pass can act on the result — escalating to `Changed` would only
//! cost the fixed-point loop one extra empty iteration.

use crate::opt::error::Result;
use crate::opt::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Detaches the inputs of every node unreachable from the function entry.
///
/// Bookkeeping only — always reports `NoChange` so it never spins the
/// fixed-point loop.
#[derive(Clone, Copy)]
pub struct DetachUnreachable;

impl Optimizer for DetachUnreachable {
    fn apply(
        &self,
        rctx: &mut strider_pattern::RewriteCtx<'_>,
        _ctx: &OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let entry = rctx.entry();
        // Detaching orphans is hygiene, not progress: deliberately do NOT
        // escalate to `Changed` (matches the prior RedundantPhis policy).
        let _ = rctx.detach_unreachable_nodes(entry);
        Ok(OptimizationResult::NoChange)
    }
}
