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
    BoolBinaryOpPat, CallOtherPat, CallPat, CaptureBuilder, FloatBinaryOpPat, FunctionArgPat,
    IfPat, IntBinaryOpPat, LoadPat, PhiPat, RetPat, StackStorePat, StackStorePhiPat, StorePat,
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
//
// Three near-identical pairs (Int/Bool/Float × `Var` / typed `*Var`) — the
// macro below expands each to the ~30-line pattern that was open-coded
// before.  `Var` impls match "any foo-const node" and bind the matched
// output; typed-Var impls additionally bind the concrete value and (in build
// position) emit a fresh const node from the captured value.

macro_rules! decl_any_const {
    (
        trait $trait:ident, sealed $sealed:ident, method $method:ident,
        variant $variant:ident, $sample:expr, $build_ty:expr,
        typed $typed:ty, $bind:ident, $get:ident, $missing:literal
    ) => {
        #[doc = concat!(
            "Sealed trait used by `any_",
            stringify!($variant),
            "` to accept either a [`Var`] (binds the matched `NodeOutputId`) or a typed capture that binds the concrete constant value."
        )]
        pub trait $trait: sealed::$sealed {
            #[doc(hidden)]
            fn $method(self) -> Pat;
        }

        impl $trait for Var {
            fn $method(self) -> Pat {
                crate::pat::node_pat::NodePat::matcher(
                    crate::pat::node_pat::KindSpec::variant(&NodeKind::$variant($sample)),
                    crate::pat::node_pat::InputsSpec::None,
                )
                .with_output_var(Some(self))
                .into_pat()
            }
        }

        impl $trait for $typed {
            fn $method(self) -> Pat {
                let tv = self;
                crate::pat::node_pat::NodePat::matcher(
                    crate::pat::node_pat::KindSpec::variant(&NodeKind::$variant($sample)),
                    crate::pat::node_pat::InputsSpec::None,
                )
                // Kind spec already enforces the variant, so the match arm
                // below is unreachable in the `_` case — but keeping it
                // lets the closure body stay a single `match`.  Binding
                // happens in `post_match` per the NodePat kind-purity rule.
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
                            .ok_or(crate::error::ErrorKind::MissingBinding($missing))?;
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
    variant IntConst, 0u64, crate::pat::node_pat::BuildTy::InheritRoot,
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

