//! Post-match guard combinators. Replace the legacy `PatKind::WithPredicate`
//! and `PatKind::WithMatchPredicate` arms.

use ir::node::NodeOutputId;

use crate::matcher::Bindings;
use crate::pat::traits::{DataPattern, MatchCtx};

/// Post-match predicate that sees only the matched output.
/// Replaces `PatKind::WithPredicate`.
///
/// `inner` is a [`Pat`](crate::pat::Pat) (not `DynDataPat`) so it can wrap
/// both Legacy- and trait-backed patterns — dispatch goes through
/// [`crate::matcher::Matcher::match_output`].
pub struct WhenPat {
    pub(crate) inner: crate::pat::Pat,
    pub(crate) func: crate::pat::PredicateFn,
}

impl DataPattern for WhenPat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let snap = b.clone();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            return false;
        }
        let Some(out_ty) = ctx.graph.graph.output_kind(target).as_value() else {
            *b = snap;
            return false;
        };
        if (self.func)(ctx.graph, out_ty, target) {
            true
        } else {
            *b = snap;
            false
        }
    }
}

/// Post-match predicate that sees the full capture bindings.
/// Replaces `PatKind::WithMatchPredicate`.
pub struct WhenMatchPat {
    pub(crate) inner: crate::pat::Pat,
    pub(crate) func: crate::pat::MatchPredicateFn,
}

impl DataPattern for WhenMatchPat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let snap = b.clone();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            return false;
        }
        let Some(out_ty) = ctx.graph.graph.output_kind(target).as_value() else {
            *b = snap;
            return false;
        };
        if (self.func)(ctx.graph, out_ty, b) {
            true
        } else {
            *b = snap;
            false
        }
    }
}
