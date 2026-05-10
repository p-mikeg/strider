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
/// Produced by consuming a [`crate::FunctionBuilder`] after all regions have been
/// wired together.  The graph can be walked, queried, and passed to
/// optimisation passes and the pattern matcher.
pub struct BuiltFunctionGraph {
    /// The sea-of-nodes graph.
    pub graph: Graph,
    /// The `Entry` node; use as the root for any graph walk.
    pub entry: NodeId,
    /// Map from [`VarId`] to the corresponding [`rsleigh::Vn`] varnode.
    ///
    /// Tightened to `pub(crate)` to keep call-sites on the
    /// [`Self::variables_map`] accessor — mutating this map post-`build()`
    /// desynchronises it from the graph's `InitialVar` / phi nodes (which
    /// key on `VarId` indices) and silently breaks pattern queries that
    /// resolve a `VarId` via `Match::get_vn`.
    pub(crate) variables: PrimaryMap<VarId, rsleigh::Vn>,
    /// Ordered list of varnodes clobbered by every `Call` node.
    /// The i-th clobbered output of any Call (output index `i + 2`) corresponds
    /// to `call_clobbered[i]`.  The list is the same for all calls.
    ///
    /// The first `ret_val_regs.len()` entries are the calling convention's
    /// return registers in ABI order (see [`BuiltFunctionGraph::ret_val_regs`]);
    /// the rest are remaining caller-clobbered registers.
    ///
    /// Tightened to `pub(crate)` so external readers go through
    /// [`Self::call_clobbered_regs`]; mutating this list post-`build()`
    /// desynchronises it from existing `Call` nodes' clobber output slots
    /// and silently breaks `Match::get_vn` (which indexes the slot list
    /// by varnode position).
    pub(crate) call_clobbered: Box<[rsleigh::Vn]>,
    /// The calling convention's return-value registers, in ABI order.
    /// Matches the first `ret_val_regs.len()` entries of
    /// [`BuiltFunctionGraph::call_clobbered`] when those regs are caller-clobbered
    /// (they normally are — callee-saved ret regs are unusual), and matches
    /// `Return` node input slots `2..2+ret_val_regs.len()`.
    ///
    /// Tightened to `pub(crate)` — read via [`Self::ret_val_regs_as_slice`].
    pub(crate) ret_val_regs: Box<[rsleigh::Vn]>,
    /// Function-default clobber list for every `CallOther` node.
    ///
    /// Equals the function's tracked-variable set (`variables.values()`)
    /// filtered to exclude the stack pointer.  Order matches the
    /// CallOther's clobber output slots: the i-th clobber output of any
    /// CallOther (output index `i + 2` for value-less CallOther,
    /// `i + 3` for CallOther with a value output) corresponds to
    /// `call_other_clobbered[i]`.  Distinct from
    /// [`Self::call_clobbered`] (which excludes both callee-saved AND
    /// SP and is per-CC) — `call_other_clobbered` is the conservative
    /// "everything except SP" set used by every CallOther unless a
    /// per-CallOther override on
    /// `Graph::call_clobbered_overrides` shadows it.
    ///
    /// Tightened to `pub(crate)` — read via [`Self::call_other_clobbered_regs`].
    pub(crate) call_other_clobbered: Box<[rsleigh::Vn]>,
    /// Function-default value of
    /// [`target::CallingConvention::no_memory_clobber`] (carried over
    /// from the building [`crate::FunctionBuilder`]).  When `true`,
    /// callers under this convention preserve all observable state
    /// including memory — `LoadReadOnly` and `StackLoadForward` may
    /// forward across them.  Set on `x86_64_all_preserving` and
    /// analogous transparent-hook presets (Linux-kernel `__fentry__`,
    /// `mcount`).  Read via [`Self::no_memory_clobber`].
    pub(crate) no_memory_clobber: bool,
}

impl std::ops::Deref for BuiltFunctionGraph {
    type Target = Graph;
    fn deref(&self) -> &Graph {
        &self.graph
    }
}

impl std::ops::DerefMut for BuiltFunctionGraph {
    fn deref_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }
}

// canonical read-only accessors for the CC
// fields.  The fields themselves remain `pub` for back-compat (the
// workspace has ~30+ direct-field readers), but new code should use
// these accessors — they're the migration path for tightening field
// visibility to `pub(crate)` in a future round.  Method bodies are
// trivial (`&self.field`) so the indirection cost is zero.
impl BuiltFunctionGraph {
    /// Read the calling convention's call-clobbered varnode list.
    /// Mirrors the [`Self::call_clobbered`] field.
    #[must_use]
    pub fn call_clobbered_regs(&self) -> &[rsleigh::Vn] {
        &self.call_clobbered
    }
    /// Read the calling convention's return-value varnode list.
    /// Mirrors the [`Self::ret_val_regs`] field.
    #[must_use]
    pub fn ret_val_regs_as_slice(&self) -> &[rsleigh::Vn] {
        &self.ret_val_regs
    }
    /// Read the function-default CallOther clobber list.
    /// Mirrors the [`Self::call_other_clobbered`] field.
    #[must_use]
    pub fn call_other_clobbered_regs(&self) -> &[rsleigh::Vn] {
        &self.call_other_clobbered
    }
    /// Function-default `no_memory_clobber` flag — whether calls under
    /// this convention preserve memory (zero-side-effect hooks like
    /// `__fentry__` / `mcount`).  When `true`, `LoadReadOnly` and
    /// `StackLoadForward` may forward across calls.
    #[must_use]
    pub fn no_memory_clobber(&self) -> bool {
        self.no_memory_clobber
    }
    /// Read the `VarId → Vn` map for tracked variables.
    /// Mirrors the [`Self::variables`] field.
    #[must_use]
    pub fn variables_map(&self) -> &PrimaryMap<VarId, rsleigh::Vn> {
        &self.variables
    }

    /// Test-only setter: overwrite [`Self::call_clobbered`].
    ///
    /// Used by `pattern` tests that construct a synthetic `Call` node
    /// shape and need a matching function-default clobber list to
    /// exercise [`crate::pattern_glue::*`] queries.  Production paths
    /// should set this via [`crate::FunctionBuilder::build`].  The
    /// `_for_test` suffix is the documented signal that the caller has
    /// verified the slot/varnode correspondence with the synthetic
    /// graph's `Call` outputs (see [`Self::call_clobbered`]'s caveat).
    pub fn set_call_clobbered_for_test(&mut self, list: Box<[rsleigh::Vn]>) {
        self.call_clobbered = list;
    }

    /// Test-only setter: overwrite [`Self::ret_val_regs`].  Same
    /// contract as [`Self::set_call_clobbered_for_test`].
    pub fn set_ret_val_regs_for_test(&mut self, list: Box<[rsleigh::Vn]>) {
        self.ret_val_regs = list;
    }

    /// Test-only setter: overwrite [`Self::call_other_clobbered`].
    /// Same contract as [`Self::set_call_clobbered_for_test`].
    pub fn set_call_other_clobbered_for_test(&mut self, list: Box<[rsleigh::Vn]>) {
        self.call_other_clobbered = list;
    }
}

impl BuiltFunctionGraph {
    /// Wraps `(graph, entry)` into a temporary `BuiltFunctionGraph` with
    /// empty `variables` / `call_clobbered` / `ret_val_regs`.
    ///
    /// **Construct a rewrite-only `BuiltFunctionGraph` with empty CC
    /// fields.**  Used by `compact`'s test fixture and a few pattern test
    /// scaffolds that intentionally bypass the build path.  Production
    /// opt-side rewrite paths use `pattern::RewriteCtx` (constructed via
    /// `opt::with_rewrite_ctx`) instead — that path doesn't need a BFG
    /// at all.
    ///
    /// # Contract — caller responsibility
    ///
    /// The returned `BuiltFunctionGraph` has **empty** `variables`,
    /// `call_clobbered`, `ret_val_regs`, and `call_other_clobbered`.
    /// Callers MUST pass it only to consumers that touch `graph` and
    /// `entry`; consulting any other field returns a meaningless
    /// empty value silently.  The `pattern::rewrite_rule` machinery
    /// and the `opt::Optimizer` trait are vetted to honour this
    /// contract.  Bespoke callers must verify by inspection.
    ///
    /// For real CC metadata use [`crate::FunctionBuilder::build`].
    ///
    /// Test-only partial-state ctor.  Production rewrite paths use
    /// `pattern::RewriteCtx::new(&mut graph, entry)` (the `opt::with_rewrite_ctx`
    /// adapter is the primary consumer).  Remaining callers are `compact`'s
    /// test fixture and a few pattern test scaffolds that need
    /// `BuiltFunctionGraph` (e.g. to set call-other clobber lists via
    /// `set_call_other_clobbered_for_test`) without going through the build
    /// path.  Hidden from docs to discourage external adoption.
    #[doc(hidden)]
    #[must_use]
    pub fn from_graph_and_entry_for_rewrite(graph: crate::graph::Graph, entry: NodeId) -> Self {
        Self {
            graph,
            entry,
            variables: PrimaryMap::new(),
            call_clobbered: Box::new([]),
            ret_val_regs: Box::new([]),
            call_other_clobbered: Box::new([]),
            no_memory_clobber: false,
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

    /// Rebuilds the underlying [`crate::graph::Graph`] to retain only
    /// nodes reachable from [`Self::entry`] via
    /// [`crate::walk::walk_graph`].  `self.entry` is remapped through
    /// the returned [`crate::graph::NodeIdRemap`]; other fields
    /// (`variables`, `call_clobbered`, `ret_val_regs`) are vn-keyed
    /// and stay valid as-is.
    ///
    /// External callers that hold any pre-compaction `NodeId` /
    /// `NodeOutputId` / `NodeInputId` MUST rewrite them through the
    /// returned remap (or drop them).
    pub fn compact(&mut self) -> crate::graph::NodeIdRemap {
        let remap = self.graph.retain_reachable(self.entry);
        // `retain_reachable` walks forward from `entry`; the entry node
        // is reachable from itself by definition, so it is always in
        // the remap.  The expect cannot fire short of an internal
        // invariant violation in `retain_reachable`.
        #[allow(clippy::expect_used)]
        let new_entry = remap
            .node_old_to_new(self.entry)
            .expect("entry must survive its own compaction");
        self.entry = new_entry;
        remap
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
        let mut bfg = BuiltFunctionGraph::from_graph_and_entry_for_rewrite(graph, entry);
        let pre_count = bfg.graph.all_node_ids().count();

        let _remap = bfg.compact();

        let post_count = bfg.graph.all_node_ids().count();
        assert!(post_count < pre_count, "compact must shrink the graph");
        // entry was remapped; new entry id still has the Control output.
        let outs: Vec<_> = bfg.graph.node_outputs(bfg.entry).into_iter().collect();
        assert_eq!(outs.len(), 1);
        assert!(bfg.graph.output_kind(outs[0]).is_control());
    }
}
