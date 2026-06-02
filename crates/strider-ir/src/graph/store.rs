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
    Node, NodeId, NodeInput, UseId, NodeInputIdList, NodeKind, NodeOutput, ValueId,
    ValueIdList, ValueKind, ValueType,
};

use super::Graph;

/// Hashes a borrowed dedup-cache key.  Must produce the same hash as the
/// derived `Hash` impl on the owned `(Node, Vec<ValueId>, Vec<ValueKind>)`
/// tuple so that lookups using the borrowed shape land in the same bucket
/// as inserts using the owned shape.  `Vec<T>: Hash` and `[T]: Hash` agree
/// (both hash the length followed by each element), and the tuple's
/// derived `Hash` hashes its fields in declaration order — so the borrowed
/// hash below matches the owned-key derived hash field-for-field.
#[inline]
fn hash_borrowed_key<S: BuildHasher>(
    hasher: &S,
    node: &Node,
    inputs: &[ValueId],
    output_kinds: &[ValueKind],
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
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        let inputs: SmallVec<[ValueId; 4]> = inputs.into_iter().collect();
        let output_kinds: SmallVec<[ValueKind; 4]> = output_kinds.into_iter().collect();

        // Single source of truth for `IntConst` payload normalisation: mask
        // the stored value to its declared integer output type's bit width
        // *before* the dedup-cache key is computed, so every `IntConst` node —
        // whatever its creation path (lifter sub-register read, rewrite
        // `int_const_with!` closure, `make_int_const`, …) — carries the
        // canonical narrow payload.  Without this, a big-endian read can mint
        // an `IntConst(0xff..ff_fffffffc):I64` while another path mints the
        // 64-bit-masked `IntConst(0xfffffffc...):I64`; both are semantically
        // `-4` at `I64` but key the cache differently and never dedup.
        //
        // Only the narrow integer `Typed` case is touched: wide
        // constants (`I256`/`I512`) flow through `IntConstWide`, and
        // non-integer / non-value outputs are left alone.  `make_int_const`
        // already masks, so this is idempotent for that path.
        let kind = match (kind, output_kinds.as_slice()) {
            (NodeKind::IntConst(v), [ValueKind::Typed(ty)])
                if ty.is_integer()
                    && !matches!(ty, ValueType::I256 | ValueType::I512) =>
            {
                NodeKind::IntConst(v & ty.bit_mask_u128())
            }
            (kind, _) => kind,
        };
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
        let inputs: SmallVec<[UseId; 2]> = inputs
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
        self.nodes[node_id].outputs = ValueIdList::from_iter(outputs, &mut self.output_pool);

        node_id
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
        let input_outputs: Vec<ValueId> = self.nodes[node_id]
            .inputs
            .as_slice(&self.input_pool)
            .iter()
            .map(|&iid| self.inputs[iid].output_id)
            .collect();
        let output_kinds: Vec<ValueKind> = self.nodes[node_id]
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
