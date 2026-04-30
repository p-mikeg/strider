use crate::builder::VarId;
use crate::dot::GraphDotDumper;
use crate::graph::Graph;
use crate::node::{NodeId, NodeOutputId};
use cranelift_entity::PrimaryMap;
use cranelift_entity::packed_option::ReservedValue;

/// An under-construction IR function graph.
///
/// Holds the node graph together with the entry-node ids that anchor the
/// control-flow and memory chains.  Call [`FunctionBuilder::build`] to
/// consume a `FunctionGraph` and produce a [`BuiltFunctionGraph`].
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
/// Produced by consuming a [`FunctionBuilder`] after all regions have been
/// wired together.  The graph can be walked, queried, and passed to
/// optimisation passes and the pattern matcher.
pub struct BuiltFunctionGraph {
    /// The sea-of-nodes graph.
    pub graph: Graph,
    /// The `Entry` node; use as the root for any graph walk.
    pub entry: NodeId,
    /// Map from [`VarId`] to the corresponding [`rsleigh::Vn`] varnode.
    pub variables: PrimaryMap<VarId, rsleigh::Vn>,
    /// Ordered list of varnodes clobbered by every `Call` node.
    /// The i-th clobbered output of any Call (output index `i + 2`) corresponds
    /// to `call_clobbered[i]`.  The list is the same for all calls.
    ///
    /// The first `ret_val_regs.len()` entries are the calling convention's
    /// return registers in ABI order (see [`BuiltFunctionGraph::ret_val_regs`]);
    /// the rest are remaining caller-clobbered registers.
    pub call_clobbered: Box<[rsleigh::Vn]>,
    /// The calling convention's return-value registers, in ABI order.
    /// Matches the first `ret_val_regs.len()` entries of
    /// [`BuiltFunctionGraph::call_clobbered`] when those regs are caller-clobbered
    /// (they normally are — callee-saved ret regs are unusual), and matches
    /// `Return` node input slots `2..2+ret_val_regs.len()`.
    pub ret_val_regs: Box<[rsleigh::Vn]>,
}

impl BuiltFunctionGraph {
    /// Wraps `(graph, entry)` into a temporary `BuiltFunctionGraph` with
    /// empty `variables` / `call_clobbered` / `ret_val_regs`.
    ///
    /// Used by `opt::with_built` to bridge `(&mut Graph, NodeId)` callers
    /// to `&mut BuiltFunctionGraph`-typed helpers (the `pattern` crate's
    /// rewrite machinery is typed against `BuiltFunctionGraph`). The dummy
    /// fields are safe because opt impls only read `graph` and `entry`;
    /// they never touch the calling-convention metadata. Not appropriate
    /// for downstream consumers that need that metadata - use
    /// [`crate::FunctionBuilder::build`] for those.
    #[must_use]
    pub fn from_graph_and_entry(graph: crate::graph::Graph, entry: NodeId) -> Self {
        Self {
            graph,
            entry,
            variables: PrimaryMap::new(),
            call_clobbered: Box::new([]),
            ret_val_regs: Box::new([]),
        }
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from [`BuiltFunctionGraph::entry`].
    #[must_use]
    pub fn preorder(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(&self.graph, self.entry)
    }

    /// Reachable preorder filtered by a predicate over the node's
    /// [`crate::node::NodeKind`].  Convenience for the common
    /// `.preorder().filter(|n| matches!(graph.node_kind(n), …))` pattern.
    pub fn preorder_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&crate::node::NodeKind) -> bool + 'a,
    {
        self.preorder()
            .filter(move |&n| pred(self.graph.node_kind(n)))
    }

    /// Iterates over **every** node id in the graph, including nodes that are
    /// not reachable from the entry via the control-flow or data-dependency
    /// chains (e.g. `Store` nodes whose memory output is not consumed by any
    /// node visible from `preorder`).
    pub fn all_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.graph.nodes.keys()
    }

    /// Returns a [`GraphDotDumper`](crate::dot::GraphDotDumper) that can render
    /// this function graph to a `.dot` / `.html` file.
    #[must_use] 
    pub fn dot_dumper<'a, R: rsleigh::MemReader>(
        &'a self,
        sleigh: &'a rsleigh::Sleigh<R>,
    ) -> crate::dot::GraphDotDumper<'a, R> {
        GraphDotDumper {
            entry: self.entry,
            graph: &self.graph,
            sleigh,
            call_clobbered: &self.call_clobbered,
            ret_val_regs: &self.ret_val_regs,
        }
    }
}
