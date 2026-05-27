use std::sync::Arc;

use strider_ir::node::{NodeKind, NodeOutputId, NodeOutputType};

use crate::pattern::var::Capture;

mod builders;
pub(crate) mod ctor;

pub(crate) mod traits;
pub(crate) mod node_pat;
pub(crate) mod any;
pub(crate) mod guards;
pub use builders::{
    CallOtherPat, CallPat, FloatBinaryOpPat, FunctionArgPat, IfPat,
    IntBinaryOpPat, LoadPat, MemPhiPat, PhiPat, RetPat, StorePat, ValuePhiPat,
};
// `BinaryOpPat<Op>` underlies the three aliases above; not re-exported
// directly because the crate-private `BinaryOpKind` bound has no
// outside-the-crate impls.
pub use ctor::*;

/// Predicate function type used by the [`guards::GuardPat`] combinator
/// (produced by [`IntoPat::when`]).  The first arg is the underlying
/// `strider_ir::Graph` (no CC fields) — matches use the rewrite-friendly
/// matcher path.  Predicates that need CC info should query a
/// [`Match`](crate::pattern::Match)-side accessor (`get_vn` etc.) post-match
/// rather than during the guard check.
pub(crate) type PredicateFn =
    Arc<dyn Fn(&strider_ir::Graph, NodeOutputType, NodeOutputId) -> bool + Send + Sync>;

/// Predicate function type used by the [`guards::GuardPat`] combinator
/// for the bindings-aware variant (produced by [`Pat::when_match`]).
pub(crate) type MatchPredicateFn = Arc<
    dyn Fn(&strider_ir::Graph, NodeOutputType, &crate::pattern::matcher::Bindings) -> bool + Send + Sync,
>;

// ── any_*_const constructors ──────────────────────────────────────────────────
//
// Each takes a [`Capture`] and builds a pattern that matches any
// `IntConst` / `FloatConst` node (the boolean case is an `IntConst`
// typed `I1`) and binds it to the capture.  After the match, callers
// extract the constant value via
// [`crate::pattern::Match::get_uint`] / `get_bool` / `get_float_bits`.
//
// These are intentionally function-only (no trait dispatch) — the
// previous overloading on `Capture` vs typed-Var is gone with the typed
// Vars themselves.

/// Matches any `IntConst` node and binds it to `c`.
///
/// Fails if the producing node is not an `IntConst` — use this instead
/// of `var(c)` when you want the pattern itself to enforce the node is
/// a compile-time constant.  Recover the value via
/// [`crate::pattern::Match::get_uint`] / [`crate::pattern::Match::get_int`].
#[must_use]
pub fn any_int_const(c: Capture) -> Pat {
    crate::pattern::pat::node_pat::NodePat::matcher(
        crate::pattern::pat::node_pat::KindSpec::variant(&NodeKind::IntConst(0u128)),
        crate::pattern::pat::node_pat::InputsSpec::None,
    )
    .into_pat()
    .capture(c)
}

/// Matches any boolean constant node and binds it to `c`.  Booleans are
/// `I1` integers, so this matches an `IntConst` whose output type is `I1`.
/// Recover the value via [`crate::pattern::Match::get_bool`].
#[must_use]
pub fn any_bool_const(c: Capture) -> Pat {
    crate::pattern::pat::node_pat::NodePat::matcher(
        crate::pattern::pat::node_pat::KindSpec::variant(&NodeKind::IntConst(0u128)),
        crate::pattern::pat::node_pat::InputsSpec::None,
    )
    .into_pat()
    .when(|_g, ty, _o| ty.is_bool())
    .capture(c)
}

/// Matches any `FloatConst` node and binds it to `c`.  Recover the
/// IEEE 754 bit pattern via [`crate::pattern::Match::get_float_bits`].
#[must_use]
pub fn any_float_const(c: Capture) -> Pat {
    crate::pattern::pat::node_pat::NodePat::matcher(
        crate::pattern::pat::node_pat::KindSpec::variant(&NodeKind::FloatConst(0u64)),
        crate::pattern::pat::node_pat::InputsSpec::None,
    )
    .into_pat()
    .capture(c)
}

// ── Core pattern type ─────────────────────────────────────────────────────────

/// A graph pattern.  Cheap to clone — the inner data is reference-counted.
#[derive(Clone)]
pub struct Pat(crate::pattern::pat::traits::DynPat);

impl Pat {
    /// Wrap a reference-counted [`Pattern`](crate::pattern::pat::traits::Pattern) as
    /// a [`Pat`].
    pub(crate) fn from_dyn(d: crate::pattern::pat::traits::DynPat) -> Self {
        Self(d)
    }

    /// Borrow the inner [`DynPat`](crate::pattern::pat::traits::DynPat).
    pub(crate) fn as_dyn(&self) -> &crate::pattern::pat::traits::DynPat {
        &self.0
    }

    /// After this pattern matches successfully, additionally run `f` with
    /// access to the full capture [`crate::pattern::matcher::Bindings`].  The match
    /// fails if `f` returns `false`.  For commutative binary ops this failure
    /// triggers the other-ordering retry automatically.
    pub fn when_match<F>(self, f: F) -> Pat
    where
        F: Fn(&strider_ir::Graph, NodeOutputType, &crate::pattern::matcher::Bindings) -> bool
            + Send
            + Sync
            + 'static,
    {
        Pat::from_dyn(Arc::new(crate::pattern::pat::guards::GuardPat {
            inner: self,
            func: crate::pattern::pat::guards::GuardFn::Bindings(Arc::new(f)),
        }))
    }
}

// ── IntoPat blanket trait ─────────────────────────────────────────────────────

/// Blanket trait that lets every builder struct (`IntBinaryOpPat`, `LoadPat`, …)
/// participate in the common `capture` / `when` suffix operations without
/// each builder re-implementing them.
///
/// Every type that implements `Into<Pat>` automatically gets `capture` and `when`
/// for free.  Import this trait to call `.capture(c)` / `.when(f)` on builder
/// types.
pub trait IntoPat: Into<Pat> + Sized {
    /// After matching, bind the matched node (and its value output, if
    /// the pattern is value-producing) to `c`.  For control-flow
    /// patterns (`Call`, `If`, `Return`, `CallOther`) only the node id
    /// is bound and [`crate::pattern::Match::output`] returns `None`.
    fn capture(self, c: Capture) -> Pat {
        let inner: Pat = self.into();
        Pat::from_dyn(Arc::new(crate::pattern::pat::any::CapturePat {
            inner,
            capture: c,
        }))
    }
    /// After matching, additionally run `f` — fails if it returns `false`.
    ///
    /// **Zero-value-output kinds.**  `f` receives the matched root's
    /// single value output.  Roots that produce zero value outputs
    /// (control-flow nodes — `If`, `Return`, `Region`, …) cannot
    /// satisfy this signature; the guard silently fails (no match) on
    /// such roots.  Use [`Pat::when_match`] (which receives the full
    /// bindings instead) when guarding control-flow patterns.
    fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&strider_ir::Graph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        let inner: Pat = self.into();
        Pat::from_dyn(Arc::new(crate::pattern::pat::guards::GuardPat {
            inner,
            func: crate::pattern::pat::guards::GuardFn::Output(Arc::new(f)),
        }))
    }
}

impl<T: Into<Pat>> IntoPat for T {}
