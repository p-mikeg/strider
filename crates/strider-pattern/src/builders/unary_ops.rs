//! Integer unary-op chained builders.
//!
//! Covers `neg` (the lone `IntUnaryOp` variant) plus the unit-variant
//! integer unary kinds (`Lzcount`, `Popcount`).  The float / boolean
//! unary builders live in their own modules; cast unary builders
//! (`truncate`, `extend`, …) live in `casts.rs` and reuse
//! `unary_node_pat` from here.

use strider_ir::node::NodeKind;
use strider_ir::IntUnaryOp;

use crate::pat_graph::{
    BuildKind, BuildSpec, BuildTy, EdgeData, KindSpec, NodeData, PatGraph, Role, merge_subgraph,
};

use super::Pat;

/// Build a one-input pattern node wrapping `inner` with the given
/// concrete `NodeKind`.  Role propagates unchanged (a unary op can't
/// widen role since it doesn't introduce a second sub-pattern).  This
/// is the shared shape consumed by every unary builder in the crate
/// (integer unary ops, lzcount / popcount, cast nodes, float unary).
pub(crate) fn unary_node_pat<R: Role>(kind: NodeKind, inner: Pat<R>) -> Pat<R> {
    let mut parent: PatGraph<R> = PatGraph::new();
    let inner_root = merge_subgraph(&mut parent, inner.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Exact(kind),
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: Some(BuildSpec {
            kind: BuildKind::Exact(kind),
            ty: BuildTy::InheritRoot,
        }),
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

/// Build an `IntUnaryOp(op)` parent pattern around `inner`.
fn int_unary_op_pat<R: Role>(op: IntUnaryOp, inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::IntUnaryOp(op), inner)
}

/// Variant-agnostic dispatcher: takes any `IntUnaryOp`.  Role
/// propagates unchanged.
#[must_use]
pub fn int_unary<R: Role>(op: IntUnaryOp, inner: Pat<R>) -> Pat<R> {
    int_unary_op_pat(op, inner)
}

/// Match `IntUnaryOp::Neg(inner)` — two's-complement negation `-inner`.
///
/// In build position (RHS of a rewrite rule), emits an `IntUnaryOp::Neg`
/// whose output type inherits the rewrite root.
#[must_use]
pub fn neg<R: Role>(inner: Pat<R>) -> Pat<R> {
    int_unary_op_pat(IntUnaryOp::Neg, inner)
}

/// Match a `Popcount(inner)` node (count-set-bits).
///
/// `Popcount` is a unit-variant `NodeKind` (not wrapped in
/// `IntUnaryOp`).  Role propagates from `inner` unchanged.
#[must_use]
pub fn popcount<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::Popcount, inner)
}

/// Match an `Lzcount(inner)` node (leading-zero-count).
///
/// `Lzcount` is a unit-variant `NodeKind` (not wrapped in
/// `IntUnaryOp`).  Role propagates from `inner` unchanged.
#[must_use]
pub fn lzcount<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_node_pat(NodeKind::Lzcount, inner)
}
