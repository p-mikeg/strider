use rustc_hash::FxHashSet;
use strider_ir::Graph;
use strider_ir::node::{NodeId, ValueId};

use crate::bindings::Bindings;
use crate::capture::Capture;

/// One match. [`strider_ir::Function::compact`] invalidates its ids: the accessors taking a
/// graph then return nothing; the others ([`Match::root`], [`Match::value`],
/// [`Match::is_bound`], [`Match::matched_nodes`], [`Match::bindings`]) are unchecked, so test
/// [`Match::is_stale`] first.
#[derive(Clone)]
pub struct Match {
    pub(crate) root: NodeId,
    pub(crate) bindings: Bindings,
    generation: u64,
}

impl Match {
    /// `generation` is the producing graph's [`Graph::generation`].
    pub fn new(root: NodeId, bindings: Bindings, generation: u64) -> Self {
        Self {
            root,
            bindings,
            generation,
        }
    }

    /// Whether `graph` was compacted since this match was produced.
    pub fn is_stale(&self, graph: &Graph) -> bool {
        self.generation != graph.generation()
    }

    /// Where the pattern's root matched. Not generation-checked.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// The typed value / op accessors, e.g. `m.bindings().get_uint(c, function)`. Not
    /// generation-checked; see [`Self::is_stale`].
    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// The bound node, or a bound value's producer. `None` if unbound or
    /// [stale](Self::is_stale).
    pub fn node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        if self.is_stale(graph) {
            return None;
        }
        self.bindings.get_node(c, graph)
    }

    /// The bound value, control edges included. `None` if unbound or bound to a node.
    pub fn value(&self, c: Capture) -> Option<ValueId> {
        self.bindings.get_value(c)
    }

    pub fn is_bound(&self, c: Capture) -> bool {
        self.bindings.is_bound(c)
    }

    /// Addresses of the instructions whose lift or rewrite fed the bound node: a superset of its
    /// contributors. Empty if unbound, stale, or
    /// [`asm_fingerprint_exempt`](strider_ir::node::NodeKind::asm_fingerprint_exempt).
    pub fn asm_fingerprint(&self, c: Capture, graph: &strider_ir::Function) -> FxHashSet<u64> {
        match self.node(c, graph.graph()) {
            Some(node) => graph.side_tables().asm_fingerprint(node),
            None => FxHashSet::default(),
        }
    }

    /// Every IR node that matched a pattern node, possibly with duplicates. Not
    /// generation-checked.
    pub fn matched_nodes(&self) -> &[NodeId] {
        self.bindings.matched_nodes()
    }

    /// Sorted, deduplicated `(capture-id, bound-node-id, bound-value-id)`
    /// triples, the value id `u32::MAX` for a capture bound to a node.
    pub fn capture_signature(&self, graph: &Graph) -> Vec<(u32, u32, u32)> {
        let mut sig: Vec<(u32, u32, u32)> = self
            .bindings
            .iter()
            .filter_map(|(c, _)| {
                let value = self.value(c).map_or(u32::MAX, |v| v.as_u32());
                self.node(c, graph).map(|n| (c.id(), n.as_u32(), value))
            })
            .collect();
        sig.sort_unstable();
        sig.dedup();
        sig
    }
}
