//! Wildcard / capture pattern constructors.
//!
//! Ported from `strider-analyze::pattern::pat::ctor::wildcards`.  The
//! storage shape is different (one-node `PatGraph<R>` instead of a
//! `NodePat`) but the semantics — `any` accepts every node kind, `var`
//! additionally binds a capture — are identical.
//!
//! `predicate`, `value_of_width`, `inputs_of_width`, `bool_value`, and
//! `bool_inputs` are deferred to a follow-up commit: they need a
//! widened `post_match` closure signature that exposes the `MatchCtx`
//! and bindings, which the current crate scaffold doesn't expose yet.

use crate::capture::Capture;
use crate::pat_graph::{Concrete, KindSpec, NodeData, PatGraph, Wildcard};

use super::Pat;

/// Match any node.  Wildcard role (not buildable).
#[must_use]
pub fn any() -> Pat<Wildcard> {
    let mut g: PatGraph<Wildcard> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: None,
        post_match: None,
        build_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match any node and bind its output to `c`.  Concrete role: the
/// capture provides a build path (resolve through `Bindings` at
/// template time).
#[must_use]
pub fn var(c: Capture) -> Pat<Concrete> {
    let mut g: PatGraph<Concrete> = PatGraph::new();
    let n = g.add_node(NodeData {
        kind: KindSpec::Any,
        output_ty: None,
        capture: Some(c.as_ref()),
        post_match: None,
        build_spec: None,
    
        force_ordered: false,
    });
    g.set_root(n);
    Pat::from_graph(g)
}

/// Match any node for which `f` returns `true`.  Equivalent to
/// `any().when_match(move |ctx, ty, _b| f(ctx, ty))` but spelled as
/// a single free function for the simple "predicate on the matched
/// output's type / function context" case.
///
/// Always returns a `Pat<Wildcard>` because a custom predicate has no
/// template counterpart.
#[must_use]
pub fn predicate<F>(f: F) -> Pat<Wildcard>
where
    F: Fn(&crate::MatchCtx, strider_ir::node::NodeOutputType) -> bool + 'static,
{
    any().when_match(move |ctx, ty, _b| f(ctx, ty))
}
