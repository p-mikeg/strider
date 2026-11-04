use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::Result;
use crate::peephole::PeepholePass;
use crate::rewrite_rule::RuleTable;

pub(crate) mod eval_float;
mod rules;
#[cfg(test)]
mod tests;

/// Folds constant expressions and applies algebraic identities.
pub struct ConstantFold;

thread_local! {
    /// Built once per thread (~250us); held outside the pass to keep it `Send`.
    static RULES: RuleTable = RuleTable::new(rules::build_rules());
}

impl PeepholePass for ConstantFold {
    type Sweep = ();
    /// A fold makes its consumers' operands constant.
    const PROPAGATE: bool = true;

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        RULES.with(|rules| rules.admits(kind))
    }

    fn try_rewrite(
        &self,
        _sweep: &mut (),
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>> {
        RULES.with(|rules| rules.first_match(edit, root))
    }
}
