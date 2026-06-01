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
    TemplateKind, TemplateSpec, TemplateTy, Combine, EdgeData, KindSpec, NodeData, PatGraph, Role,
    Wildcard, merge_subgraph,
};

use super::consts::int_const;
use super::int_ops::{or, xor};
use super::unary_ops::unary_node_pat;
use super::Pat;

/// Build a two-input `FloatBinaryOp(op)` parent pattern around `lhs` /
/// `rhs`.  Role propagates through `Combine`.
fn float_binary_op_pat<R1, R2>(
    op: FloatBinaryOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let kind = NodeKind::FloatBinaryOp(op);
    let mut parent: PatGraph<<R1 as Combine<R2>>::Output> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Exact(kind),
        output_ty: None,
        capture: None,
        post_match: None,
        template_spec: Some(TemplateSpec {
            kind: TemplateKind::Exact(kind),
            ty: TemplateTy::InheritRoot,
        }),
    
        force_ordered: false,
    });
    parent.add_edge(
        lhs_root,
        root,
        EdgeData {
            consumer_slot: 0,
            producer_output_slot: 0,
        },
    );
    parent.add_edge(
        rhs_root,
        root,
        EdgeData {
            consumer_slot: 1,
            producer_output_slot: 0,
        },
    );
    parent.set_root(root);
    Pat::from_graph(parent)
}

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
    float_binary_op_pat(op, lhs, rhs)
}

/// Match **any** `FloatBinaryOp` regardless of variant.  Wildcard role.
#[must_use]
pub fn float_binary_any<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let exemplar = NodeKind::FloatBinaryOp(FloatBinaryOp::Add);
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        post_match: None,
        template_spec: None,
    
        force_ordered: false,
    });
    parent.add_edge(
        lhs_root,
        root,
        EdgeData {
            consumer_slot: 0,
            producer_output_slot: 0,
        },
    );
    parent.add_edge(
        rhs_root,
        root,
        EdgeData {
            consumer_slot: 1,
            producer_output_slot: 0,
        },
    );
    parent.set_root(root);
    Pat::from_graph(parent)
}

/// Match a float addition node `lhs + rhs` (commutative under IEEE 754).
#[must_use]
pub fn float_add<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    float_binary_op_pat(FloatBinaryOp::Add, lhs, rhs)
}

/// Match a float multiplication node `lhs * rhs` (commutative).
#[must_use]
pub fn float_mul<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    float_binary_op_pat(FloatBinaryOp::Mul, lhs, rhs)
}

/// Match a float division node `lhs / rhs` (directional).
#[must_use]
pub fn float_div<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    float_binary_op_pat(FloatBinaryOp::Div, lhs, rhs)
}

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

/// Build a one-input `FloatUnaryOp(op)` parent pattern around `inner`.
fn float_unary_op_pat<R: Role>(op: FloatUnaryOp, inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::FloatUnaryOp(op), inner)
}

/// Variant-agnostic typed dispatcher: takes any `FloatUnaryOp`.
#[must_use]
pub fn float_unary<R: Role>(op: FloatUnaryOp, inner: Pat<R>) -> Pat<R> {
    float_unary_op_pat(op, inner)
}

/// Match **any** `FloatUnaryOp` regardless of variant.  Wildcard role.
#[must_use]
pub fn float_unary_any<R: Role>(inner: Pat<R>) -> Pat<Wildcard> {
    let exemplar = NodeKind::FloatUnaryOp(FloatUnaryOp::Neg);
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let inner_root = merge_subgraph(&mut parent, inner.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        post_match: None,
        template_spec: None,
    
        force_ordered: false,
    });
    parent.add_edge(
        inner_root,
        root,
        EdgeData {
            consumer_slot: 0,
            producer_output_slot: 0,
        },
    );
    parent.set_root(root);
    Pat::from_graph(parent)
}

/// Match a float negation node `-x`.
#[must_use]
pub fn float_neg<R: Role>(inner: Pat<R>) -> Pat<R> {
    float_unary_op_pat(FloatUnaryOp::Neg, inner)
}

/// Match a float absolute-value node `|x|`.
#[must_use]
pub fn float_abs<R: Role>(inner: Pat<R>) -> Pat<R> {
    float_unary_op_pat(FloatUnaryOp::Abs, inner)
}

/// Match a float square-root node `√x`.
#[must_use]
pub fn float_sqrt<R: Role>(inner: Pat<R>) -> Pat<R> {
    float_unary_op_pat(FloatUnaryOp::Sqrt, inner)
}

/// Match a float ceiling node `⌈x⌉`.
#[must_use]
pub fn float_ceil<R: Role>(inner: Pat<R>) -> Pat<R> {
    float_unary_op_pat(FloatUnaryOp::Ceil, inner)
}

/// Match a float floor node `⌊x⌋`.
#[must_use]
pub fn float_floor<R: Role>(inner: Pat<R>) -> Pat<R> {
    float_unary_op_pat(FloatUnaryOp::Floor, inner)
}

/// Match a float round-to-nearest-even node `round(x)`.
#[must_use]
pub fn float_round<R: Role>(inner: Pat<R>) -> Pat<R> {
    float_unary_op_pat(FloatUnaryOp::Round, inner)
}

/// Build a two-input `FloatCmpOp(op)` parent pattern around `lhs` /
/// `rhs`.  Output is `I1`.
fn float_cmp_pat<R1, R2>(
    op: FloatCmpOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let kind = NodeKind::FloatCmpOp(op);
    let mut parent: PatGraph<<R1 as Combine<R2>>::Output> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Exact(kind),
        output_ty: Some(NodeOutputType::I1),
        capture: None,
        post_match: None,
        template_spec: Some(TemplateSpec {
            kind: TemplateKind::Exact(kind),
            ty: TemplateTy::Fixed(NodeOutputType::I1),
        }),
    
        force_ordered: false,
    });
    parent.add_edge(
        lhs_root,
        root,
        EdgeData {
            consumer_slot: 0,
            producer_output_slot: 0,
        },
    );
    parent.add_edge(
        rhs_root,
        root,
        EdgeData {
            consumer_slot: 1,
            producer_output_slot: 0,
        },
    );
    parent.set_root(root);
    Pat::from_graph(parent)
}

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
    float_cmp_pat(op, lhs, rhs)
}

/// Match **any** `FloatCmpOp` regardless of variant.  Wildcard role.
#[must_use]
pub fn float_cmp_any<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<Wildcard>
where
    R1: Role,
    R2: Role,
{
    let exemplar = NodeKind::FloatCmpOp(FloatCmpOp::Equal);
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: Some(NodeOutputType::I1),
        capture: None,
        post_match: None,
        template_spec: None,
    
        force_ordered: false,
    });
    parent.add_edge(
        lhs_root,
        root,
        EdgeData {
            consumer_slot: 0,
            producer_output_slot: 0,
        },
    );
    parent.add_edge(
        rhs_root,
        root,
        EdgeData {
            consumer_slot: 1,
            producer_output_slot: 0,
        },
    );
    parent.set_root(root);
    Pat::from_graph(parent)
}

/// Match a float equality comparison `lhs == rhs` (commutative).
#[must_use]
pub fn float_eq<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    float_cmp_pat(FloatCmpOp::Equal, lhs, rhs)
}

/// Match a float less-than comparison `lhs < rhs` (directional).
#[must_use]
pub fn float_lt<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    float_cmp_pat(FloatCmpOp::Less, lhs, rhs)
}

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
    let inner: Pat<Wildcard> = float_cmp_pat(FloatCmpOp::Equal, lhs_w, rhs_w);
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
    let less_branch: Pat<Wildcard> = float_cmp_pat(FloatCmpOp::Less, lhs_w, rhs_w);
    let equal_branch: Pat<Wildcard> = float_cmp_pat(FloatCmpOp::Equal, lhs_a, rhs_a);
    or(less_branch, equal_branch)
}

/// Match `float_is_nan(x)` — the canonical shape is
/// `xor(float_eq(x, x), int_const(1)):I1` (a value is NaN iff `x == x`
/// is false under IEEE 754).
#[must_use]
pub fn float_is_nan<R: Role>(x: Pat<R>) -> Pat<Wildcard> {
    let x_w: Pat<Wildcard> = x.into_wildcard();
    let x_w2: Pat<Wildcard> = clone_pat(&x_w);
    let eq: Pat<Wildcard> = float_cmp_pat(FloatCmpOp::Equal, x_w, x_w2);
    let one_wild: Pat<Wildcard> = int_const(1).into_wildcard();
    xor(eq, one_wild)
}

/// Structurally clone a `Pat<Wildcard>` for the small set of builders
/// (`float_le`, `float_is_nan`) that need to reference the same input
/// twice.  Now that `NodeData` closures are `Rc<dyn Fn>` and `NodeData`
/// itself is `Clone`, this just delegates to `Pat::clone()` — the
/// previous payload-dropping behaviour is no longer required.
fn clone_pat(src: &Pat<Wildcard>) -> Pat<Wildcard> {
    src.clone()
}
