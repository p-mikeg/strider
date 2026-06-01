//! Boolean binary / unary chained builders.
//!
//! Booleans are 1-bit integers (`I1`) in this IR: there is no separate
//! `BoolBinaryOp` / `BoolUnaryOp` node kind.  A boolean AND / OR / XOR
//! is an `IntBinaryOp` (`And` / `Or` / `Xor`) whose output is `I1`,
//! and a logical NOT is `Xor(x, IntConst(1)):I1` (since `~x ≡ x ^ all_ones`
//! and the all-ones constant at `I1` is `IntConst(1)`).
//!
//! Each builder records `output_ty: Some(I1)` on its parent pat node
//! and `TemplateSpec::ty = Fixed(I1)` so the matcher's forthcoming
//! output-type guard rejects same-shaped wide integer ops (e.g. a
//! 64-bit `And`) that share the same `NodeKind`.

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::IntBinaryOp;

use crate::pat_graph::{
    Combine, Concrete, KindSpec, Role, TemplateKind, TemplateSpec, TemplateTy, Wildcard,
};

use super::consts::int_const;
use super::shared::binary_pat;
use super::Pat;

/// Variant-agnostic typed dispatcher: takes any `IntBinaryOp` to be
/// matched at `I1`.  Use `And` / `Or` / `Xor` for the canonical
/// boolean operations.
#[must_use]
pub fn bool_binary<R1, R2>(
    op: IntBinaryOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let kind = NodeKind::IntBinaryOp(op);
    binary_pat(
        KindSpec::Exact(kind),
        Some(NodeOutputType::I1),
        Some(TemplateSpec {
            kind: TemplateKind::Exact(kind),
            ty: TemplateTy::Fixed(NodeOutputType::I1),
        }),
        lhs,
        rhs,
    )
}

/// Match **any** `IntBinaryOp` whose output is `I1` regardless of
/// variant.  Mirrors `strider-analyze::pattern::pat::ctor::
/// variant_agnostic::bool_binary_any` — the matcher's eventual
/// output-type guard filters wide integer ops away from this pattern.
/// Wildcard role.
#[must_use]
pub fn bool_binary_any<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let exemplar = NodeKind::IntBinaryOp(IntBinaryOp::And);
    binary_pat(
        KindSpec::Variant(std::mem::discriminant(&exemplar)),
        Some(NodeOutputType::I1),
        None,
        lhs.into_wildcard(),
        rhs.into_wildcard(),
    )
}

/// Emit a typed `IntBinaryOp` dispatcher at `I1`: `pub fn $name<R1, R2>(lhs, rhs)
/// -> Pat<<R1 ⊕ R2>::Output>` calling `bool_binary(IntBinaryOp::$variant, …)`.
macro_rules! bool_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
        where
            R1: Combine<R2>,
            R2: Role,
        {
            bool_binary(IntBinaryOp::$variant, lhs, rhs)
        }
    };
}

bool_op!(bool_and, And, "Match a boolean AND node (`IntBinaryOp::And` at `I1`).  Commutative.");
bool_op!(bool_or, Or, "Match a boolean OR node (`IntBinaryOp::Or` at `I1`).  Commutative.");
bool_op!(bool_xor, Xor, "Match a boolean XOR node (`IntBinaryOp::Xor` at `I1`).  Commutative.");

/// Match a boolean NOT node — `Xor(operand, IntConst(1)):I1` (logical
/// NOT of an `I1` value).  Role propagates from `operand`: since
/// `int_const(1)` is `Pat<Concrete>`, the role widening rule
/// ([`Combine`]) only widens when `operand` itself is `Wildcard`.
/// This keeps `bool_not(var(x))` (where `var(x)` is `Pat<Concrete>`)
/// usable on the RHS of a rewrite rule.
#[must_use]
pub fn bool_not<R>(operand: Pat<R>) -> Pat<R>
where
    R: Combine<Concrete, Output = R>,
{
    bool_binary(IntBinaryOp::Xor, operand, int_const(1))
}
