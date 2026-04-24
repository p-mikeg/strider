//! Post-match guard combinators.
//!
//! [`WhenPat`] runs a predicate that sees only the matched output;
//! [`WhenMatchPat`] runs a predicate with access to the full capture
//! bindings.  Produced by [`crate::pat::IntoPat::when`] and
//! [`crate::pat::Pat::when_match`] respectively.

use ir::node::NodeOutputId;

use crate::matcher::Bindings;
use crate::pat::traits::{MatchCtx, Pattern};

/// Post-match predicate that sees only the matched output.
///
/// `inner` is a [`Pat`](crate::pat::Pat) so the fluent builder API
/// (`impl Into<Pat>`) can wrap arbitrary patterns uniformly; dispatch goes
/// through [`crate::matcher::Matcher::match_output`].
pub struct WhenPat {
    pub(crate) inner: crate::pat::Pat,
    pub(crate) func: crate::pat::PredicateFn,
}

impl Pattern for WhenPat {
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
pub struct WhenMatchPat {
    pub(crate) inner: crate::pat::Pat,
    pub(crate) func: crate::pat::MatchPredicateFn,
}

impl Pattern for WhenMatchPat {
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
