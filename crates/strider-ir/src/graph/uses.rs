//! Use-list bookkeeping — the validator's use-list-consistency contract.
//!
//! Every method that mutates a `(use → value)` edge updates both
//! directions of the doubly-linked use-list as a pair. A single-direction
//! update is a bug: the validator's use-list-consistency walk would catch it, but the
//! mutation itself must be correct or any later traversal sees a corrupt
//! graph. Cacheable nodes have their stale dedup-cache entry evicted before
//! the mutation (via `evict_cache_entry_if_cacheable` in `store`).

use anyhow::anyhow;
use smallvec::SmallVec;

use crate::iterators::InputCursor;
use crate::node::{NodeId, UseData, UseId, ValueId};

use super::Graph;

impl Graph {
    /// Inserts `input_id` at the head of the use-list of the value it
    /// references.
    ///
    /// Maintains the doubly-linked list stored inside [`UseData`] and
    /// [`crate::node::ValueData`] so that all consumers of a value can be
    /// iterated.
    pub(super) fn link_use_to_value_list(&mut self, input_id: UseId) {
        // Callers guarantee input_id is freshly created (next/prev are None by construction).
        let input = &mut self.inputs[input_id];

        let value_id = input.value_id;
        let next_value_use = self.outputs[value_id].first_use;

        // Put it at the start of the linked list
        input.next = next_value_use;
        if let Some(next_use) = next_value_use.expand() {
            // The old head's prev must point to the new head, not to itself.
            self.inputs[next_use].prev = Some(input_id).into();
        }

        // Update the linked list of value_id uses
        self.outputs[value_id].first_use = Some(input_id).into();
    }

    /// Removes `input_id` from the use-list of the value it references.
    ///
    /// After this call the `prev`/`next` pointers of `input_id` are cleared
    /// so the entry can be safely abandoned.
    pub(super) fn unlink_use_from_value_list(&mut self, input_id: UseId) {
        // Get the new input to be the use value_id
        let (value_id, prev, next) = {
            let input = &self.inputs[input_id];
            (input.value_id, input.prev, input.next)
        };
        let output = &mut self.outputs[value_id];

        // The input we want to remove is the first one - we need to update the value to point at the next one
        if output.first_use.expand() == Some(input_id) {
            output.first_use = next;
        }

        // Change the previous one to point at the next one after input
        if let Some(prev) = prev.expand() {
            self.inputs[prev].next = next;
        }

        // Change the next one to point at the next one before input
        if let Some(next) = next.expand() {
            self.inputs[next].prev = prev;
        }

        // Remove the pointers so the current input won't point to junk when we don't track it anymore
        self.inputs[input_id].prev = None.into();
        self.inputs[input_id].next = None.into();
    }

    /// Appends a new input to `node_id` referencing `value_id`.
    ///
    /// Only valid for non-cacheable nodes (those whose inputs can grow after
    /// creation, e.g. `Region` and `VarPhi`).
    ///
    /// # Errors
    ///
    /// Returns an error if `node_id` has a cacheable kind — adding inputs
    /// after creation would invalidate the dedup cache key inserted by
    /// [`Graph::create_node`].
    pub fn add_node_input(
        &mut self,
        node_id: NodeId,
        value_id: ValueId,
    ) -> crate::error::Result<()> {
        if self.node_kind(node_id).is_cacheable() {
            return Err(anyhow!(
                "attempted to add input to cacheable node {node_id:?}"
            ));
        }

        // Get the last input index to know the index for the new input
        let input_index = self.nodes[node_id].inputs.len(&self.input_pool) as u32;
        // Create the new input
        let use_id = self
            .inputs
            .push(UseData::new(value_id, node_id, input_index));
        // Add it to the inputs of the node
        self.nodes[node_id]
            .inputs
            .push(use_id, &mut self.input_pool);
        // Track the input in the linked list
        self.link_use_to_value_list(use_id);
        Ok(())
    }

    /// Removes the input at position `index` from `node_id`.
    ///
    /// Adjusts the `input_index` of all subsequent inputs so that indices
    /// remain contiguous.  Only valid for non-cacheable nodes.
    ///
    /// # Errors
    ///
    /// - Returns an error if `node_id` has a cacheable kind.
    /// - Returns an error if `index` is past the node's current input count.
    pub fn remove_node_input(&mut self, node_id: NodeId, index: u32) -> crate::error::Result<()> {
        if self.node_kind(node_id).is_cacheable() {
            return Err(anyhow!(
                "attempted to remove input from cacheable node {node_id:?}"
            ));
        }
        let index = index as usize;
        let inputs = &mut self.nodes[node_id].inputs;
        let slice = inputs.as_slice(&self.input_pool);
        let len = slice.len();
        let delete_use_id = *slice.get(index).ok_or_else(|| {
            anyhow!(
                "input index {index} out of bounds for node {node_id:?} (len={len})"
            )
        })?;

        inputs.remove(index, &mut self.input_pool);
        for &use_id in &inputs.as_slice(&self.input_pool)[index..] {
            self.inputs[use_id].input_index -= 1;
        }
        self.unlink_use_from_value_list(delete_use_id);
        Ok(())
    }

    /// Redirects `input_id` to reference `value_id` instead of its current
    /// value.
    ///
    /// Removes `input_id` from the old value's use-list and inserts it into
    /// `value_id`'s use-list. If `input_id`'s owner node is cacheable, the
    /// stale dedup-cache entry is evicted so that a later `create_node` with
    /// the pre-change `(kind, inputs, outputs)` key cannot resurrect this
    /// now-modified node.
    pub fn update_input(&mut self, input_id: UseId, value_id: ValueId) {
        // Self-redirect: nothing changes, so do nothing. Avoids a spurious
        // unlink/relink (which would re-order the use-list) and a redundant
        // cache eviction.
        if self.inputs[input_id].value_id == value_id {
            return;
        }

        // Evict the cacheable owner's stale entry *before* mutating, while the
        // current (kind, inputs, output_kinds) tuple still describes the node.
        let owner = self.inputs[input_id].node_id;
        self.evict_cache_entry_if_cacheable(owner);

        // Remove the input usage on the current value id
        self.unlink_use_from_value_list(input_id);
        self.inputs[input_id].value_id = value_id;
        // Add usage of the new value_id
        self.link_use_to_value_list(input_id);
    }

    /// Returns a cursor over the use-list of `value_id`.
    ///
    /// The cursor allows iterating and modifying the use-list in place.
    #[inline]
    pub fn value_use_cursor(&mut self, value_id: ValueId) -> InputCursor<'_> {
        let first_use = self.outputs[value_id].first_use.expand();
        InputCursor {
            graph: self,
            current: first_use,
        }
    }

    /// Removes all inputs from `node_id` and unlinks them from their
    /// respective value use-lists.
    ///
    /// After this call `node_id` has no inputs.
    pub fn detach_node_inputs(&mut self, node_id: NodeId) {
        // Evict before mutating — see `evict_cache_entry_if_cacheable` doc.
        self.evict_cache_entry_if_cacheable(node_id);

        let use_ids: SmallVec<[UseId; 4]> =
            self.nodes[node_id].inputs.as_slice(&self.input_pool).into();
        for &use_id in &use_ids {
            self.unlink_use_from_value_list(use_id);
        }
        self.nodes[node_id].inputs.clear(&mut self.input_pool);
    }

    /// Returns an iterator over all inputs that consume `value_id`.
    /// Each item is `(consumer_node_id, input_index)` and the iteration
    /// follows the per-value use-list's intrusive next-pointer chain.
    #[inline]
    pub fn value_uses(&self, value_id: ValueId) -> impl Iterator<Item = (NodeId, u32)> + '_ {
        let first_use = self.outputs[value_id].first_use.expand();
        core::iter::successors(first_use, move |id| self.inputs[*id].next.expand()).map(
            move |id| {
                let use_data = &self.inputs[id];
                (use_data.node_id, use_data.input_index)
            },
        )
    }

    /// Returns `true` if `value` is consumed by exactly one input.
    #[inline]
    pub fn value_has_one_use(&self, value: ValueId) -> bool {
        let mut uses = self.value_uses(value);
        uses.next().is_some() && uses.next().is_none()
    }

    /// Returns the head of `value`'s use-list as a raw [`UseId`] (not
    /// wrapped in `OutputUsageIter`).  Intended for the validator to walk the
    /// list directly for corruption checks.
    #[inline]
    pub fn value_first_use_id(&self, value: ValueId) -> Option<UseId> {
        self.outputs[value].first_use.expand()
    }

    /// Returns the `next` pointer of `use_id` in its use-list.  Intended for
    /// the validator to walk the use-list directly.
    #[inline]
    pub fn next_use(&self, use_id: UseId) -> Option<UseId> {
        self.inputs[use_id].next.expand()
    }

    // ── Test-only corruption helpers ───────────────────────────────────────

    /// Test-only: forcibly clears the use-list head of `value`, breaking the
    /// forward link from the producer to its consumers.  Used to construct
    /// the corrupted state that the validator's use-list check should detect.
    #[cfg(test)]
    pub(crate) fn test_only_clear_first_use(&mut self, value: ValueId) {
        self.outputs[value].first_use = None.into();
    }

    /// Test-only: forcibly retargets `use_id` to reference `new_target`
    /// without updating either the old or new value's use-list.  Used to
    /// construct the corrupted state that the validator's use-list check
    /// should detect.
    #[cfg(test)]
    pub(crate) fn test_only_retarget_input(
        &mut self,
        use_id: UseId,
        new_target: ValueId,
    ) {
        self.inputs[use_id].value_id = new_target;
    }
}
