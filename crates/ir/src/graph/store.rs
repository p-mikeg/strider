//! Node arena, dedup cache, side-tables.
//!
//! Owns the methods that allocate nodes and feed the dedup cache that the
//! validator's Layer A consults indirectly. The eviction helper used by both
//! `update_input` and `detach_node_inputs` lives here too — both callers
//! invoke it before mutating, so the cache key always matches the node's
//! current inputs.

use smallvec::SmallVec;

use crate::node::{
    Node, NodeId, NodeInput, NodeInputId, NodeInputIdList, NodeKind, NodeOutput, NodeOutputId,
    NodeOutputIdList, NodeOutputKind,
};

use super::Graph;

impl Graph {
    /// Returns a reference to the kind of `node_id`.
    #[inline]
    #[must_use]
    pub fn node_kind(&self, node_id: NodeId) -> &NodeKind {
        &self.nodes[node_id].kind
    }

    /// Replaces the [`NodeKind`] of `node_id`.  Only valid when the
    /// pre-edit and post-edit kinds share the SAME input and output
    /// signatures (so the existing edges remain well-typed) and BOTH
    /// kinds are non-cacheable (so the dedup cache stays consistent —
    /// cacheable kinds key on `(kind, inputs, outputs)` so a kind
    /// mutation could orphan or collide cache entries).
    ///
    /// Used by the indirect-branch resolver to rewrite an
    /// `IndirectBranch` placeholder into a real `Return` in place,
    /// keeping the same `NodeId` so cached `exit_control` handles stay
    /// valid.
    ///
    /// # Errors
    ///
    /// Returns an error when either the old or the new kind is
    /// cacheable.
    pub fn set_node_kind(&mut self, node_id: NodeId, kind: NodeKind) -> crate::Result<()> {
        let old_kind = self.nodes[node_id].kind;
        if old_kind.is_cacheable() || kind.is_cacheable() {
            return Err(anyhow::anyhow!(
                "set_node_kind requires both kinds non-cacheable: old={old_kind:?}, new={kind:?}"
            ));
        }
        self.nodes[node_id].kind = kind;
        Ok(())
    }

    /// Returns the per-predecessor SP-relative offsets associated with a
    /// [`NodeKind::StackStorePhi`] node, or an empty slice if none are set.
    #[inline]
    #[must_use]
    pub fn stack_phi_offsets(&self, node_id: NodeId) -> &[i64] {
        // SecondaryMap returns the default value (empty Vec) for unset
        // ids, so an unset node yields an empty slice — same contract as
        // the previous HashMap-keyed accessor.
        self.stack_phi_offsets[node_id].as_slice()
    }

    /// Associates a list of per-predecessor SP-relative offsets with a
    /// [`NodeKind::StackStorePhi`] node.  Replaces any prior value.
    #[inline]
    pub fn set_stack_phi_offsets(&mut self, node_id: NodeId, offsets: Vec<i64>) {
        self.stack_phi_offsets[node_id] = offsets;
    }

    /// Returns the user-op name associated with a [`NodeKind::CallOther`]
    /// node, or `None` if no name has been recorded for that node.
    #[inline]
    #[must_use]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.call_other_names[node_id].as_deref()
    }

    /// Associates a user-op name with a [`NodeKind::CallOther`] node.
    /// Replaces any prior value.
    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: String) {
        self.call_other_names[node_id] = Some(name);
    }

    /// Returns the per-Call clobber-list override for `node_id`, or
    /// `None` if the Call uses the function-default
    /// [`crate::function::BuiltFunctionGraph::call_clobbered`].
    #[inline]
    #[must_use]
    pub fn call_clobbered_override(&self, node_id: NodeId) -> Option<&[rsleigh::Vn]> {
        self.call_clobbered_overrides[node_id].as_deref()
    }

    /// Records `clobbered` as the per-Call clobber-list override for
    /// `node_id`.  Replaces any prior value.  Pass an empty `Vec` to
    /// declare "this Call clobbers nothing" (e.g. `__fentry__`).
    #[inline]
    pub fn set_call_clobbered_override(&mut self, node_id: NodeId, clobbered: Vec<rsleigh::Vn>) {
        self.call_clobbered_overrides[node_id] = Some(clobbered);
    }

    /// Returns the asm-instruction-address fingerprint of `node_id` as a
    /// sorted-deduplicated slice.  Returns an empty slice when no
    /// contributors have been recorded.
    #[inline]
    #[must_use]
    pub fn asm_fingerprint(&self, node_id: NodeId) -> &[u64] {
        self.asm_fingerprints[node_id].as_slice()
    }

    /// Replaces `node_id`'s fingerprint with `addrs`.
    ///
    /// Sorts and deduplicates `addrs` first so callers cannot accidentally
    /// install an unsorted entry.  This is the test-only / synthetic-graph
    /// entry point: production passes use
    /// [`Self::extend_asm_fingerprint`] / [`Self::extend_asm_fingerprint_from`]
    /// to preserve the superset-only invariant.
    #[inline]
    pub fn set_asm_fingerprint(&mut self, node_id: NodeId, mut addrs: Vec<u64>) {
        addrs.sort_unstable();
        addrs.dedup();
        self.asm_fingerprints[node_id] = addrs;
    }

    /// Unions `contributors` into `node_id`'s fingerprint.  Result is
    /// kept sorted and deduplicated.  Existing entries are never
    /// removed: this satisfies the no-shrink contract.  Empty
    /// `contributors` is a no-op (no allocation, no reallocation).
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        if contributors.is_empty() {
            return;
        }
        let existing = &mut self.asm_fingerprints[node_id];
        // Fast path: pushing strictly-greater elements one by one is the
        // common lift-time case (insns processed in increasing address order
        // means the new contributor is usually `>` the last entry).
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

    /// Unions the fingerprint of `src` into `dst`.  Equivalent to
    /// `extend_asm_fingerprint(dst, &asm_fingerprint(src).to_vec())` but
    /// avoids the intermediate allocation.  Self-extension (`src == dst`)
    /// is a no-op.
    pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        // SAFETY-WORKAROUND: SecondaryMap doesn't allow simultaneous
        // borrows.  Snapshot the source slice into a tiny stack-friendly
        // buffer.  Fingerprints are typically small.
        let src_slice = self.asm_fingerprints[src].clone();
        self.extend_asm_fingerprint(dst, &src_slice);
    }

    /// Creates a new node with the given kind, inputs, and output kinds.
    ///
    /// For cacheable node kinds (see [`NodeKind::is_cacheable`]), an identical
    /// node that already exists in the graph is returned instead of creating a
    /// duplicate.  Non-cacheable nodes always produce a fresh [`NodeId`].
    ///
    /// The inputs are recorded as [`NodeInput`] entries and added to the
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

        // Build the cache key only for cacheable kinds; otherwise the two
        // `Vec`s would be allocated and discarded on every call.
        let cache_key = if kind.is_cacheable() {
            let key = (node, inputs.to_vec(), output_kinds.to_vec());
            if let Some(node_id) = self.node_to_id.get(&key) {
                return *node_id;
            }
            Some(key)
        } else {
            None
        };

        let node_id = self.nodes.push(node);
        if let Some(key) = cache_key {
            self.node_to_id.insert(key, node_id);
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
