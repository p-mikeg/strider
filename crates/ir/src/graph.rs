use cranelift_entity::{ListPool, PrimaryMap};
use std::collections::HashMap;

use smallvec::SmallVec;

use crate::iterators::InputCursor;

use super::iterators::{Inputs, OutputUsageIter, Outputs};
use super::node::*;

/// The core IR graph structure.
///
/// Stores nodes, their input/output slots, and a deduplication cache for
/// cacheable node kinds.  All ids (node, output, input) are small integers
/// allocated from dense entity maps, so they can be used as cheap, copyable
/// handles.
#[derive(Clone)]
pub struct Graph {
    /// Dense map from [`NodeId`] to [`Node`] metadata.
    pub(crate) nodes: PrimaryMap<NodeId, Node>,
    /// Dense map from [`NodeOutputId`] to [`NodeOutput`] metadata.
    pub(crate) outputs: PrimaryMap<NodeOutputId, NodeOutput>,
    /// Dense map from [`NodeInputId`] to [`NodeInput`] metadata.
    pub(crate) inputs: PrimaryMap<NodeInputId, NodeInput>,
    /// Pool backing the per-node output id lists.
    pub(crate) output_pool: ListPool<NodeOutputId>,
    /// Pool backing the per-node input id lists.
    pub(crate) input_pool: ListPool<NodeInputId>,
    /// Deduplication cache: maps `(Node, inputs, output_kinds)` → `NodeId`
    /// for cacheable node kinds.
    pub(crate) node_to_id: HashMap<(Node, Vec<NodeOutputId>, Vec<NodeOutputKind>), NodeId>,
    /// Side-map from [`NodeKind::StackStorePhi`] nodes to their per-predecessor
    /// SP-relative offsets.  Kept external so that `NodeKind` stays `Copy`.
    pub(crate) stack_phi_offsets: HashMap<NodeId, Vec<i64>>,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    /// Creates an empty graph.
    pub fn new() -> Self {
        Graph {
            nodes: PrimaryMap::new(),
            outputs: PrimaryMap::new(),
            inputs: PrimaryMap::new(),
            output_pool: ListPool::new(),
            input_pool: ListPool::new(),
            node_to_id: HashMap::new(),
            stack_phi_offsets: HashMap::new(),
        }
    }

    /// Returns a reference to the kind of `node_id`.
    #[inline]
    pub fn node_kind(&self, node_id: NodeId) -> &NodeKind {
        &self.nodes[node_id].kind
    }

    /// Returns the per-predecessor SP-relative offsets associated with a
    /// [`NodeKind::StackStorePhi`] node, or an empty slice if none are set.
    #[inline]
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

        // Check if node is already in cache
        let node_entry = (node, inputs.to_vec(), output_kinds.to_vec());
        if let Some(node_id) = self.node_to_id.get(&node_entry) {
            return *node_id;
        }
        // Create a new node id
        let node_id = self.nodes.push(node);
        // Store the new node id if the node is allowed to be cached
        if kind.is_cacheable() {
            self.node_to_id.insert(node_entry, node_id);
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

    /// Inserts `input_id` at the head of the use-list of the output it
    /// references.
    ///
    /// Maintains the doubly-linked list stored inside [`NodeInput`] and
    /// [`NodeOutput`] so that all consumers of an output can be iterated.
    fn link_input_to_output_list(&mut self, input_id: NodeInputId) {
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
    fn unlink_input_from_output_list(&mut self, input_id: NodeInputId) {
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
    /// creation, e.g. `ControlState` and `ControlPhi`).  Panics if called on
    /// a cacheable node.
    pub fn add_node_input(
        &mut self,
        node_id: NodeId,
        output_id: NodeOutputId,
    ) -> crate::error::Result<()> {
        if self.node_kind(node_id).is_cacheable() {
            return Err(crate::error::ErrorKind::AddInputToCacheableNode(node_id).into());
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
    pub fn remove_node_input(&mut self, node_id: NodeId, index: u32) -> crate::error::Result<()> {
        if self.node_kind(node_id).is_cacheable() {
            return Err(crate::error::ErrorKind::AddInputToCacheableNode(node_id).into());
        }
        let index = index as usize;
        let inputs = &mut self.nodes[node_id].inputs;
        // Store the input to unlink later
        let delete_input_id = inputs.as_slice(&self.input_pool)[index];

        // Remove the input from the node
        inputs.remove(index, &mut self.input_pool);
        // Adjust input indices for any of the remaining inputs
        for &input_id in &inputs.as_slice(&self.input_pool)[index..] {
            self.inputs[input_id].input_index -= 1;
        }
        // Untrack the output usage in the linked list
        self.unlink_input_from_output_list(delete_input_id);
        Ok(())
    }

    /// Returns the [`NodeOutputKind`] of `output_id`.
    pub fn output_kind(&self, output_id: NodeOutputId) -> NodeOutputKind {
        self.outputs[output_id].kind
    }

    /// Returns the `(NodeId, output_index)` pair that defines `output_id`.
    #[inline]
    pub fn output_definition(&self, output_id: NodeOutputId) -> (NodeId, u32) {
        let data = &self.outputs[output_id];
        (data.source_id, data.output_index)
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
        // If the owning node is cacheable, evict its current dedup-cache entry
        // *before* mutating the input. The key is built from the current
        // (kind, inputs, output_kinds) triple, which is about to change.
        let owner = self.inputs[input_id].node_id;
        if self.nodes[owner].kind.is_cacheable() {
            let input_outputs: Vec<NodeOutputId> = self.nodes[owner]
                .inputs
                .as_slice(&self.input_pool)
                .iter()
                .map(|&iid| self.inputs[iid].output_id)
                .collect();
            let output_kinds: Vec<NodeOutputKind> = self.nodes[owner]
                .outputs
                .as_slice(&self.output_pool)
                .iter()
                .map(|&oid| self.outputs[oid].kind)
                .collect();
            let stale_key = (
                Node::new(self.nodes[owner].kind),
                input_outputs,
                output_kinds,
            );
            self.node_to_id.remove(&stale_key);
        }

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
        // Get all input ids of the node
        let input_ids: SmallVec<[NodeInputId; 4]> =
            self.nodes[node_id].inputs.as_slice(&self.input_pool).into();

        // Remove the node from the dedup cache before we mutate its inputs —
        // otherwise a later `create_node` with the original (kind, inputs,
        // outputs) key would return this zombie with an empty input list.
        if self.nodes[node_id].kind.is_cacheable() {
            let input_outputs: Vec<NodeOutputId> = input_ids
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

        // Remove their dependency on the output
        for &input_id in &input_ids {
            self.unlink_input_from_output_list(input_id);
        }
        // Delete the inputs from the node
        self.nodes[node_id].inputs.clear(&mut self.input_pool);
    }

    /// Returns the slice of output ids for `node_id`.
    #[inline]
    pub fn node_outputs(&self, node_id: NodeId) -> Outputs<'_> {
        Outputs(self.nodes[node_id].outputs.as_slice(&self.output_pool))
    }

    /// Returns exactly `N` output ids for `node_id`.
    ///
    /// Returns an error if the node does not have exactly `N` outputs.
    #[inline]
    pub fn node_outputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[NodeOutputId; N]> {
        let outputs = self.node_outputs(node_id);
        if outputs.len() != N {
            return Err(
                crate::error::ErrorKind::WrongOutputCount(node_id, N, outputs.len()).into(),
            );
        }
        let mut result = [NodeOutputId::default(); N];
        for (i, v) in outputs.into_iter().enumerate() {
            result[i] = v;
        }
        Ok(result)
    }

    /// Returns an iterator over the values consumed by `node_id`'s inputs.
    #[inline]
    pub fn node_inputs(&self, node_id: NodeId) -> Inputs<'_> {
        Inputs {
            graph: self,
            use_list: self.nodes[node_id].inputs.as_slice(&self.input_pool),
        }
    }

    /// Returns exactly `N` input values for `node_id`.
    ///
    /// Returns an error if the node does not have exactly `N` inputs.
    #[inline]
    pub fn node_inputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[NodeOutputId; N]> {
        let inputs = self.node_inputs(node_id);
        if inputs.len() != N {
            return Err(crate::error::ErrorKind::WrongInputCount(node_id, N, inputs.len()).into());
        }
        let mut result = [NodeOutputId::default(); N];
        for (i, v) in inputs.into_iter().enumerate() {
            result[i] = v;
        }
        Ok(result)
    }

    /// Returns the [`NodeId`] that produces `output_id`.
    #[inline]
    pub fn get_node_from_output(&self, output_id: NodeOutputId) -> NodeId {
        self.outputs[output_id].source_id
    }

    /// Returns an iterator over all inputs that consume `output_id`.
    #[inline]
    pub fn output_uses(&self, output_id: NodeOutputId) -> OutputUsageIter<'_> {
        let first_use = self.outputs[output_id].first_use.expand();
        OutputUsageIter {
            graph: self,
            cur_use: first_use,
        }
    }

    /// Returns `true` if `value` is consumed by exactly one input.
    #[inline]
    pub fn output_has_one_usage(&self, value: NodeOutputId) -> bool {
        let mut uses = self.output_uses(value);
        uses.next().is_some() && uses.next().is_none()
    }

    /// Returns the [`NodeInputId`] of the input slot at position `idx` of
    /// `node`.
    ///
    /// Panics if `idx` is out of range; intended for consumers (such as the
    /// validator) that have already established the slot exists.
    #[inline]
    pub fn node_input_id_at(&self, node: NodeId, idx: usize) -> NodeInputId {
        self.nodes[node].inputs.as_slice(&self.input_pool)[idx]
    }

    /// Returns the [`NodeOutputId`] that `input` currently references.
    #[inline]
    pub fn input_output_id(&self, input: NodeInputId) -> NodeOutputId {
        self.inputs[input].output_id
    }

    /// Returns the head of `output`'s use-list as a raw [`NodeInputId`] (not
    /// wrapped in `OutputUsageIter`).  Intended for the validator to walk the
    /// list directly for corruption checks.
    #[inline]
    pub fn output_first_use_id(&self, output: NodeOutputId) -> Option<NodeInputId> {
        self.outputs[output].first_use.expand()
    }

    /// Returns the `next` pointer of `input` in its use-list.  Intended for
    /// the validator to walk the use-list directly.
    #[inline]
    pub fn input_next_use(&self, input: NodeInputId) -> Option<NodeInputId> {
        self.inputs[input].next.expand()
    }

    // ── Test-only corruption helpers ───────────────────────────────────────

    /// Test-only: forcibly clears the use-list head of `output`, breaking the
    /// forward link from the producer to its consumers.  Used to construct
    /// the corrupted state that Layer B of the validator should detect.
    #[cfg(test)]
    pub(crate) fn test_only_clear_first_use(&mut self, output: NodeOutputId) {
        self.outputs[output].first_use = None.into();
    }

    /// Test-only: forcibly retargets `input` to reference `new_target`
    /// without updating either the old or new output's use-list.  Used to
    /// construct the corrupted state that Layer B of the validator should
    /// detect.
    #[cfg(test)]
    pub(crate) fn test_only_retarget_input(
        &mut self,
        input: NodeInputId,
        new_target: NodeOutputId,
    ) {
        self.inputs[input].output_id = new_target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    #[track_caller]
    fn check_node_inputs(
        graph: &Graph,
        node_id: NodeId,
        expected: impl IntoIterator<Item = NodeOutputId>,
    ) {
        let expected: Vec<_> = expected.into_iter().collect();
        let actual: Vec<_> = graph.node_inputs(node_id).into_iter().collect();
        assert_eq!(actual, expected);
    }

    #[track_caller]
    fn check_node_output_kinds(
        graph: &Graph,
        node_id: NodeId,
        expected: impl IntoIterator<Item = NodeOutputKind>,
    ) {
        let expected: Vec<_> = expected.into_iter().collect();
        let actual: Vec<_> = graph
            .node_outputs(node_id)
            .into_iter()
            .map(|output_id| graph.output_kind(output_id))
            .collect();
        assert_eq!(actual, expected);
    }

    #[track_caller]
    fn check_node_output_defintions(
        graph: &Graph,
        node_id: NodeId,
        expected: impl IntoIterator<Item = (NodeId, u32)>,
    ) {
        let expected: Vec<_> = expected.into_iter().collect();
        let actual: Vec<_> = graph
            .node_outputs(node_id)
            .into_iter()
            .map(|output_id| graph.output_definition(output_id))
            .collect();
        assert_eq!(actual, expected);
    }

    /// Creates a simple constant node (no inputs) and checks that its
    /// metadata is stored correctly.
    #[test]
    fn create_single_node() {
        let mut graph = Graph::new();
        let node_id = graph.create_node(
            NodeKind::IntConst(5),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        assert_eq!(graph.node_kind(node_id), &NodeKind::IntConst(5));
        assert_eq!(graph.nodes.len(), 1);
        check_node_inputs(&graph, node_id, []);
        check_node_output_kinds(
            &graph,
            node_id,
            vec![NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        check_node_output_defintions(&graph, node_id, vec![(node_id, 0)]);
    }

    /// Cacheable nodes with identical kind and inputs must be deduplicated:
    /// the second call must return the same [`NodeId`] as the first and must
    /// not grow the node table.
    #[test]
    fn cacheable_node_is_deduplicated() {
        let mut graph = Graph::new();
        let id_a = graph.create_node(
            NodeKind::IntConst(42),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let id_b = graph.create_node(
            NodeKind::IntConst(42),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        assert_eq!(
            id_a, id_b,
            "identical cacheable nodes must alias to the same id"
        );
        assert_eq!(
            graph.nodes.len(),
            1,
            "deduplication must not create a second node"
        );
    }

    /// Non-cacheable nodes (e.g. `Return`) must always produce fresh ids even
    /// when all arguments are identical.
    #[test]
    fn non_cacheable_node_is_never_deduplicated() {
        let mut graph = Graph::new();
        let id_a = graph.create_node(NodeKind::Return, [], []);
        let id_b = graph.create_node(NodeKind::Return, [], []);
        assert_ne!(
            id_a, id_b,
            "non-cacheable nodes must always produce distinct ids"
        );
    }

    /// Two adjacent `Call` nodes with identical target and argument outputs
    /// must stay distinct — Call is non-cacheable because `CallStackArgCollect`
    /// mutates its inputs after construction.
    #[test]
    fn adjacent_calls_with_same_args_are_distinct() {
        let mut graph = Graph::new();
        let ctrl_a = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem_a = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let [ctrl_out] = graph.node_outputs_exact::<1>(ctrl_a).unwrap();
        let [mem_out] = graph.node_outputs_exact::<1>(mem_a).unwrap();
        let target = graph.create_node(
            NodeKind::IntConst(0x1000),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [target_out] = graph.node_outputs_exact::<1>(target).unwrap();
        let outs = [NodeOutputKind::Control, NodeOutputKind::Memory];
        let call_a = graph.create_node(NodeKind::Call, [ctrl_out, mem_out, target_out], outs);
        let call_b = graph.create_node(NodeKind::Call, [ctrl_out, mem_out, target_out], outs);
        assert_ne!(
            call_a, call_b,
            "Call is non-cacheable so identical-argument calls must be distinct"
        );
    }

    /// `StackStorePhi` is non-cacheable; its offsets live in a side-map and
    /// two distinct phis with the same space and inputs must remain distinct.
    #[test]
    fn stack_store_phi_is_never_deduplicated() {
        let mut graph = Graph::new();
        let space = rsleigh::VnSpace::RAM;
        let id_a = graph.create_node(
            NodeKind::StackStorePhi { space },
            [],
            [NodeOutputKind::Memory],
        );
        let id_b = graph.create_node(
            NodeKind::StackStorePhi { space },
            [],
            [NodeOutputKind::Memory],
        );
        assert_ne!(id_a, id_b);
        graph.set_stack_phi_offsets(id_a, vec![0, -4]);
        assert_eq!(graph.stack_phi_offsets(id_a), &[0, -4]);
        assert_eq!(graph.stack_phi_offsets(id_b), &[] as &[i64]);
    }

    /// After adding an input to a non-cacheable node the output's use-list
    /// must contain exactly that input, and `node_inputs` must reflect it.
    #[test]
    fn add_node_input_registers_use() {
        let mut graph = Graph::new();
        // Produce a value
        let const_node = graph.create_node(
            NodeKind::IntConst(1),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [const_out] = graph.node_outputs_exact::<1>(const_node).unwrap();

        // Create a non-cacheable sink
        let ret_node = graph.create_node(NodeKind::Return, [], []);

        graph.add_node_input(ret_node, const_out).unwrap();

        // The input must appear in node_inputs
        check_node_inputs(&graph, ret_node, [const_out]);

        // The output's use-list must contain this input
        let use_count = graph.output_uses(const_out).count();
        assert_eq!(use_count, 1);
    }

    /// `remove_node_input` must shrink the input list, update subsequent
    /// input indices, and unregister the use from the output's use-list.
    #[test]
    fn remove_node_input_cleans_up_use_list() {
        let mut graph = Graph::new();

        let c0 = graph.create_node(
            NodeKind::IntConst(0),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [out0] = graph.node_outputs_exact::<1>(c0).unwrap();

        let c1 = graph.create_node(
            NodeKind::IntConst(1),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [out1] = graph.node_outputs_exact::<1>(c1).unwrap();

        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, out0).unwrap();
        graph.add_node_input(ret, out1).unwrap();

        // Remove the first input (index 0 = out0)
        graph.remove_node_input(ret, 0).unwrap();

        // Only out1 should remain
        check_node_inputs(&graph, ret, [out1]);

        // out0 must no longer be used
        assert_eq!(
            graph.output_uses(out0).count(),
            0,
            "out0 should have no uses after removal"
        );
        // out1 must still be used
        assert_eq!(
            graph.output_uses(out1).count(),
            1,
            "out1 should still have one use"
        );

        // The surviving input must have its index adjusted to 0
        let inputs_slice = graph.nodes[ret].inputs.as_slice(&graph.input_pool);
        assert_eq!(graph.inputs[inputs_slice[0]].input_index, 0);
    }

    /// `update_input` must move the use from the old output to the new one
    /// so that use-lists stay consistent.
    #[test]
    fn update_input_moves_use_to_new_output() {
        let mut graph = Graph::new();

        let old = graph.create_node(
            NodeKind::IntConst(10),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [old_out] = graph.node_outputs_exact::<1>(old).unwrap();

        let new = graph.create_node(
            NodeKind::IntConst(20),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [new_out] = graph.node_outputs_exact::<1>(new).unwrap();

        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, old_out).unwrap();

        // Find the single input id
        let input_id = graph.nodes[ret].inputs.as_slice(&graph.input_pool)[0];

        graph.update_input(input_id, new_out);

        // old_out must have no uses; new_out must have one
        assert_eq!(graph.output_uses(old_out).count(), 0);
        assert_eq!(graph.output_uses(new_out).count(), 1);

        // The node input must now reference new_out
        check_node_inputs(&graph, ret, [new_out]);
    }

    /// `detach_node_inputs` must clear all inputs from the node and remove
    /// them from every output's use-list.
    #[test]
    fn detach_node_inputs_removes_all_uses() {
        let mut graph = Graph::new();

        let c = graph.create_node(
            NodeKind::IntConst(5),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [out] = graph.node_outputs_exact::<1>(c).unwrap();

        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, out).unwrap();
        graph.add_node_input(ret, out).unwrap(); // same output used twice

        assert_eq!(graph.output_uses(out).count(), 2);

        graph.detach_node_inputs(ret);

        assert_eq!(
            graph.output_uses(out).count(),
            0,
            "all uses must be removed after detach"
        );
        assert_eq!(
            graph.node_inputs(ret).len(),
            0,
            "node must have no inputs after detach"
        );
    }

    /// After `detach_node_inputs` on a cacheable node, a subsequent
    /// `create_node` call with the same `(kind, inputs, output_kinds)` must
    /// produce a fresh, fully-connected node — not return the detached
    /// zombie whose input list is empty.
    ///
    /// Regression: before the dedup-cache was cleaned on detach, optimizer
    /// passes that created identical Adds after `RedundantPhis` had detached
    /// the original unreachable Add would alias to the zombie, and any
    /// follow-up pass calling `node_inputs_exact::<2>` would fail with
    /// `WrongInputCount(..., 2, 0)`.
    #[test]
    fn detach_evicts_cacheable_node_from_dedup_cache() {
        use crate::ops::IntBinaryOp;
        let mut graph = Graph::new();
        let lhs = graph.create_node(
            NodeKind::IntConst(7),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let rhs = graph.create_node(
            NodeKind::IntConst(9),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [lhs_out] = graph.node_outputs_exact::<1>(lhs).unwrap();
        let [rhs_out] = graph.node_outputs_exact::<1>(rhs).unwrap();

        let ty = NodeOutputKind::OutputType(NodeOutputType::U32);
        let add_a = graph.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [lhs_out, rhs_out],
            [ty],
        );

        graph.detach_node_inputs(add_a);
        assert_eq!(graph.node_inputs(add_a).len(), 0);

        let add_b = graph.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [lhs_out, rhs_out],
            [ty],
        );

        assert_ne!(
            add_a, add_b,
            "detach must evict the zombie from the dedup cache so a re-created \
             identical node is fresh"
        );
        assert_eq!(
            graph.node_inputs(add_b).len(),
            2,
            "the re-created node must be fully connected"
        );
    }

    /// An output consumed by a single node must be reported by
    /// `output_has_one_usage` as `true`; consuming it a second time must
    /// flip it to `false`.
    #[test]
    fn output_has_one_usage_tracks_consumer_count() {
        let mut graph = Graph::new();

        let c = graph.create_node(
            NodeKind::IntConst(99),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [out] = graph.node_outputs_exact::<1>(c).unwrap();

        assert!(!graph.output_has_one_usage(out), "zero uses is not one");

        let ret1 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret1, out).unwrap();
        assert!(
            graph.output_has_one_usage(out),
            "one use should return true"
        );

        let ret2 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret2, out).unwrap();
        assert!(
            !graph.output_has_one_usage(out),
            "two uses should return false"
        );
    }

    /// `get_node_from_output` must return the node that created the output.
    #[test]
    fn get_node_from_output_returns_source_node() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::IntConst(7),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U8)],
        );
        let [out] = graph.node_outputs_exact::<1>(node).unwrap();
        assert_eq!(graph.get_node_from_output(out), node);
    }

    /// A node with two outputs must expose both with correct kinds and
    /// definitions.
    #[test]
    fn node_with_multiple_outputs() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::If,
            [],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [true_ctrl, false_ctrl] = graph.node_outputs_exact::<2>(node).unwrap();
        assert_eq!(graph.output_kind(true_ctrl), NodeOutputKind::Control);
        assert_eq!(graph.output_kind(false_ctrl), NodeOutputKind::Control);
        assert_eq!(graph.output_definition(true_ctrl), (node, 0));
        assert_eq!(graph.output_definition(false_ctrl), (node, 1));
    }

    /// `output_uses` must yield one `(node_id, input_index)` tuple per
    /// consumer, with the correct node id and position within that node's
    /// input list.  Three independent consumers all at input-index 0 must
    /// all appear exactly once.
    #[test]
    fn output_uses_reports_all_consumers_with_correct_indices() {
        let mut graph = Graph::new();
        let src = graph.create_node(
            NodeKind::IntConst(7),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [out] = graph.node_outputs_exact::<1>(src).unwrap();

        let ret0 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret0, out).unwrap();
        let ret1 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret1, out).unwrap();
        let ret2 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret2, out).unwrap();

        let uses: Vec<(NodeId, u32)> = graph.output_uses(out).collect();
        assert_eq!(uses.len(), 3, "all three consumers must appear");

        for expected_node in [ret0, ret1, ret2] {
            assert!(
                uses.iter().any(|(n, _)| *n == expected_node),
                "consumer {expected_node:?} missing from output_uses"
            );
        }
        // Each of the three nodes has exactly one input, so input_index is 0.
        for (_, idx) in &uses {
            assert_eq!(*idx, 0, "each single-input node's input_index must be 0");
        }
    }

    /// When a node has multiple inputs from the same output, `output_uses`
    /// must report all of them with their correct positional indices.
    #[test]
    fn output_uses_same_output_multiple_times_reports_each_position() {
        let mut graph = Graph::new();
        let src = graph.create_node(
            NodeKind::IntConst(3),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [out] = graph.node_outputs_exact::<1>(src).unwrap();

        // Same output at positions 0 and 1 of the same sink node.
        let sink = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink, out).unwrap(); // input_index 0
        graph.add_node_input(sink, out).unwrap(); // input_index 1

        let uses: Vec<(NodeId, u32)> = graph.output_uses(out).collect();
        assert_eq!(uses.len(), 2);

        let mut indices: Vec<u32> = uses.iter().map(|(_, i)| *i).collect();
        indices.sort_unstable();
        assert_eq!(indices, vec![0, 1], "both positional indices must appear");
    }

    /// `output_use_cursor` iterates the same set as `output_uses`.
    /// `replace_current_with` must redirect the first use to a new output
    /// and advance past it so the remaining use is untouched.
    #[test]
    fn output_use_cursor_replace_redirects_first_use() {
        let mut graph = Graph::new();

        let old_src = graph.create_node(
            NodeKind::IntConst(1),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [old_out] = graph.node_outputs_exact::<1>(old_src).unwrap();

        let new_src = graph.create_node(
            NodeKind::IntConst(2),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [new_out] = graph.node_outputs_exact::<1>(new_src).unwrap();

        // Two consumers of old_out.
        let ret0 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret0, old_out).unwrap();
        let ret1 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret1, old_out).unwrap();

        assert_eq!(graph.output_uses(old_out).count(), 2);
        assert_eq!(graph.output_uses(new_out).count(), 0);

        // Redirect the first consumer to new_out.
        {
            let mut cursor = graph.output_use_cursor(old_out);
            cursor.replace_current_with(new_out).unwrap();
        }

        // After one replacement: old_out has one use, new_out has one use.
        assert_eq!(
            graph.output_uses(old_out).count(),
            1,
            "one use must remain on old_out"
        );
        assert_eq!(
            graph.output_uses(new_out).count(),
            1,
            "one use must move to new_out"
        );
    }

    /// `output_use_cursor` with `replace_current_with` applied to every
    /// element must leave the original output with no uses and transfer all
    /// uses to the replacement.
    #[test]
    fn output_use_cursor_replace_all_drains_source() {
        let mut graph = Graph::new();

        let old_src = graph.create_node(
            NodeKind::IntConst(10),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [old_out] = graph.node_outputs_exact::<1>(old_src).unwrap();

        let new_src = graph.create_node(
            NodeKind::IntConst(20),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [new_out] = graph.node_outputs_exact::<1>(new_src).unwrap();

        // Three consumers.
        for _ in 0..3 {
            let r = graph.create_node(NodeKind::Return, [], []);
            graph.add_node_input(r, old_out).unwrap();
        }
        assert_eq!(graph.output_uses(old_out).count(), 3);

        // Replace all uses in a single cursor pass.
        let mut cursor = graph.output_use_cursor(old_out);
        while cursor.current().is_some() {
            cursor.replace_current_with(new_out).unwrap();
        }

        assert_eq!(
            graph.output_uses(old_out).count(),
            0,
            "all uses must be drained from old_out"
        );
        assert_eq!(
            graph.output_uses(new_out).count(),
            3,
            "all uses must land on new_out"
        );
    }

    /// Removing the middle input of a three-input node must: leave the
    /// two survivors in order, re-number their indices contiguously from 0,
    /// and remove the deleted input from its output's use-list.
    #[test]
    fn remove_node_input_from_middle_reindexes_remaining() {
        let mut graph = Graph::new();

        let out0 = {
            let n = graph.create_node(
                NodeKind::IntConst(10),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U64)],
            );
            graph.node_outputs_exact::<1>(n).unwrap()[0]
        };
        let out1 = {
            let n = graph.create_node(
                NodeKind::IntConst(20),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U64)],
            );
            graph.node_outputs_exact::<1>(n).unwrap()[0]
        };
        let out2 = {
            let n = graph.create_node(
                NodeKind::IntConst(30),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U64)],
            );
            graph.node_outputs_exact::<1>(n).unwrap()[0]
        };

        let sink = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink, out0).unwrap(); // index 0
        graph.add_node_input(sink, out1).unwrap(); // index 1
        graph.add_node_input(sink, out2).unwrap(); // index 2

        graph.remove_node_input(sink, 1).unwrap(); // remove middle

        check_node_inputs(&graph, sink, [out0, out2]);
        assert_eq!(graph.output_uses(out1).count(), 0, "out1 must be removed");
        assert_eq!(graph.output_uses(out0).count(), 1);
        assert_eq!(graph.output_uses(out2).count(), 1);

        let inputs_slice = graph.nodes[sink].inputs.as_slice(&graph.input_pool);
        assert_eq!(
            graph.inputs[inputs_slice[0]].input_index, 0,
            "surviving input 0 must have index 0"
        );
        assert_eq!(
            graph.inputs[inputs_slice[1]].input_index, 1,
            "surviving input 1 must have index 1"
        );
    }

    /// Removing the last input must not disturb the preceding inputs.
    #[test]
    fn remove_node_input_from_end_leaves_others_intact() {
        let mut graph = Graph::new();

        let out0 = {
            let n = graph.create_node(
                NodeKind::IntConst(1),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U64)],
            );
            graph.node_outputs_exact::<1>(n).unwrap()[0]
        };
        let out1 = {
            let n = graph.create_node(
                NodeKind::IntConst(2),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U64)],
            );
            graph.node_outputs_exact::<1>(n).unwrap()[0]
        };

        let sink = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink, out0).unwrap();
        graph.add_node_input(sink, out1).unwrap();

        graph.remove_node_input(sink, 1).unwrap(); // remove last

        check_node_inputs(&graph, sink, [out0]);
        assert_eq!(graph.output_uses(out1).count(), 0);
        assert_eq!(graph.output_uses(out0).count(), 1);

        let inputs_slice = graph.nodes[sink].inputs.as_slice(&graph.input_pool);
        assert_eq!(graph.inputs[inputs_slice[0]].input_index, 0);
    }

    /// `update_input` on an input belonging to a cacheable node must evict the
    /// stale dedup-cache entry. Otherwise a later `create_node` with the
    /// original `(kind, inputs, outputs)` triple returns the now-modified
    /// node, which has different inputs — silent miscompilation by the
    /// optimizer (which calls `update_input` via `replace_all_uses`).
    #[test]
    fn update_input_on_cacheable_evicts_stale_cache_entry() {
        use crate::ops::IntBinaryOp;
        let mut graph = Graph::new();

        let a = graph.create_node(
            NodeKind::IntConst(1),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let b = graph.create_node(
            NodeKind::IntConst(2),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let c = graph.create_node(
            NodeKind::IntConst(3),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [a_out] = graph.node_outputs_exact::<1>(a).unwrap();
        let [b_out] = graph.node_outputs_exact::<1>(b).unwrap();
        let [c_out] = graph.node_outputs_exact::<1>(c).unwrap();
        let ty = NodeOutputKind::OutputType(NodeOutputType::U32);

        // Cache key inserted: (Add, [a, b], [ty]) → add_ab.
        let add_ab = graph.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [a_out, b_out],
            [ty],
        );

        // Redirect input[0] from a → c. Node now actually has inputs [c, b],
        // but the cache (if not maintained) still maps [a, b] → add_ab.
        let in0 = graph.node_input_id_at(add_ab, 0);
        graph.update_input(in0, c_out);

        // Re-create with the ORIGINAL key. Must NOT return add_ab — its
        // current inputs are [c, b], not [a, b].
        let fresh = graph.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [a_out, b_out],
            [ty],
        );
        assert_ne!(
            add_ab, fresh,
            "the stale cache entry must be evicted — re-creating the original \
             (kind, inputs, outputs) triple after update_input has redirected \
             one of those inputs must produce a fresh NodeId"
        );
    }

    /// `update_input` where the new output equals the old output must leave
    /// the use count unchanged and keep the node input pointing at the same
    /// output.
    #[test]
    fn update_input_to_same_output_is_idempotent() {
        let mut graph = Graph::new();

        let src = graph.create_node(
            NodeKind::IntConst(99),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [out] = graph.node_outputs_exact::<1>(src).unwrap();

        let sink = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink, out).unwrap();

        let input_id = graph.nodes[sink].inputs.as_slice(&graph.input_pool)[0];
        graph.update_input(input_id, out);

        assert_eq!(
            graph.output_uses(out).count(),
            1,
            "self-update must not change use count"
        );
        check_node_inputs(&graph, sink, [out]);
    }

    /// After `detach_node_inputs`, re-adding the same inputs must restore
    /// the use-list count to its original value.
    #[test]
    fn detach_then_readd_restores_use_count() {
        let mut graph = Graph::new();

        let src = graph.create_node(
            NodeKind::IntConst(42),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [out] = graph.node_outputs_exact::<1>(src).unwrap();

        let sink = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink, out).unwrap();
        graph.add_node_input(sink, out).unwrap();
        assert_eq!(graph.output_uses(out).count(), 2);

        graph.detach_node_inputs(sink);
        assert_eq!(
            graph.output_uses(out).count(),
            0,
            "uses cleared after detach"
        );
        assert_eq!(graph.node_inputs(sink).len(), 0);

        // Re-add; use count must be restored.
        graph.add_node_input(sink, out).unwrap();
        graph.add_node_input(sink, out).unwrap();
        assert_eq!(
            graph.output_uses(out).count(),
            2,
            "re-adding inputs must restore use count"
        );
        assert_eq!(graph.node_inputs(sink).len(), 2);
    }

    /// Two independent sinks each consuming the same output must all appear
    /// in the use-list.  This verifies the linked-list stays consistent when
    /// multiple distinct nodes reference the same output.
    #[test]
    fn two_independent_consumers_both_in_use_list() {
        let mut graph = Graph::new();

        let src = graph.create_node(
            NodeKind::IntConst(1),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [out] = graph.node_outputs_exact::<1>(src).unwrap();

        let b = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(b, out).unwrap();
        let c = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(c, out).unwrap();

        let uses: Vec<_> = graph.output_uses(out).collect();
        assert_eq!(uses.len(), 2);
        let nodes: Vec<_> = uses.iter().map(|(n, _)| *n).collect();
        assert!(nodes.contains(&b), "b must appear in use-list");
        assert!(nodes.contains(&c), "c must appear in use-list");
    }

    /// `node_outputs_exact` must panic when asked for a count that does not
    /// match the actual number of outputs.
    #[test]
    #[should_panic]
    fn node_outputs_exact_panics_on_wrong_count() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::IntConst(0),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U8)],
        );
        // Node has 1 output; requesting 2 must panic.
        graph.node_outputs_exact::<2>(node).unwrap();
    }

    /// `node_inputs_exact` must panic when asked for a count that does not
    /// match the actual number of inputs.
    #[test]
    #[should_panic]
    fn node_inputs_exact_panics_on_wrong_count() {
        let mut graph = Graph::new();
        let src = graph.create_node(
            NodeKind::IntConst(0),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [out] = graph.node_outputs_exact::<1>(src).unwrap();

        let sink = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink, out).unwrap(); // exactly 1 input

        // Asking for 2 must panic.
        graph.node_inputs_exact::<2>(sink).unwrap();
    }
}
