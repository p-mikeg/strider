//! Small wildcard / capture combinators.

use ir::node::NodeOutputId;

use crate::error::{ErrorKind, Result};
use crate::matcher::Bindings;
use crate::pat::traits::{BuildCtx, BuildOutcome, MatchCtx, Pattern};
use crate::var::Var;

/// Matches any output unconditionally.
pub struct AnyPat;

impl Pattern for AnyPat {
    fn try_match(&self, _: &MatchCtx, _: NodeOutputId, _: &mut Bindings) -> bool {
        true
    }
}

/// Matches any output and binds it to `var`.  Dedicated type so the very
/// common `var(v)` path avoids the double dispatch + snapshot of the
/// general-purpose [`CapturePat`] wrapping [`AnyPat`].  Produced only by
/// [`crate::pat::var`]; `any().capture(v)` still yields a `CapturePat`.
pub struct VarPat {
    pub(crate) var: Var,
}

impl Pattern for VarPat {
    fn try_match(&self, _: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        // `bind_var` is self-contained: it returns false on conflict without
        // mutating, so no snapshot is needed.
        b.bind_var(self.var, target)
    }

    fn try_build(&self, ctx: &mut BuildCtx<'_>) -> Result<BuildOutcome> {
        let out = ctx
            .bindings
            .get(self.var)
            .ok_or(ErrorKind::MissingBinding("Var"))?;
        Ok(BuildOutcome::Out(out))
    }
}

/// Matches `inner`, then additionally binds the matched output to `var`.
/// Produced by [`crate::pat::IntoPat::capture`] /
/// [`crate::pat::Pat::capture_impl`].
///
/// `inner` is stored as a [`Pat`](crate::pat::Pat) so the fluent builder
/// API (`impl Into<Pat>`) can wrap arbitrary patterns uniformly; dispatch
/// goes through [`crate::matcher::Matcher::match_output`].
pub struct CapturePat {
    pub(crate) inner: crate::pat::Pat,
    pub(crate) var: Var,
}

impl Pattern for CapturePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        // Journal mark so that either the inner match or a failed
        // `bind_var` afterwards leaves the bindings untouched on return.
        let mark = b.mark();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            // Inner is expected to clean up its own speculative bindings,
            // but restore here too for pattern-local cleanliness: truncate
            // is idempotent if the inner already rolled back.
            b.restore(mark);
            return false;
        }
        if b.bind_var(self.var, target) {
            true
        } else {
            b.restore(mark);
            false
        }
    }

    fn try_build(&self, ctx: &mut BuildCtx<'_>) -> Result<BuildOutcome> {
        // In build position, `var(v)` materializes by looking up the
        // `NodeOutputId` bound during the match. Used by rewrite rules
        // that reuse captured operands verbatim in the RHS.
        let out = ctx
            .bindings
            .get(self.var)
            .ok_or(ErrorKind::MissingBinding("Var"))?;
        Ok(BuildOutcome::Out(out))
    }
}
