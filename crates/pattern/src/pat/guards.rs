//! Post-match guard combinator.
//!
//! [`GuardPat`] runs a predicate after the inner pattern matches.  Two
//! predicate flavors share the same dispatch shape — they only differ in
//! what they see — so they're represented by one struct with a
//! [`GuardFn`] enum for the closure signature.  Produced by
//! [`crate::pat::IntoPat::when`] and [`crate::pat::Pat::when_match`].

use ir::node::NodeOutputId;

use crate::matcher::Bindings;
use crate::pat::traits::{MatchCtx, Pattern};

/// The two predicate shapes accepted by [`GuardPat`]:
///
/// * [`Self::Output`] sees only the matched output — cheapest; used by the
///   common [`crate::pat::IntoPat::when`] combinator.
/// * [`Self::Bindings`] sees the full capture [`Bindings`] map — needed by
///   guards that cross-reference multiple captures
///   ([`crate::pat::Pat::when_match`]).
pub enum GuardFn {
    Output(crate::pat::PredicateFn),
    Bindings(crate::pat::MatchPredicateFn),
}

/// Post-match predicate combinator.  `inner` is a [`crate::pat::Pat`] so the
/// fluent builder API (`impl Into<Pat>`) can wrap arbitrary patterns
/// uniformly; dispatch goes through [`crate::matcher::Matcher::match_output`].
///
/// **Caveat (zero-output bases):** `GuardPat`'s default
/// [`Pattern::try_match_node`] iterates the node's outputs and calls
/// [`try_match`](Pattern::try_match) on each.  Zero-output node kinds —
/// `Return` and the dangling-output forms of `CallOther` — have no
/// outputs to iterate, so `ret().when(predicate)` never matches.  The
/// default-impl loop returns `false` *silently* for these bases.  Both
/// [`GuardFn`] variants require a [`ir::node::NodeOutputType`] (sourced
/// from `target`'s output kind) which a zero-output node cannot supply,
/// so the limitation is structural rather than something this combinator
/// can bridge transparently.  Callers wanting to filter zero-output
/// roots should:
///
/// 1. Wrap a value-producing sub-pattern that *witnesses* the zero-output
///    root — e.g. `ret().preceded_by(<witness>.when(p))` — so the predicate
///    runs on a value-bearing inner output rather than the Return itself.
/// 2. Implement a custom `Pattern` that handles zero-output dispatch
///    directly.
pub struct GuardPat {
    pub(crate) inner: crate::pat::Pat,
    pub(crate) func: GuardFn,
}

impl Pattern for GuardPat {
    fn kind_spec(&self) -> crate::pat::node_pat::KindSpec {
        // Guards inherit the inner pattern's spec — the predicate narrows
        // the match but doesn't broaden the accepted kinds.
        self.inner.as_dyn().kind_spec()
    }

    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let mark = b.mark();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            b.restore(mark);
            return false;
        }
        let Some(out_ty) = ctx.require_value_output(target) else {
            b.restore(mark);
            return false;
        };
        let ok = match &self.func {
            GuardFn::Output(f) => f(ctx.graph, out_ty, target),
            GuardFn::Bindings(f) => f(ctx.graph, out_ty, b),
        };
        if ok {
            true
        } else {
            b.restore(mark);
            false
        }
    }

}
