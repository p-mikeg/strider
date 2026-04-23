//! Small wildcard / capture combinators.

use ir::node::NodeOutputId;

use crate::matcher::Bindings;
use crate::pat::traits::{DataPattern, MatchCtx};
use crate::var::Var;

/// Matches any output unconditionally.
pub struct AnyPat;

impl DataPattern for AnyPat {
    fn try_match(&self, _: &MatchCtx, _: NodeOutputId, _: &mut Bindings) -> bool {
        true
    }
}

/// Matches `inner`, then additionally binds the matched output to `var`.
/// Produced by [`crate::pat::IntoPat::capture`] /
/// [`crate::pat::Pat::capture_impl`].
///
/// `inner` is stored as a [`Pat`](crate::pat::Pat) rather than a `DynDataPat`
/// so the fluent builder API (`impl Into<Pat>`) can wrap either a data or
/// control pattern uniformly; dispatch goes through
/// [`crate::matcher::Matcher::match_output`].
pub struct CapturePat {
    pub(crate) inner: crate::pat::Pat,
    pub(crate) var: Var,
}

impl DataPattern for CapturePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        // Snapshot so that a failed `bind_var` after a successful inner
        // match leaves the bindings untouched.
        let snap = b.clone();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            return false;
        }
        if b.bind_var(self.var, target) {
            true
        } else {
            *b = snap;
            false
        }
    }
}
