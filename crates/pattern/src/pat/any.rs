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
/// (`Pat::capture_impl` today wraps via `PatKind::WithCapture` — this is the
/// trait-based replacement.)
///
/// `inner` is stored as a [`Pat`](crate::pat::Pat) rather than a `DynDataPat`
/// so it can wrap either Legacy- or trait-backed patterns during the
/// migration; dispatch goes through [`crate::matcher::Matcher::match_output`].
pub struct CapturePat {
    pub(crate) inner: crate::pat::Pat,
    pub(crate) var: Var,
}

impl DataPattern for CapturePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        // Snapshot so that a failed `bind_var` after a successful inner match
        // leaves the bindings untouched — matches the legacy
        // `matcher::data::constants::PatKind::Capture` semantics where the
        // bind was the only mutation, so any failure left bindings clean.
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
