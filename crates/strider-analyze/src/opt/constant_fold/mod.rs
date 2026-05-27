use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::peephole::{PeepholePass, impl_optimizer_from_peephole};
use crate::opt::pipeline::OptimizationResult;

pub(crate) mod eval_float;
pub(crate) mod eval_int;
mod rules;
#[cfg(test)]
mod tests;

use rules::*;

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Folds constant expressions and applies algebraic identities.
///
/// Handles full constant evaluation for all arithmetic, comparison, boolean,
/// truncation, and extension operations.  Also applies identities such as
/// `x + 0 → x`, `x ^ x → 0`, and nested AND-mask merging `(a & C1) & C2 →
/// a & (C1 & C2)`.
#[derive(Clone)]
pub struct ConstantFold;

impl PeepholePass for ConstantFold {
    /// Constant-fold rule groups cover most node kinds (int / bool / float
    /// arithmetic + cmp, casts, truncate / extend, identity rewrites on
    /// just about any binary op).  Seeding the worklist with every
    /// reachable node preserves the prior behaviour of the hand-written
    /// `optimize` impl; the per-group rules already kind-filter
    /// internally.
    fn matches_kind(&self, _kind: &NodeKind) -> bool {
        true
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        apply_all_rules(ctx, root)
    }
}

impl_optimizer_from_peephole!(ConstantFold);
