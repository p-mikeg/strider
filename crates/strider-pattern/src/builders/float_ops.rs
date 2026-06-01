//! Float binary / unary / comparison chained builders.
//!
//! Mirrors `strider-analyze::pattern::pat::ctor::float`.  Each builder
//! returns a `Pat<R>` (or `<R1 ⊕ R2>::Output` for binary shapes) and,
//! for comparisons, pins the output type to `I1`.
//!
//! Lift-time canonicalisations:
//!
//! - `float_sub(a, b)` → `float_add(a, float_neg(b))`.
//! - `float_ne(a, b)` → `xor(float_eq(a, b), int_const(1)):I1`.
//! - `float_le(a, b)` → `or(float_lt(a, b), float_eq(a, b))` at `I1`
//!   (NaN-aware; the source's comment notes IEEE 754 `<=` is false on
//!   NaN, so the `xor(less_swap, 1)` shortcut used for the integer
//!   `int_le` is unsound for floats).
//! - `float_is_nan(x)` → `xor(float_eq(x, x), int_const(1)):I1`.
//!
//! Each expansion uses existing helper builders (`xor`, `or`,
//! `int_const`) so the resulting `PatGraph` is structurally identical
//! to what pcode-lift produces.  Canonicalised builders return
//! `Pat<Wildcard>` for the same reason as `int_ne` / `int_le`: the
//! injected `int_const(1)` makes role-preservation impractical
//! without extra trait machinery.

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::pat_graph::{
    Combine, KindSpec, Role, TemplateKind, TemplateSpec, TemplateTy, Wildcard,
};

use super::consts::int_const;
use super::int_ops::{or, xor};
use super::shared::{binary_pat, unary_pat};
use super::Pat;

/// Variant-agnostic typed dispatcher: takes any `FloatBinaryOp`.
#[must_use]
pub fn float_binary<R1, R2>(
    op: FloatBinaryOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let kind = NodeKind::FloatBinaryOp(op);
    binary_pat(
        KindSpec::Exact(kind),
        None,
        Some(TemplateSpec {
            kind: TemplateKind::Exact(kind),
            ty: TemplateTy::InheritRoot,
        }),
        lhs,
        rhs,
    )
}

/// Match **any** `FloatBinaryOp` regardless of variant.  Wildcard role.
#[must_use]
pub fn float_binary_any<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let exemplar = NodeKind::FloatBinaryOp(FloatBinaryOp::Add);
    binary_pat(
        KindSpec::Variant(std::mem::discriminant(&exemplar)),
        None,
        None,
        lhs.into_wildcard(),
        rhs.into_wildcard(),
    )
}

/// Emit a typed `FloatBinaryOp` dispatcher: `pub fn $name<R1, R2>(lhs, rhs)
/// -> Pat<<R1 ⊕ R2>::Output>` calling `float_binary(FloatBinaryOp::$variant, …)`.
macro_rules! binary_float_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
        where
            R1: Combine<R2>,
            R2: Role,
        {
            float_binary(FloatBinaryOp::$variant, lhs, rhs)
        }
    };
}

binary_float_op!(float_add, Add, "Match a float addition node `lhs + rhs` (commutative under IEEE 754).");
binary_float_op!(float_mul, Mul, "Match a float multiplication node `lhs * rhs` (commutative).");
binary_float_op!(float_div, Div, "Match a float division node `lhs / rhs` (directional).");

/// Match a float subtraction `lhs - rhs`.
///
/// `FloatBinaryOp::Sub` is not a primitive; pcode-lift lowers
/// `FloatSub(a, b)` at lift time to `FloatAdd(a, FloatUnaryOp::Neg(b))`.
/// This builder produces the lowered shape directly.
#[must_use]
pub fn float_sub<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let neg_rhs: Pat<R2> = float_neg(rhs);
    float_add(lhs, neg_rhs)
}

/// Variant-agnostic typed dispatcher: takes any `FloatUnaryOp`.
#[must_use]
pub fn float_unary<R: Role>(op: FloatUnaryOp, inner: Pat<R>) -> Pat<R> {
    let kind = NodeKind::FloatUnaryOp(op);
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

/// Match **any** `FloatUnaryOp` regardless of variant.  Wildcard role.
#[must_use]
pub fn float_unary_any<R: Role>(inner: Pat<R>) -> Pat<Wildcard> {
    let exemplar = NodeKind::FloatUnaryOp(FloatUnaryOp::Neg);
    unary_pat(
        KindSpec::Variant(std::mem::discriminant(&exemplar)),
        None,
        None,
        inner.into_wildcard(),
    )
}

/// Emit a typed `FloatUnaryOp` dispatcher: `pub fn $name<R>(inner) -> Pat<R>`
/// calling `unary_pat` with `KindSpec::Exact(NodeKind::FloatUnaryOp(FloatUnaryOp::$variant))`
/// and a matching `TemplateSpec` (build-side replays the same kind, inheriting
/// the rewrite-root's output type).
macro_rules! unary_float_op {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<R: Role>(inner: Pat<R>) -> Pat<R> {
            let kind = NodeKind::FloatUnaryOp(FloatUnaryOp::$variant);
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

unary_float_op!(float_neg, Neg, "Match a float negation node `-x`.");
unary_float_op!(float_abs, Abs, "Match a float absolute-value node `|x|`.");
unary_float_op!(float_sqrt, Sqrt, "Match a float square-root node `√x`.");
unary_float_op!(float_ceil, Ceil, "Match a float ceiling node `⌈x⌉`.");
unary_float_op!(float_floor, Floor, "Match a float floor node `⌊x⌋`.");
unary_float_op!(float_round, Round, "Match a float round-to-nearest-even node `round(x)`.");

/// Variant-agnostic typed dispatcher: takes any `FloatCmpOp`.
#[must_use]
pub fn float_cmp<R1, R2>(
    op: FloatCmpOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let kind = NodeKind::FloatCmpOp(op);
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

/// Match **any** `FloatCmpOp` regardless of variant.  Wildcard role.
#[must_use]
pub fn float_cmp_any<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let exemplar = NodeKind::FloatCmpOp(FloatCmpOp::Equal);
    binary_pat(
        KindSpec::Variant(std::mem::discriminant(&exemplar)),
        Some(NodeOutputType::I1),
        None,
        lhs.into_wildcard(),
        rhs.into_wildcard(),
    )
}

/// Emit a typed `FloatCmpOp` dispatcher: `pub fn $name<R1, R2>(lhs, rhs)
/// -> Pat<<R1 ⊕ R2>::Output>` calling `float_cmp(FloatCmpOp::$variant, …)`.
macro_rules! binary_float_cmp {
    ($name:ident, $variant:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
        where
            R1: Combine<R2>,
            R2: Role,
        {
            float_cmp(FloatCmpOp::$variant, lhs, rhs)
        }
    };
}

binary_float_cmp!(float_eq, Equal, "Match a float equality comparison `lhs == rhs` (commutative).");
binary_float_cmp!(float_lt, Less, "Match a float less-than comparison `lhs < rhs` (directional).");

/// Match a float not-equal comparison `lhs != rhs`.
///
/// `FloatCmpOp::NotEqual` is not a primitive; pcode-lift lowers
/// `FloatNotEqual(a, b)` to `Xor(FloatEqual(a, b), IntConst(1)):I1`.
/// This builder produces the lowered shape.
#[must_use]
pub fn float_ne<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let lhs_w: Pat<Wildcard> = lhs.into_wildcard();
    let rhs_w: Pat<Wildcard> = rhs.into_wildcard();
    let inner: Pat<Wildcard> = float_cmp(FloatCmpOp::Equal, lhs_w, rhs_w);
    let one_wild: Pat<Wildcard> = int_const(1).into_wildcard();
    xor(inner, one_wild)
}

/// Match a float less-or-equal comparison `lhs <= rhs`.
///
/// `FloatCmpOp::LessEqual` is not a primitive; pcode-lift lowers
/// `FloatLessEqual(a, b)` to `Or(FloatLess(a, b), FloatEqual(a, b))` —
/// NaN-aware (the integer swap+xor trick would return true on NaN,
/// which IEEE 754 `<=` does not).
#[must_use]
pub fn float_le<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let lhs_w: Pat<Wildcard> = lhs.into_wildcard();
    let rhs_w: Pat<Wildcard> = rhs.into_wildcard();
    // The canonical shape uses each input twice (once in the `<` branch,
    // once in the `==` branch).  `Pat<R>` is move-only (closures inside
    // are `Box<dyn Fn>`), so we structurally clone the two sub-graphs
    // via the `clone_pat` helper below — see its doc for the caveats
    // around `VariantWith` predicates and `post_match` hooks.
    let lhs_a = clone_pat(&lhs_w);
    let rhs_a = clone_pat(&rhs_w);
    let less_branch: Pat<Wildcard> = float_cmp(FloatCmpOp::Less, lhs_w, rhs_w);
    let equal_branch: Pat<Wildcard> = float_cmp(FloatCmpOp::Equal, lhs_a, rhs_a);
    or(less_branch, equal_branch)
}

/// Match `float_is_nan(x)` — the canonical shape is
/// `xor(float_eq(x, x), int_const(1)):I1` (a value is NaN iff `x == x`
/// is false under IEEE 754).
#[must_use]
pub fn float_is_nan<R: Role>(x: Pat<R>) -> Pat<Wildcard> {
    let x_w: Pat<Wildcard> = x.into_wildcard();
    let x_w2: Pat<Wildcard> = clone_pat(&x_w);
    let eq: Pat<Wildcard> = float_cmp(FloatCmpOp::Equal, x_w, x_w2);
    let one_wild: Pat<Wildcard> = int_const(1).into_wildcard();
    xor(eq, one_wild)
}

/// Structurally clone a `Pat<Wildcard>` for the small set of builders
/// (`float_le`, `float_is_nan`) that need to reference the same input
/// twice.  `Pat<R>` is move-only (closures inside `NodeData` are
/// `Box<dyn Fn>`); this helper does a node-by-node structural copy
/// that **drops** closure payloads:
///
/// * `KindSpec::VariantWith { check, .. }` → `KindSpec::Variant(_)` —
///   the cloned node accepts every payload of the same discriminant
///   instead of the predicate-narrowed set.
/// * `template_spec` with a `TemplateKind::Fn` closure → dropped.
/// * `post_match` hook → dropped.
///
/// Acceptable for `float_le` / `float_is_nan`: their operands are
/// typically `var(Capture)` / `any_()` shapes that carry no closures,
/// so the structural clone is lossless in practice.  A future Rust-
/// surface API that needs the same input twice should accept a
/// builder thunk (`impl Fn() -> Pat<R>`) instead of a `&Pat<R>` to
/// rebuild the sub-pattern with all closures intact.
fn clone_pat(src: &Pat<Wildcard>) -> Pat<Wildcard> {
    use crate::pat_graph::{KindSpec, NodeData, PatGraph, TemplateKind, TemplateSpec};
    let mut dst: PatGraph<Wildcard> = PatGraph::new();
    let src_inner = &src.0.inner;

    let mut remap: std::collections::HashMap<
        petgraph::stable_graph::NodeIndex,
        petgraph::stable_graph::NodeIndex,
    > = std::collections::HashMap::new();

    for src_idx in src_inner.node_indices() {
        let Some(src_nd) = src_inner.node_weight(src_idx) else {
            continue;
        };
        let new_kind = match &src_nd.kind {
            KindSpec::Any => KindSpec::Any,
            KindSpec::Variant(d) => KindSpec::Variant(*d),
            KindSpec::Exact(k) => KindSpec::Exact(*k),
            // VariantWith carries a move-only closure; downgrade to
            // Variant (kind-only) — see fn docs above.
            KindSpec::VariantWith { discriminant, .. } => KindSpec::Variant(*discriminant),
        };
        let new_template = src_nd.template_spec.as_ref().and_then(|ts| match &ts.kind {
            TemplateKind::Exact(k) => Some(TemplateSpec {
                kind: TemplateKind::Exact(*k),
                ty: ts.ty,
            }),
            // TemplateKind::Fn carries a move-only closure; dropped.
            TemplateKind::Fn(_) => None,
        });
        let new_idx = dst.add_node(NodeData {
            kind: new_kind,
            output_ty: src_nd.output_ty,
            capture: src_nd.capture,
            // node_filter / post_match are move-only; clones drop the
            // hook (the matcher treats `None` as "always accept", which
            // is strictly weaker — acceptable for the leaf shapes used
            // by float_le / float_is_nan).
            node_filter: None,
            post_match: None,
            template_spec: new_template,
            force_ordered: src_nd.force_ordered,
        });
        remap.insert(src_idx, new_idx);
    }

    for edge_idx in src_inner.edge_indices() {
        let Some((producer_src, consumer_src)) = src_inner.edge_endpoints(edge_idx) else {
            continue;
        };
        let Some(weight) = src_inner.edge_weight(edge_idx) else {
            continue;
        };
        let producer = remap[&producer_src];
        let consumer = remap[&consumer_src];
        dst.add_edge(producer, consumer, *weight);
    }

    if let Some(src_root) = src.0.root {
        dst.set_root(remap[&src_root]);
    }
    Pat::from_graph(dst)
}
