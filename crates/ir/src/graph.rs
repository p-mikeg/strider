use std::{collections::HashMap};
use core::array;
use cranelift_entity::{
    PrimaryMap, ListPool
};

use smallvec::SmallVec;

use crate::iterators::InputCursor;

use super::node::*;
use super::iterators::{Inputs, OutputUsageIter, Outputs};

#[derive(Clone)]
pub struct Graph {
    // Structure to have a small unique identifier for each node
    pub(crate) nodes: PrimaryMap<NodeId, Node>,
    // Structure to have a small unique identifier for each output
    pub(crate) outputs: PrimaryMap<NodeOutputId, NodeOutput>,
    // Structure to have a small unique identifier for each input
    pub(crate) inputs: PrimaryMap<NodeInputId, NodeInput>,
    // List of all unique output identifiers
    pub(crate) output_pool: ListPool<NodeOutputId>,
    // List of all unique input identifiers
    pub(crate) input_pool: ListPool<NodeInputId>,
    // A map of Node to its id - only used when adding new nodes to know their id
    pub(crate) node_to_id: HashMap<(Node, Vec<NodeOutputId>, Vec<NodeOutputKind>), NodeId>
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            nodes: PrimaryMap::new(),
            outputs: PrimaryMap::new(),
            inputs: PrimaryMap::new(),
            output_pool: ListPool::new(),
            input_pool: ListPool::new(),
            node_to_id: HashMap::new()
        }
    }

    #[inline]
    pub fn node_kind(&self, node_id: NodeId) -> &NodeKind {
        &self.nodes[node_id].kind
    }

    #[inline]
    pub fn node_kind_mut(&mut self, node_id: NodeId) -> &mut NodeKind {
        &mut self.nodes[node_id].kind
    }

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
        let node_entry = (
            node,
            inputs.to_vec(),
            output_kinds.to_vec(),
        );
        if let Some(node_id) = self.node_to_id.get(&node_entry) {
            return *node_id;
        }
        // Create a new node id
        let node_id =  self.nodes.push(node);
        // Store the new node id if the node is allowed to be cached
        if kind.is_cacheable() {
            self.node_to_id.insert(node_entry, node_id);
        }

        // Add all inputs to the graph
        let inputs: SmallVec<[NodeInputId; 2]> = inputs.into_iter().enumerate().map(|(index, output)| {
                self.inputs.push(NodeInput::new(output, node_id, index as u32))
            }).collect();

        // Make sure that the inputs store their usage of the output
        for &input_use in &inputs {
            self.link_input_to_output_list(input_use);
        }

        // Create outputs for the given node
        let outputs = output_kinds.into_iter().enumerate()
            .map(|(index, kind)| {
            self.outputs.push(NodeOutput::new(kind, node_id, index as u32))
        });

        // Update the node state
        self.nodes[node_id].inputs = NodeInputIdList::from_iter(inputs, &mut self.input_pool); 
        self.nodes[node_id].outputs = NodeOutputIdList::from_iter(outputs, &mut self.output_pool); 
        node_id
    }

    // This function adds a new input to store its output usage for tracking
    fn link_input_to_output_list(&mut self, input_id: NodeInputId) {
        // Get the new input to be the use output_id
        let input = &mut self.inputs[input_id];

        // Check that we didn't link it before
        assert!(input.next.is_none());
        assert!(input.prev.is_none());

    
        let output_id = input.output_id;
        let next_output_use = self.outputs[output_id].first_use;

        // Put it at the start of the linked list
        input.next = next_output_use;
        if let Some(next_use) = next_output_use.expand() {
            self.inputs[next_use].prev = next_output_use.into();
        }

        // Update the linked list of output_id yses
        self.outputs[output_id].first_use = Some(input_id).into();
    }

    // This function adds a new input to store its output usage for tracking
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


     pub fn add_node_input(&mut self, node_id: NodeId, output_id: NodeOutputId) {
        assert!(!self.node_kind(node_id).is_cacheable());

        // Get the last input index to know the index for the new input
        let input_index = self.nodes[node_id].inputs.len(&self.input_pool) as u32;
        // Create the new input
        let input_id = self.inputs.push(NodeInput::new(output_id, node_id, input_index));
        // Add it to the inputs of the node
        self.nodes[node_id].inputs.push(input_id, &mut self.input_pool);
        // Track the input in the linked list
        self.link_input_to_output_list(input_id);
    }

    pub fn remove_node_input(&mut self, node_id: NodeId, index: u32) {
        assert!(!self.node_kind(node_id).is_cacheable());
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
    }


    pub fn output_kind(&self, output_id: NodeOutputId) -> NodeOutputKind {
        self.outputs[output_id].kind
    }
    #[inline]
    pub fn output_definition(&self, output_id: NodeOutputId) -> (NodeId, u32) {
        let data = &self.outputs[output_id];
        (data.source_id, data.output_index)
    }

    pub fn update_input(&mut self, input_id: NodeInputId, output_id: NodeOutputId) {
        // Remove the input usage on the current output id
        self.unlink_input_from_output_list(input_id);
        self.inputs[input_id].output_id = output_id;
        // Add usage of the new output_id
        self.link_input_to_output_list(input_id);
    }

    #[inline]
    pub fn output_use_cursor(&mut self, output_id: NodeOutputId) -> InputCursor<'_> {
        let first_use = self.outputs[output_id].first_use.expand();
        InputCursor {
            graph: self,
            current: first_use,
        }
    }

    pub fn detach_node_inputs(&mut self, node_id: NodeId) {
        // Get all input ids of the node
        let input_ids: SmallVec<[NodeInputId; 4]> =
            self.nodes[node_id].inputs.as_slice(&self.input_pool).into();
        // Remove their dependency on the output
        for &input_id in &input_ids {
            self.unlink_input_from_output_list(input_id);
        }
        // Delete the inputs from the node
        self.nodes[node_id].inputs.clear(&mut self.input_pool);
    }

    #[inline]
    pub fn node_outputs(&self, node_id: NodeId) -> Outputs<'_> {
        Outputs(self.nodes[node_id].outputs.as_slice(&self.output_pool))
    }

    #[inline]
    pub fn node_outputs_exact<const N: usize>(&self, node_id: NodeId) -> [NodeOutputId; N] {
        let outputs = self.node_outputs(node_id);
        assert!(outputs.len() == N);
        array::from_fn(|i| outputs[i])
    }

    #[inline]
    pub fn node_inputs(&self, node_id: NodeId) -> Inputs<'_> {
        Inputs {
            graph: self,
            use_list: self.nodes[node_id].inputs.as_slice(&self.input_pool),
        }
    }

    #[inline]
    pub fn node_inputs_exact<const N: usize>(&self, node_id: NodeId) -> [NodeOutputId; N] {
        let inputs = self.node_inputs(node_id);
        assert!(inputs.len() == N);
        array::from_fn(|i| inputs[i])
    }

    #[inline]
    pub fn get_node_from_output(&self, output_id: NodeOutputId) -> NodeId {
        self.outputs[output_id].source_id
    }

    #[inline]
    pub fn output_uses(&self, output_id: NodeOutputId) -> OutputUsageIter<'_> {
        let first_use = self.outputs[output_id].first_use.expand();
        OutputUsageIter {
            graph: self,
            cur_use: first_use,
        }
    }

    #[inline]
    pub fn output_has_one_usage(&self, value: NodeOutputId) -> bool {
        let mut uses = self.output_uses(value);
        uses.next().is_some() && uses.next().is_none()
    }

}

#[cfg(test)]
mod tests {
    use super::*;
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

    // #[track_caller]
    // fn check_node_outputs(
    //     graph: &Graph,
    //     node_id: NodeId,
    //     expected: impl IntoIterator<Item = NodeOutputId>,
    // ) {
    //     let expected: Vec<_> = expected.into_iter().collect();
    //     let actual: Vec<_> = graph.node_outputs(node_id).into_iter().collect();
    //     assert_eq!(actual, expected);
    // }

    #[track_caller]
    fn check_node_output_kinds(
        graph: &Graph,
        node_id: NodeId,
        expected: impl IntoIterator<Item = NodeOutputKind>,
    ) {
        let expected: Vec<_> = expected.into_iter().collect();
        let actual: Vec<_> = graph.node_outputs(node_id).into_iter()
                    .map(|output_id| graph.output_kind(output_id)).collect();
        assert_eq!(actual, expected);
    }

    #[track_caller]
    fn check_node_output_defintions(
        graph: &Graph,
        node_id: NodeId,
        expected: impl IntoIterator<Item = (NodeId, u32)>,
    ) {
        let expected: Vec<_> = expected.into_iter().collect();
        let actual: Vec<_> = graph.node_outputs(node_id).into_iter()
                    .map(|output_id| graph.output_definition(output_id)).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn create_single_node() {
        let mut graph = Graph::new();
        let node_id = graph.create_node(NodeKind::IntConst(5), [], 
            [NodeOutputKind::OutputType(NodeOutputType::U64)]);
        assert_eq!(graph.node_kind(node_id), &NodeKind::IntConst(5));
        assert_eq!(graph.nodes.len(), 1);
        check_node_inputs(&graph, node_id, []);
        check_node_output_kinds(&graph, node_id, vec![NodeOutputKind::OutputType(NodeOutputType::U64)]);
        check_node_output_defintions(&graph, node_id, vec![(node_id, 0)]);
    }



    // TODO: copy tests and add tests for each function
}