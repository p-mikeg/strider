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
use crate::node::{NodeId, NodeOutputId};

/// Per-node varnode-flavoured metadata.  A single side-table on
/// [`Function`] (`vn_meta`) carries one of these per `NodeId` that
/// needs it; the variants are mutually exclusive because they apply
/// to disjoint node kinds (`Phi` vs `Call`).
#[derive(Debug, Clone)]
pub(crate) enum NodeVnMeta {
    /// Source-level varnode tag for a lift-time
    /// [`crate::node::NodeKind::Phi`] tracking a specific varnode.
    /// `None`-typed entries (the `Option` outer layer in
    /// `vn_meta: SecondaryMap<NodeId, Option<NodeVnMeta>>`) represent
    /// anonymous phis synthesised by opt passes.
    PhiVar(rsleigh::Vn),
    /// Per-[`crate::node::NodeKind::Call`] clobber-list override
    /// shadowing the function-default [`CcMetadata::call_clobbered`]
    /// for one call site.
    CallClobber(Vec<rsleigh::Vn>),
}

/// A lifted function: structural [`Graph`] plus per-function overlay state.
///
/// `FunctionBuilder::build` is the canonical constructor.  For synthetic /
/// test graphs, use [`Function::new`] and populate via [`Function::graph_mut`]
/// and [`Function::set_entry`].
///
/// `Function` derefs to `Graph`, so all [`Graph`] read accessors (e.g.
/// `node_kind`, `walk_from`, `all_node_ids`) are available directly on a
/// `&Function`.
#[derive(Default)]
pub struct Function {
    pub(crate) graph: Graph,
    entry: Option<NodeId>,
    /// Calling-convention metadata.  Populated incrementally during
    /// `FunctionBuilder` construction; always present (possibly with
    /// every field empty / default) on any `Function` value.
    pub(crate) cc_metadata: CcMetadata,

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
    // `SmallVec<[u64; 2]>` because the common case is 1–2 contributor
    // addresses per node.  Inlining those avoids a heap allocation per
    // non-empty entry — on graphs with thousands of nodes this drops
    // thousands of small allocations from the lift+optimize pipeline.
    // The wider lifter contract (`set_asm_fingerprint`,
    // `extend_asm_fingerprint`) keeps using `&[u64]` /
    // `impl IntoIterator<Item = u64>` so callers are unaffected.
    pub(crate) asm_fingerprints:
        SecondaryMap<NodeId, smallvec::SmallVec<[u64; 2]>>,
    /// Per-node varnode-flavoured metadata.  A single [`NodeVnMeta`]
    /// covers both the lift-time `Phi` varnode tag and the per-Call
    /// clobber-list override; the two never apply to the same `NodeId`
    /// (a `Phi` is never a `Call`), so a single-tag enum is sound and
    /// halves the per-`NodeId` overlay footprint.
    pub(crate) vn_meta: SecondaryMap<NodeId, Option<NodeVnMeta>>,
    /// Per-Call override of stack-arg offsets when the orchestrator
    /// pre-resolved a per-address CC override.  `None` (or no entry)
    /// means use the function-default CC's offsets.
    pub(crate) call_stack_arg_offsets_overrides: SecondaryMap<NodeId, Option<Vec<i64>>>,

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

    /// Stack slot for Store/Load nodes whose address decomposes to
    /// `base + K` for a single concrete `K`, where `base` is the SP-derived
    /// terminal node (`InitialVar(sp)` or an alignment-masked `sp & -16`).
    /// Stored as `(base, K)`: the offset `K` is only meaningful relative to
    /// its `base`, and two accesses are the same slot iff they share both.
    /// Populated by the `StackOffsetDetect` classifier.  The phi-of-offsets
    /// case (address is a phi of different constants per branch) is not
    /// recorded — consumers can re-decompose via `decompose_sp` if needed.
    stack_offsets: SecondaryMap<NodeId, Option<(NodeOutputId, i64)>>,

    /// O(1) varnode → `InitialVar(vn)` node-id accelerator for
    /// indirect-resolve sites and the lifter's lazy `read_or_init_var`
    /// fallback.  Maintained at every canonical `InitialVar`
    /// creation site (the lift-time path and the orchestrator
    /// fallback) and remapped through [`NodeIdRemap`] by
    /// [`Self::compact`].
    initial_var_index: FxHashMap<rsleigh::Vn, NodeId>,
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

    /// Read-only access to the calling-convention metadata.  Always
    /// present (possibly empty / default).
    #[inline]
    #[must_use]
    pub fn cc_metadata(&self) -> &CcMetadata {
        &self.cc_metadata
    }

    /// Mutable access to the calling-convention metadata.  Used by
    /// [`crate::FunctionBuilder`] to write through during lift.
    #[inline]
    pub fn cc_metadata_mut(&mut self) -> &mut CcMetadata {
        &mut self.cc_metadata
    }

    /// Read the calling convention's call-clobbered varnode list.
    #[inline]
    #[must_use]
    pub fn call_clobbered_regs(&self) -> &[rsleigh::Vn] {
        &self.cc_metadata.call_clobbered
    }

    /// Read the calling convention's combined return-value register
    /// list (integer + float, in ABI order).
    #[inline]
    #[must_use]
    pub fn ret_val_regs(&self) -> &[rsleigh::Vn] {
        &self.cc_metadata.ret_val_regs
    }

    /// Function-default `preserves_memory` flag.  Delegates to the
    /// embedded calling convention; defaults to `false` for synthetic
    /// functions built without one.
    #[inline]
    #[must_use]
    pub(crate) fn preserves_memory(&self) -> bool {
        self.cc_metadata.cc.as_ref().is_some_and(|c| c.preserves_memory)
    }

    /// Calling convention's stack-pointer varnode, or `None` for
    /// synthetic test functions that don't model an SP.
    #[inline]
    #[must_use]
    pub(crate) fn stack_vn(&self) -> Option<rsleigh::Vn> {
        self.cc_metadata.cc.as_ref().map(|c| c.stack_vn)
    }

    /// Net byte change the callee's `ret` inflicts on the caller's
    /// stack pointer.  `0` on link-register ISAs and on synthetic
    /// functions built without a CC.
    #[inline]
    #[must_use]
    pub(crate) fn ret_stack_pop(&self) -> i64 {
        self.cc_metadata.cc.as_ref().map_or(0, |c| c.ret_stack_pop)
    }

    /// Read the function-default CallOther clobber list.
    #[inline]
    #[must_use]
    pub fn call_other_clobbered_regs(&self) -> &[rsleigh::Vn] {
        &self.cc_metadata.call_other_clobbered
    }

    /// Iterate the function's tracked varnodes in `VarId` (insertion) order.
    #[inline]
    pub fn tracked_vns(&self) -> impl Iterator<Item = rsleigh::Vn> + '_ {
        self.cc_metadata.var_table.values().copied()
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
        match &self.vn_meta[node_id] {
            Some(NodeVnMeta::PhiVar(vn)) => Some(*vn),
            _ => None,
        }
    }

    /// Sets the source-level varnode tag for `node_id`.  Callers must
    /// guarantee that `node_id`'s kind is [`crate::node::NodeKind::Phi`].
    #[inline]
    pub fn set_phi_var_tag(&mut self, node_id: NodeId, vn: rsleigh::Vn) {
        self.vn_meta[node_id] = Some(NodeVnMeta::PhiVar(vn));
    }

    /// Returns the per-Call clobber-list override for `node_id`, or `None`
    /// if the Call uses the function-default
    /// `CcMetadata::call_clobbered`.
    #[inline]
    #[must_use]
    pub fn call_clobbered_override(&self, node_id: NodeId) -> Option<&[rsleigh::Vn]> {
        match &self.vn_meta[node_id] {
            Some(NodeVnMeta::CallClobber(c)) => Some(c.as_slice()),
            _ => None,
        }
    }

    /// Records `clobbered` as the per-Call clobber-list override for
    /// `node_id`.  Replaces any prior value.
    #[inline]
    pub fn set_call_clobbered_override(&mut self, node_id: NodeId, clobbered: Vec<rsleigh::Vn>) {
        self.vn_meta[node_id] = Some(NodeVnMeta::CallClobber(clobbered));
    }

    /// Returns the per-Call stack-arg offsets override for `node_id`, or
    /// `None` if the Call uses the function-default CC's stack-arg offsets.
    #[inline]
    #[must_use]
    pub fn call_stack_arg_offsets_override(&self, node_id: NodeId) -> Option<&[i64]> {
        self.call_stack_arg_offsets_overrides[node_id].as_deref()
    }

    /// Records `offsets` as the per-Call stack-arg offsets override for
    /// `node_id`.  Replaces any prior value.
    #[inline]
    pub fn set_call_stack_arg_offsets_override(&mut self, node_id: NodeId, offsets: Vec<i64>) {
        self.call_stack_arg_offsets_overrides[node_id] = Some(offsets);
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
    pub fn iter_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.arg_index_to_nodes.keys().copied()
    }

    /// Drop every registered argument carrier.
    ///
    /// Lets the arg-detection pass rebuild the side-table idempotently from
    /// the live graph: it can be re-run on the same `Function` (e.g. on each
    /// stable iteration of the orchestrator's fixed-point loop) without
    /// accumulating duplicate carrier ids.
    #[inline]
    pub fn clear_arg_nodes(&mut self) {
        self.arg_index_to_nodes.clear();
    }

    // ── stack_offsets accessors ───────────────────────────────────────────

    /// Returns the stack slot `(base, offset)` recorded for a Store/Load
    /// node, or `None` if the node has no recorded slot (non-stack node, or
    /// a phi-of-offsets address whose single concrete offset cannot be
    /// named).  `base` is the SP-derived terminal node the offset is
    /// relative to; the offset is only comparable against another access's
    /// offset when their bases match.
    #[must_use]
    #[inline]
    pub fn stack_offset(&self, id: NodeId) -> Option<(NodeOutputId, i64)> {
        self.stack_offsets[id]
    }

    /// Records a concrete stack slot `(base, offset)` for a Store/Load node.
    #[inline]
    pub fn set_stack_offset(&mut self, id: NodeId, base: NodeOutputId, offset: i64) {
        self.stack_offsets[id] = Some((base, offset));
    }

    /// Iterates over all `(NodeId, base, offset)` triples in the side-table.
    #[inline]
    pub fn stack_offsets(&self) -> impl Iterator<Item = (NodeId, NodeOutputId, i64)> + '_ {
        self.stack_offsets
            .iter()
            .filter_map(|(id, slot)| slot.map(|(base, off)| (id, base, off)))
    }

    // ── initial_var_index accessors ───────────────────────────────────────

    /// Returns the [`NodeId`] of the canonical `InitialVar(vn)` node for
    /// `vn`, or `None` if none is registered.  O(1).
    ///
    /// Callers that want to skip detached zombie nodes must validate the
    /// returned id themselves (typically by checking that the node's
    /// single output's use-list is non-empty via [`Graph::output_uses`]).
    #[inline]
    #[must_use]
    pub fn initial_var_for(&self, vn: rsleigh::Vn) -> Option<NodeId> {
        self.initial_var_index.get(&vn).copied()
    }

    /// Registers `(vn, node_id)` in the `InitialVar` index.  Replaces
    /// any prior entry for `vn`.  Callers must guarantee that
    /// `node_id`'s kind is `NodeKind::InitialVar(vn)` — the index is
    /// advisory and never re-checked.
    #[inline]
    pub fn register_initial_var(&mut self, vn: rsleigh::Vn, node_id: NodeId) {
        self.initial_var_index.insert(vn, node_id);
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
        self.asm_fingerprints[id] = addrs.into_iter().collect();
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
    pub fn walk(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph_opt(&self.graph, self.entry)
    }

    /// Returns the entry-reachable nodes in **global reverse-post-order**
    /// (entry-first), filtered to those whose [`crate::node::NodeKind`]
    /// satisfies `pred`.
    ///
    /// Derives the entry from [`Self::entry`]; yields an empty iterator
    /// when the entry has not yet been set.  The reachable SET is the
    /// same as [`Self::walk`]'s; only the ORDER is canonicalised to RPO
    /// (every producer precedes its consumers), so passes that seed a
    /// worklist or scan in this order see operands before consumers.
    pub fn rpo_filter<'a>(
        &'a self,
        pred: impl Fn(&crate::node::NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        crate::walk::rpo_reachable_opt(&self.graph, self.entry)
            .into_iter()
            .filter(move |&n| pred(self.graph.node_kind(n)))
    }

    /// Reachable preorder filtered by a predicate over the node's kind.
    pub fn walk_kind<'a, P>(
        &'a self,
        mut pred: P,
    ) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&crate::node::NodeKind) -> bool + 'a,
    {
        self.walk()
            .filter(move |&n| pred(self.graph.node_kind(n)))
    }

    /// Counts reachable nodes whose [`crate::node::NodeKind`] satisfies
    /// `predicate`.  Walks in pre-order from [`Self::entry`].
    pub fn count_kind<F: Fn(&crate::node::NodeKind) -> bool>(&self, predicate: F) -> usize {
        self.walk()
            .filter(|nid| predicate(self.graph.node_kind(*nid)))
            .count()
    }

    /// Returns `true` when at least one reachable node satisfies
    /// `predicate`.  Short-circuits at the first match.
    pub fn has_kind<F: Fn(&crate::node::NodeKind) -> bool>(&self, predicate: F) -> bool {
        self.walk().any(|nid| predicate(self.graph.node_kind(nid)))
    }

    /// Rebuilds the function's graph to retain only nodes reachable from
    /// [`Self::entry`].  The entry node id is remapped; the stored entry
    /// is updated to the new id.  Every `NodeId`-keyed overlay table
    /// (the `SecondaryMap` side-tables, `initial_var_index`, and
    /// `arg_index_to_nodes`) is remapped through the same translation;
    /// entries whose node did not survive compaction are dropped.
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
        // Remap the NodeId-keyed overlay tables through the
        // old→new translation table produced by `retain_reachable`.
        self.call_other_names.remap_node_keyed(&remap);
        self.asm_fingerprints.remap_node_keyed(&remap);
        self.vn_meta.remap_node_keyed(&remap);
        self.call_stack_arg_offsets_overrides.remap_node_keyed(&remap);
        // `stack_offsets` is the only NodeId-keyed side-table whose VALUE
        // also references a node — the slot `base` (a `NodeOutputId`).  So
        // remap both the key (NodeId) and the value's base through the same
        // translation table.  An entry whose node or base didn't survive
        // compaction is dropped (the slot becomes "unknown", which is safe —
        // consumers treat a missing entry as non-stack).
        let mut new_stack_offsets: SecondaryMap<NodeId, Option<(NodeOutputId, i64)>> =
            SecondaryMap::new();
        for (old_id, slot) in self.stack_offsets.iter() {
            let Some((old_base, off)) = *slot else {
                continue;
            };
            if let (Some(new_id), Some(new_base)) = (
                remap.node_old_to_new(old_id),
                remap.output_old_to_new(old_base),
            ) {
                new_stack_offsets[new_id] = Some((new_base, off));
            }
        }
        self.stack_offsets = new_stack_offsets;
        // `initial_var_index` is `FxHashMap<Vn, NodeId>` — Vn-keyed, not
        // NodeId-keyed, so the standard `SecondaryMap` remap helper
        // doesn't fit.  Entries whose NodeId didn't survive compaction
        // (the InitialVar became unreachable and was dropped) are
        // silently elided — the orchestrator's `read_or_init_var`
        // fallback will lazily re-create them as needed.
        let mut new_index: FxHashMap<rsleigh::Vn, NodeId> =
            FxHashMap::with_capacity_and_hasher(self.initial_var_index.len(), Default::default());
        for (vn, old_id) in self.initial_var_index.drain() {
            if let Some(new_id) = remap.node_old_to_new(old_id) {
                new_index.insert(vn, new_id);
            }
        }
        self.initial_var_index = new_index;
        // `arg_index_to_nodes` is `FxHashMap<u32, Vec<NodeId>>` — index-keyed
        // with NodeId payloads, so (like `initial_var_index`) it needs an
        // inline remap.  Carrier ids whose node didn't survive compaction are
        // dropped; an index whose carriers all vanished is removed entirely.
        let mut new_arg_index: FxHashMap<u32, Vec<NodeId>> =
            FxHashMap::with_capacity_and_hasher(self.arg_index_to_nodes.len(), Default::default());
        for (index, old_ids) in self.arg_index_to_nodes.drain() {
            let mapped: Vec<NodeId> = old_ids
                .into_iter()
                .filter_map(|old_id| remap.node_old_to_new(old_id))
                .collect();
            if !mapped.is_empty() {
                new_arg_index.insert(index, mapped);
            }
        }
        self.arg_index_to_nodes = new_arg_index;
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
    ) -> crate::Result<crate::function_dot::FunctionDotDumper<'a, R>> {
        let entry = self.entry.ok_or_else(|| {
            anyhow::anyhow!("Function::dot_dumper: entry node is not set")
        })?;
        let node_to_arg_indices = crate::function_dot::build_arg_reverse_map(self);
        Ok(crate::function_dot::FunctionDotDumper {
            entry,
            function: self,
            sleigh,
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
            .create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

        // Register two NodeIds for arg index 3 (the stack-args multi-Load case).
        f.register_arg_node(3, n1);
        f.register_arg_node(3, n2);

        let nodes = f.arg_index_to_nodes(3);
        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains(&n1));
        assert!(nodes.contains(&n2));

        // iter_arg_indices contains the registered index.
        assert!(f.iter_arg_indices().any(|i| i == 3));
    }
}

#[cfg(test)]
mod compact_tests {
    #![allow(clippy::unwrap_used)]

    use super::Function;
    use crate::graph::CcMetadata;
    use crate::node::{NodeKind, NodeOutputKind};

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut f = Function::new();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(0xdead),
            [],
            [NodeOutputKind::OutputType(crate::node::NodeOutputType::I64)],
        );
        f.set_entry(entry);
        f.cc_metadata = CcMetadata {
            var_table: crate::graph::VarTable::default(),
            call_clobbered: Vec::new(),
            ret_val_regs: Vec::new(),
            call_other_clobbered: Vec::new(),
            arg_passing_vars: Vec::new(),
            cc: None,
        };
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

    /// Asm-fingerprints survive compaction on every reachable node.
    /// Regression guard: a node remap must carry the fingerprint side-
    /// table through to its new NodeId.  Otherwise pattern queries
    /// against optimised IR lose contributor-asm attribution for any
    /// surviving node whose id was remapped.
    #[test]
    fn retain_reachable_preserves_asm_fingerprint_on_surviving_node() {
        use crate::node::NodeOutputType;

        let mut f = Function::new();
        f.cc_metadata = CcMetadata {
            var_table: crate::graph::VarTable::default(),
            call_clobbered: Vec::new(),
            ret_val_regs: Vec::new(),
            call_other_clobbered: Vec::new(),
            arg_passing_vars: Vec::new(),
            cc: None,
        };
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = f.node_outputs_exact::<1>(mem).unwrap();
        // Reachable IntConst whose Return-input consumer keeps it live.
        let surviving = f.graph_mut().create_node(
            NodeKind::IntConst(0xCAFE_u128),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        let [surv_out] = f.node_outputs_exact::<1>(surviving).unwrap();
        let _ret = f.graph_mut().create_node(
            NodeKind::Return,
            [entry_ctrl, mem_out, surv_out],
            [],
        );
        f.set_entry(entry);

        // Stamp three asm addresses on the surviving IntConst before compact.
        f.set_asm_fingerprint(surviving, vec![0x1000, 0x1004, 0x1008]);

        let remap = f.compact().expect("compact must succeed");
        let new_id = remap
            .node_old_to_new(surviving)
            .expect("surviving IntConst must remain after compact");
        assert_eq!(
            f.asm_fingerprint(new_id),
            &[0x1000, 0x1004, 0x1008],
            "surviving node's asm-fingerprint must transfer to its post-compact NodeId"
        );
    }

    /// A cacheable zombie node that has no live uses must be absent after
    /// `Function::compact`.  Regression guard against compaction skipping
    /// detached-but-still-arena-present nodes.
    #[test]
    fn retain_reachable_drops_zombie_node() {
        use crate::node::NodeOutputType;
        use crate::graph::NodeIdRemap;

        let mut f = Function::new();
        f.cc_metadata = CcMetadata {
            var_table: crate::graph::VarTable::default(),
            call_clobbered: Vec::new(),
            ret_val_regs: Vec::new(),
            call_other_clobbered: Vec::new(),
            arg_passing_vars: Vec::new(),
            cc: None,
        };
        // Entry + InitialMemory + a Return (minimal reachable graph).
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_out], []);
        f.set_entry(entry);

        // Zombie: a cacheable IntConst not connected to anything reachable.
        let zombie = f.graph_mut().create_node(
            NodeKind::IntConst(0xC0FFEE_u64 as u128),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );

        // Zombie must be in the arena before compact.
        let pre_ids: Vec<_> = f.all_node_ids().collect();
        assert!(pre_ids.contains(&zombie), "zombie must be present before compact");

        let _remap: NodeIdRemap = f.compact().expect("compact must succeed");

        // After compact the zombie NodeId is invalid; verify by checking
        // that the remap returns None for it (it was dropped).
        assert!(_remap.node_old_to_new(zombie).is_none(), "zombie must be dropped by compact");
        // Node count must decrease.
        assert!(
            f.all_node_ids().count() < pre_ids.len(),
            "compact must remove unreachable nodes"
        );
    }

    /// The `phi_var_tag` and `stack_offsets` side-tables must NOT contain
    /// stale entries pointing to zombie (dropped) NodeIds after compaction.
    #[test]
    fn retain_reachable_drops_side_table_entry_for_dropped_node() {
        use crate::node::NodeOutputType;

        let mut f = Function::new();
        f.cc_metadata = CcMetadata {
            var_table: crate::graph::VarTable::default(),
            call_clobbered: Vec::new(),
            ret_val_regs: Vec::new(),
            call_other_clobbered: Vec::new(),
            arg_passing_vars: Vec::new(),
            cc: None,
        };
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = f.node_outputs_exact::<1>(mem).unwrap();
        let _ret = f.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_out], []);
        f.set_entry(entry);

        // Zombie Phi node with a phi_var_tag entry.
        let zombie_phi = f.graph_mut().create_node(
            NodeKind::Phi,
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        let dead_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x88,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        f.set_phi_var_tag(zombie_phi, dead_vn);
        assert_eq!(
            f.phi_var_tag(zombie_phi),
            Some(dead_vn),
            "tag must be set before compact"
        );

        // Zombie IntConst node with a stack_offsets entry.
        let zombie_stack = f.graph_mut().create_node(
            NodeKind::IntConst(0xBEEF_u64 as u128),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        let zombie_out = f.node_outputs(zombie_stack).iter().copied().next().unwrap();
        f.set_stack_offset(zombie_stack, zombie_out, -8);
        assert_eq!(
            f.stack_offset(zombie_stack),
            Some((zombie_out, -8)),
            "offset must be set before compact"
        );

        let remap = f.compact().expect("compact must succeed");

        // Both zombies must have been dropped.
        assert!(remap.node_old_to_new(zombie_phi).is_none());
        assert!(remap.node_old_to_new(zombie_stack).is_none());

        // Side-table entries for dropped nodes must not exist.
        // After compact the old NodeIds are invalid; the secondary maps
        // were rebuilt over only surviving nodes, so querying the OLD id
        // would index into a fresh map that has no entry for that slot
        // (secondary maps default-initialise to the Default::default() which
        // is None for Option<Vn> / None for Option<i64>).
        //
        // `phi_var_tag` and `stack_offset` use SecondaryMap<NodeId, Option<_>>;
        // after remap the old zombie ids are not present in the new map.
        // We verify indirectly: neither surviving node carries the tag/offset.
        let surviving_with_tag = f
            .all_node_ids()
            .any(|n| f.phi_var_tag(n) == Some(dead_vn));
        assert!(
            !surviving_with_tag,
            "dead_vn phi_var_tag must not survive compaction"
        );
        let surviving_with_offset = f
            .all_node_ids()
            .any(|n| f.stack_offset(n).map(|(_, o)| o) == Some(-8));
        assert!(
            !surviving_with_offset,
            "stack_offset -8 must not survive compaction on a surviving node"
        );
    }

    /// The `arg_index_to_nodes` side-table must be remapped through the
    /// compaction translation, like every other `NodeId`-keyed overlay.
    /// Regression guard: the orchestrator's default finalize path runs the
    /// destructive pipeline (which removes nodes) and then `compact()`,
    /// while `FunctionArgDetect` (the pass that populates
    /// `arg_index_to_nodes`) runs only in the *stable* pipeline — so the
    /// carrier ids stored before compaction must be translated to their
    /// post-compaction ids, otherwise `function_arg(N)` pattern queries and
    /// dot rendering read stale / aliased NodeIds.
    #[test]
    fn compact_remaps_arg_index_to_nodes() {
        use crate::node::NodeOutputType;

        let mut f = Function::new();
        f.cc_metadata = CcMetadata {
            var_table: crate::graph::VarTable::default(),
            call_clobbered: Vec::new(),
            ret_val_regs: Vec::new(),
            call_other_clobbered: Vec::new(),
            arg_passing_vars: Vec::new(),
            cc: None,
        };
        let entry = f.graph_mut().create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        // A zombie created *before* the arg carrier so that compaction
        // reassigns the carrier's NodeId (the zombie's slot is dropped).
        let _zombie = f.graph_mut().create_node(
            NodeKind::IntConst(0xDEAD_u128),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        // The arg carrier: a register-arg-style InitialVar kept live by Return.
        let arg_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let arg_node = f.graph_mut().create_node(
            NodeKind::InitialVar(arg_vn),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = f.node_outputs_exact::<1>(mem).unwrap();
        let [arg_out] = f.node_outputs_exact::<1>(arg_node).unwrap();
        let _ret = f
            .graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, mem_out, arg_out], []);
        f.set_entry(entry);
        f.register_arg_node(0, arg_node);

        let remap = f.compact().expect("compact must succeed");
        let new_arg = remap
            .node_old_to_new(arg_node)
            .expect("the live arg carrier must survive compaction");

        assert_eq!(
            f.arg_index_to_nodes(0),
            &[new_arg],
            "arg_index_to_nodes must carry the carrier's post-compaction NodeId"
        );
        // Every stored carrier id must be a live node in the compacted graph.
        for &id in f.arg_index_to_nodes(0) {
            assert!(
                f.all_node_ids().any(|n| n == id),
                "arg carrier id {id:?} must be a live post-compaction node"
            );
        }
    }
}
