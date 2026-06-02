//! Opaque handles returned by [`MatcherBuilder`](super::MatcherBuilder)
//! while wiring a pattern graph.
//!
//! Both wrap a `petgraph` [`NodeIndex`] into the pattern's bipartite
//! store: [`PatValueRef`] points at an output vertex (a value/control
//! output a downstream node can consume), and [`PatNodeRef`] points at
//! a node vertex (for the variadic / control builders that wire inputs
//! and outputs by hand).

use petgraph::stable_graph::NodeIndex;

/// Handle to a pattern **output** vertex.
#[derive(Clone, Copy)]
pub struct PatValueRef(pub(crate) NodeIndex);

/// Handle to a pattern **node** vertex.
#[derive(Clone, Copy)]
pub struct PatNodeRef(pub(crate) NodeIndex);
