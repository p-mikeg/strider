//! Wildcard, capture, and constant-literal pattern constructors.

use ir::BuiltFunctionGraph;
use ir::node::{NodeOutputId, NodeOutputType};

use crate::pat::{IntoAnyBoolConst, IntoAnyFloatConst, IntoAnyIntConst, IntoPat, Pat, PatKind};
use crate::var::Var;

/// Matches any single output unconditionally.
pub fn any() -> Pat {
    Pat::new(PatKind::Any)
}

/// Matches any output and binds it to `v`.
///
/// If `v` is already bound the output must equal the stored binding.
/// Shorthand for `any().capture(v)`.
pub fn var(v: Var) -> Pat {
    Pat::new(PatKind::Capture(v))
}

/// Matches an `IntConst` node with value exactly `v`.
pub fn int_const(v: u64) -> Pat {
    Pat::new(PatKind::IntConst(v))
}

/// Matches a `BoolConst` node with value exactly `v`.
pub fn bool_const(v: bool) -> Pat {
    Pat::new(PatKind::BoolConst(v))
}

/// Matches a `FloatConst` node with the exact bit pattern `bits`.
pub fn float_const(bits: u64) -> Pat {
    Pat::new(PatKind::FloatConst(bits))
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
