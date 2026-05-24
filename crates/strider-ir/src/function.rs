//! [`Function`] — a [`Graph`] plus per-function overlay state (`entry`,
//! `cc_metadata`, side tables).
//!
//! [`Graph`] holds structural state (nodes/edges/wide_const interning, dedup
//! cache).  [`Function`] holds the overlay that gives those nodes their
//! function-level meaning: which node is the entry, the calling convention
//! metadata, asm fingerprint attribution, and other `NodeId`-keyed side
//! tables.
//!
//! Passes that only need structure take `&Graph`; passes that need the overlay
//! (most opt passes, the validator, dot rendering) take `&Function` or
//! `&mut Function`.
//!
//! `Function` implements `Deref<Target = Graph>` and `DerefMut` so all
//! [`Graph`] methods are available on a `&Function` / `&mut Function`
//! without going through the explicit `.graph()` accessor.

use crate::graph::{CcMetadata, Graph, NodeIdRemap};
use crate::node::NodeId;

/// A lifted function: structural [`Graph`] plus per-function overlay state.
///
/// `FunctionBuilder::build` is the canonical constructor.  For synthetic /
/// test graphs, use [`Function::new`] and populate via [`Function::graph_mut`]
/// and [`Function::set_entry`].  For wrapping an already-built graph plus a
/// known entry, use [`Function::from_built_graph`].
///
/// `Function` derefs to `Graph`, so all [`Graph`] read accessors (e.g.
/// `node_kind`, `walk_from`, `all_node_ids`) are available directly on a
/// `&Function`.
#[derive(Default)]
pub struct Function {
    graph: Graph,
    entry: Option<NodeId>,
    /// Calling-convention metadata.  `None` before `FunctionBuilder::build`
    /// completes; `Some(_)` on every fully-built function returned to callers.
    cc_metadata: Option<CcMetadata>,
}

impl std::ops::Deref for Function {
    type Target = Graph;

    #[inline]
    fn deref(&self) -> &Graph {
        &self.graph
    }
}

impl std::ops::DerefMut for Function {
    #[inline]
    fn deref_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }
}

impl Function {
    /// Creates a `Function` with an empty graph and no entry node.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Wraps an already-built `graph` and records `entry` as the entry node.
    ///
    /// Use this adapter when you have a `Graph` + a known entry `NodeId`
    /// (e.g. returned from a lower-level builder path) and need to present
    /// a `Function`.
    #[must_use]
    pub fn from_built_graph(graph: Graph, entry: NodeId) -> Self {
        Self {
            graph,
            entry: Some(entry),
            cc_metadata: None,
        }
    }

    /// Returns a shared reference to the underlying graph.
    #[inline]
    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Returns a mutable reference to the underlying graph.
    #[inline]
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    /// Returns the entry node, if one has been recorded.
    #[inline]
    #[must_use]
    pub fn entry(&self) -> Option<NodeId> {
        self.entry
    }

    /// Records `entry` as the function's entry node.
    #[inline]
    pub fn set_entry(&mut self, entry: NodeId) {
        self.entry = Some(entry);
    }

    /// Read-only access to the calling-convention metadata, or `None` if
    /// the function has not yet been finalised by [`crate::FunctionBuilder::build`].
    #[inline]
    #[must_use]
    pub fn cc_metadata(&self) -> Option<&CcMetadata> {
        self.cc_metadata.as_ref()
    }

    /// Sets the calling-convention metadata.  Called by
    /// [`crate::FunctionBuilder::build`] to populate the field.
    #[inline]
    pub fn set_cc_metadata(&mut self, cc: CcMetadata) {
        self.cc_metadata = Some(cc);
    }

    /// Read the calling convention's call-clobbered varnode list.
    /// Convenience for `function.cc_metadata().call_clobbered`.  Returns
    /// an empty slice when `cc_metadata` is `None`.
    #[inline]
    #[must_use]
    pub fn call_clobbered_regs(&self) -> &[rsleigh::Vn] {
        self.cc_metadata
            .as_ref()
            .map_or(&[], |cc| &cc.call_clobbered)
    }

    /// Function-default `no_memory_clobber` flag.  Returns `false` when
    /// `cc_metadata` is `None`.
    #[inline]
    #[must_use]
    pub fn no_memory_clobber(&self) -> bool {
        self.cc_metadata
            .as_ref()
            .is_some_and(|cc| cc.no_memory_clobber)
    }

    /// Read the function-default CallOther clobber list.
    /// Convenience for `function.cc_metadata().call_other_clobbered`.
    /// Returns an empty slice when `cc_metadata` is `None`.
    #[inline]
    #[must_use]
    pub fn call_other_clobbered_regs(&self) -> &[rsleigh::Vn] {
        self.cc_metadata
            .as_ref()
            .map_or(&[], |cc| &cc.call_other_clobbered)
    }

    /// Read the `VarId → Vn` map for tracked variables.
    /// Returns `None` when `cc_metadata` is `None`.
    #[inline]
    #[must_use]
    pub fn variables_map(
        &self,
    ) -> Option<&cranelift_entity::PrimaryMap<crate::builder::VarId, rsleigh::Vn>> {
        self.cc_metadata.as_ref().map(|cc| &cc.variables)
    }

    /// Returns the asm-fingerprint addresses attributed to `id`.
    ///
    /// Delegates to [`Graph`] storage.
    #[inline]
    #[must_use]
    pub fn asm_fingerprint(&self, id: NodeId) -> &[u64] {
        self.graph.asm_fingerprint(id)
    }

    /// Sets the asm-fingerprint for `id` to `fp` (sorted, deduplicated).
    ///
    /// Delegates to [`Graph`] storage.
    #[inline]
    pub fn set_asm_fingerprint(&mut self, id: NodeId, fp: Vec<u64>) {
        self.graph.set_asm_fingerprint(id, fp);
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from [`Function::entry`].  Yields an empty walk on a
    /// function whose entry has not yet been set.
    #[must_use]
    pub fn preorder(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph_opt(&self.graph, self.entry)
    }

    /// Reachable preorder filtered by a predicate over the node's kind.
    pub fn preorder_kind<'a, P>(
        &'a self,
        mut pred: P,
    ) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&crate::node::NodeKind) -> bool + 'a,
    {
        self.preorder()
            .filter(move |&n| pred(self.graph.node_kind(n)))
    }

    /// Counts reachable nodes whose [`crate::node::NodeKind`] satisfies
    /// `predicate`.  Walks in pre-order from [`Self::entry`].
    pub fn count_kind<F: Fn(&crate::node::NodeKind) -> bool>(&self, predicate: F) -> usize {
        self.preorder()
            .filter(|nid| predicate(self.graph.node_kind(*nid)))
            .count()
    }

    /// Returns `true` when at least one reachable node satisfies
    /// `predicate`.  Short-circuits at the first match.
    pub fn has_kind<F: Fn(&crate::node::NodeKind) -> bool>(&self, predicate: F) -> bool {
        self.preorder().any(|nid| predicate(self.graph.node_kind(nid)))
    }

    /// Rebuilds the function's graph to retain only nodes reachable from
    /// [`Self::entry`].  The entry node id is remapped; the stored entry
    /// is updated to the new id.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::entry`] is `None`, or if the retain-
    /// reachable remap doesn't include the entry (invariant violation).
    pub fn compact(&mut self) -> crate::Result<NodeIdRemap> {
        let entry = self.entry.ok_or_else(|| {
            anyhow::anyhow!("Function::compact: entry node is not set")
        })?;
        let remap = self.graph.retain_reachable(entry)?;
        let new_entry = remap.node_old_to_new(entry).ok_or_else(|| {
            anyhow::anyhow!(
                "Function::compact: entry {:?} missing from remap (invariant violation)",
                entry
            )
        })?;
        self.entry = Some(new_entry);
        Ok(remap)
    }

    /// Returns a dot dumper for rendering this function's graph to HTML / DOT.
    ///
    /// # Errors
    ///
    /// Returns an error if `entry` or `cc_metadata` is not set (i.e. the
    /// function has not been fully built).
    pub fn dot_dumper<'a, R: rsleigh::MemReader>(
        &'a self,
        sleigh: &'a rsleigh::Sleigh<R>,
    ) -> crate::Result<crate::graph_dot::GraphDotDumper<'a, R>> {
        let entry = self.entry.ok_or_else(|| {
            anyhow::anyhow!("Function::dot_dumper: entry node is not set")
        })?;
        let cc = self.cc_metadata().ok_or_else(|| {
            anyhow::anyhow!("Function::dot_dumper: cc_metadata is not set")
        })?;
        Ok(crate::graph_dot::GraphDotDumper {
            entry,
            graph: &self.graph,
            sleigh,
            call_clobbered: &cc.call_clobbered,
            ret_val_regs: &cc.ret_val_regs,
            node_filter: None,
        })
    }
}

#[cfg(test)]
mod function_skeleton_tests {
    use super::Function;
    use crate::node::{NodeKind, NodeOutputKind};

    #[test]
    fn function_new_carries_an_empty_graph() {
        let f = Function::new();
        assert_eq!(f.graph().all_node_ids().count(), 0);
        assert!(f.entry().is_none());
    }

    #[test]
    fn function_records_entry_via_set_entry() {
        let mut f = Function::new();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        f.set_entry(entry);
        assert_eq!(f.entry(), Some(entry));
    }

    #[test]
    fn function_asm_fingerprint_round_trips() {
        let mut f = Function::new();
        let n = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        f.set_asm_fingerprint(n, vec![0xDEAD_BEEF]);
        assert_eq!(f.asm_fingerprint(n), &[0xDEAD_BEEF]);
    }
}

#[cfg(test)]
mod compact_tests {
    #![allow(clippy::unwrap_used)]

    use super::Function;
    use crate::graph::CcMetadata;
    use crate::node::{NodeKind, NodeOutputKind};
    use cranelift_entity::PrimaryMap;

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut f = Function::new();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(0xdead),
            [],
            [NodeOutputKind::OutputType(crate::node::NodeOutputType::U64)],
        );
        f.set_entry(entry);
        f.set_cc_metadata(CcMetadata {
            variables: PrimaryMap::new(),
            call_clobbered: Box::new([]),
            ret_val_regs: Box::new([]),
            call_other_clobbered: Box::new([]),
            no_memory_clobber: false,
        });
        let pre_count = f.all_node_ids().count();

        let _remap = f.compact().expect("compact succeeds on a valid function");

        let post_count = f.all_node_ids().count();
        assert!(post_count < pre_count, "compact must shrink the graph");
        // entry was remapped; new entry id still has the Control output.
        let entry_id = f.entry().unwrap();
        let outs: Vec<_> = f.node_outputs(entry_id).to_vec();
        assert_eq!(outs.len(), 1);
        assert!(f.output_kind(outs[0]).is_control());
    }
}
