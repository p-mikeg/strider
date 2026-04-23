//! Small wildcard / capture combinators.

#![allow(dead_code)]

use ir::node::NodeOutputId;

use crate::matcher::Bindings;
use crate::pat::traits::{DataPattern, DynDataPat, MatchCtx};
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
pub struct CapturePat {
    pub(crate) inner: DynDataPat,
    pub(crate) var: Var,
}

impl DataPattern for CapturePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        if !self.inner.try_match(ctx, target, b) {
            return false;
        }
        b.bind_var(self.var, target)
    }
}
