use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite, first_matching_rule};

pub(crate) mod eval_float;
pub(crate) mod eval_int;
mod rules;
#[cfg(test)]
mod tests;

use std::rc::Rc;

/// Folds constant expressions and applies algebraic identities.
#[derive(Clone)]
pub struct ConstantFold {
    rules: Rc<Vec<crate::BoxedRule>>,
}

thread_local! {
    /// Rebuilding the rule set costs about as much as one run of the pass.
    static RULES: Rc<Vec<crate::BoxedRule>> = Rc::new(rules::build_rules());
}

impl ConstantFold {
    pub fn new() -> Self {
        Self {
            rules: RULES.with(Rc::clone),
        }
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
        let opt = first_matching_rule(&self.rules, edit, root)?;
        Ok(PeepholeRewrite::from_new_value(edit, opt))
    }
}
