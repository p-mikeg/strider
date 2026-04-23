use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeKind, NodeOutputId, NodeOutputType};

use crate::var::{BoolVar, FloatVar, IntVar, Var};

mod builders;
mod ctor;

pub(crate) mod traits;
pub(crate) mod node_pat;
pub(crate) mod control_pat;
pub(crate) mod any;
pub(crate) mod guards;
pub(crate) mod contains;
pub use builders::{
    BoolBinaryOpPat, CallOtherPat, CallPat, FloatBinaryOpPat, FunctionArgPat, IfPat,
    IntBinaryOpPat, LoadPat, PhiPat, RetPat, StackStorePat, StackStorePhiPat, StorePat,
};
pub use ctor::*;

/// Predicate function type used by the `WhenPat` combinator (produced by
/// [`IntoPat::when`] / [`Pat::when_impl`]).
pub type PredicateFn =
    Arc<dyn Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync>;

/// Predicate function type used by the `WhenMatchPat` combinator (produced by
/// [`Pat::when_match`]).
///
/// Unlike [`PredicateFn`], this variant sees the full capture [`crate::matcher::Bindings`]
/// map, not just the single matched output — useful for guards that
/// reference multiple captures.
pub type MatchPredicateFn = Arc<
    dyn Fn(&BuiltFunctionGraph, NodeOutputType, &crate::matcher::Bindings) -> bool + Send + Sync,
>;

// ── Const-capture overloading traits ──────────────────────────────────────────

/// Sealed trait used by [`any_int_const`] to accept either a [`Var`] (binds
/// the matched `NodeOutputId`) or an [`IntVar`] (binds the concrete
/// constant value as `u64`).
pub trait IntoAnyIntConst: sealed::SealedAnyIntConst {
    #[doc(hidden)]
    fn into_any_int_const_pat(self) -> Pat;
}

impl IntoAnyIntConst for Var {
    fn into_any_int_const_pat(self) -> Pat {
        // Match any IntConst; bind the output to `self` via NodePat.output_var.
        Pat::from_dyn(Arc::new(crate::pat::node_pat::NodePat {
            kind_match: Arc::new(|ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::IntConst(_))
            }),
            inputs: crate::pat::node_pat::InputsSpec::None,
            post_match: None,
            output_var: Some(self),
            node_var: None,
        }))
    }
}

impl IntoAnyIntConst for IntVar {
    fn into_any_int_const_pat(self) -> Pat {
        // Match any IntConst; bind the concrete value to the IntVar.
        let iv = self;
        Pat::from_dyn(Arc::new(crate::pat::node_pat::NodePat {
            kind_match: Arc::new(move |ctx, node, b| match ctx.graph.graph.node_kind(node) {
                NodeKind::IntConst(v) => b.bind_int(iv, *v),
                _ => false,
            }),
            inputs: crate::pat::node_pat::InputsSpec::None,
            post_match: None,
            output_var: None,
            node_var: None,
        }))
    }
}

/// Sealed trait used by [`any_bool_const`] to accept either a [`Var`] or a
/// [`BoolVar`].
pub trait IntoAnyBoolConst: sealed::SealedAnyBoolConst {
    #[doc(hidden)]
    fn into_any_bool_const_pat(self) -> Pat;
}

impl IntoAnyBoolConst for Var {
    fn into_any_bool_const_pat(self) -> Pat {
        Pat::from_dyn(Arc::new(crate::pat::node_pat::NodePat {
            kind_match: Arc::new(|ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::BoolConst(_))
            }),
            inputs: crate::pat::node_pat::InputsSpec::None,
            post_match: None,
            output_var: Some(self),
            node_var: None,
        }))
    }
}

impl IntoAnyBoolConst for BoolVar {
    fn into_any_bool_const_pat(self) -> Pat {
        let bv = self;
        Pat::from_dyn(Arc::new(crate::pat::node_pat::NodePat {
            kind_match: Arc::new(move |ctx, node, b| match ctx.graph.graph.node_kind(node) {
                NodeKind::BoolConst(v) => b.bind_bool(bv, *v),
                _ => false,
            }),
            inputs: crate::pat::node_pat::InputsSpec::None,
            post_match: None,
            output_var: None,
            node_var: None,
        }))
    }
}

/// Sealed trait used by [`any_float_const`] to accept either a [`Var`] or a
/// [`FloatVar`].
pub trait IntoAnyFloatConst: sealed::SealedAnyFloatConst {
    #[doc(hidden)]
    fn into_any_float_const_pat(self) -> Pat;
}

impl IntoAnyFloatConst for Var {
    fn into_any_float_const_pat(self) -> Pat {
        Pat::from_dyn(Arc::new(crate::pat::node_pat::NodePat {
            kind_match: Arc::new(|ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatConst(_))
            }),
            inputs: crate::pat::node_pat::InputsSpec::None,
            post_match: None,
            output_var: Some(self),
            node_var: None,
        }))
    }
}

impl IntoAnyFloatConst for FloatVar {
    fn into_any_float_const_pat(self) -> Pat {
        let fv = self;
        Pat::from_dyn(Arc::new(crate::pat::node_pat::NodePat {
            kind_match: Arc::new(move |ctx, node, b| match ctx.graph.graph.node_kind(node) {
                NodeKind::FloatConst(bits) => b.bind_float(fv, *bits),
                _ => false,
            }),
            inputs: crate::pat::node_pat::InputsSpec::None,
            post_match: None,
            output_var: None,
            node_var: None,
        }))
    }
}

mod sealed {
    use crate::var::{BoolVar, FloatVar, IntVar, Var};

    pub trait SealedAnyIntConst {}
    impl SealedAnyIntConst for Var {}
    impl SealedAnyIntConst for IntVar {}

    pub trait SealedAnyBoolConst {}
    impl SealedAnyBoolConst for Var {}
    impl SealedAnyBoolConst for BoolVar {}

    pub trait SealedAnyFloatConst {}
    impl SealedAnyFloatConst for Var {}
    impl SealedAnyFloatConst for FloatVar {}
}

// ── Core pattern type ─────────────────────────────────────────────────────────

/// A graph pattern.  Cheap to clone — the inner data is reference-counted.
#[derive(Clone)]
pub struct Pat(PatInner);

/// Internal representation of a [`Pat`].
///
/// Transitional enum: the crate is migrating from the legacy `PatKind`-enum
/// dispatch to a trait-based dispatch.  Data patterns implement
/// [`DataPattern`](crate::pat::traits::DataPattern) and are held in
/// [`PatInner::Dyn`]; control patterns implement
/// [`ControlPattern`](crate::pat::traits::ControlPattern) and are held in
/// [`PatInner::Ctrl`].  After Phase 3.1 the Legacy variant is uninhabited
/// (PatKind has zero variants) — Phase 4.1 removes both.
#[derive(Clone)]
pub(crate) enum PatInner {
    #[allow(dead_code)] // empty enum placeholder; Phase 4.1 removes the variant along with PatKind
    Legacy(Arc<PatKind>),
    Dyn(crate::pat::traits::DynDataPat),
    Ctrl(crate::pat::traits::DynCtrlPat),
}

impl Pat {
    #[allow(dead_code)]
    pub(crate) fn new(kind: PatKind) -> Self {
        Self(PatInner::Legacy(Arc::new(kind)))
    }

    /// Constructor for the new trait-based dispatch path.  Phase 2+ will use
    /// this as constructors migrate one family at a time.
    #[allow(dead_code)]
    pub(crate) fn from_dyn(d: crate::pat::traits::DynDataPat) -> Self {
        Self(PatInner::Dyn(d))
    }

    /// Constructor for control-level trait-based patterns (Phase 3.1+).
    pub(crate) fn from_ctrl(d: crate::pat::traits::DynCtrlPat) -> Self {
        Self(PatInner::Ctrl(d))
    }

    /// Legacy accessor: `Some(kind)` if this pat is still on the legacy path,
    /// `None` if it has migrated to the trait-based path.  Phase 4 will
    /// delete this method along with `PatKind`.
    #[allow(dead_code)] // Phase 4.1 removes this alongside PatKind
    pub(crate) fn as_legacy(&self) -> Option<&PatKind> {
        match &self.0 {
            PatInner::Legacy(k) => Some(k),
            PatInner::Dyn(_) | PatInner::Ctrl(_) => None,
        }
    }

    /// New accessor for the trait-based data path.
    #[allow(dead_code)]
    pub(crate) fn as_dyn(&self) -> Option<&crate::pat::traits::DynDataPat> {
        match &self.0 {
            PatInner::Dyn(d) => Some(d),
            PatInner::Legacy(_) | PatInner::Ctrl(_) => None,
        }
    }

    /// Accessor for the trait-based control path (Phase 3.1+).
    pub(crate) fn as_ctrl(&self) -> Option<&crate::pat::traits::DynCtrlPat> {
        match &self.0 {
            PatInner::Ctrl(d) => Some(d),
            PatInner::Legacy(_) | PatInner::Dyn(_) => None,
        }
    }

    /// After this pattern matches successfully, additionally bind the matched
    /// output to `v`.  If `v` is already bound the output must equal the
    /// stored binding, otherwise the match fails.
    fn capture_impl(self, v: Var) -> Pat {
        Pat::from_dyn(Arc::new(crate::pat::any::CapturePat {
            inner: self,
            var: v,
        }))
    }

    /// After this pattern matches successfully, additionally run `f` against
    /// the matched output.  The match fails if `f` returns `false`.
    fn when_impl<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from_dyn(Arc::new(crate::pat::guards::WhenPat {
            inner: self,
            func: Arc::new(f),
        }))
    }

    /// After this pattern matches successfully, additionally run `f` with
    /// access to the full capture [`crate::matcher::Bindings`].  The match
    /// fails if `f` returns `false`.  For commutative binary ops this failure
    /// triggers the other-ordering retry automatically.
    pub fn when_match<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputType, &crate::matcher::Bindings) -> bool
            + Send
            + Sync
            + 'static,
    {
        Pat::from_dyn(Arc::new(crate::pat::guards::WhenMatchPat {
            inner: self,
            func: Arc::new(f),
        }))
    }
}

// ── IntoPat blanket trait ─────────────────────────────────────────────────────

/// Blanket trait that lets every builder struct (`IntBinaryOpPat`, `LoadPat`, …)
/// participate in the common `capture` / `when` suffix operations without
/// each builder re-implementing them.
///
/// Every type that implements `Into<Pat>` automatically gets `capture` and `when`
/// for free.  Import this trait to call `.capture(v)` / `.when(f)` on builder
/// types.
pub trait IntoPat: Into<Pat> + Sized {
    /// After matching, bind the matched output to `v`.
    fn capture(self, v: Var) -> Pat {
        self.into().capture_impl(v)
    }
    /// After matching, additionally run `f` — fails if it returns `false`.
    fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        self.into().when_impl(f)
    }
}

impl<T: Into<Pat>> IntoPat for T {}

// ── PatKind ───────────────────────────────────────────────────────────────────
//
// After Phase 3.1 every legacy variant has been migrated to the trait-based
// engine. The enum is kept as an uninhabited placeholder until Phase 4.1
// deletes it (along with `PatInner::Legacy` and the `as_legacy` accessor).
#[allow(dead_code)]
pub enum PatKind {}

