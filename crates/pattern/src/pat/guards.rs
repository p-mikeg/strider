//! Post-match guard combinators. Replace the legacy `PatKind::WithPredicate`
//! and `PatKind::WithMatchPredicate` arms.

#![allow(dead_code)]

use ir::node::NodeOutputId;

use crate::matcher::Bindings;
use crate::pat::traits::{DataPattern, DynDataPat, MatchCtx};

/// Post-match predicate that sees only the matched output.
/// Replaces `PatKind::WithPredicate`.
pub struct WhenPat {
    pub(crate) inner: DynDataPat,
    pub(crate) func: crate::pat::PredicateFn,
}

impl DataPattern for WhenPat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let snap = b.clone();
        if !self.inner.try_match(ctx, target, b) {
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
    pub(crate) inner: DynDataPat,
    pub(crate) func: crate::pat::MatchPredicateFn,
}

impl DataPattern for WhenMatchPat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let snap = b.clone();
        if !self.inner.try_match(ctx, target, b) {
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
