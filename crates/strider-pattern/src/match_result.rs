use rustc_hash::FxHashSet;
use strider_ir::Graph;
use strider_ir::node::{NodeId, ValueId};

use crate::bindings::Bindings;
use crate::capture::Capture;

/// Holds raw ids, which [`strider_ir::Function::compact`] invalidates. The
/// graph generation it was produced at is stamped alongside, so the accessors
/// taking a graph report nothing instead of indexing a renumbered arena; the
/// graph-free ones ([`Match::root`], [`Match::value`], [`Match::is_bound`],
/// [`Match::matched_nodes`]) cannot check and return stale ids.
#[derive(Clone)]
pub struct Match {
    pub(crate) root: NodeId,
    pub(crate) bindings: Bindings,
    /// `None` where the producer had no graph to stamp against, which disables
    /// the check.
    generation: Option<u64>,
}

impl Match {
    /// Packages an in-progress [`Bindings`] journal as a `Match`, so a
    /// `.when()` predicate receives a real `Match` for the attempt still in
    /// flight, `root` set to the node the guarded sub-pattern matched at.
    /// Unstamped; see [`Self::from_root_in`].
    pub fn from_root(root: NodeId, bindings: Bindings) -> Self {
        Self {
            root,
            bindings,
            generation: None,
        }
    }

    /// [`Self::from_root`] stamped with `graph`'s generation.
    pub fn from_root_in(root: NodeId, bindings: Bindings, graph: &Graph) -> Self {
        Self {
            root,
            bindings,
            generation: Some(graph.generation()),
        }
    }

    /// Whether `graph` was compacted since this match was produced, which
    /// invalidates every id it holds. Always `false` for an unstamped match.
    pub fn is_stale(&self, graph: &Graph) -> bool {
        self.generation.is_some_and(|g| g != graph.generation())
    }

    /// Where the top-level pattern matched. Not generation-checked; see the
    /// type's own docs.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Typed value / op accessors all live on [`Bindings`]:
    /// `m.bindings().get_uint(c, function)`.
    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// A value-producing capture recovers its owning node via
    /// [`strider_ir::Graph::producer`], hence the `&Graph`. `None` for an
    /// unbound capture and for a [stale](Self::is_stale) match.
    pub fn node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        if self.is_stale(graph) {
            return None;
        }
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
    /// Empty when the capture is unbound or the match is stale, and
    /// legitimately empty for the region / phi / initial-state kinds
    /// `SideTables::asm_fingerprint` exempts. The contract is superset-only: passes may grow a fingerprint
    /// but never shrink it, so these addresses always cover every contributor.
    pub fn asm_fingerprint(&self, c: Capture, graph: &strider_ir::Function) -> FxHashSet<u64> {
        match self.node(c, graph.graph()) {
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
    /// paths. Not generation-checked; see the type's own docs.
    pub fn matched_nodes(&self) -> &[NodeId] {
        self.bindings.matched_nodes()
    }

    /// Sorted, deduplicated `(capture-id, bound-node-id)` pairs: a match's
    /// identity by *what it binds* rather than by root.
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
