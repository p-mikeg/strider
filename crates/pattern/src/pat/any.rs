//! Small wildcard / capture combinators.

use ir::node::{NodeId, NodeOutputId};

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
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        // Value-kind gate: a `Var` binds a data edge, never a Control or
        // Memory slot.  For a multi-output node (e.g. `Load` =
        // `[Memory, Value]`) this causes iteration in `try_match_node` to
        // skip the non-value slots and land on the value output — the
        // caller's intent for `var(v)` / `.capture(v)`.
        if ctx.graph.graph.output_kind(target).as_value().is_none() {
            return false;
        }
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
    fn kind_spec(&self) -> crate::pat::node_pat::KindSpec {
        // A capture inherits the inner pattern's kind spec — wrapping
        // `add(...)` in `.capture(v)` must not forfeit the find_all
        // prefilter speedup.
        self.inner.as_dyn().kind_spec()
    }

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
        // Value-kind gate: see `VarPat::try_match` — a `Var` refers to a
        // data edge, so non-value outputs (Memory / Control) cause the
        // whole capture to fail.  On multi-output nodes this steers
        // `try_match_node`'s iteration to the value slot.
        if ctx.graph.graph.output_kind(target).as_value().is_none() {
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

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        // A `CapturePat` binds the matched value output.  For zero-output
        // nodes (e.g. `Return`) there is no value slot to bind, so fail
        // explicitly instead of letting the default outputs-iterator
        // report a silent miss.  This also means a control-flow pattern
        // wrapped in `.capture(Var)` is a clear no-match, not an
        // indeterminate fall-through.
        let outputs = ctx.graph.graph.node_outputs(node);
        if outputs.is_empty() {
            return false;
        }
        for out in outputs {
            let mark = b.mark();
            if self.try_match(ctx, out, b) {
                return true;
            }
            b.restore(mark);
        }
        false
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
