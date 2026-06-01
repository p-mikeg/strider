//! `PatGraph<R>` — the petgraph-backed sea-of-nodes pattern graph.

mod merge;
mod node_data;
mod role;
mod topo;

pub use node_data::{BuildKind, BuildSpec, BuildTy, EdgeData, KindSpec, NodeData, PostMatchFn};
pub use role::{Combine, Concrete, Role, Wildcard};
// `merge_subgraph`, `topo_order_from_root`, and `assert_dag` are wired in
// the next batch of tasks (builders + `into_pat` finalisation); the
// `unused_imports` allow keeps the storage skeleton committable on its
// own (the `topo` tests use `topo_order_from_root` directly).
#[allow(unused_imports)]
pub(crate) use merge::merge_subgraph;
#[allow(unused_imports)]
pub(crate) use topo::{assert_dag, topo_order_from_root};

use std::marker::PhantomData;

use petgraph::stable_graph::{NodeIndex, StableDiGraph};

/// Pattern graph parametrised by a role marker.
///
/// `R = Wildcard` — graph contains at least one node that cannot be
/// instantiated (kind-`Any` or a custom predicate).  Matchable; NOT a
/// Template.
///
/// `R = Concrete` — every node has a build path (concrete `NodeKind`
/// or capture).  Matchable AND buildable.
///
/// The role parameter is purely a type-level marker; the runtime
/// representation is identical regardless of `R`.
pub struct PatGraph<R> {
    pub(crate) inner: StableDiGraph<NodeData, EdgeData>,
    pub(crate) root: Option<NodeIndex>,
    pub(crate) _role: PhantomData<R>,
}

// Wired in upcoming tasks: every builder uses `add_node` / `add_edge` /
// `set_root`; the `dead_code` allow keeps the storage skeleton committable.
#[allow(dead_code)]
impl<R> PatGraph<R> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: StableDiGraph::new(),
            root: None,
            _role: PhantomData,
        }
    }
    pub fn add_node(&mut self, data: NodeData) -> NodeIndex {
        self.inner.add_node(data)
    }
    pub fn add_edge(
        &mut self,
        producer: NodeIndex,
        consumer: NodeIndex,
        data: EdgeData,
    ) {
        self.inner.add_edge(producer, consumer, data);
    }
    pub fn set_root(&mut self, n: NodeIndex) {
        self.root = Some(n);
    }
    pub(crate) fn root(&self) -> Option<NodeIndex> {
        self.root
    }

    /// Always-safe role widening (Concrete → Wildcard).
    #[must_use]
    pub fn into_wildcard(self) -> PatGraph<Wildcard> {
        PatGraph {
            inner: self.inner,
            root: self.root,
            _role: PhantomData,
        }
    }
}

impl<R> Default for PatGraph<R> {
    fn default() -> Self {
        Self::new()
    }
}
