//! Use-list bookkeeping — the validator's use-list-consistency contract.
//!
//! Every method that mutates an `(input → output)` edge updates both
//! directions of the doubly-linked use-list as a pair. A single-direction
//! update is a bug: the validator's use-list-consistency walk would catch it, but the
//! mutation itself must be correct or any later traversal sees a corrupt
//! graph. Cacheable nodes have their stale dedup-cache entry evicted before
//! the mutation (via `evict_cache_entry_if_cacheable` in `store`).

use anyhow::anyhow;
use smallvec::SmallVec;

use crate::iterators::InputCursor;
use crate::node::{NodeId, NodeInput, NodeInputId, NodeOutputId};

use super::Graph;

impl Graph {
    /// Inserts `input_id` at the head of the use-list of the output it
    /// references.
    ///
    /// Maintains the doubly-linked list stored inside [`NodeInput`] and
    /// [`crate::node::NodeOutput`] so that all consumers of an output can be
    /// iterated.
    pub(super) fn link_input_to_output_list(&mut self, input_id: NodeInputId) {
        // Callers guarantee input_id is freshly created (next/prev are None by construction).
        let input = &mut self.inputs[input_id];

        let output_id = input.output_id;
        let next_output_use = self.outputs[output_id].first_use;

        // Put it at the start of the linked list
        input.next = next_output_use;
        if let Some(next_use) = next_output_use.expand() {
            // The old head's prev must point to the new head, not to itself.
            self.inputs[next_use].prev = Some(input_id).into();
        }

        // Update the linked list of output_id uses
        self.outputs[output_id].first_use = Some(input_id).into();
    }

    /// Removes `input_id` from the use-list of the output it references.
    ///
    /// After this call the `prev`/`next` pointers of `input_id` are cleared
    /// so the entry can be safely abandoned.
    pub(super) fn unlink_input_from_output_list(&mut self, input_id: NodeInputId) {
        // Get the new input to be the use output_id
        let (output_id, prev, next) = {
            let input = &self.inputs[input_id];
            (input.output_id, input.prev, input.next)
        };
        let output = &mut self.outputs[output_id];

        // The input we want to remove is the first one - we need to update the output to point at the next one
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

    /// Appends a new input to `node_id` referencing `output_id`.
    ///
    /// Only valid for non-cacheable nodes (those whose inputs can grow after
    /// creation, e.g. `ControlState` and `VarPhi`).
    ///
    /// # Errors
    ///
    /// Returns an error if `node_id` has a cacheable kind — adding inputs
    /// after creation would invalidate the dedup cache key inserted by
    /// [`Graph::create_node`].
    pub fn add_node_input(
        &mut self,
        node_id: NodeId,
        output_id: NodeOutputId,
    ) -> crate::error::Result<()> {
        if self.node_kind(node_id).is_cacheable() {
            return Err(anyhow!(
                "attempted to add input to cacheable node {node_id:?}"
            ));
        }

        // Get the last input index to know the index for the new input
        let input_index = self.nodes[node_id].inputs.len(&self.input_pool) as u32;
        // Create the new input
        let input_id = self
            .inputs
            .push(NodeInput::new(output_id, node_id, input_index));
        // Add it to the inputs of the node
        self.nodes[node_id]
            .inputs
            .push(input_id, &mut self.input_pool);
        // Track the input in the linked list
        self.link_input_to_output_list(input_id);
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
        let delete_input_id = *slice.get(index).ok_or_else(|| {
            anyhow!(
                "input index {index} out of bounds for node {node_id:?} (len={len})"
            )
        })?;

        inputs.remove(index, &mut self.input_pool);
        for &input_id in &inputs.as_slice(&self.input_pool)[index..] {
            self.inputs[input_id].input_index -= 1;
        }
        self.unlink_input_from_output_list(delete_input_id);
        Ok(())
    }

    /// Redirects `input_id` to reference `output_id` instead of its current
    /// output.
    ///
    /// Removes `input_id` from the old output's use-list and inserts it into
    /// `output_id`'s use-list. If `input_id`'s owner node is cacheable, the
    /// stale dedup-cache entry is evicted so that a later `create_node` with
    /// the pre-change `(kind, inputs, outputs)` key cannot resurrect this
    /// now-modified node.
    pub fn update_input(&mut self, input_id: NodeInputId, output_id: NodeOutputId) {
        // Self-redirect: nothing changes, so do nothing. Avoids a spurious
        // unlink/relink (which would re-order the use-list) and a redundant
        // cache eviction.
        if self.inputs[input_id].output_id == output_id {
            return;
        }

        // Evict the cacheable owner's stale entry *before* mutating, while the
        // current (kind, inputs, output_kinds) tuple still describes the node.
        let owner = self.inputs[input_id].node_id;
        self.evict_cache_entry_if_cacheable(owner);

        // Remove the input usage on the current output id
        self.unlink_input_from_output_list(input_id);
        self.inputs[input_id].output_id = output_id;
        // Add usage of the new output_id
        self.link_input_to_output_list(input_id);
    }

    /// Returns a cursor over the use-list of `output_id`.
    ///
    /// The cursor allows iterating and modifying the use-list in place.
    #[inline]
    pub fn output_use_cursor(&mut self, output_id: NodeOutputId) -> InputCursor<'_> {
        let first_use = self.outputs[output_id].first_use.expand();
        InputCursor {
            graph: self,
            current: first_use,
        }
    }

    /// Removes all inputs from `node_id` and unlinks them from their
    /// respective output use-lists.
    ///
    /// After this call `node_id` has no inputs.
    pub fn detach_node_inputs(&mut self, node_id: NodeId) {
        // Evict before mutating — see `evict_cache_entry_if_cacheable` doc.
        self.evict_cache_entry_if_cacheable(node_id);

        let input_ids: SmallVec<[NodeInputId; 4]> =
            self.nodes[node_id].inputs.as_slice(&self.input_pool).into();
        for &input_id in &input_ids {
            self.unlink_input_from_output_list(input_id);
        }
        self.nodes[node_id].inputs.clear(&mut self.input_pool);
    }

    /// Returns an iterator over all inputs that consume `output_id`.
    /// Each item is `(consumer_node_id, input_index)` and the iteration
    /// follows the per-output use-list's intrusive next-pointer chain.
    #[inline]
    #[must_use]
    pub fn output_uses(&self, output_id: NodeOutputId) -> impl Iterator<Item = (NodeId, u32)> + '_ {
        let first_use = self.outputs[output_id].first_use.expand();
        core::iter::successors(first_use, move |id| self.inputs[*id].next.expand()).map(
            move |id| {
                let use_data = &self.inputs[id];
                (use_data.node_id, use_data.input_index)
            },
        )
    }

    /// Returns `true` if `value` is consumed by exactly one input.
    #[inline]
    #[must_use]
    pub fn output_has_one_usage(&self, value: NodeOutputId) -> bool {
        let mut uses = self.output_uses(value);
        uses.next().is_some() && uses.next().is_none()
    }

    /// Returns the head of `output`'s use-list as a raw [`NodeInputId`] (not
    /// wrapped in `OutputUsageIter`).  Intended for the validator to walk the
    /// list directly for corruption checks.
    #[inline]
    #[must_use]
    pub fn output_first_use_id(&self, output: NodeOutputId) -> Option<NodeInputId> {
        self.outputs[output].first_use.expand()
    }

    /// Returns the `next` pointer of `input` in its use-list.  Intended for
    /// the validator to walk the use-list directly.
    #[inline]
    #[must_use]
    pub fn input_next_use(&self, input: NodeInputId) -> Option<NodeInputId> {
        self.inputs[input].next.expand()
    }

    // ── Test-only corruption helpers ───────────────────────────────────────

    /// Test-only: forcibly clears the use-list head of `output`, breaking the
    /// forward link from the producer to its consumers.  Used to construct
    /// the corrupted state that the validator's use-list check should detect.
    #[cfg(test)]
    pub(crate) fn test_only_clear_first_use(&mut self, output: NodeOutputId) {
        self.outputs[output].first_use = None.into();
    }

    /// Test-only: forcibly retargets `input` to reference `new_target`
    /// without updating either the old or new output's use-list.  Used to
    /// construct the corrupted state that the validator's use-list check
    /// should detect.
    #[cfg(test)]
    pub(crate) fn test_only_retarget_input(
        &mut self,
        input: NodeInputId,
        new_target: NodeOutputId,
    ) {
        self.inputs[input].output_id = new_target;
    }
}
