use rustc_hash::FxHashSet;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Graph, IRViewer};

use crate::bindings::{Binding, Bindings};
use crate::capture::Capture;

#[derive(Clone)]
pub struct Match {
    pub(crate) root: NodeId,
    pub(crate) bindings: Bindings,
}

impl Match {
    /// Packages an in-progress [`Bindings`] journal as a `Match`, so a
    /// `.when()` predicate receives a real `Match` for the attempt still in
    /// flight, `root` set to the node the guarded sub-pattern matched at.
    pub fn from_root(root: NodeId, bindings: Bindings) -> Self {
        Self { root, bindings }
    }

    /// Where the top-level pattern matched.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Typed value / op accessors all live on [`Bindings`]:
    /// `m.bindings().get_uint(c, function)`.
    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// A value-producing capture recovers its owning node via
    /// [`strider_ir::Graph::producer`], hence the `&Graph`.
    pub fn node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        self.bindings.get_node(c, graph)
    }

    /// `None` for an unbound or control-flow capture. A multi-output node
    /// such as `Call = [Control, Memory, ..results]` binds the slot its
    /// capture's vertex sits at.
    pub fn value(&self, c: Capture) -> Option<ValueId> {
        self.bindings.get_value(c)
    }

    /// Graph-free: answers only "did this capture fire?".
    pub fn is_bound(&self, c: Capture) -> bool {
        self.bindings.is_bound(c)
    }

    /// Well-defined for only two producer kinds, `None` for everything else:
    ///
    /// * `InitialVar(vn)`: the varnode read at function entry.
    /// * `Call` / `CallOther` clobber outputs: the clobbered register.
    pub fn get_vn(&self, c: Capture, function: &strider_ir::Function) -> Option<rsleigh::Vn> {
        let binding = self.bindings.get_binding(c)?;
        if let Binding::Value(value) = binding {
            let (node, _slot) = function.value_definition(value);
            let kind = function.node_kind(node);
            // Control / Memory / value outputs are absent from `value_vn`,
            // so a missing entry correctly falls through.
            if matches!(kind, NodeKind::Call | NodeKind::CallOther { .. })
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

    /// The machine instructions whose lifting or subsequent rewrite fed the
    /// bound node's value: the proof-of-correctness aid for a query.
    ///
    /// Empty when the capture is unbound, and legitimately empty for the
    /// region / phi / initial-state kinds `SideTables::asm_fingerprint`
    /// exempts. The contract is superset-only: passes may grow a fingerprint
    /// but never shrink it, so these addresses always cover every contributor.
    pub fn asm_fingerprint(&self, c: Capture, graph: &strider_ir::Function) -> FxHashSet<u64> {
        match self.bindings.get_node(c, graph.graph()) {
            Some(node) => graph.side_tables().asm_fingerprint(node),
            None => FxHashSet::default(),
        }
    }

    /// Drops the `Matcher` borrow, e.g. before mutating the graph.
    pub fn bindings_clone(&self) -> Bindings {
        self.bindings.clone()
    }

    /// Every IR node that matched a pat node: root, interior and captured
    /// leaves. May hold duplicates when a DAG sub-pattern matched along two
    /// paths.
    pub fn matched_nodes(&self) -> &[NodeId] {
        self.bindings.matched_nodes()
    }

    /// Sorted, deduplicated `(capture-id, bound-node-id)` pairs: a match's
    /// identity by *what it binds* rather than by root.
    pub fn capture_signature(&self, graph: &Graph) -> Vec<(u32, u32)> {
        let mut sig: Vec<(u32, u32)> = self
            .bindings
            .iter()
            .filter_map(|(c, _)| {
                self.bindings
                    .get_node(c, graph)
                    .map(|n| (c.id(), n.as_u32()))
            })
            .collect();
        sig.sort_unstable();
        sig.dedup();
        sig
    }
}
