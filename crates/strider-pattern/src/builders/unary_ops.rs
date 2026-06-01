//! Integer unary-op chained builders.
//!
//! This commit ships `neg` only — it's the bare minimum needed for the
//! `sub(a, b) = add(a, neg(b))` lift-time desugar in `int_ops`.  The
//! remaining `IntUnaryOp` variants (none in the IR today; only `Neg`
//! exists) and the float / boolean unary builders land in follow-up
//! commits as the surface grows.

use strider_ir::node::NodeKind;
use strider_ir::IntUnaryOp;

use crate::pat_graph::{
    BuildKind, BuildSpec, BuildTy, EdgeData, KindSpec, NodeData, PatGraph, Role, merge_subgraph,
};

use super::Pat;

/// Build a one-input pattern node wrapping `inner` with the unary op
/// `op`.  Role propagates unchanged (a unary op can't widen role since
/// it doesn't introduce a second sub-pattern).
fn unary_op_pat<R: Role>(op: IntUnaryOp, inner: Pat<R>) -> Pat<R> {
    let kind = NodeKind::IntUnaryOp(op);
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

/// Match `IntUnaryOp::Neg(inner)` — two's-complement negation `-inner`.
///
/// In build position (RHS of a rewrite rule), emits an `IntUnaryOp::Neg`
/// whose output type inherits the rewrite root.
#[must_use]
pub fn neg<R: Role>(inner: Pat<R>) -> Pat<R> {
    unary_op_pat(IntUnaryOp::Neg, inner)
}
