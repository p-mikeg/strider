//! Wildcard, capture, and constant-literal pattern constructors.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeKind, NodeOutputId, NodeOutputType};

use crate::pat::any::AnyPat;
use crate::pat::node_pat::{InputsSpec, NodePat};
use crate::pat::{IntoAnyBoolConst, IntoAnyFloatConst, IntoAnyIntConst, IntoPat, Pat};
use crate::var::Var;

/// Matches any single output unconditionally.
pub fn any() -> Pat {
    Pat::from_dyn(Arc::new(AnyPat))
}

/// Matches any output and binds it to `v`.
///
/// If `v` is already bound the output must equal the stored binding.
/// Shorthand for `any().capture(v)`.
pub fn var(v: Var) -> Pat {
    any().capture(v)
}

/// Matches an `IntConst` node with value exactly `v`.
///
/// In build position (RHS of a rewrite rule), constructs an `IntConst(v)`
/// node at the root type.
pub fn int_const(v: u64) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |_b| Ok(NodeKind::IntConst(v)))),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::IntConst(c) if *c == v)
        }),
        inputs: InputsSpec::None,
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}

/// Matches a `BoolConst` node with value exactly `v`.
pub fn bool_const(v: bool) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |_b| Ok(NodeKind::BoolConst(v)))),
        build_result_ty: crate::pat::node_pat::BuildTy::Fixed(NodeOutputType::Bool),
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::BoolConst(c) if *c == v)
        }),
        inputs: InputsSpec::None,
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}

/// Matches a `FloatConst` node with the exact bit pattern `bits`.
pub fn float_const(bits: u64) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |_b| Ok(NodeKind::FloatConst(bits)))),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatConst(c) if *c == bits)
        }),
        inputs: InputsSpec::None,
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}

/// Matches any `IntConst` node and binds either the output (for a [`Var`]) or
/// the concrete constant value (for an [`crate::var::IntVar`]).
///
/// Fails if the producing node is not an `IntConst` — use this instead of
/// `var(v)` when you want the pattern itself to enforce the node is a
/// compile-time constant.
pub fn any_int_const<C: IntoAnyIntConst>(v: C) -> Pat {
    v.into_any_int_const_pat()
}

/// Matches any `BoolConst` node and binds its output (for a [`Var`]) or its
/// value (for a [`crate::var::BoolVar`]).
pub fn any_bool_const<C: IntoAnyBoolConst>(v: C) -> Pat {
    v.into_any_bool_const_pat()
}

/// Matches any `FloatConst` node and binds either the output (for a [`Var`])
/// or its IEEE 754 bit pattern (for a [`crate::var::FloatVar`]).
pub fn any_float_const<C: IntoAnyFloatConst>(v: C) -> Pat {
    v.into_any_float_const_pat()
}

/// Matches any output for which `f` returns `true`.  Equivalent to `any().when(f)`.
pub fn predicate<F>(f: F) -> Pat
where
    F: Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
{
    any().when(f)
}
