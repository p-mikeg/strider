use crate::builder::VarId;
use crate::graph::{CcMetadata, Graph};
use crate::graph_dot::GraphDotDumper;
use crate::node::{NodeId, NodeOutputId};
use cranelift_entity::PrimaryMap;
use cranelift_entity::packed_option::ReservedValue;

/// An under-construction IR function graph.
///
/// Holds the node graph together with the entry-node ids that anchor the
/// control-flow and memory chains.  Call [`crate::FunctionBuilder::build`]
/// to consume a `FunctionGraph` and produce a [`BuiltFunctionGraph`].
#[derive(Clone)]
pub struct FunctionGraph {
    /// The sea-of-nodes graph being built.
    pub graph: Graph,
    /// The `Entry` node that serves as the root of the function.
    pub entry: NodeId,
    /// The single `Control` output of the `Entry` node.
    pub entry_control: NodeOutputId,
    /// The single `Memory` output of the `InitialMemory` node.
    pub entry_memory: NodeOutputId,
}

impl FunctionGraph {
    /// Creates a `FunctionGraph` with all ids set to their reserved
    /// (invalid) sentinel values.  Used as a placeholder by the builder
    /// before the real entry nodes are emitted; not part of the public
    /// surface because consumers should never observe a partial graph.
    pub(crate) fn new_invalid() -> Self {
        Self {
            graph: Graph::new(),
            entry: NodeId::reserved_value(),
            entry_control: NodeOutputId::reserved_value(),
            entry_memory: NodeOutputId::reserved_value(),
        }
    }
}

/// A fully-built, immutable IR function graph ready for analysis.
///
/// Produced by consuming a [`crate::FunctionBuilder`] after all regions
/// have been wired together.  Internally, a `BuiltFunctionGraph` is a
/// thin wrapper around a [`Graph`] whose `entry` and `cc_metadata` fields
/// are guaranteed `Some(_)` — the wrapper exists purely to encode that
/// invariant in the type system.  All CC metadata (variables map,
/// call-clobbered list, ret-val regs, call-other-clobbered list,
/// no-memory-clobber flag) lives on the wrapped `Graph` itself, in its
/// `cc_metadata` side-table.
///
/// Implements [`Clone`] — every field is `Clone`.  Cloning produces a
/// structural copy of the sea-of-nodes arena (typical functions:
/// hundreds of nodes → microseconds), which is meaningfully cheaper
/// than re-lifting from pcode.
#[derive(Clone)]
pub struct BuiltFunctionGraph {
    /// The wrapped graph.  Guaranteed (by the wrapper) to have
    /// `entry.is_some()` and `cc_metadata.is_some()`.
    inner: Graph,
}

impl std::ops::Deref for BuiltFunctionGraph {
    type Target = Graph;
    fn deref(&self) -> &Graph {
        &self.inner
    }
}

impl std::ops::DerefMut for BuiltFunctionGraph {
    fn deref_mut(&mut self) -> &mut Graph {
        &mut self.inner
    }
}

impl BuiltFunctionGraph {
    /// Wraps a `Graph` whose `entry` and `cc_metadata` fields have been
    /// populated by [`crate::FunctionBuilder::build`].  Asserts the
    /// `Some(_)` invariant in debug builds; release builds trust the
    /// caller (only `FunctionBuilder::build` constructs wrappers).
    pub(crate) fn from_graph(graph: Graph) -> Self {
        debug_assert!(
            graph.entry.is_some(),
            "BuiltFunctionGraph requires graph.entry to be Some(_)"
        );
        debug_assert!(
            graph.cc_metadata.is_some(),
            "BuiltFunctionGraph requires graph.cc_metadata to be Some(_)"
        );
        Self { inner: graph }
    }

    /// The `Entry` node of the function — the root for any graph walk.
    #[must_use]
    pub fn entry(&self) -> NodeId {
        self.inner
            .entry
            .expect("BuiltFunctionGraph invariant: entry is Some")
    }

    /// Read-only access to the wrapped [`Graph`].
    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.inner
    }

    /// Mutable access to the wrapped [`Graph`].  Callers must not clear
    /// `entry` or `cc_metadata` — the wrapper invariant assumes both
    /// remain `Some(_)`.
    #[must_use]
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.inner
    }

    /// Read-only access to the calling-convention metadata captured at
    /// build time.  See [`CcMetadata`].
    #[must_use]
    pub fn cc_metadata(&self) -> &CcMetadata {
        self.inner
            .cc_metadata
            .as_ref()
            .expect("BuiltFunctionGraph invariant: cc_metadata is Some")
    }

    /// Read the calling convention's call-clobbered varnode list.
    /// Convenience for `bfg.cc_metadata().call_clobbered`.
    #[must_use]
    pub fn call_clobbered_regs(&self) -> &[rsleigh::Vn] {
        &self.cc_metadata().call_clobbered
    }

    /// Function-default `no_memory_clobber` flag — whether calls under
    /// this convention preserve memory (zero-side-effect hooks like
    /// `__fentry__` / `mcount`).  When `true`, `LoadReadOnly` and
    /// `StackLoadForward` may forward across calls.
    #[must_use]
    pub fn no_memory_clobber(&self) -> bool {
        self.cc_metadata().no_memory_clobber
    }

    /// Read the function-default CallOther clobber list.
    /// Convenience for `bfg.cc_metadata().call_other_clobbered`.
    #[must_use]
    pub fn call_other_clobbered_regs(&self) -> &[rsleigh::Vn] {
        &self.cc_metadata().call_other_clobbered
    }

    /// Read the `VarId → Vn` map for tracked variables.
    /// Convenience for `bfg.cc_metadata().variables`.
    #[must_use]
    pub fn variables_map(&self) -> &PrimaryMap<VarId, rsleigh::Vn> {
        &self.cc_metadata().variables
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from [`Self::entry`].
    #[must_use]
    pub fn preorder(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(&self.inner, self.entry())
    }

    /// Reachable preorder filtered by a predicate over the node's
    /// [`crate::node::NodeKind`].  Convenience for the common
    /// `.preorder().filter(|n| matches!(graph.node_kind(n), …))` pattern.
    pub fn preorder_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&crate::node::NodeKind) -> bool + 'a,
    {
        self.preorder()
            .filter(move |&n| pred(self.inner.node_kind(n)))
    }

    /// Iterates over **every** node id in the graph, including nodes
    /// that are not reachable from the entry via the control-flow or
    /// data-dependency chains (e.g. `Store` nodes whose memory output
    /// is not consumed by any node visible from `preorder`).
    pub fn all_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.inner.nodes.keys()
    }

    /// Rebuilds the underlying [`Graph`] to retain only nodes reachable
    /// from [`Self::entry`] via [`crate::walk::walk_graph`].  The entry
    /// node id is remapped on the wrapped graph; CC metadata is
    /// vn-keyed and stays valid as-is.
    ///
    /// External callers that hold any pre-compaction `NodeId` /
    /// `NodeOutputId` / `NodeInputId` MUST rewrite them through the
    /// returned remap (or drop them).
    ///
    /// # Errors
    ///
    /// Returns an error if `retain_reachable`'s remap doesn't contain
    /// the entry node.  By construction this can never fire —
    /// `retain_reachable` walks forward from `entry`, so the entry is
    /// always reachable from itself — but propagating as `Err` rather
    /// than panicking keeps every error path typed so Python users see
    /// a clean exception.
    pub fn compact(&mut self) -> crate::Result<crate::graph::NodeIdRemap> {
        let entry = self.entry();
        let remap = self.inner.retain_reachable(entry)?;
        let new_entry = remap.node_old_to_new(entry).ok_or_else(|| {
            anyhow::anyhow!(
                "BuiltFunctionGraph::compact: entry {:?} missing from retain_reachable remap (invariant violation)",
                entry
            )
        })?;
        self.inner.entry = Some(new_entry);
        Ok(remap)
    }

    /// Returns a [`GraphDotDumper`](crate::graph_dot::GraphDotDumper) that can
    /// render this function graph to a `.dot` / `.html` file.
    #[must_use]
    pub fn dot_dumper<'a, R: rsleigh::MemReader>(
        &'a self,
        sleigh: &'a rsleigh::Sleigh<R>,
    ) -> crate::graph_dot::GraphDotDumper<'a, R> {
        let cc = self.cc_metadata();
        GraphDotDumper {
            entry: self.entry(),
            graph: &self.inner,
            sleigh,
            call_clobbered: &cc.call_clobbered,
            ret_val_regs: &cc.ret_val_regs,
        }
    }
}

#[cfg(test)]
mod compact_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::node::{NodeKind, NodeOutputKind};

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut graph = crate::graph::Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _zombie = graph.create_node(
            NodeKind::IntConst(0xdead),
            [],
            [NodeOutputKind::OutputType(crate::node::NodeOutputType::U64)],
        );
        graph.entry = Some(entry);
        graph.cc_metadata = Some(CcMetadata {
            variables: PrimaryMap::new(),
            call_clobbered: Box::new([]),
            ret_val_regs: Box::new([]),
            call_other_clobbered: Box::new([]),
            no_memory_clobber: false,
        });
        let mut bfg = BuiltFunctionGraph::from_graph(graph);
        let pre_count = bfg.graph().all_node_ids().count();

        let _remap = bfg.compact().expect("compact succeeds on a valid graph");

        let post_count = bfg.graph().all_node_ids().count();
        assert!(post_count < pre_count, "compact must shrink the graph");
        // entry was remapped; new entry id still has the Control output.
        let entry_id = bfg.entry();
        let outs: Vec<_> = bfg.graph().node_outputs(entry_id).into_iter().collect();
        assert_eq!(outs.len(), 1);
        assert!(bfg.graph().output_kind(outs[0]).is_control());
    }
}
