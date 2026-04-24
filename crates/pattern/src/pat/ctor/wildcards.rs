//! Wildcard, capture, and constant-literal pattern constructors.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeKind, NodeOutputId, NodeOutputType};

use crate::pat::any::{AnyPat, VarPat};
use crate::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};
use crate::pat::{IntoAnyBoolConst, IntoAnyFloatConst, IntoAnyIntConst, IntoPat, Pat};
use crate::var::Var;

/// Matches any single output unconditionally.
#[must_use]
pub fn any() -> Pat {
    Pat::from_dyn(Arc::new(AnyPat))
}

/// Matches any output and binds it to `v`.
///
/// If `v` is already bound the output must equal the stored binding.
/// Equivalent in behavior to `any().capture(v)`, but constructs a dedicated
/// [`VarPat`] rather than wrapping [`AnyPat`] in a [`CapturePat`] — one
/// fewer vtable hop and no backtracking snapshot per match.
#[must_use]
pub fn var(v: Var) -> Pat {
    Pat::from_dyn(Arc::new(VarPat { var: v }))
}

/// Matches an `IntConst` node with value exactly `v`.
///
/// In build position (RHS of a rewrite rule), constructs an `IntConst(v)`
/// node at the root type.
#[must_use]
pub fn int_const(v: u64) -> Pat {
    NodePat::matcher(KindSpec::Exact(NodeKind::IntConst(v)), InputsSpec::None)
        .with_build_exact(NodeKind::IntConst(v), BuildTy::InheritRoot)
        .into_pat()
}

/// Matches a `BoolConst` node with value exactly `v`.
#[must_use]
pub fn bool_const(v: bool) -> Pat {
    NodePat::matcher(KindSpec::Exact(NodeKind::BoolConst(v)), InputsSpec::None)
        .with_build_exact(NodeKind::BoolConst(v), BuildTy::Fixed(NodeOutputType::Bool))
        .into_pat()
}

/// Matches a `FloatConst` node with the exact bit pattern `bits`.
#[must_use]
pub fn float_const(bits: u64) -> Pat {
    NodePat::matcher(KindSpec::Exact(NodeKind::FloatConst(bits)), InputsSpec::None)
        .with_build_exact(NodeKind::FloatConst(bits), BuildTy::InheritRoot)
        .into_pat()
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
