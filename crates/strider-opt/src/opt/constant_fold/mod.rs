use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite};

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
    /// Every constant-fold rule roots at an int/float **operation** — the
    /// arithmetic, comparison, cast, truncate/extend, and conversion kinds
    /// below.  A rule can never fire on a `Region` / `Phi` / `Call` / `Load` /
    /// constant / control node, so seeding those (the former `true`) just paid
    /// ~46 failing rule attempts per node.  `run_peephole` honours `matches_kind`
    /// for both the seed walk AND the re-enqueue of a rewrite's consumers/new
    /// node, so narrowing to the foldable kinds cannot drop a fold — it only
    /// stops probing nodes that never fold.
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(
            kind,
            NodeKind::IntUnaryOp(_)
                | NodeKind::IntBinaryOp(_)
                | NodeKind::IntCmpOp(_)
                | NodeKind::Truncate
                | NodeKind::Popcount
                | NodeKind::Lzcount
                | NodeKind::Extend(_)
                | NodeKind::FloatBinaryOp(_)
                | NodeKind::FloatUnaryOp(_)
                | NodeKind::FloatCmpOp(_)
                | NodeKind::IntToFloat
                | NodeKind::FloatToInt
                | NodeKind::FloatToFloat
                | NodeKind::IntBitsToFloat
                | NodeKind::FloatBitsToInt
        )
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        let opt = self.rules.apply_all(ctx, root)?;
        Ok(PeepholeRewrite::from_new_value(ctx, opt))
    }
}
