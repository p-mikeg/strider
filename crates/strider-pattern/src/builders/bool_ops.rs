//! Boolean binary / unary chained builders.
//!
//! Booleans are 1-bit integers (`I1`) in this IR: there is no separate
//! `BoolBinaryOp` / `BoolUnaryOp` node kind.  A boolean AND / OR / XOR
//! is an `IntBinaryOp` (`And` / `Or` / `Xor`) whose output is `I1`,
//! and a logical NOT is `Xor(x, IntConst(1)):I1` (since `~x ≡ x ^ all_ones`
//! and the all-ones constant at `I1` is `IntConst(1)`).
//!
//! Each builder records `output_ty: Some(I1)` on its parent pat node
//! and `BuildSpec::ty = Fixed(I1)` so the matcher's forthcoming
//! output-type guard rejects same-shaped wide integer ops (e.g. a
//! 64-bit `And`) that share the same `NodeKind`.

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::IntBinaryOp;

use crate::pat_graph::{
    BuildKind, BuildSpec, BuildTy, Combine, Concrete, EdgeData, KindSpec, NodeData, PatGraph,
    Role, Wildcard, merge_subgraph,
};

use super::consts::int_const;
use super::Pat;

/// Build a two-input `IntBinaryOp(op)` parent pattern around `lhs` /
/// `rhs` with the output pinned to `I1`.  Role propagates through
/// `Combine`.
fn bool_binary_op_pat<R1, R2>(
    op: IntBinaryOp,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let kind = NodeKind::IntBinaryOp(op);
    let mut parent: PatGraph<<R1 as Combine<R2>>::Output> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Exact(kind),
        output_ty: Some(NodeOutputType::I1),
        capture: None,
        post_match: None,
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(kind),
            ty: BuildTy::Fixed(NodeOutputType::I1),
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
    bool_binary_op_pat(op, lhs, rhs)
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
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: Some(NodeOutputType::I1),
        capture: None,
        post_match: None,
        build_spec: None,
    
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

/// Match a boolean AND node (`IntBinaryOp::And` at `I1`).  Commutative.
#[must_use]
pub fn bool_and<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    bool_binary_op_pat(IntBinaryOp::And, lhs, rhs)
}

/// Match a boolean OR node (`IntBinaryOp::Or` at `I1`).  Commutative.
#[must_use]
pub fn bool_or<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    bool_binary_op_pat(IntBinaryOp::Or, lhs, rhs)
}

/// Match a boolean XOR node (`IntBinaryOp::Xor` at `I1`).  Commutative.
#[must_use]
pub fn bool_xor<R1, R2>(lhs: Pat<R1>, rhs: Pat<R2>) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    bool_binary_op_pat(IntBinaryOp::Xor, lhs, rhs)
}

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
    bool_binary_op_pat(IntBinaryOp::Xor, operand, int_const(1))
}
