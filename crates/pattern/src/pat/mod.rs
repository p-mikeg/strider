use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeKind, NodeOutputId, NodeOutputType};

use crate::var::{BoolVar, Capture, FloatVar, IntVar};

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

/// Predicate function type used by the [`guards::GuardPat`] combinator
/// (produced by [`IntoPat::when`]).
pub(crate) type PredicateFn =
    Arc<dyn Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync>;

/// Predicate function type used by the [`guards::GuardPat`] combinator
/// for the bindings-aware variant (produced by [`Pat::when_match`]).
pub(crate) type MatchPredicateFn = Arc<
    dyn Fn(&BuiltFunctionGraph, NodeOutputType, &crate::matcher::Bindings) -> bool + Send + Sync,
>;

// ── Const-capture overloading traits ──────────────────────────────────────────
//
// Three near-identical pairs (Capture/typed `*Capture`) — the macro below
// expands each to the ~30-line pattern that was open-coded before.
// `Capture` impls match "any foo-const node" and bind the matched
// output; typed-Capture impls additionally bind the concrete value and (in
// build position) emit a fresh const node from the captured value.

macro_rules! decl_any_const {
    (
        trait $trait:ident, sealed $sealed:ident, method $method:ident,
        variant $variant:ident, $sample:expr, $build_ty:expr,
        typed $typed:ty, $bind:ident, $get:ident, $missing:literal
    ) => {
        #[doc = concat!(
            "Sealed trait used by `any_",
            stringify!($variant),
            "` to accept either a [`Capture`] (binds the matched node + output) or a typed capture that binds the concrete constant value."
        )]
        pub trait $trait: sealed::$sealed {
            #[doc(hidden)]
            fn $method(self) -> Pat;
        }

        impl $trait for Capture {
            fn $method(self) -> Pat {
                crate::pat::node_pat::NodePat::matcher(
                    crate::pat::node_pat::KindSpec::variant(&NodeKind::$variant($sample)),
                    crate::pat::node_pat::InputsSpec::None,
                )
                .into_pat()
                .capture(self)
            }
        }

        impl $trait for $typed {
            fn $method(self) -> Pat {
                let tv = self;
                crate::pat::node_pat::NodePat::matcher(
                    crate::pat::node_pat::KindSpec::variant(&NodeKind::$variant($sample)),
                    crate::pat::node_pat::InputsSpec::None,
                )
                // The `_` arm is defensive — the kind spec normally
                // restricts to this variant, but we don't depend on
                // that.  Binding happens in `post_match` per the
                // NodePat kind-purity rule.
                .with_post_match(Arc::new(move |ctx, node, b| {
                    match ctx.graph.graph.node_kind(node) {
                        NodeKind::$variant(v) => b.$bind(tv, *v),
                        _ => false,
                    }
                }))
                .with_build_fn(
                    Arc::new(move |ctx| {
                        let v = ctx
                            .bindings
                            .$get(tv)
                            .ok_or_else(|| crate::error::missing_binding($missing))?;
                        Ok(NodeKind::$variant(v))
                    }),
                    $build_ty,
                )
                .into_pat()
            }
        }
    };
}

decl_any_const!(
    trait IntoAnyIntConst, sealed SealedAnyIntConst, method into_any_int_const_pat,
    variant IntConst, 0u128, crate::pat::node_pat::BuildTy::InheritRoot,
    typed IntVar, bind_int, get_int, "IntVar"
);
decl_any_const!(
    trait IntoAnyBoolConst, sealed SealedAnyBoolConst, method into_any_bool_const_pat,
    variant BoolConst, false, crate::pat::node_pat::BuildTy::Fixed(NodeOutputType::Bool),
    typed BoolVar, bind_bool, get_bool, "BoolVar"
);
decl_any_const!(
    trait IntoAnyFloatConst, sealed SealedAnyFloatConst, method into_any_float_const_pat,
    variant FloatConst, 0u64, crate::pat::node_pat::BuildTy::InheritRoot,
    typed FloatVar, bind_float, get_float_bits, "FloatVar"
);

mod sealed {
    use crate::var::{BoolVar, Capture, FloatVar, IntVar};

    pub trait SealedAnyIntConst {}
    impl SealedAnyIntConst for Capture {}
    impl SealedAnyIntConst for IntVar {}

    pub trait SealedAnyBoolConst {}
    impl SealedAnyBoolConst for Capture {}
    impl SealedAnyBoolConst for BoolVar {}

    pub trait SealedAnyFloatConst {}
    impl SealedAnyFloatConst for Capture {}
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
        Pat::from_dyn(Arc::new(crate::pat::guards::GuardPat {
            inner: self,
            func: crate::pat::guards::GuardFn::Bindings(Arc::new(f)),
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
    /// is bound and [`crate::Match::output`] returns `None`.
    fn capture(self, c: Capture) -> Pat {
        let inner: Pat = self.into();
        Pat::from_dyn(Arc::new(crate::pat::any::CapturePat {
            inner,
            capture: c,
        }))
    }
    /// After matching, additionally run `f` — fails if it returns `false`.
    fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        let inner: Pat = self.into();
        Pat::from_dyn(Arc::new(crate::pat::guards::GuardPat {
            inner,
            func: crate::pat::guards::GuardFn::Output(Arc::new(f)),
        }))
    }
}

impl<T: Into<Pat>> IntoPat for T {}
