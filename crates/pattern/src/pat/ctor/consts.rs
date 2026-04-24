//! Build-only constant constructors ([`int_const_with_fn`],
//! [`bool_const_with_fn`], [`float_const_with_fn`]) plus the [`FromCtx`]
//! trait and [`first_value_input_type`] helper the `*_const_with!`
//! macros expand against.
//!
//! These constructors are match-only-false ([`NodePat::kind_match`]
//! returns `false`), so accidentally pasting one on the LHS of a rule
//! causes a silent no-match rather than a panic.  Their purpose is to
//! materialize an `IntConst` / `BoolConst` / `FloatConst` node whose
//! value is computed from captured bindings at build time.

use std::sync::Arc;

use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

use crate::error::{ErrorKind, Result};
use crate::pat::Pat;
use crate::pat::node_pat::{BuildTy, InputsSpec, KindFilter, NodePat};
use crate::pat::traits::BuildCtx;
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar,
};

/// Type alias for the closure stored by `int_const_with_fn` /
/// `bool_const_with_fn` / `float_const_with_fn`.
pub type BuildValueFn<T> = Arc<dyn Fn(&BuildCtx<'_>) -> Result<T> + Send + Sync>;

/// Returns the [`NodeOutputType`] of the matched root's first value input,
/// or `None` if the root has no inputs or its first input isn't a value
/// edge.  Used by the `*_const_with!` macros to expose the magic `in_ty`
/// identifier — for `IntCmp(lhs, rhs)` rules where the comparison's input
/// type (needed for signed / carry handling) differs from the root's
/// output type (always `Bool`).
pub fn first_value_input_type(ctx: &BuildCtx<'_>) -> Option<NodeOutputType> {
    let inputs = ctx.graph.graph.node_inputs(ctx.root);
    let inp = inputs.into_iter().next()?;
    match ctx.graph.graph.output_kind(inp) {
        NodeOutputKind::OutputType(t) => Some(t),
        _ => None,
    }
}

/// Match-only-false kind_match shared by every `*_const_with_fn` — these
/// patterns are build-only; landing one on an LHS is a silent no-match
/// rather than a panic.
fn never_match_kind() -> crate::pat::node_pat::NodeKindCheck {
    Arc::new(|_ctx, _node, _b| false)
}

/// Builds an `IntConst` node whose value is computed by `f` at build time.
pub fn int_const_with_fn<F>(f: F) -> Pat
where
    F: Fn(&BuildCtx<'_>) -> Result<u64> + Send + Sync + 'static,
{
    let f: BuildValueFn<u64> = Arc::new(f);
    NodePat::matcher(KindFilter::Any, never_match_kind(), InputsSpec::None)
        .with_build(Arc::new(move |ctx| Ok(NodeKind::IntConst(f(ctx)?))))
        .into_pat()
}

/// Builds a `BoolConst` node whose value is computed by `f` at build time.
pub fn bool_const_with_fn<F>(f: F) -> Pat
where
    F: Fn(&BuildCtx<'_>) -> Result<bool> + Send + Sync + 'static,
{
    let f: BuildValueFn<bool> = Arc::new(f);
    NodePat::matcher(KindFilter::Any, never_match_kind(), InputsSpec::None)
        .with_build(Arc::new(move |ctx| Ok(NodeKind::BoolConst(f(ctx)?))))
        .with_build_ty(BuildTy::Fixed(NodeOutputType::Bool))
        .into_pat()
}

/// Builds a `FloatConst` node whose IEEE 754 bit pattern is computed by
/// `f` at build time.
pub fn float_const_with_fn<F>(f: F) -> Pat
where
    F: Fn(&BuildCtx<'_>) -> Result<u64> + Send + Sync + 'static,
{
    let f: BuildValueFn<u64> = Arc::new(f);
    NodePat::matcher(KindFilter::Any, never_match_kind(), InputsSpec::None)
        .with_build(Arc::new(move |ctx| Ok(NodeKind::FloatConst(f(ctx)?))))
        .into_pat()
}

/// Extract a typed value from a [`BuildCtx`] given a capture variable.
///
/// Used by the `*_const_with!` macros to resolve each capture identifier
/// into its concrete value.  Implemented for every capture-var type
/// ([`Var`](crate::var::Var) binding is not useful for const-with — it
/// yields a `NodeOutputId`, not a value — so that impl is intentionally
/// omitted).
#[allow(clippy::wrong_self_convention)]
pub trait FromCtx {
    type Output;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output>;
}

macro_rules! impl_from_ctx {
    ($ty:ty, $out:ty, $getter:ident, $kind_name:literal) => {
        impl FromCtx for $ty {
            type Output = $out;
            fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
                ctx.bindings
                    .$getter(*self)
                    .ok_or_else(|| ErrorKind::MissingBinding($kind_name).into())
            }
        }
    };
}

impl_from_ctx!(IntVar, u64, get_int, "IntVar");
impl_from_ctx!(BoolVar, bool, get_bool, "BoolVar");
impl_from_ctx!(FloatVar, u64, get_float_bits, "FloatVar");
impl_from_ctx!(
    IntBinaryOpVar,
    ir::IntBinaryOp,
    get_int_binary_op,
    "IntBinaryOpVar"
);
impl_from_ctx!(
    IntUnaryOpVar,
    ir::IntUnaryOp,
    get_int_unary_op,
    "IntUnaryOpVar"
);
impl_from_ctx!(IntCmpOpVar, ir::IntCmpOp, get_int_cmp_op, "IntCmpOpVar");
impl_from_ctx!(
    BoolBinaryOpVar,
    ir::BoolBinaryOp,
    get_bool_binary_op,
    "BoolBinaryOpVar"
);
impl_from_ctx!(
    BoolUnaryOpVar,
    ir::BoolUnaryOp,
    get_bool_unary_op,
    "BoolUnaryOpVar"
);
impl_from_ctx!(
    FloatBinaryOpVar,
    ir::FloatBinaryOp,
    get_float_binary_op,
    "FloatBinaryOpVar"
);
impl_from_ctx!(
    FloatUnaryOpVar,
    ir::FloatUnaryOp,
    get_float_unary_op,
    "FloatUnaryOpVar"
);
impl_from_ctx!(
    FloatCmpOpVar,
    ir::FloatCmpOp,
    get_float_cmp_op,
    "FloatCmpOpVar"
);
