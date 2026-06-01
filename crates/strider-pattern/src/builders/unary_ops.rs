//! Integer unary-op chained builders.
//!
//! Covers `neg` (the lone `IntUnaryOp` variant) plus the unit-variant
//! integer unary kinds (`Lzcount`, `Popcount`).  The float / boolean
//! unary builders live in their own modules; cast unary builders
//! (`truncate`, `extend`, …) live in `casts.rs`.  All of them invoke
//! the shared [`unary_pat`](crate::builders::shared::unary_pat) helper
//! directly with `KindSpec::Exact + Some(TemplateSpec::Exact)`.

use strider_ir::node::NodeKind;
use strider_ir::IntUnaryOp;

use crate::pat_graph::{KindSpec, Role, TemplateKind, TemplateSpec, TemplateTy, Wildcard};

use super::shared::unary_pat;
use super::Pat;

/// Variant-agnostic dispatcher: takes any `IntUnaryOp`.  Role
/// propagates unchanged.
#[must_use]
pub fn int_unary<R: Role>(op: IntUnaryOp, inner: Pat<R>) -> Pat<R> {
    let kind = NodeKind::IntUnaryOp(op);
    unary_pat(
        KindSpec::Exact(kind),
        None,
        Some(TemplateSpec {
            kind: TemplateKind::Exact(kind),
            ty: TemplateTy::InheritRoot,
        }),
        inner,
    )
}

/// Match **any** `IntUnaryOp` variant.  Wildcard role; kind dispatch
/// is by [`KindSpec::Variant`] on the `IntUnaryOp` discriminant —
/// payload is ignored.  No [`TemplateSpec`] so this is match-only.
/// Recover the matched variant via `Bindings::get_int_unary_op(c,
/// &graph)`.
#[must_use]
pub fn int_unary_any<R: Role>(inner: Pat<R>) -> Pat<Wildcard> {
    let exemplar = NodeKind::IntUnaryOp(IntUnaryOp::Neg);
    unary_pat(
        KindSpec::Variant(std::mem::discriminant(&exemplar)),
        None,
        None,
        inner.into_wildcard(),
    )
}

/// Emit a unit-variant unary builder: `pub fn $name<R>(inner) -> Pat<R>`
/// expanding to `unary_pat(KindSpec::Exact($kind), None,
/// Some(TemplateSpec { kind: TemplateKind::Exact($kind), ty:
/// InheritRoot }), inner)`.  `$kind` is a full `NodeKind` expression
/// shared between match-side and build-side stamping.
macro_rules! unary_exact_op {
    ($name:ident, $kind:expr, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<R: Role>(inner: Pat<R>) -> Pat<R> {
            let kind = $kind;
            unary_pat(
                KindSpec::Exact(kind),
                None,
                Some(TemplateSpec {
                    kind: TemplateKind::Exact(kind),
                    ty: TemplateTy::InheritRoot,
                }),
                inner,
            )
        }
    };
}

unary_exact_op!(neg, NodeKind::IntUnaryOp(IntUnaryOp::Neg), "Match `IntUnaryOp::Neg(inner)` — two's-complement negation `-inner`.  In build position (RHS of a rewrite rule), emits an `IntUnaryOp::Neg` whose output type inherits the rewrite root.");
unary_exact_op!(popcount, NodeKind::Popcount, "Match a `Popcount(inner)` node (count-set-bits).  `Popcount` is a unit-variant `NodeKind` (not wrapped in `IntUnaryOp`).  Role propagates from `inner` unchanged.");
unary_exact_op!(lzcount, NodeKind::Lzcount, "Match an `Lzcount(inner)` node (leading-zero-count).  `Lzcount` is a unit-variant `NodeKind` (not wrapped in `IntUnaryOp`).  Role propagates from `inner` unchanged.");
