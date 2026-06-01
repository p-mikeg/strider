//! Integer comparison chained builders.
//!
//! Each comparison produces an `I1` (1-bit) output — booleans are
//! 1-bit integers in this IR.  The `TemplateSpec::ty` is therefore pinned
//! to `Fixed(I1)` (mirrors `strider-analyze::pattern::pat::builders::
//! cmp_op::cmp_pat`'s `TemplateTy::Fixed(I1)`), and the pat node's
//! `output_ty` is set so the matcher's forthcoming output-type guard
//! filters away same-shape wide ops.
//!
//! Lift-time canonicalisations covered:
//!
//! - `int_ne(a, b)` → `xor(int_eq(a, b), int_const(1)):I1`.
//! - `int_le(a, b)` → `xor(int_lt(b, a), int_const(1)):I1` (operand swap +
//!   logical NOT — sound for unsigned because `Less` is total over
//!   the unsigned range).
//! - `int_sle(a, b)` → `xor(int_slt(b, a), int_const(1)):I1` (same shape
//!   for the signed comparison).
//!
//! Each canonicalisation expands through the existing `xor` /
//! `int_const` builders so the resulting `PatGraph` is structurally
//! identical to what pcode-lift produces.  The canonicalised builders
//! return `Pat<Wildcard>` — the resulting graph contains an injected
//! `int_const(1)` node (`Concrete`), but the matcher-side role doesn't
//! benefit from threading the original input roles through, and
//! the Rust type system can't reduce `<R ⊕ Concrete>` to `R` without
//! extra trait machinery.  Widening to `Wildcard` keeps the surface
//! tractable; build-side use of `int_le` / `int_sle` / `int_ne` on a
//! rewrite RHS is not a supported workflow today (the canonical RHS
//! always uses the lowered shape directly).

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::IntCmpOp;

use crate::pat_graph::{
    Combine, KindSpec, Role, TemplateKind, TemplateSpec, TemplateTy, Wildcard,
};

use super::consts::int_const;
use super::int_ops::xor;
use super::shared::binary_pat;
use super::Pat;

/// Variant-agnostic typed dispatcher: takes any `IntCmpOp`.
///
/// Commutativity is decided per-IR-node by the matcher
/// (`NodeKind::is_commutative()` is the single source of truth), so
/// `int_cmp(IntCmpOp::Equal, a, b)` automatically tries both operand
/// orderings; the directional variants (`Less`, `Sless`, `Sborrow`)
/// don't.
#[must_use]
pub fn int_cmp<R1, R2>(
    op: IntCmpOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let kind = NodeKind::IntCmpOp(op);
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

/// Match **any** `IntCmpOp` regardless of variant.  Wildcard role;
/// useful for generic queries that recover the op via a separate
/// inspection pass.  No payload predicate — kinds are dispatched by
/// `KindSpec::Variant` on the `IntCmpOp` discriminant.
#[must_use]
pub fn int_cmp_any<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let exemplar = NodeKind::IntCmpOp(IntCmpOp::Equal);
    binary_pat(
        KindSpec::Variant(std::mem::discriminant(&exemplar)),
        Some(NodeOutputType::I1),
        None,
        lhs.into_wildcard(),
        rhs.into_wildcard(),
    )
}

/// Emit a typed `IntCmpOp` dispatcher: `pub fn $name<R1, R2>(lhs, rhs)
/// -> Pat<<R1 ⊕ R2>::Output>` calling `binary_pat` with
/// `KindSpec::Exact(NodeKind::IntCmpOp($variant))` and a matching
/// `TemplateSpec` pinned to `I1`.
macro_rules! binary_int_cmp {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
        where
            R1: Combine<R2>,
            R2: Role,
        {
            int_cmp(IntCmpOp::$variant, lhs, rhs)
        }
    };
}

binary_int_cmp!(int_eq, Equal, "Match an unsigned equality comparison `lhs == rhs` (commutative).");
binary_int_cmp!(int_lt, Less, "Match an unsigned less-than comparison `lhs < rhs` (directional).");
binary_int_cmp!(int_slt, Sless, "Match a signed less-than comparison `(signed)lhs < (signed)rhs` (directional).");
binary_int_cmp!(int_carry, Carry, "Match an unsigned addition carry-out (commutative).");
binary_int_cmp!(int_scarry, Scarry, "Match a signed addition overflow check (commutative).");
binary_int_cmp!(int_sborrow, Sborrow, "Match a signed subtraction borrow check (directional).");

/// Match an unsigned not-equal comparison `lhs != rhs`.
///
/// `IntCmpOp::NotEqual` is not a primitive in this IR; pcode-lift lowers
/// it to `Xor(IntEqual(a, b), IntConst(1)):I1`.  This builder produces
/// the lowered shape directly.  Returns `Pat<Wildcard>` — the injected
/// `int_const(1)` makes role-preservation impractical without extra
/// trait machinery, so the canonicalised builders settle on `Wildcard`.
#[must_use]
pub fn int_ne<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    // Widen child roles to Wildcard up front so the `Combine` bound
    // collapses to `Wildcard ⊕ Wildcard = Wildcard`.
    let lhs_w: Pat<Wildcard> = lhs.into_wildcard();
    let rhs_w: Pat<Wildcard> = rhs.into_wildcard();
    let inner: Pat<Wildcard> = int_cmp(IntCmpOp::Equal, lhs_w, rhs_w);
    let one_wild: Pat<Wildcard> = int_const(1).into_wildcard();
    xor(inner, one_wild)
}

/// Match an unsigned less-or-equal comparison `lhs <= rhs`.
///
/// `IntCmpOp::LessEqual` is not a primitive in this IR; pcode-lift
/// lowers it to `Xor(IntLess(b, a), IntConst(1)):I1` (operand swap +
/// logical NOT).
#[must_use]
pub fn int_le<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let lhs_w: Pat<Wildcard> = lhs.into_wildcard();
    let rhs_w: Pat<Wildcard> = rhs.into_wildcard();
    let inner: Pat<Wildcard> = int_cmp(IntCmpOp::Less, rhs_w, lhs_w);
    let one_wild: Pat<Wildcard> = int_const(1).into_wildcard();
    xor(inner, one_wild)
}

/// Match a signed less-or-equal comparison `(signed)lhs <= (signed)rhs`.
///
/// `IntCmpOp::SlessEqual` is not a primitive in this IR; pcode-lift
/// lowers it to `Xor(IntSless(b, a), IntConst(1)):I1` (same shape as
/// the unsigned `int_le` with `Sless` substituted for `Less`).
#[must_use]
pub fn int_sle<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let lhs_w: Pat<Wildcard> = lhs.into_wildcard();
    let rhs_w: Pat<Wildcard> = rhs.into_wildcard();
    let inner: Pat<Wildcard> = int_cmp(IntCmpOp::Sless, rhs_w, lhs_w);
    let one_wild: Pat<Wildcard> = int_const(1).into_wildcard();
    xor(inner, one_wild)
}
