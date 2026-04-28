//! Node arena, dedup cache, side-tables.
//!
//! Owns the methods that allocate nodes and feed the dedup cache that the
//! validator's Layer A consults indirectly. The eviction helper used by both
//! `update_input` and `detach_node_inputs` lives here too — both callers
//! invoke it before mutating, so the cache key always matches the node's
//! current inputs.

use smallvec::SmallVec;

use crate::node::{
    Fingerprint, Node, NodeId, NodeInput, NodeInputId, NodeInputIdList, NodeKind, NodeOutput,
    NodeOutputId, NodeOutputIdList, NodeOutputKind,
};

use super::Graph;

impl Graph {
    /// Returns a reference to the kind of `node_id`.
    #[inline]
    #[must_use]
    pub fn node_kind(&self, node_id: NodeId) -> &NodeKind {
        &self.nodes[node_id].kind
    }

    /// Returns the per-predecessor SP-relative offsets associated with a
    /// [`NodeKind::StackStorePhi`] node, or an empty slice if none are set.
    #[inline]
    #[must_use]
    pub fn stack_phi_offsets(&self, node_id: NodeId) -> &[i64] {
        self.stack_phi_offsets
            .get(&node_id)
            .map_or(&[], |v| v.as_slice())
    }

    /// Associates a list of per-predecessor SP-relative offsets with a
    /// [`NodeKind::StackStorePhi`] node.  Replaces any prior value.
    #[inline]
    pub fn set_stack_phi_offsets(&mut self, node_id: NodeId, offsets: Vec<i64>) {
        self.stack_phi_offsets.insert(node_id, offsets);
    }

    /// Returns the user-op name associated with a [`NodeKind::CallOther`]
    /// node, or `None` if no name has been recorded for that node.
    #[inline]
    #[must_use]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.call_other_names.get(&node_id).map(|s| s.as_str())
    }

    /// Associates a user-op name with a [`NodeKind::CallOther`] node.
    /// Replaces any prior value.
    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: String) {
        self.call_other_names.insert(node_id, name);
    }

    /// Creates a new node with the given kind, inputs, and output kinds.
    ///
    /// For cacheable node kinds (see [`NodeKind::is_cacheable`]), an identical
    /// node that already exists in the graph is returned instead of creating a
    /// duplicate.  Non-cacheable nodes always produce a fresh [`NodeId`].
    ///
    /// The inputs are recorded as [`NodeInput`] entries and added to the
    /// use-list of each referenced output so that consumers can be iterated.
    ///
    /// F1 provenance: the new node's [`Fingerprint`] is auto-merged from the
    /// input producers' fingerprints, so most call sites get correct
    /// provenance with zero source changes.  Lift sites that need to seed
    /// the per-pcode-insn address (and constant-fold rules whose replacement
    /// nodes have no inputs) override via [`Graph::set_fingerprint`].
    pub fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        let inputs: SmallVec<[NodeOutputId; 4]> = inputs.into_iter().collect();
        let output_kinds: SmallVec<[NodeOutputKind; 4]> = output_kinds.into_iter().collect();
        let node = Node::new(kind);

        // F1: pre-compute the merged input fingerprint BEFORE the cache hit
        // check so cache-hits and fresh nodes share the same propagation
        // contract.  For a cache hit, an identical (kind, inputs, output_kinds)
        // already exists, so its fingerprint is necessarily a superset of (or
        // equal to) the merge — which is what we want; the cached node carries
        // the full provenance from the first construction.
        //
        // CORRECTNESS: cache lookup happens against (kind, inputs,
        // output_kinds), not fingerprint.  Two structurally identical creates
        // dedup; the fingerprint merge below would be the same.
        let merged_fp = Fingerprint::merge_many(
            inputs
                .iter()
                .map(|out| self.fingerprint_of(self.get_node_from_output(*out))),
        );

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

        // F1: install the auto-merged input fingerprint.  Lift sites or
        // fold-rule overrides may replace this via set_fingerprint.
        self.fingerprints[node_id] = merged_fp;
        node_id
    }

    /// Returns the [`Fingerprint`] associated with `node_id`.
    ///
    /// Nodes that never had an explicit fingerprint set return the default
    /// (empty) fingerprint without panicking — synthetic test nodes and
    /// graphs built before fingerprint plumbing both rely on this.
    #[inline]
    #[must_use]
    pub fn fingerprint_of(&self, node_id: NodeId) -> &Fingerprint {
        &self.fingerprints[node_id]
    }

    /// Replaces the [`Fingerprint`] for `node_id` with `fp`.
    ///
    /// Used at lift sites to seed the originating pcode address and by
    /// fold rules whose replacement nodes have no inputs (constant-fold
    /// of `Add(IntConst(a), IntConst(b))` to `IntConst(a+b)`, where the
    /// auto-merge yields the empty fingerprint and the rule must inherit
    /// the OLD node's provenance instead).
    #[inline]
    pub fn set_fingerprint(&mut self, node_id: NodeId, fp: Fingerprint) {
        self.fingerprints[node_id] = fp;
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
