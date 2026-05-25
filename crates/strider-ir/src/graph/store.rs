//! Node arena, dedup cache.
//!
//! Owns the methods that allocate nodes and feed the dedup cache that the
//! validator's local-typing check consults indirectly. The eviction helper used by both
//! `update_input` and `detach_node_inputs` lives here too — both callers
//! invoke it before mutating, so the cache key always matches the node's
//! current inputs.

use std::hash::{BuildHasher, Hash, Hasher};

use hashbrown::hash_map::RawEntryMut;
use smallvec::SmallVec;

use crate::node::{
    Node, NodeId, NodeInput, NodeInputId, NodeInputIdList, NodeKind, NodeOutput, NodeOutputId,
    NodeOutputIdList, NodeOutputKind,
};

use super::Graph;

/// Hashes a borrowed dedup-cache key.  Must produce the same hash as the
/// derived `Hash` impl on the owned `(Node, Vec<NodeOutputId>, Vec<NodeOutputKind>)`
/// tuple so that lookups using the borrowed shape land in the same bucket
/// as inserts using the owned shape.  `Vec<T>: Hash` and `[T]: Hash` agree
/// (both hash the length followed by each element), and the tuple's
/// derived `Hash` hashes its fields in declaration order — so the borrowed
/// hash below matches the owned-key derived hash field-for-field.
#[inline]
fn hash_borrowed_key<S: BuildHasher>(
    hasher: &S,
    node: &Node,
    inputs: &[NodeOutputId],
    output_kinds: &[NodeOutputKind],
) -> u64 {
    let mut h = hasher.build_hasher();
    node.hash(&mut h);
    inputs.hash(&mut h);
    output_kinds.hash(&mut h);
    h.finish()
}

impl Graph {
    /// Returns a reference to the kind of `node_id`.
    #[inline]
    #[must_use]
    pub fn node_kind(&self, node_id: NodeId) -> &NodeKind {
        &self.nodes[node_id].kind
    }

/// Returns the [`NodeId`] of the `InitialVar(vn)` node registered
    /// for `vn`, or `None` if none has been registered on this graph.
    ///
    /// O(1) hash lookup.  Callers that want to skip detached zombie
    /// `InitialVar` nodes must validate the returned id themselves —
    /// typically by checking the node's single output's use-list via
    /// [`Self::output_uses`].
    ///
    /// Maintained at every canonical `InitialVar` creation site (the
    /// lift-time `FunctionBuilder::set_entry_region` path and the
    /// orchestrator's lazy `read_or_init_var` fallback).
    #[inline]
    #[must_use]
    pub fn initial_var_for(&self, vn: rsleigh::Vn) -> Option<NodeId> {
        self.initial_var_index.get(&vn).copied()
    }

    /// Registers `(vn, node_id)` in the `InitialVar` index.  Replaces
    /// any prior entry for `vn`.  See [`Self::initial_var_for`].
    ///
    /// Callers must guarantee that `node_id`'s kind is
    /// `NodeKind::InitialVar(vn)` — the index is advisory and the
    /// graph does not re-check the kind on lookup.
    #[inline]
    pub fn register_initial_var(&mut self, vn: rsleigh::Vn, node_id: NodeId) {
        self.initial_var_index.insert(vn, node_id);
    }

    /// Creates a new node with the given kind, inputs, and output kinds.
    ///
    /// For cacheable node kinds (see [`NodeKind::is_cacheable`]), an identical
    /// node that already exists in the graph is returned instead of creating a
    /// duplicate.  Non-cacheable nodes always produce a fresh [`NodeId`].
    ///
    /// The inputs are recorded as `NodeInput` entries and added to the
    /// use-list of each referenced output so that consumers can be iterated.
    pub fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        let inputs: SmallVec<[NodeOutputId; 4]> = inputs.into_iter().collect();
        let output_kinds: SmallVec<[NodeOutputKind; 4]> = output_kinds.into_iter().collect();
        let node = Node::new(kind);

        // For cacheable kinds, look up via a borrowed `(&Node, &[…], &[…])`
        // shape so a cache *hit* never allocates the two `Vec`s.  Only the
        // miss path allocates the owned key for insertion below.
        //
        // We hash the borrowed triple manually (`hash_borrowed_key`) and
        // probe via `raw_entry_mut().from_hash(…)`; the comparator then
        // dereferences the owned key tuple's fields and compares them as
        // slices against our borrowed view.  See `hash_borrowed_key`'s
        // doc-comment for why the borrowed and owned hashes coincide.
        //
        // The `BuildHasher` is cloned out of the map up-front so we can
        // re-use it inside `insert_with_hasher`'s rehash closure (which
        // can't reborrow `self.node_to_id` while the `RawEntryMut`
        // already holds it mutably).
        let cache_slot = if kind.is_cacheable() {
            let hasher = self.node_to_id.hasher().clone();
            let hash = hash_borrowed_key(&hasher, &node, &inputs, &output_kinds);
            match self.node_to_id.raw_entry_mut().from_hash(hash, |k| {
                k.0 == node
                    && k.1.as_slice() == inputs.as_slice()
                    && k.2.as_slice() == output_kinds.as_slice()
            }) {
                RawEntryMut::Occupied(entry) => return *entry.get(),
                RawEntryMut::Vacant(entry) => Some((hasher, hash, entry)),
            }
        } else {
            None
        };

        let node_id = self.nodes.push(node);
        if let Some((hasher, hash, entry)) = cache_slot {
            entry.insert_with_hasher(
                hash,
                (node, inputs.to_vec(), output_kinds.to_vec()),
                node_id,
                |k| hash_borrowed_key(&hasher, &k.0, k.1.as_slice(), k.2.as_slice()),
            );
        }

        // Add all inputs to the graph
        let inputs: SmallVec<[NodeInputId; 2]> = inputs
            .into_iter()
            .enumerate()
            .map(|(index, output)| {
                self.inputs
                    .push(NodeInput::new(output, node_id, index as u32))
            })
            .collect();

        // Make sure that the inputs store their usage of the output
        for &input_use in &inputs {
            self.link_input_to_output_list(input_use);
        }

        // Create outputs for the given node
        let outputs = output_kinds.into_iter().enumerate().map(|(index, kind)| {
            self.outputs
                .push(NodeOutput::new(kind, node_id, index as u32))
        });

        // Update the node state
        self.nodes[node_id].inputs = NodeInputIdList::from_iter(inputs, &mut self.input_pool);
        self.nodes[node_id].outputs = NodeOutputIdList::from_iter(outputs, &mut self.output_pool);

        node_id
    }

    /// Re-types the `Memory(_)` output `output_id` to carry partition
    /// `partition` (use `None` to revert to unified).  Errors if the
    /// output is not a `Memory(_)` slot.
    ///
    /// Used by the `AliasSplit` optimization to retype the memory
    /// outputs of `Store` / `MemPhi`
    /// nodes that fall inside a single-alias-class subgraph.  The
    /// cacheable owner's stale dedup-cache entry is evicted *before*
    /// the mutation (same discipline as [`Graph::update_input`]) so a
    /// later `create_node` with the pre-change `(kind, inputs, outputs)`
    /// key cannot resurrect this now-modified node.
    ///
    /// # Errors
    ///
    /// Returns an error when `output_id` is not a `Memory(_)` slot —
    /// repartitioning a Control / Value / PhiToken output is meaningless
    /// and indicates a caller bug.
    pub fn set_memory_partition(
        &mut self,
        output_id: NodeOutputId,
        partition: Option<crate::mem_project::AliasClass>,
    ) -> crate::error::Result<()> {
        let current = self.outputs[output_id].kind;
        if !matches!(current, NodeOutputKind::Memory(_)) {
            return Err(anyhow::anyhow!(
                "Graph::set_memory_partition: output {output_id:?} is not a Memory slot \
                 (current kind = {current:?})"
            ));
        }
        // Evict the owner's stale cache entry before mutating so the
        // pre-change key cannot resurrect this node from a later
        // `create_node` call.
        let owner = self.outputs[output_id].source_id;
        self.evict_cache_entry_if_cacheable(owner);
        self.outputs[output_id].kind = NodeOutputKind::Memory(partition);
        Ok(())
    }

    /// Removes `node_id` from the dedup cache (using its *current* inputs and
    /// output kinds as the key) when its kind is cacheable. No-op for
    /// non-cacheable kinds, which were never inserted in the first place.
    ///
    /// Both `update_input` and `detach_node_inputs` call this *before*
    /// mutating the node, so the stale entry can never resurrect a node whose
    /// inputs no longer match the original key.
    pub(super) fn evict_cache_entry_if_cacheable(&mut self, node_id: NodeId) {
        if !self.nodes[node_id].kind.is_cacheable() {
            return;
        }
        let input_outputs: Vec<NodeOutputId> = self.nodes[node_id]
            .inputs
            .as_slice(&self.input_pool)
            .iter()
            .map(|&iid| self.inputs[iid].output_id)
            .collect();
        let output_kinds: Vec<NodeOutputKind> = self.nodes[node_id]
            .outputs
            .as_slice(&self.output_pool)
            .iter()
            .map(|&oid| self.outputs[oid].kind)
            .collect();
        let key = (
            Node::new(self.nodes[node_id].kind),
            input_outputs,
            output_kinds,
        );
        self.node_to_id.remove(&key);
    }
}
