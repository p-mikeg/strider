//! [`Function`] — a [`Graph`] plus per-function overlay state (`entry`,
//! `cc_metadata`, side tables).
//!
//! [`Graph`] holds structural state (nodes/edges/wide_const interning, dedup
//! cache).  [`Function`] holds the overlay that gives those nodes their
//! function-level meaning: which node is the entry, the calling convention
//! metadata, asm fingerprint attribution, and the other four `NodeId`-keyed
//! side tables.
//!
//! Passes that only need structure take `&Graph`; passes that need the overlay
//! (most opt passes, the validator, dot rendering) take `&Function` or
//! `&mut Function`.
//!
//! `Function` implements `Deref<Target = Graph>` and `DerefMut` so all
//! [`Graph`] methods are available on a `&Function` / `&mut Function`
//! without going through the explicit `.graph()` accessor.

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;

use crate::graph::{CcMetadata, Graph, NodeIdRemap, SideTableRemap};
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
    pub(crate) graph: Graph,
    entry: Option<NodeId>,
    /// Calling-convention metadata.  `None` before `FunctionBuilder::build`
    /// completes; `Some(_)` on every fully-built function returned to callers.
    cc_metadata: Option<CcMetadata>,

    // ── NodeId-keyed overlay tables ────────────────────────────────────────
    //
    // These four side tables hold per-function data that is keyed by NodeId
    // but is not part of the structural graph identity.  They are remapped
    // through [`NodeIdRemap`] by [`Self::compact`] whenever the arena is
    // compacted.

    /// User-op name resolved from Sleigh for [`crate::node::NodeKind::CallOther`]
    /// nodes.
    pub(crate) call_other_names: SecondaryMap<NodeId, Option<String>>,
    /// Per-node sorted-deduplicated list of machine-instruction addresses
    /// whose lifting or rewrite contributed to the node's value.
    pub(crate) asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>,
    /// Per-Call clobber-list override (shadows `CcMetadata::call_clobbered`
    /// for a specific call site).
    pub(crate) call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>,
    /// Source-level varnode tag for lift-time [`crate::node::NodeKind::Phi`]
    /// nodes.  `Some(vn)` = register-identity phi; `None` = anonymous phi.
    pub(crate) phi_var_tag: SecondaryMap<NodeId, Option<rsleigh::Vn>>,

    /// Maps each calling-convention argument index to the [`NodeId`](s) of the
    /// underlying carrier nodes: [`crate::node::NodeKind::InitialVar`] for
    /// register args, [`crate::node::NodeKind::Load`] for stack args.
    ///
    /// `Vec<NodeId>` per index because a stack slot may have multiple `Load`
    /// nodes at the same `sp+K` offset but different widths.  Register args
    /// have a `Vec` of size 1.
    ///
    /// Populated by `FunctionArgDetect`; empty until that pass runs.
    arg_index_to_nodes: FxHashMap<u32, Vec<NodeId>>,

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
            call_other_names: SecondaryMap::new(),
            asm_fingerprints: SecondaryMap::new(),
            call_clobbered_overrides: SecondaryMap::new(),
            phi_var_tag: SecondaryMap::new(),
            arg_index_to_nodes: FxHashMap::default(),
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

    // ── NodeId-keyed overlay accessors ────────────────────────────────────

    /// Returns the user-op name associated with a
    /// [`crate::node::NodeKind::CallOther`] node, or `None` if no name has
    /// been recorded for that node.
    #[inline]
    #[must_use]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.call_other_names[node_id].as_deref()
    }

    /// Associates a user-op name with a [`crate::node::NodeKind::CallOther`]
    /// node.  Replaces any prior value.
    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: String) {
        self.call_other_names[node_id] = Some(name);
    }

    /// Returns the source-level varnode tag for `node_id` if it is a
    /// [`crate::node::NodeKind::Phi`] created at lift time tracking a specific
    /// varnode, or `None` for anonymous phis (synthesised by opt passes) or
    /// non-phi nodes.
    #[inline]
    #[must_use]
    pub fn phi_var_tag(&self, node_id: NodeId) -> Option<rsleigh::Vn> {
        self.phi_var_tag[node_id]
    }

    /// Sets the source-level varnode tag for `node_id`.  Callers must
    /// guarantee that `node_id`'s kind is [`crate::node::NodeKind::Phi`].
    #[inline]
    pub fn set_phi_var_tag(&mut self, node_id: NodeId, vn: rsleigh::Vn) {
        self.phi_var_tag[node_id] = Some(vn);
    }

    /// Returns the per-Call clobber-list override for `node_id`, or `None`
    /// if the Call uses the function-default
    /// [`CcMetadata::call_clobbered`].
    #[inline]
    #[must_use]
    pub fn call_clobbered_override(&self, node_id: NodeId) -> Option<&[rsleigh::Vn]> {
        self.call_clobbered_overrides[node_id].as_deref()
    }

    /// Records `clobbered` as the per-Call clobber-list override for
    /// `node_id`.  Replaces any prior value.
    #[inline]
    pub fn set_call_clobbered_override(&mut self, node_id: NodeId, clobbered: Vec<rsleigh::Vn>) {
        self.call_clobbered_overrides[node_id] = Some(clobbered);
    }

    // ── arg_index_to_nodes accessors ─────────────────────────────────────

    /// All [`NodeId`]s registered as carriers for argument `index`.
    ///
    /// Returns `&[]` if no nodes have been registered for that index.
    /// Register args have a slice of length 1; stack args may have multiple
    /// entries (different-width [`crate::node::NodeKind::Load`]s at the same
    /// `sp+K` offset).
    #[inline]
    #[must_use]
    pub fn arg_index_to_nodes(&self, index: u32) -> &[NodeId] {
        self.arg_index_to_nodes
            .get(&index)
            .map_or(&[], Vec::as_slice)
    }

    /// Register `node` as the underlying carrier for argument `index`.
    ///
    /// Appends to the per-index `Vec`; multiple nodes per index are allowed
    /// (the stack-args case may register multiple `Load`s at different widths
    /// for the same offset).
    #[inline]
    pub fn register_arg_node(&mut self, index: u32, node: NodeId) {
        self.arg_index_to_nodes
            .entry(index)
            .or_default()
            .push(node);
    }

    /// Iterate over all registered argument indices (unordered).
    #[inline]
    pub fn arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.arg_index_to_nodes.keys().copied()
    }

    /// Returns the asm-instruction-address fingerprint of `node_id` as a
    /// sorted-deduplicated slice.  Returns an empty slice when no
    /// contributors have been recorded.
    #[inline]
    #[must_use]
    pub fn asm_fingerprint(&self, id: NodeId) -> &[u64] {
        self.asm_fingerprints[id].as_slice()
    }

    /// Replaces `node_id`'s fingerprint with `addrs`.
    ///
    /// Sorts and deduplicates `addrs` first so callers cannot accidentally
    /// install an unsorted entry.  This is the test-only / synthetic-graph
    /// entry point: production passes use
    /// [`Self::extend_asm_fingerprint`] / [`Self::extend_asm_fingerprint_from`]
    /// to preserve the superset-only invariant.
    #[inline]
    pub fn set_asm_fingerprint(&mut self, id: NodeId, mut addrs: Vec<u64>) {
        addrs.sort_unstable();
        addrs.dedup();
        self.asm_fingerprints[id] = addrs;
    }

    /// Unions `contributors` into `node_id`'s fingerprint.  Result is kept
    /// sorted and deduplicated.  Existing entries are never removed: this
    /// satisfies the no-shrink contract.  Empty `contributors` is a no-op.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        if contributors.is_empty() {
            return;
        }
        let existing = &mut self.asm_fingerprints[node_id];
        let mut needs_resort = false;
        for &addr in contributors {
            match existing.last() {
                None => existing.push(addr),
                Some(&last) if addr > last => existing.push(addr),
                Some(&last) if addr == last => { /* already present */ }
                Some(_) => {
                    existing.push(addr);
                    needs_resort = true;
                }
            }
        }
        if needs_resort {
            existing.sort_unstable();
            existing.dedup();
        }
    }

    /// Unions the fingerprint of `src` into `dst`.  Self-extension
    /// (`src == dst`) is a no-op.
    pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        let src_slice: smallvec::SmallVec<[u64; 4]> =
            self.asm_fingerprints[src].iter().copied().collect();
        self.extend_asm_fingerprint(dst, &src_slice);
    }

    /// Same as [`Graph::create_node`] plus unions the asm-fingerprint of
    /// every node in `contributors` into the resulting node.
    pub fn create_node_attributed(
        &mut self,
        kind: crate::node::NodeKind,
        inputs: impl IntoIterator<Item = crate::node::NodeOutputId>,
        output_kinds: impl IntoIterator<Item = crate::node::NodeOutputKind>,
        contributors: &[NodeId],
    ) -> NodeId {
        let node_id = self.graph.create_node(kind, inputs, output_kinds);
        for &src in contributors {
            self.extend_asm_fingerprint_from(node_id, src);
        }
        node_id
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
    /// is updated to the new id.  All five `NodeId`-keyed overlay tables are
    /// remapped through the same translation.
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
        // Remap the four NodeId-keyed overlay tables through the
        // old→new translation table produced by `retain_reachable`.
        self.call_other_names.remap_node_keyed(&remap);
        self.asm_fingerprints.remap_node_keyed(&remap);
        self.call_clobbered_overrides.remap_node_keyed(&remap);
        self.phi_var_tag.remap_node_keyed(&remap);
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
        let node_to_arg_indices = crate::graph_dot::build_arg_reverse_map(self);
        Ok(crate::graph_dot::GraphDotDumper {
            entry,
            function: self,
            sleigh,
            call_clobbered: &cc.call_clobbered,
            ret_val_regs: &cc.ret_val_regs,
            node_filter: None,
            node_to_arg_indices,
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

    #[test]
    fn arg_index_to_nodes_returns_empty_for_unregistered() {
        let f = Function::new();
        assert!(f.arg_index_to_nodes(0).is_empty());
        assert!(f.arg_index_to_nodes(99).is_empty());
    }

    #[test]
    fn register_arg_node_supports_multiple_nodes_per_index() {
        let mut f = Function::new();
        let n1 = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let n2 = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);

        // Register two NodeIds for arg index 3 (the stack-args multi-Load case).
        f.register_arg_node(3, n1);
        f.register_arg_node(3, n2);

        let nodes = f.arg_index_to_nodes(3);
        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains(&n1));
        assert!(nodes.contains(&n2));

        // arg_indices contains the registered index.
        assert!(f.arg_indices().any(|i| i == 3));
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
