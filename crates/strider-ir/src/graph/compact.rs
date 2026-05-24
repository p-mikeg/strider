//! `Graph::retain_reachable` — compact the IR arena down to nodes
//! reachable from `entry` via [`Graph::walk_from`] (control-out +
//! data-in), returning the old→new id translation table so external
//! callers can fix up any ids they hold.

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
/// Used by [`Graph::retain_reachable`] to fold every `NodeId`-keyed
/// side-table through one iteration site — adding a new side-table now
/// means adding one entry to [`Graph::node_keyed_side_tables_mut`] and
/// no new logic.
///
/// The Vn-keyed `initial_var_index` does **not** fit this shape (its
/// key is `rsleigh::Vn`, not `NodeId`) and stays inline in
/// `retain_reachable`.
trait SideTableRemap {
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
    /// `None` if `old`'s producing node was dropped.
    #[inline]
    #[must_use]
    pub fn output_old_to_new(&self, old: NodeOutputId) -> Option<NodeOutputId> {
        self.outputs[old]
    }

    /// Returns the post-compaction `NodeInputId` for `old`, or `None`
    /// if `old`'s consuming node was dropped.
    #[inline]
    #[must_use]
    pub fn input_old_to_new(&self, old: NodeInputId) -> Option<NodeInputId> {
        self.inputs[old]
    }
}

impl Graph {
    /// Returns the set of `SecondaryMap<NodeId, _>`-shaped side-tables
    /// that participate in [`Self::retain_reachable`]'s id remap.
    ///
    /// Adding a new `NodeId`-keyed side-table means: declare the field
    /// on `Graph`, then append `&mut self.new_field` to this iterator.
    /// `retain_reachable` picks it up automatically — no fixed array
    /// size to update.
    fn node_keyed_side_tables_mut(
        &mut self,
    ) -> impl IntoIterator<Item = &mut dyn SideTableRemap> {
        [
            &mut self.stack_phi_offsets as &mut dyn SideTableRemap,
            &mut self.call_other_names,
            &mut self.asm_fingerprints,
            &mut self.call_clobbered_overrides,
            &mut self.phi_var_tag,
        ]
    }

    /// Rebuilds the arena to retain only nodes reachable from `entry`
    /// via [`Graph::walk_from`] (control-out forward + data-in
    /// backward).  Returns the old→new id translation table.
    ///
    /// Pre-compaction `NodeId` / `NodeOutputId` / `NodeInputId` values
    /// are invalidated by this call.  Callers that hold any such ids
    /// MUST rewrite them through the returned [`NodeIdRemap`] (or
    /// drop them).
    ///
    /// The dedup cache is rebuilt from scratch.  All side-tables
    /// (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`,
    /// `call_clobbered_overrides`, `phi_var_tag`) are remapped through
    /// the translation table; entries for dropped nodes are dropped.
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

        // 8. Remap every `SecondaryMap<NodeId, _>`-shaped side-table
        // through `node_keyed_side_tables_mut`.  Each table's
        // `remap_node_keyed` iterates the surviving (old → new) pairs
        // straight off `remap.nodes` and writes the old entry into the
        // fresh table at the new id via `std::mem::take`; the post-
        // remap source is left at `Default::default()` for every slot.
        // SecondaryMap stores `Default` cheaply (no allocation for
        // `Vec`/`Option`), so the prior `if !is_empty() { ... }` micro-
        // optimization isn't worth the complexity here.
        //
        // Implementation note: we used to materialise an intermediate
        // `HashMap<NodeId, NodeId>` of surviving pairs and pass that to
        // each side-table; that was a lossy copy of `remap.nodes` (which
        // is itself a `SecondaryMap<NodeId, Option<NodeId>>`).  Iterating
        // `remap.nodes` directly drops one hash-insert per surviving
        // node per compaction.
        for tbl in self.node_keyed_side_tables_mut() {
            tbl.remap_node_keyed(&remap);
        }

        // Remap the InitialVar Vn→NodeId index.  Entries whose NodeId
        // didn't survive compaction (i.e. the InitialVar became
        // unreachable and was dropped) are silently elided — the
        // orchestrator's `read_or_init_var` fallback will lazily
        // re-create them as needed.
        let mut new_initial_var_index: rustc_hash::FxHashMap<rsleigh::Vn, NodeId> =
            rustc_hash::FxHashMap::with_capacity_and_hasher(
                self.initial_var_index.len(),
                Default::default(),
            );
        for (vn, old_id) in self.initial_var_index.drain() {
            if let Some(new_id) = remap.nodes[old_id] {
                new_initial_var_index.insert(vn, new_id);
            }
        }
        self.initial_var_index = new_initial_var_index;

        Ok(remap)
    }

    /// Rebuilds [`Self::wide_consts`] + [`Self::wide_const_dedup`] over
    /// only the values referenced by surviving `IntConstWide` nodes.
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
        use crate::wide_const::{WideConstId, WideConstStorage};

        // Build the live-id set + collect every IntConstWide node's old id.
        let mut live_old_ids: Vec<WideConstId> = Vec::new();
        let mut wide_nodes: Vec<crate::node::NodeId> = Vec::new();
        for node in self.nodes.keys() {
            if let NodeKind::IntConstWide(id) = self.nodes[node].kind {
                wide_nodes.push(node);
                live_old_ids.push(id);
            }
        }
        if live_old_ids.is_empty() && self.wide_consts.is_empty() {
            return;
        }

        // Rebuild the side-table + dedup map over only live values.
        let mut new_consts: cranelift_entity::PrimaryMap<WideConstId, WideConstStorage> =
            cranelift_entity::PrimaryMap::new();
        let mut new_dedup: rustc_hash::FxHashMap<WideConstStorage, WideConstId> =
            rustc_hash::FxHashMap::default();
        let mut old_to_new: rustc_hash::FxHashMap<WideConstId, WideConstId> =
            rustc_hash::FxHashMap::default();
        for old_id in live_old_ids {
            if old_to_new.contains_key(&old_id) {
                continue;
            }
            let value = self.wide_consts[old_id].clone();
            let new_id = if let Some(&existing) = new_dedup.get(&value) {
                existing
            } else {
                let id = new_consts.push(value.clone());
                new_dedup.insert(value, id);
                id
            };
            old_to_new.insert(old_id, new_id);
        }
        self.wide_consts = new_consts;
        self.wide_const_dedup = new_dedup;

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
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        let [const_out] = graph.node_outputs_exact::<1>(const_node).unwrap();
        let ret_node = graph.create_node(
            NodeKind::Return,
            [entry_ctrl, mem_out, const_out],
            [],
        );
        graph.entry = Some(entry);
        (entry, const_node, ret_node, mem)
    }

    /// `asm_fingerprints` is the project's proof-of-correctness side-table.
    /// After `retain_reachable`, surviving nodes must keep their exact
    /// fingerprints; entries previously installed for zombie nodes must be
    /// dropped (i.e. the new id's `asm_fingerprint` must not surface stale
    /// addresses from a now-dropped predecessor).
    #[test]
    fn asm_fingerprints_remap_through_retain() {
        let mut graph = Graph::new();
        let (entry, const_node, ret_node, mem_node) = build_anchor(&mut graph, 0xAB);
        // Zombie value with its own fingerprint — must be dropped.
        let zombie = graph.create_node(
            NodeKind::IntConst(0xDEAD),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        graph.set_asm_fingerprint(entry, vec![0x1000]);
        graph.set_asm_fingerprint(const_node, vec![0x2000, 0x2004]);
        graph.set_asm_fingerprint(ret_node, vec![0x3000]);
        graph.set_asm_fingerprint(mem_node, vec![0x4000]);
        // Stale fingerprint on a zombie — must not survive.
        graph.set_asm_fingerprint(zombie, vec![0xBADBAD]);

        let remap = graph.retain_reachable(entry).unwrap();

        let new_entry = remap.node_old_to_new(entry).unwrap();
        let new_const = remap.node_old_to_new(const_node).unwrap();
        let new_ret = remap.node_old_to_new(ret_node).unwrap();
        let new_mem = remap.node_old_to_new(mem_node).unwrap();

        assert_eq!(graph.asm_fingerprint(new_entry), &[0x1000]);
        assert_eq!(graph.asm_fingerprint(new_const), &[0x2000, 0x2004]);
        assert_eq!(graph.asm_fingerprint(new_ret), &[0x3000]);
        assert_eq!(graph.asm_fingerprint(new_mem), &[0x4000]);

        // The zombie's fingerprint must not have leaked onto any
        // surviving node.  Scan every surviving id.
        for id in graph.all_node_ids() {
            assert!(
                !graph.asm_fingerprint(id).contains(&0xBADBAD),
                "zombie fingerprint 0xBADBAD leaked onto surviving id {id:?}",
            );
        }
    }

    /// `stack_phi_offsets` is keyed by `StackStorePhi` node id.  Verify
    /// the side-table is remapped to the new id and that zombie entries
    /// are dropped.
    #[test]
    fn stack_phi_offsets_remap_through_retain() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let const_node = graph.create_node(
            NodeKind::IntConst(0),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        // Reachable StackStorePhi wired into Return's data inputs (so it
        // shows up in walk_from(entry) via data-in).
        let space = rsleigh::VnSpace::RAM;
        let live_ssp = graph.create_node(
            NodeKind::StackStorePhi { space },
            [],
            [NodeOutputKind::Memory],
        );
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        let [const_out] = graph.node_outputs_exact::<1>(const_node).unwrap();
        let [live_ssp_out] = graph.node_outputs_exact::<1>(live_ssp).unwrap();
        let _ret = graph.create_node(
            NodeKind::Return,
            [entry_ctrl, mem_out, const_out, live_ssp_out],
            [],
        );
        // Zombie StackStorePhi — never used by anything reachable.
        let zombie_ssp = graph.create_node(
            NodeKind::StackStorePhi { space },
            [],
            [NodeOutputKind::Memory],
        );

        graph.set_stack_phi_offsets(live_ssp, vec![0, -4, -8]);
        graph.set_stack_phi_offsets(zombie_ssp, vec![999, 1000]);

        graph.set_asm_fingerprint(entry, vec![0x1000]);
        graph.set_asm_fingerprint(mem, vec![0x1004]);
        graph.set_asm_fingerprint(const_node, vec![0x1008]);
        graph.set_asm_fingerprint(live_ssp, vec![0x100c]);

        let remap = graph.retain_reachable(entry).unwrap();
        let new_live = remap.node_old_to_new(live_ssp).unwrap();
        assert_eq!(graph.stack_phi_offsets(new_live), &[0, -4, -8]);
        // Zombie must be dropped and its offsets must not surface on
        // any surviving id.
        assert!(remap.node_old_to_new(zombie_ssp).is_none());
        for id in graph.all_node_ids() {
            assert_ne!(
                graph.stack_phi_offsets(id),
                &[999, 1000],
                "zombie offsets leaked onto surviving id {id:?}",
            );
        }
    }

    /// `call_other_names` is keyed by `CallOther` node id.  Verify remap
    /// and zombie drop.
    #[test]
    fn call_other_names_remap_through_retain() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        // Reachable CallOther — chained on control so walk_from picks it up.
        let live_co = graph.create_node(
            NodeKind::CallOther { user_op_id: 1 },
            [entry_ctrl, mem_out],
            [NodeOutputKind::Control, NodeOutputKind::Memory],
        );
        let [co_ctrl, co_mem] = graph.node_outputs_exact::<2>(live_co).unwrap();
        let _ret = graph.create_node(NodeKind::Return, [co_ctrl, co_mem], []);
        // Zombie CallOther — isolated, never used.
        let zombie_co = graph.create_node(
            NodeKind::CallOther { user_op_id: 99 },
            [],
            [NodeOutputKind::Control, NodeOutputKind::Memory],
        );

        graph.set_call_other_name(live_co, "live_op".to_string());
        graph.set_call_other_name(zombie_co, "zombie_op".to_string());

        graph.set_asm_fingerprint(entry, vec![0x1000]);
        graph.set_asm_fingerprint(mem, vec![0x1004]);
        graph.set_asm_fingerprint(live_co, vec![0x1008]);

        let remap = graph.retain_reachable(entry).unwrap();
        let new_co = remap.node_old_to_new(live_co).unwrap();
        assert_eq!(graph.call_other_name(new_co), Some("live_op"));
        assert!(remap.node_old_to_new(zombie_co).is_none());
        for id in graph.all_node_ids() {
            assert_ne!(
                graph.call_other_name(id),
                Some("zombie_op"),
                "zombie call_other_name leaked onto surviving id {id:?}",
            );
        }
    }

    /// `call_clobbered_overrides` is keyed by `Call` node id.  Verify
    /// remap and zombie drop.
    #[test]
    fn call_clobbered_overrides_remap_through_retain() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        // Reachable Call wired into the control / memory chain.
        let live_call = graph.create_node(
            NodeKind::Call,
            [entry_ctrl, mem_out],
            [NodeOutputKind::Control, NodeOutputKind::Memory],
        );
        let [call_ctrl, call_mem] = graph.node_outputs_exact::<2>(live_call).unwrap();
        let _ret = graph.create_node(NodeKind::Return, [call_ctrl, call_mem], []);
        // Zombie Call — never consumed.
        let zombie_call = graph.create_node(
            NodeKind::Call,
            [],
            [NodeOutputKind::Control, NodeOutputKind::Memory],
        );

        let live_clobs = vec![rsleigh::Vn {
            size: 8,
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
        }];
        let zombie_clobs = vec![rsleigh::Vn {
            size: 8,
            addr_off: 0xDEAD,
            addr_space: rsleigh::VnSpace::REGISTER,
        }];
        graph.set_call_clobbered_override(live_call, live_clobs.clone());
        graph.set_call_clobbered_override(zombie_call, zombie_clobs.clone());

        graph.set_asm_fingerprint(entry, vec![0x1000]);
        graph.set_asm_fingerprint(mem, vec![0x1004]);
        graph.set_asm_fingerprint(live_call, vec![0x1008]);

        let remap = graph.retain_reachable(entry).unwrap();
        let new_call = remap.node_old_to_new(live_call).unwrap();
        assert_eq!(
            graph.call_clobbered_override(new_call),
            Some(live_clobs.as_slice()),
        );
        assert!(remap.node_old_to_new(zombie_call).is_none());
        for id in graph.all_node_ids() {
            assert_ne!(
                graph.call_clobbered_override(id),
                Some(zombie_clobs.as_slice()),
                "zombie call_clobbered_override leaked onto surviving id {id:?}",
            );
        }
    }

    /// `phi_var_tag` is keyed by `Phi` node id (and is the Vn-tag
    /// side-table the indirect-branch classifier consults).  Verify
    /// remap and zombie drop.
    #[test]
    fn phi_var_tag_remap_through_retain() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [entry_ctrl_for_cs] = graph.node_outputs_exact::<1>(entry).unwrap();
        let cs = graph.create_node(
            NodeKind::Region,
            [entry_ctrl_for_cs],
            [NodeOutputKind::Control, NodeOutputKind::PhiToken],
        );
        let [cs_ctrl, cs_token] = graph.node_outputs_exact::<2>(cs).unwrap();
        let const_node = graph.create_node(
            NodeKind::IntConst(7),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [const_out] = graph.node_outputs_exact::<1>(const_node).unwrap();
        // Reachable Phi — consumed by Return.
        let live_phi = graph.create_node(
            NodeKind::Phi,
            [cs_token, const_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [phi_out] = graph.node_outputs_exact::<1>(live_phi).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        let _ret = graph.create_node(NodeKind::Return, [cs_ctrl, mem_out, phi_out], []);
        // Zombie Phi — never consumed.
        let zombie_phi = graph.create_node(
            NodeKind::Phi,
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        let live_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let zombie_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0xBEEF,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        graph.set_phi_var_tag(live_phi, live_vn);
        graph.set_phi_var_tag(zombie_phi, zombie_vn);

        graph.set_asm_fingerprint(entry, vec![0x1000]);
        graph.set_asm_fingerprint(mem, vec![0x1004]);
        graph.set_asm_fingerprint(const_node, vec![0x1008]);

        let remap = graph.retain_reachable(entry).unwrap();
        let new_phi = remap.node_old_to_new(live_phi).unwrap();
        assert_eq!(graph.phi_var_tag(new_phi), Some(live_vn));
        assert!(remap.node_old_to_new(zombie_phi).is_none());
        for id in graph.all_node_ids() {
            assert_ne!(
                graph.phi_var_tag(id),
                Some(zombie_vn),
                "zombie phi_var_tag leaked onto surviving id {id:?}",
            );
        }
    }

    /// `initial_var_index` is Vn-keyed (`FxHashMap<Vn, NodeId>`) and
    /// inline-remapped by `retain_reachable` — NOT part of the
    /// SecondaryMap registry.  Verify that reachable mappings survive,
    /// dropped-NodeId mappings are removed entirely (so callers don't
    /// see stale Vn→old_id entries pointing into the freshly-rebuilt
    /// arena).
    #[test]
    fn initial_var_index_remap_through_retain() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let live_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let zombie_vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x28,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let live_iv = graph.create_node(
            NodeKind::InitialVar(live_vn),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let zombie_iv = graph.create_node(
            NodeKind::InitialVar(zombie_vn),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        graph.register_initial_var(live_vn, live_iv);
        graph.register_initial_var(zombie_vn, zombie_iv);

        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        let [live_iv_out] = graph.node_outputs_exact::<1>(live_iv).unwrap();
        // Only live_iv is wired into Return.
        let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, live_iv_out], []);

        graph.set_asm_fingerprint(entry, vec![0x1000]);
        graph.set_asm_fingerprint(mem, vec![0x1004]);
        graph.set_asm_fingerprint(live_iv, vec![0x1008]);

        let remap = graph.retain_reachable(entry).unwrap();
        let new_live_iv = remap.node_old_to_new(live_iv).unwrap();
        assert_eq!(graph.initial_var_for(live_vn), Some(new_live_iv));
        // The zombie Vn→id mapping must be gone (not stale, not None
        // accidentally pointing into garbage).
        assert_eq!(graph.initial_var_for(zombie_vn), None);
        assert!(remap.node_old_to_new(zombie_iv).is_none());
    }

    /// The `NodeIdRemap` accessors return `None` for old ids whose
    /// source node was unreachable and therefore dropped during
    /// compaction.  Surviving ids return `Some(new_id)`.  Verifies the
    /// translation table is sparse and faithful at the boundary.
    #[test]
    fn node_id_remap_returns_none_for_dropped() {
        let mut graph = Graph::new();
        let (entry, const_node, ret_node, mem_node) = build_anchor(&mut graph, 1);
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
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [zombie_out] = graph.node_outputs_exact::<1>(zombie).unwrap();

        // Stamp fingerprints so the validator's superset-only check
        // doesn't fire on surviving nodes (we don't run validate here,
        // but keep the test honest).
        graph.set_asm_fingerprint(entry, vec![0x1000]);
        graph.set_asm_fingerprint(const_node, vec![0x1004]);
        graph.set_asm_fingerprint(ret_node, vec![0x1008]);
        graph.set_asm_fingerprint(mem_node, vec![0x100c]);

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
    /// Side-tables previously remapped survive the second call intact.
    #[test]
    fn retain_reachable_is_idempotent() {
        let mut graph = Graph::new();
        let (entry, const_node, ret_node, mem_node) = build_anchor(&mut graph, 0x42);
        let _zombie = graph.create_node(
            NodeKind::IntConst(0xDEAD),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        graph.set_asm_fingerprint(entry, vec![0x1000]);
        graph.set_asm_fingerprint(const_node, vec![0x2000]);
        graph.set_asm_fingerprint(ret_node, vec![0x3000]);
        graph.set_asm_fingerprint(mem_node, vec![0x4000]);

        let _ = graph.retain_reachable(entry).unwrap();
        let post_first_count = graph.all_node_ids().count();
        let new_entry = graph.entry.unwrap_or_else(|| {
            // retain_reachable invalidates pre-call ids; re-derive entry
            // from the unique NodeKind::Entry in the surviving arena.
            graph
                .all_node_ids()
                .find(|&id| matches!(graph.node_kind(id), NodeKind::Entry))
                .unwrap()
        });
        // Snapshot every surviving fingerprint pre-second-call.
        let pre_fps: Vec<(NodeId, Vec<u64>)> = graph
            .all_node_ids()
            .map(|id| (id, graph.asm_fingerprint(id).to_vec()))
            .collect();

        let remap2 = graph.retain_reachable(new_entry).unwrap();
        let post_second_count = graph.all_node_ids().count();
        assert_eq!(
            post_first_count, post_second_count,
            "second retain_reachable must not drop further nodes",
        );
        // Every pre-second id remaps to Some(_).
        for (old_id, expected_fp) in &pre_fps {
            let new_id = remap2.node_old_to_new(*old_id).unwrap_or_else(|| {
                panic!("second retain_reachable dropped already-compact id {old_id:?}")
            });
            assert_eq!(
                graph.asm_fingerprint(new_id),
                expected_fp.as_slice(),
                "fingerprint changed across idempotent retain_reachable on id {old_id:?}",
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
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let one_b = graph.create_node(
            NodeKind::IntConst(7),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        assert_eq!(one_a, one_b, "dedup cache must be rebuilt by retain_reachable");
    }

    /// A graph with no entries in any of the `NodeId`-keyed side-tables
    /// must compact cleanly — no panic, no garbage entries, surviving
    /// nodes' accessors return defaults.
    #[test]
    fn empty_side_table_compacts_cleanly() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem).unwrap();
        let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out], []);

        // Deliberately set NOTHING on any side-table.  Compact must
        // tolerate empty SecondaryMaps and an empty initial_var_index.
        let remap = graph.retain_reachable(entry).unwrap();

        let new_entry = remap.node_old_to_new(entry).unwrap();
        let new_mem = remap.node_old_to_new(mem).unwrap();
        // Default accessors over surviving ids return empty / None.
        assert_eq!(graph.asm_fingerprint(new_entry), &[] as &[u64]);
        assert_eq!(graph.stack_phi_offsets(new_entry), &[] as &[i64]);
        assert_eq!(graph.call_other_name(new_entry), None);
        assert_eq!(graph.call_clobbered_override(new_entry), None);
        assert_eq!(graph.phi_var_tag(new_entry), None);
        assert_eq!(graph.asm_fingerprint(new_mem), &[] as &[u64]);
        // The Vn-keyed index started empty and stays empty.
        assert_eq!(graph.initial_var_index.len(), 0);
    }
}
