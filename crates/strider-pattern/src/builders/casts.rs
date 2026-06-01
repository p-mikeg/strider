//! Cast / coercion / bit-width-change pattern constructors.
//!
//! Each cast is a unary-shape `Pat<R>` that wraps an inner pat with a
//! concrete `NodeKind` (one of `Truncate`, `Extend(_)`, `IntToFloat`,
//! `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`).
//! Role propagates from the inner pat unchanged — these builders never
//! introduce a second sub-pattern, so the `<R ⊕ ?>` plumbing the binary
//! builders need doesn't apply here.
//!
//! All casts use `TemplateTy::InheritRoot` on their `TemplateSpec`, mirroring
//! the proven semantics in
//! `strider-analyze::pattern::pat::ctor::casts::unary_node`.

use strider_ir::node::NodeKind;
use strider_ir::ExtendOp;

use crate::pat_graph::Role;

use super::unary_ops::unary_node_pat;
use super::Pat;

/// Match a `Truncate(inner)` node (narrows an integer to fewer bits).
#[must_use]
pub fn truncate<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::Truncate, inner)
}

/// Match an `Extend(op)` node — variant-agnostic taking a runtime
/// `ExtendOp`.  The Rust builder pins the exact extension kind so the
/// pat node matches `Extend(ZeroExtend)` distinctly from
/// `Extend(SignExtend)`; use `zero_extend` / `sign_extend` for the
/// per-variant aliases.
#[must_use]
pub fn extend<R: Role>(op: ExtendOp, inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::Extend(op), inner)
}

/// Match a zero-extension node (`Extend(ZeroExtend)(inner)`).
#[must_use]
pub fn zero_extend<R: Role>(inner: Pat<R>) -> Pat<R> {
    extend(ExtendOp::ZeroExtend, inner)
}

/// Match a sign-extension node (`Extend(SignExtend)(inner)`).
#[must_use]
pub fn sign_extend<R: Role>(inner: Pat<R>) -> Pat<R> {
    extend(ExtendOp::SignExtend, inner)
}

/// Match an `IntToFloat(inner)` value-conversion (integer → float;
/// `(float)n`).
#[must_use]
pub fn int_to_float<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::IntToFloat, inner)
}

/// Match a `FloatToInt(inner)` value-conversion (float → integer
/// truncated toward zero; `(int)f`).
#[must_use]
pub fn float_to_int<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::FloatToInt, inner)
}

/// Match an `IntBitsToFloat(inner)` bitcast (same-width reinterpret of
/// an integer's bit pattern as a float).
#[must_use]
pub fn int_bits_to_float<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::IntBitsToFloat, inner)
}

/// Match a `FloatBitsToInt(inner)` bitcast (same-width reinterpret of
/// a float's bit pattern as an integer).
#[must_use]
pub fn float_bits_to_int<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::FloatBitsToInt, inner)
}

/// Match a `FloatToFloat(inner)` precision-conversion (F32 ↔ F64).
#[must_use]
pub fn float_to_float<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::FloatToFloat, inner)
}
