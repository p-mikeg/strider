//! Internal builder helpers shared across the per-kind builder modules.
//!
//! Three helpers, one per arity (leaf / unary / binary).  Each takes
//! a [`KindSpec`] (caller spells the match-side dispatch — `Exact`,
//! `Variant`, …), an optional `output_ty` width pin, and an
//! `Option<TemplateSpec>` build path.  Pass `Some(...)` for a
//! buildable pattern (concrete role at the leaf, role-combined for
//! unary / binary); pass `None` for a match-only pattern (the
//! `*_any` builders and the wildcard leaves).
//!
//! The `*_any` builders that take any `R: Role` operands widen their
//! inputs to [`Wildcard`] up front; `Wildcard ⊕ Wildcard = Wildcard`
//! propagates through [`Combine`] to give the expected `Pat<Wildcard>`
//! result.

use strider_ir::node::NodeOutputType;

use crate::pat_graph::{
    Combine, EdgeData, KindSpec, NodeData, PatGraph, Role, TemplateSpec, merge_subgraph,
};

use super::Pat;

/// Build a zero-input leaf pat node.  When `template_spec` is `Some`,
/// the resulting node is buildable; when `None`, it's match-only.
/// Role-parameter `R` is the caller's responsibility (typically
/// [`Wildcard`](crate::pat_graph::Wildcard) for the `None` case,
/// [`Concrete`](crate::pat_graph::Concrete) for the `Some` case).
pub(crate) fn leaf_pat<R: Role>(
    kind: KindSpec,
    output_ty: Option<NodeOutputType>,
    template_spec: Option<TemplateSpec>,
) -> Pat<R> {
    let mut g: PatGraph<R> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind,
        output_ty,
        capture: None,
        node_filter: None,
        post_match: None,
        template_spec,
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Build a one-input pat node consuming `inner` at consumer slot 0.
/// Role propagates from `inner` unchanged.  Same `Option<TemplateSpec>`
/// shape as [`leaf_pat`].
pub(crate) fn unary_pat<R: Role>(
    kind: KindSpec,
    output_ty: Option<NodeOutputType>,
    template_spec: Option<TemplateSpec>,
    inner: Pat<R>,
) -> Pat<R> {
    let mut parent: PatGraph<R> = PatGraph::new();
    let inner_root = merge_subgraph(&mut parent, inner.0);
    let root = parent.add_node(NodeData {
        kind,
        output_ty,
        capture: None,
        node_filter: None,
        post_match: None,
        template_spec,
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

/// Build a two-input pat node consuming `lhs` / `rhs` at consumer slots
/// 0 / 1.  Role propagates through [`Combine`]; the result's role is
/// the weaker of the two children's roles.
pub(crate) fn binary_pat<R1, R2>(
    kind: KindSpec,
    output_ty: Option<NodeOutputType>,
    template_spec: Option<TemplateSpec>,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<<R1 as Combine<R2>>::Output>
where
    R1: Combine<R2>,
    R2: Role,
{
    let mut parent: PatGraph<<R1 as Combine<R2>>::Output> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind,
        output_ty,
        capture: None,
        node_filter: None,
        post_match: None,
        template_spec,
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
