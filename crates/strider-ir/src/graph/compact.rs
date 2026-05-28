//! `Graph::retain_reachable` — compact the IR arena down to nodes
//! reachable from `entry` via [`Graph::walk_from`] (control-out +
//! data-in), returning the old→new id translation table so external
//! callers can fix up any ids they hold.
//!
//! The four `NodeId`-keyed side tables (`call_other_names`,
//! `asm_fingerprints`, `call_clobbered_overrides`, `phi_var_tag`) live
//! on [`crate::Function`], not on `Graph`.
//! `Graph::retain_reachable` only compacts the structural arena;
//! [`crate::Function::compact`] applies the returned [`NodeIdRemap`] to
//! all four overlay tables via [`SideTableRemap::remap_node_keyed`].

use cranelift_entity::{ListPool, PrimaryMap, SecondaryMap};

use crate::node::{
    Node, NodeId, NodeInput, NodeInputId, NodeInputIdList, NodeOutput, NodeOutputId,
    NodeOutputIdList, NodeOutputKind,
};
use super::Graph;

/// Remap-in-place trait for `SecondaryMap<NodeId, _>`-shaped side-tables.
///
/// Implementors expose a single method that rebuilds the table under the
/// old→new translation, draining the source via `std::mem::take` so the
/// post-remap source is left at `Default::default()` for every slot.
/// Used by [`crate::Function::compact`] to fold every `NodeId`-keyed
/// overlay table through one iteration site.
///
/// The Vn-keyed `initial_var_index` does **not** fit this shape (its
/// key is `rsleigh::Vn`, not `NodeId`) and is remapped inline in
/// `Graph::retain_reachable`.
pub(crate) trait SideTableRemap {
    fn remap_node_keyed(&mut self, remap: &NodeIdRemap);
}

impl<T: Default + Clone> SideTableRemap for SecondaryMap<NodeId, T> {
    fn remap_node_keyed(&mut self, remap: &NodeIdRemap) {
        let mut dst: SecondaryMap<NodeId, T> = SecondaryMap::new();
        for (old_id, new_id) in remap
            .nodes
            .iter()
            .filter_map(|(o, new)| new.map(|n| (o, n)))
        {
            dst[new_id] = std::mem::take(&mut self[old_id]);
        }
        *self = dst;
    }
}

/// Old→new id translation table produced by
/// [`Graph::retain_reachable`].  Sparse: only entries for surviving
/// ids are populated; dropped ids return `None`.
#[derive(Debug, Clone, Default)]
pub struct NodeIdRemap {
    nodes: SecondaryMap<NodeId, Option<NodeId>>,
    outputs: SecondaryMap<NodeOutputId, Option<NodeOutputId>>,
    inputs: SecondaryMap<NodeInputId, Option<NodeInputId>>,
}

impl NodeIdRemap {
    /// Returns the post-compaction `NodeId` for `old`, or `None` if
    /// `old` was unreachable and dropped.
    #[inline]
    #[must_use]
    pub fn node_old_to_new(&self, old: NodeId) -> Option<NodeId> {
        self.nodes[old]
    }

    /// Returns the post-compaction `NodeOutputId` for `old`, or
    /// `None` if `old`'s producing node was dropped.  Used by
    /// `Function::compact` to remap the `stack_offsets` side-table's slot
    /// `base` (a `NodeOutputId` stored in the value), the one side-table
    /// whose value references a node.
    #[inline]
    #[must_use]
    pub(crate) fn output_old_to_new(&self, old: NodeOutputId) -> Option<NodeOutputId> {
        self.outputs[old]
    }

    /// Returns the post-compaction `NodeInputId` for `old`, or `None`
    /// if `old`'s consuming node was dropped.  Test-only (same
    /// rationale as [`Self::output_old_to_new`]).
    #[cfg(test)]
    #[inline]
    #[must_use]
    pub(crate) fn input_old_to_new(&self, old: NodeInputId) -> Option<NodeInputId> {
        self.inputs[old]
    }
}

impl Graph {
    /// Rebuilds the arena to retain only nodes reachable from `entry`
    /// via [`Graph::walk_from`] (control-out forward + data-in
    /// backward).  Returns the old→new id translation table.
    ///
    /// Pre-compaction `NodeId` / `NodeOutputId` / `NodeInputId` values
    /// are invalidated by this call.  Callers that hold any such ids
    /// MUST rewrite them through the returned [`NodeIdRemap`] (or
    /// drop them).
    ///
    /// The dedup cache is rebuilt from scratch.  The four `NodeId`-keyed
    /// overlay tables (`call_other_names`, `asm_fingerprints`,
    /// `call_clobbered_overrides`, `phi_var_tag`) live on
    /// [`crate::Function`], not on `Graph`.  The caller
    /// ([`crate::Function::compact`]) applies the returned [`NodeIdRemap`]
    /// to those tables after this method returns.
    ///
    /// # Errors
    ///
    /// Returns an error if the two-pass remap invariant is violated.
    /// By construction this cannot fire — pass 1 installs every
    /// reachable node into `remap.nodes`, and pass 2 iterates the
    /// same `reachable` set — but propagating as `Err` rather than
    /// panicking keeps every error path typed so Python users see a
    /// clean exception.
    pub fn retain_reachable(&mut self, entry: NodeId) -> crate::Result<NodeIdRemap> {
        // 0. Bump the generation counter.  Every pre-call NodeId /
        // NodeOutputId / NodeInputId is invalidated by the arena
        // reshuffle below; external callers that captured a snapshot
        // generation see a mismatch via `Graph::generation()` and can
        // surface a typed error instead of dereferencing into the
        // wrong post-compaction slot.
        self.generation = self.generation.wrapping_add(1);

        // 1. Compute reachable set.
        let reachable: Vec<NodeId> = self.walk_from(entry).collect();

        // 2. Build fresh arenas.
        let mut new_nodes: PrimaryMap<NodeId, Node> = PrimaryMap::new();
        let mut new_outputs: PrimaryMap<NodeOutputId, NodeOutput> = PrimaryMap::new();
        let mut new_inputs: PrimaryMap<NodeInputId, NodeInput> = PrimaryMap::new();
        let mut new_output_pool = ListPool::<NodeOutputId>::new();
        let mut new_input_pool = ListPool::<NodeInputId>::new();

        let mut remap = NodeIdRemap::default();

        // 3. First pass: copy nodes (placeholder input/output lists)
        // and outputs.  We need every new NodeId / NodeOutputId before
        // the second pass can rewrite input.output_id references.
        for &old_node_id in &reachable {
            let old_kind = self.nodes[old_node_id].kind;
            let new_node_id = new_nodes.push(Node::new(old_kind));
            remap.nodes[old_node_id] = Some(new_node_id);

            // Outputs: copy NodeOutput, leaving first_use cleared.
            // The use-list is rebuilt in pass 4.
            let old_out_ids: Vec<NodeOutputId> = self.nodes[old_node_id]
                .outputs
                .as_slice(&self.output_pool)
                .to_vec();
            let mut new_output_ids: Vec<NodeOutputId> = Vec::with_capacity(old_out_ids.len());
            for old_out_id in old_out_ids {
                let old_out = &self.outputs[old_out_id];
                let kind = old_out.kind;
                let output_index = old_out.output_index;
                let new_out = NodeOutput::new(kind, new_node_id, output_index);
                let new_out_id = new_outputs.push(new_out);
                remap.outputs[old_out_id] = Some(new_out_id);
                new_output_ids.push(new_out_id);
            }
            new_nodes[new_node_id].outputs =
                NodeOutputIdList::from_iter(new_output_ids, &mut new_output_pool);
        }

        // 4. Second pass: copy inputs (rewrite output_id through remap).
        for &old_node_id in &reachable {
            // Pass 1 (above) installed every reachable node into
            // `remap.nodes`; we are iterating the same `reachable` set,
            // so the lookup cannot return None.  Same logic applies to
            // `remap.outputs[old_input.output_id]` below: every input's
            // output producer is reachable iff the input's owning node
            // is reachable.  Both are propagated as `Err` rather than
            // `expect` so a hypothetical invariant violation surfaces
            // as a typed error, not a Python crash.
            let new_node_id = remap.nodes[old_node_id].ok_or_else(|| {
                anyhow::anyhow!(
                    "retain_reachable: reachable node {old_node_id:?} missing from pass-1 remap"
                )
            })?;
            let old_input_ids: Vec<NodeInputId> = self.nodes[old_node_id]
                .inputs
                .as_slice(&self.input_pool)
                .to_vec();
            let mut new_input_ids: Vec<NodeInputId> = Vec::with_capacity(old_input_ids.len());
            for old_input_id in old_input_ids {
                let old_input = &self.inputs[old_input_id];
                let new_output_id = remap.outputs[old_input.output_id].ok_or_else(|| {
                    anyhow::anyhow!(
                        "retain_reachable: input {old_input_id:?} references output {:?} \
                         whose producing node is unreachable (use-list invariant violation)",
                        old_input.output_id
                    )
                })?;
                let input_index = old_input.input_index;
                let new_input = NodeInput::new(new_output_id, new_node_id, input_index);
                let new_input_id = new_inputs.push(new_input);
                remap.inputs[old_input_id] = Some(new_input_id);
                new_input_ids.push(new_input_id);
            }
            new_nodes[new_node_id].inputs =
                NodeInputIdList::from_iter(new_input_ids, &mut new_input_pool);
        }

        // 5. Swap the arenas onto self before rebuilding use-lists —
        // `link_input_to_output_list` mutates `self.outputs` /
        // `self.inputs`.
        self.nodes = new_nodes;
        self.outputs = new_outputs;
        self.inputs = new_inputs;
        self.output_pool = new_output_pool;
        self.input_pool = new_input_pool;

        // 6. Rebuild use-list pointers.  Iterate every input and re-
        // attach via the existing helper (which sets first_use on the
        // referenced output and chains next_use).
        let all_input_ids: Vec<NodeInputId> = self.inputs.keys().collect();
        for input_id in all_input_ids {
            self.link_input_to_output_list(input_id);
        }

        // 6b. GC the wide-const side-table BEFORE rebuilding the dedup
        // cache.  The dedup cache keys on `Node` (which carries the
        // `NodeKind`, including `IntConstWide(WideConstId)`); rewriting
        // wide-const ids must happen first so the cache is built over
        // the post-GC payloads.
        self.gc_wide_consts();

        // 7. Rebuild the dedup cache from scratch.
        self.node_to_id.clear();
        let all_node_ids: Vec<NodeId> = self.nodes.keys().collect();
        for new_node_id in all_node_ids {
            let kind = self.nodes[new_node_id].kind;
            if !kind.is_cacheable() {
                continue;
            }
            let input_outputs: Vec<NodeOutputId> = self.nodes[new_node_id]
                .inputs
                .as_slice(&self.input_pool)
                .iter()
                .map(|&iid| self.inputs[iid].output_id)
                .collect();
            let output_kinds: Vec<NodeOutputKind> = self.nodes[new_node_id]
                .outputs
                .as_slice(&self.output_pool)
                .iter()
                .map(|&oid| self.outputs[oid].kind)
                .collect();
            let key = (Node::new(kind), input_outputs, output_kinds);
            // Last writer wins; reachable nodes with identical keys are
            // already deduped pre-compaction so collisions shouldn't
            // happen, but if they do the surviving entry is still valid.
            self.node_to_id.insert(key, new_node_id);
        }

        // 8. The NodeId-keyed overlay tables (call_other_names,
        // asm_fingerprints, call_clobbered_overrides, phi_var_tag,
        // stack_offsets) and the Vn-keyed `initial_var_index` all
        // live on `Function`, not on `Graph`.  `Function::compact`
        // applies the returned remap to those tables after this call.

        Ok(remap)
    }

    /// Rebuilds [`Self::wide_const_interner`] over only the values
    /// referenced by surviving `IntConstWide` nodes.
    /// Each `IntConstWide(old_id)` in the arena is rewritten in place
    /// to carry the new id assigned by the rebuilt side-table.
    ///
    /// Called from [`Self::retain_reachable`] after the node arena
    /// remap has settled — at that point `self.nodes.keys()` only
    /// iterates surviving nodes, so the live-id scan correctly excludes
    /// zombie `IntConstWide` references.
    ///
    /// **Not safe to call standalone on a non-compacted graph:** the
    /// scan would include zombie nodes' wide-const ids, defeating the
    /// GC purpose.  `pub(crate)` rather than fully private only because
    /// `retain_reachable` is in a sibling module; callers outside that
    /// path should call `retain_reachable` instead.
    pub(crate) fn gc_wide_consts(&mut self) {
        use crate::node::NodeKind;
        use crate::wide_const::WideConstId;

        // Build the live-id set + collect every IntConstWide node's old id.
        let mut live_old_ids: Vec<WideConstId> = Vec::new();
        let mut wide_nodes: Vec<crate::node::NodeId> = Vec::new();
        for node in self.nodes.keys() {
            if let NodeKind::IntConstWide(id) = self.nodes[node].kind {
                wide_nodes.push(node);
                live_old_ids.push(id);
            }
        }
        if live_old_ids.is_empty() && self.wide_const_interner.is_empty() {
            return;
        }

        // Rebuild the interner over only live values; `intern` dedups, so
        // distinct old ids that aliased one value collapse to one new id.
        let mut new_interner: entity_utils::EntityInterner<
            WideConstId,
            crate::wide_const::WideConstStorage,
        > = entity_utils::EntityInterner::default();
        let mut old_to_new: rustc_hash::FxHashMap<WideConstId, WideConstId> =
            rustc_hash::FxHashMap::default();
        for old_id in live_old_ids {
            if old_to_new.contains_key(&old_id) {
                continue;
            }
            let value = self.wide_const_interner[old_id].clone();
            let new_id = new_interner.intern(value);
            old_to_new.insert(old_id, new_id);
        }
        self.wide_const_interner = new_interner;

        // Rewrite the surviving IntConstWide nodes' payloads in place.
        for node in wide_nodes {
            if let NodeKind::IntConstWide(ref mut id) = self.nodes[node].kind
                && let Some(&new_id) = old_to_new.get(id)
            {
                *id = new_id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::node::{NodeKind, NodeOutputType};

    /// Builds a minimal graph: `Entry → Return(value, mem)` where
    /// `value` is the IntConst returned by `value_kind` and `mem` is
    /// `InitialMemory`.  Returns `(entry, value_node, return_node, mem_node)`.
    /// Useful for tests that need a small reachable set anchored by Entry
    /// plus a known value-producing node in that set.
    fn build_anchor(graph: &mut Graph, value: u128) -> (NodeId, NodeId, NodeId, NodeId) {
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let const_node = graph.create_node(
            NodeKind::IntConst(value),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        let [const_out] = graph.node_outputs_exact::<1>(const_node).unwrap();
        let ret_node = graph.create_node(
            NodeKind::Return,
            [entry_ctrl, mem_out, const_out],
            [],
        );
        (entry, const_node, ret_node, mem)
    }

    // NOTE: Tests for the four NodeId-keyed overlay tables
    // (asm_fingerprints, call_other_names, call_clobbered_overrides,
    // phi_var_tag) remap through Function::compact — see the
    // compact_tests module in function.rs.


    // NOTE: `initial_var_index` is Vn-keyed and lives on `Function`,
    // not `Graph`.  The remap behaviour is covered by
    // `initial_var_index_remap_through_compact` in `function.rs`.

    /// The `NodeIdRemap` accessors return `None` for old ids whose
    /// source node was unreachable and therefore dropped during
    /// compaction.  Surviving ids return `Some(new_id)`.  Verifies the
    /// translation table is sparse and faithful at the boundary.
    #[test]
    fn node_id_remap_returns_none_for_dropped() {
        let mut graph = Graph::new();
        let (entry, const_node, ret_node, _mem_node) = build_anchor(&mut graph, 1);
        let [const_old_out] = graph.node_outputs_exact::<1>(const_node).unwrap();
        // Grab the pre-compaction NodeInputId slots on Return via crate-
        // private arena access — there's no public accessor for raw
        // input-slot ids; the `node_inputs` iterator yields the consumed
        // NodeOutputIds, not the slot ids we want to test against
        // `input_old_to_new`.
        let ret_old_input_slots: Vec<NodeInputId> = graph.nodes[ret_node]
            .inputs
            .as_slice(&graph.input_pool)
            .to_vec();

        // Zombie with its own output id and (no) input ids.
        let zombie = graph.create_node(
            NodeKind::IntConst(0xC0FFEE),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        let [zombie_out] = graph.node_outputs_exact::<1>(zombie).unwrap();

        let remap = graph.retain_reachable(entry).unwrap();

        // Surviving ids resolve to Some(_).
        assert!(remap.node_old_to_new(entry).is_some());
        assert!(remap.node_old_to_new(const_node).is_some());
        assert!(remap.node_old_to_new(ret_node).is_some());
        assert!(remap.output_old_to_new(const_old_out).is_some());
        for &input_id in &ret_old_input_slots {
            assert!(
                remap.input_old_to_new(input_id).is_some(),
                "reachable Return input {input_id:?} should remap to Some(_)",
            );
        }

        // Dropped ids resolve to None.
        assert!(remap.node_old_to_new(zombie).is_none());
        assert!(remap.output_old_to_new(zombie_out).is_none());
    }

    /// Calling `retain_reachable` a second time on an already-compacted
    /// graph leaves node count unchanged and produces a remap whose
    /// every entry is `Some(_)` (no further drops possible).
    #[test]
    fn retain_reachable_is_idempotent() {
        let mut graph = Graph::new();
        let (entry, _const_node, _ret_node, _mem_node) = build_anchor(&mut graph, 0x42);
        let _zombie = graph.create_node(
            NodeKind::IntConst(0xDEAD),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );

        let _ = graph.retain_reachable(entry).unwrap();
        let post_first_count = graph.all_node_ids().count();
        // retain_reachable invalidates pre-call ids; re-derive the entry
        // from the unique NodeKind::Entry in the surviving arena.
        let new_entry = graph
            .all_node_ids()
            .find(|&id| matches!(graph.node_kind(id), NodeKind::Entry))
            .unwrap();

        let remap2 = graph.retain_reachable(new_entry).unwrap();
        let post_second_count = graph.all_node_ids().count();
        assert_eq!(
            post_first_count, post_second_count,
            "second retain_reachable must not drop further nodes",
        );
        // Every pre-second id remaps to Some(_).
        for old_id in graph.all_node_ids().collect::<Vec<_>>() {
            assert!(
                remap2.node_old_to_new(old_id).is_some(),
                "second retain_reachable dropped already-compact id {old_id:?}",
            );
        }
    }

    /// After `retain_reachable` compacts the graph, the dedup cache
    /// must have been rebuilt: creating a cacheable node with
    /// identical `(kind, inputs, output_kinds)` after the compaction
    /// must still alias to a single survivor.  A regression that left
    /// the cache stale (or skipped the rebuild step) would yield two
    /// distinct `NodeId`s for the same logical value — silent graph
    /// blowup.
    #[test]
    fn retain_reachable_rebuilds_dedup_cache() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _remap = graph.retain_reachable(entry).unwrap();

        // After compaction, creating a cacheable node with identical
        // (kind, inputs, output_kinds) must dedup.
        let one_a = graph.create_node(
            NodeKind::IntConst(7),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        let one_b = graph.create_node(
            NodeKind::IntConst(7),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        assert_eq!(one_a, one_b, "dedup cache must be rebuilt by retain_reachable");
    }

    /// A graph with no side-table entries must compact cleanly — no panic.
    #[test]
    fn empty_side_table_compacts_cleanly() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out], []);

        let remap = graph.retain_reachable(entry).unwrap();

        assert!(remap.node_old_to_new(entry).is_some());
        assert!(remap.node_old_to_new(mem).is_some());
    }
}
