use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite, first_matching_rule};

pub(crate) mod eval_float;
pub(crate) mod eval_int;
mod rules;
#[cfg(test)]
mod tests;

/// Folds constant expressions and applies algebraic identities.
#[derive(Clone)]
pub struct ConstantFold;

thread_local! {
    /// Building the set costs ~250us, more than a whole run of the pass over a
    /// small function, so it is built once per thread. Held HERE rather than in
    /// the pass, which keeps the pass `Send`: a `BoxedRule` carries no
    /// auto-trait bound at all, so an owning field of ANY kind, `Rc`, `Arc` or
    /// plain `Box`, would make the pass `!Send` and a pipeline unable to cross
    /// a thread.
    static RULES: Vec<crate::BoxedRule> = rules::build_rules();
}

impl ConstantFold {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConstantFold {
    fn default() -> Self {
        Self::new()
    }
}

impl PeepholePass for ConstantFold {
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
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        let opt = RULES.with(|rules| first_matching_rule(rules, edit, root))?;
        Ok(PeepholeRewrite::from_new_value(edit, opt))
    }
}
