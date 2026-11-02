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
    /// Building the set costs ~250us, more than a whole run of the pass over a
    /// small function, so it is built once per thread. Held HERE rather than in
    /// the pass, which keeps the pass `Send`: a rule's closures carry no
    /// auto-trait bound at all, so an owning field of ANY kind, `Rc`, `Arc` or
    /// plain `Box`, would make the pass `!Send` and a pipeline unable to cross
    /// a thread.
    static RULES: RuleTable = RuleTable::new(rules::build_rules());
}

impl PeepholePass for ConstantFold {
    type Sweep = ();
    /// A fold makes its consumers' operands constant, which the same sweep can
    /// fold next.
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
