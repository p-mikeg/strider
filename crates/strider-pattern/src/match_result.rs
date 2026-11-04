use rustc_hash::FxHashSet;
use strider_ir::Graph;
use strider_ir::node::{NodeId, ValueId};

use crate::bindings::Bindings;
use crate::capture::Capture;

/// One match. Its ids are invalidated by [`strider_ir::Function::compact`]: accessors taking a
/// graph then return nothing, while the graph-free ones ([`Match::root`], [`Match::value`],
/// [`Match::is_bound`], [`Match::matched_nodes`]) return stale ids.
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

    /// The typed value / op accessors, e.g. `m.bindings().get_uint(c, function)`.
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

    /// Whether the capture is bound.
    pub fn is_bound(&self, c: Capture) -> bool {
        self.bindings.is_bound(c)
    }

    /// The varnode of a bound `InitialVar` or `Call` / `CallOther` clobber output; else `None`.
    #[cfg(any(test, feature = "test-util"))]
    pub fn get_vn(&self, c: Capture, function: &strider_ir::Function) -> Option<rsleigh::Vn> {
        use crate::bindings::Binding;
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;
        if self.is_stale(function.graph()) {
            return None;
        }
        let binding = self.bindings.get_binding(c)?;
        if let Binding::Value(value) = binding {
            let (node, _slot) = function.value_definition(value);
            let kind = function.node_kind(node);
            if matches!(kind, NodeKind::Call { .. } | NodeKind::CallOther { .. })
                && let Some(vn) = function.get_vn_for_value(value)
            {
                return Some(vn);
            }
        }
        // An `InitialVar` tags the owning node, not the value.
        let node = self.bindings.get_node(c, function.graph())?;
        match function.node_kind(node) {
            NodeKind::InitialVar(id) => Some(function.initial_vn(*id)),
            _ => None,
        }
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

    /// Sorted, deduplicated `(capture-id, bound-node-id)` pairs.
    pub fn capture_signature(&self, graph: &Graph) -> Vec<(u32, u32)> {
        let mut sig: Vec<(u32, u32)> = self
            .bindings
            .iter()
            .filter_map(|(c, _)| self.node(c, graph).map(|n| (c.id(), n.as_u32())))
            .collect();
        sig.sort_unstable();
        sig.dedup();
        sig
    }
}
