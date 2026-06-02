use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::peephole::{PeepholePass, PeepholeRewrite};

pub(crate) mod eval_float;
pub(crate) mod eval_int;
mod rules;
#[cfg(test)]
mod tests;

use rules::ConstFoldRules;
use std::rc::Rc;

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Folds constant expressions and applies algebraic identities.
///
/// Handles full constant evaluation for all arithmetic, comparison, boolean,
/// truncation, and extension operations.  Also applies identities such as
/// `x + 0 → x`, `x ^ x → 0`, and nested AND-mask merging `(a & C1) & C2 →
/// a & (C1 & C2)`.
///
/// The rule set is built once by [`ConstantFold::new`] and held behind an
/// [`Rc`] so the pass stays cheaply `Clone` (the boxed rule closures are
/// not `Clone`); cloning the pass shares the same rule set.
#[derive(Clone)]
pub struct ConstantFold {
    rules: Rc<ConstFoldRules>,
}

impl ConstantFold {
    /// Builds the constant-fold rule set once and returns a pass that owns it.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: Rc::new(ConstFoldRules::build()),
        }
    }
}

impl Default for ConstantFold {
    fn default() -> Self {
        Self::new()
    }
}

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
        ctx: &mut strider_pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        Ok(match self.rules.apply_all(ctx, root)? {
            Some(new_value) => PeepholeRewrite::Changed {
                new_node: Some(ctx.producer(new_value)),
            },
            None => PeepholeRewrite::NoChange,
        })
    }
}

