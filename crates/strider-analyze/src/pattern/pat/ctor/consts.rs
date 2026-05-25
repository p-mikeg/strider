//! Build-only constant constructors ([`int_const_with_fn`],
//! [`bool_const_with_fn`], [`float_const_with_fn`]) plus the [`first_value_input_type`]
//! helper the `*_const_with!` macros expand against.
//!
//! These constructors are match-only-false (their `post_match` always
//! returns `false`), so accidentally pasting one on the LHS of a rule
//! causes a silent no-match rather than a panic.  Their purpose is to
//! materialize an `IntConst` / `BoolConst` / `FloatConst` node whose
//! value is computed from captured bindings at build time.

use std::sync::Arc;

use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

use crate::pattern::error::Result;
use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodeKindCheck, NodePat};
use crate::pattern::pat::traits::BuildCtx;

/// Match-only-false post_match shared by every `*_const_with_fn` — these
/// patterns are build-only; landing one on an LHS is a silent no-match
/// rather than a panic.
fn never_match() -> NodeKindCheck {
    Arc::new(|_ctx, _node, _b| false)
}

/// Type alias for the closure stored by `int_const_with_fn` /
/// `bool_const_with_fn` / `float_const_with_fn`.
pub(crate) type BuildValueFn<T> = Arc<dyn Fn(&BuildCtx<'_>) -> Result<T> + Send + Sync>;

/// Returns the [`NodeOutputType`] of the matched root's first value input,
/// or `None` if the root has no inputs or its first input isn't a value
/// edge.  Used by the `*_const_with!` macros to expose the magic `in_ty`
/// identifier — for `IntCmp(lhs, rhs)` rules where the comparison's input
/// type (needed for signed / carry handling) differs from the root's
/// output type (always `Bool`).
#[must_use]
pub(crate) fn first_value_input_type(ctx: &BuildCtx<'_>) -> Option<NodeOutputType> {
    let inputs = ctx.graph.node_inputs(ctx.root);
    let inp = inputs.into_iter().next()?;
    match ctx.graph.output_kind(inp) {
        NodeOutputKind::OutputType(t) => Some(t),
        _ => None,
    }
}

/// Builds an `IntConst` node whose value is computed by `f` at build time.
pub(crate) fn int_const_with_fn<F>(f: F) -> Pat
where
    F: Fn(&BuildCtx<'_>) -> Result<u128> + Send + Sync + 'static,
{
    let f: BuildValueFn<u128> = Arc::new(f);
    NodePat::matcher(KindSpec::Any, InputsSpec::None)
        .with_post_match(never_match())
        .with_build_fn(
            Arc::new(move |ctx| Ok(NodeKind::IntConst(f(ctx)?))),
            BuildTy::InheritRoot,
        )
        .into_pat()
}

/// Builds a `BoolConst` node whose value is computed by `f` at build time.
pub(crate) fn bool_const_with_fn<F>(f: F) -> Pat
where
    F: Fn(&BuildCtx<'_>) -> Result<bool> + Send + Sync + 'static,
{
    let f: BuildValueFn<bool> = Arc::new(f);
    NodePat::matcher(KindSpec::Any, InputsSpec::None)
        .with_post_match(never_match())
        .with_build_fn(
            Arc::new(move |ctx| Ok(NodeKind::BoolConst(f(ctx)?))),
            BuildTy::Fixed(NodeOutputType::Bool),
        )
        .into_pat()
}

/// Builds a `FloatConst` node whose IEEE 754 bit pattern is computed by
/// `f` at build time.
pub(crate) fn float_const_with_fn<F>(f: F) -> Pat
where
    F: Fn(&BuildCtx<'_>) -> Result<u64> + Send + Sync + 'static,
{
    let f: BuildValueFn<u64> = Arc::new(f);
    NodePat::matcher(KindSpec::Any, InputsSpec::None)
        .with_post_match(never_match())
        .with_build_fn(
            Arc::new(move |ctx| Ok(NodeKind::FloatConst(f(ctx)?))),
            BuildTy::InheritRoot,
        )
        .into_pat()
}
