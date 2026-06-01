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

/// Match an `Extend(op)` node — variant-agnostic taking a runtime
/// `ExtendOp`.  The Rust builder pins the exact extension kind so the
/// pat node matches `Extend(ZeroExtend)` distinctly from
/// `Extend(SignExtend)`; use `zero_extend` / `sign_extend` for the
/// per-variant aliases.
#[must_use]
pub fn extend<R: Role>(op: ExtendOp, inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::Extend(op), inner)
}

/// Emit a unit-variant cast builder: `pub fn $name<R>(inner) -> Pat<R>`
/// expanding to `unary_node_pat($kind, inner)`.  `$kind` is a full
/// `NodeKind` expression so the `Extend(ExtendOp::*)` aliases can share
/// the macro with the unit-variant cast kinds.
macro_rules! unary_cast {
    ($name:ident, $kind:expr, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<R: Role>(inner: Pat<R>) -> Pat<R> {
            unary_node_pat($kind, inner)
        }
    };
}

unary_cast!(
    truncate,
    NodeKind::Truncate,
    "Match a `Truncate(inner)` node (narrows an integer to fewer bits)."
);
unary_cast!(
    zero_extend,
    NodeKind::Extend(ExtendOp::ZeroExtend),
    "Match a zero-extension node (`Extend(ZeroExtend)(inner)`)."
);
unary_cast!(
    sign_extend,
    NodeKind::Extend(ExtendOp::SignExtend),
    "Match a sign-extension node (`Extend(SignExtend)(inner)`)."
);
unary_cast!(
    int_to_float,
    NodeKind::IntToFloat,
    "Match an `IntToFloat(inner)` value-conversion (integer → float; `(float)n`)."
);
unary_cast!(
    float_to_int,
    NodeKind::FloatToInt,
    "Match a `FloatToInt(inner)` value-conversion (float → integer truncated toward zero; `(int)f`)."
);
unary_cast!(
    int_bits_to_float,
    NodeKind::IntBitsToFloat,
    "Match an `IntBitsToFloat(inner)` bitcast (same-width reinterpret of an integer's bit pattern as a float)."
);
unary_cast!(
    float_bits_to_int,
    NodeKind::FloatBitsToInt,
    "Match a `FloatBitsToInt(inner)` bitcast (same-width reinterpret of a float's bit pattern as an integer)."
);
unary_cast!(
    float_to_float,
    NodeKind::FloatToFloat,
    "Match a `FloatToFloat(inner)` precision-conversion (F32 ↔ F64)."
);
