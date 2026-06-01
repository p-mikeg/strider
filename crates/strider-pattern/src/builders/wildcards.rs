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

// TODO: predicate / value_of_width / inputs_of_width / bool_value /
// bool_inputs builders land once the `post_match` closure signature
// widens to take `(MatchCtx, NodeId, &mut Bindings)`.  The closure is
// currently the Task-2 stub `Box<dyn Fn() -> bool>`, which can't
// inspect a node's output type.
