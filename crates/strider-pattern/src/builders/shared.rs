//! Internal builder helpers shared across the per-kind builder modules.
//!
//! Two flavours of helper live here:
//!
//! * **`*_variant_pat`** — build a `KindSpec::Variant(_)` parent node
//!   that accepts any payload of the named `NodeKind` discriminant.
//!   Consumed by the `*_any` builders (`int_binary_any`, `float_cmp_any`,
//!   …) that quantify over every variant of a kind family.
//! * **`variant_leaf`** — same shape but no children (zero-input pat
//!   node).  Consumed by the `any_*_const` builders.
//!
//! These factor out the `PatGraph::new` + `add_node` + `add_edge` +
//! `set_root` + `Pat::from_graph` boilerplate that every `*_any` and
//! `any_*` builder repeats.

use strider_ir::node::{NodeKind, NodeOutputType};

use crate::pat_graph::{
    EdgeData, KindSpec, NodeData, PatGraph, Wildcard, merge_subgraph,
};

use super::Pat;

/// Build a single-node `Pat<Wildcard>` whose `KindSpec` is a `Variant`
/// matching every payload of `exemplar`'s `NodeKind` discriminant.
/// Used by `any_int_const`, `any_bool_const`, `any_float_const`,
/// `initial_var`.  `output_ty` pins the matched node's output width
/// (e.g. `I1` for `any_bool_const`).
pub(crate) fn variant_leaf(exemplar: NodeKind, output_ty: Option<NodeOutputType>) -> Pat<Wildcard> {
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty,
        capture: None,
        node_filter: None,
        post_match: None,
        template_spec: None,
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Build a one-input `Pat<Wildcard>` whose root is a `KindSpec::Variant`
/// of `exemplar`'s discriminant, with `inner` wired at consumer slot 0.
/// Used by `int_unary_any`, `float_unary_any`.
pub(crate) fn unary_variant_pat<R: crate::pat_graph::Role>(
    exemplar: NodeKind,
    inner: Pat<R>,
) -> Pat<Wildcard> {
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let inner_root = merge_subgraph(&mut parent, inner.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty: None,
        capture: None,
        node_filter: None,
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

/// Build a two-input `Pat<Wildcard>` whose root is a `KindSpec::Variant`
/// of `exemplar`'s discriminant, with `lhs` / `rhs` wired at consumer
/// slots 0 / 1.  Used by `int_binary_any`, `int_cmp_any`,
/// `float_binary_any`, `float_cmp_any`, `bool_binary_any`.  `output_ty`
/// pins the matched node's output width (e.g. `I1` for cmp / bool).
pub(crate) fn binary_variant_pat<R1: crate::pat_graph::Role, R2: crate::pat_graph::Role>(
    exemplar: NodeKind,
    output_ty: Option<NodeOutputType>,
    lhs: Pat<R1>,
    rhs: Pat<R2>,
) -> Pat<Wildcard> {
    let mut parent: PatGraph<Wildcard> = PatGraph::new();
    let lhs_root = merge_subgraph(&mut parent, lhs.0);
    let rhs_root = merge_subgraph(&mut parent, rhs.0);
    let root = parent.add_node(NodeData {
        kind: KindSpec::Variant(std::mem::discriminant(&exemplar)),
        output_ty,
        capture: None,
        node_filter: None,
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
