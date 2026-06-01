//! `PatGraph<R>` — the petgraph-backed sea-of-nodes pattern graph.

mod merge;
mod node_data;
mod role;
mod topo;

pub use node_data::{TemplateKind, TemplateSpec, TemplateTy, EdgeData, KindSpec, NodeData, PostMatchFn};
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
///
/// Move-only: closure-bearing fields inside `NodeData` are
/// `Box<dyn Fn>`, so cloning would require dropping closures.  A lossy
/// structural clone (`crate::pat_graph::clone_lossy`) exists for the
/// small set of builders that need to reference the same operand
/// twice; refcounting / reuse for the Python wrapper lives in
/// `strider-py`, not here.
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

    /// Verify at runtime that every node has either a `TemplateSpec` or
    /// a `Capture` — i.e. the graph is structurally `Concrete` even
    /// though the role marker may not enforce it.
    ///
    /// Used by [`rewrite_rule_dynamic`](crate::rewrite::rewrite_rule_dynamic)
    /// at rule-construction time so a `Pat<Wildcard>` RHS that's
    /// secretly Wildcard-only surfaces the failure up front instead of
    /// during the first match.
    ///
    /// # Errors
    ///
    /// Returns an error naming the offending node if any reachable
    /// node lacks both a build path and a capture binding, or if the
    /// graph is rootless.
    pub fn assert_concrete_at_runtime(&self) -> anyhow::Result<()> {
        let Some(root) = self.root else {
            return Err(anyhow::anyhow!("PatGraph has no root"));
        };
        // Walk reachable-from-root only — disconnected nodes can't
        // participate in instantiation anyway.
        let order = crate::pat_graph::topo_order_from_root(&self.inner, root)?;
        for pn in order {
            let Some(nd) = self.inner.node_weight(pn) else {
                continue;
            };
            if nd.capture.is_some() || nd.template_spec.is_some() {
                continue;
            }
            return Err(anyhow::anyhow!(
                "Wildcard RHS contains a node with neither a TemplateSpec nor a Capture — \
                 every node in a rewrite RHS must be concrete (an explicit builder like \
                 `int_const(0)` / `add(...)`) or a `var(c)` capture bound by the LHS.  \
                 Offending pat-node index: {}",
                pn.index()
            ));
        }
        Ok(())
    }
}

impl<R> Default for PatGraph<R> {
    fn default() -> Self {
        Self::new()
    }
}
