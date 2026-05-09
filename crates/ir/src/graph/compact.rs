//! `Graph::retain_reachable` — compact the IR arena down to nodes
//! reachable from `entry` via [`crate::walk::walk_graph`] (control-out +
//! data-in), returning the old→new id translation table so external
//! callers can fix up any ids they hold.

use cranelift_entity::{ListPool, PrimaryMap, SecondaryMap};
use std::collections::HashMap;

use crate::node::{
    Node, NodeId, NodeInput, NodeInputId, NodeInputIdList, NodeOutput, NodeOutputId,
    NodeOutputIdList, NodeOutputKind,
};
use crate::walk::walk_graph;

use super::Graph;

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
    /// Rebuilds the arena to retain only nodes reachable from `entry`
    /// via [`crate::walk::walk_graph`] (control-out forward + data-in
    /// backward).  Returns the old→new id translation table.
    ///
    /// Pre-compaction `NodeId` / `NodeOutputId` / `NodeInputId` values
    /// are invalidated by this call.  Callers that hold any such ids
    /// MUST rewrite them through the returned [`NodeIdRemap`] (or
    /// drop them).
    ///
    /// The dedup cache is rebuilt from scratch.  All four side-tables
    /// (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`,
    /// `call_clobbered_overrides`) are remapped through the
    /// translation table; entries for dropped nodes are dropped.
    pub fn retain_reachable(&mut self, entry: NodeId) -> NodeIdRemap {
        // 1. Compute reachable set.
        let reachable: Vec<NodeId> = walk_graph(self, entry).collect();

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
            // is reachable, which it is here by construction.
            #[allow(clippy::expect_used)]
            let new_node_id = remap.nodes[old_node_id]
                .expect("just installed in pass 1");
            let old_input_ids: Vec<NodeInputId> = self.nodes[old_node_id]
                .inputs
                .as_slice(&self.input_pool)
                .to_vec();
            let mut new_input_ids: Vec<NodeInputId> = Vec::with_capacity(old_input_ids.len());
            for old_input_id in old_input_ids {
                let old_input = &self.inputs[old_input_id];
                #[allow(clippy::expect_used)]
                let new_output_id = remap.outputs[old_input.output_id].expect(
                    "input references an output whose producing node was unreachable",
                );
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

        // 8. Remap all four side-tables.  For each table, iterate the
        // surviving (old → new) pairs and write the old entry into the
        // fresh table at the new id.
        let mut new_stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>> = SecondaryMap::new();
        let mut new_call_other_names: SecondaryMap<NodeId, Option<String>> = SecondaryMap::new();
        let mut new_asm_fingerprints: SecondaryMap<NodeId, Vec<u64>> = SecondaryMap::new();
        let mut new_call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>> =
            SecondaryMap::new();
        // Iterate over the *original* reachable set: those are the
        // only old ids worth remapping.
        let mut old_to_new_pairs: HashMap<NodeId, NodeId> = HashMap::new();
        for &old_id in &reachable {
            if let Some(new_id) = remap.nodes[old_id] {
                old_to_new_pairs.insert(old_id, new_id);
            }
        }
        for (&old_id, &new_id) in &old_to_new_pairs {
            let phi = std::mem::take(&mut self.stack_phi_offsets[old_id]);
            if !phi.is_empty() {
                new_stack_phi_offsets[new_id] = phi;
            }
            let name = self.call_other_names[old_id].take();
            if let Some(n) = name {
                new_call_other_names[new_id] = Some(n);
            }
            let fp = std::mem::take(&mut self.asm_fingerprints[old_id]);
            if !fp.is_empty() {
                new_asm_fingerprints[new_id] = fp;
            }
            let ovr = self.call_clobbered_overrides[old_id].take();
            if let Some(v) = ovr {
                new_call_clobbered_overrides[new_id] = Some(v);
            }
        }
        self.stack_phi_offsets = new_stack_phi_offsets;
        self.call_other_names = new_call_other_names;
        self.asm_fingerprints = new_asm_fingerprints;
        self.call_clobbered_overrides = new_call_clobbered_overrides;

        remap
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
