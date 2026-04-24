use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeKind, NodeOutputId, NodeOutputType};

use crate::var::{BoolVar, FloatVar, IntVar, Var};

mod builders;
pub(crate) mod ctor;

pub(crate) mod traits;
pub(crate) mod node_pat;
pub(crate) mod any;
pub(crate) mod guards;
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
            kind_build: None,
            build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
            outputs: crate::pat::node_pat::OutputsSpec::None,
            consumers: crate::pat::node_pat::ConsumersSpec::None,
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
        // In build position, emit `IntConst(bindings[iv])` at the root type.
        let iv = self;
        Pat::from_dyn(Arc::new(crate::pat::node_pat::NodePat {
            kind_build: Some(Arc::new(move |ctx| {
                let v = ctx.bindings
                    .get_int(iv)
                    .ok_or(crate::error::ErrorKind::MissingBinding("IntVar"))?;
                Ok(NodeKind::IntConst(v))
            })),
            build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
            outputs: crate::pat::node_pat::OutputsSpec::None,
            consumers: crate::pat::node_pat::ConsumersSpec::None,
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
            kind_build: None,
            build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
            outputs: crate::pat::node_pat::OutputsSpec::None,
            consumers: crate::pat::node_pat::ConsumersSpec::None,
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
            kind_build: Some(Arc::new(move |ctx| {
                let v = ctx.bindings
                    .get_bool(bv)
                    .ok_or(crate::error::ErrorKind::MissingBinding("BoolVar"))?;
                Ok(NodeKind::BoolConst(v))
            })),
            build_result_ty: crate::pat::node_pat::BuildTy::Fixed(NodeOutputType::Bool),
            outputs: crate::pat::node_pat::OutputsSpec::None,
            consumers: crate::pat::node_pat::ConsumersSpec::None,
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
            kind_build: None,
            build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
            outputs: crate::pat::node_pat::OutputsSpec::None,
            consumers: crate::pat::node_pat::ConsumersSpec::None,
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
            kind_build: Some(Arc::new(move |ctx| {
                let bits = ctx.bindings
                    .get_float_bits(fv)
                    .ok_or(crate::error::ErrorKind::MissingBinding("FloatVar"))?;
                Ok(NodeKind::FloatConst(bits))
            })),
            build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
            outputs: crate::pat::node_pat::OutputsSpec::None,
            consumers: crate::pat::node_pat::ConsumersSpec::None,
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
pub struct Pat(crate::pat::traits::DynPat);

impl Pat {
    /// Wrap a reference-counted [`Pattern`](crate::pat::traits::Pattern) as
    /// a [`Pat`].
    pub(crate) fn from_dyn(d: crate::pat::traits::DynPat) -> Self {
        Self(d)
    }

    /// Borrow the inner [`DynPat`](crate::pat::traits::DynPat).
    pub(crate) fn as_dyn(&self) -> &crate::pat::traits::DynPat {
        &self.0
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

