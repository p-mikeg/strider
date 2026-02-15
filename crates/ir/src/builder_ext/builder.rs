use cranelift_entity::packed_option::ReservedValue;

use crate::node::{NodeId, NodeOutputId, NodeOutputKind, NodeOutputType, NodeKind};
use crate::graph::Graph;


// This is the object that is returned to the user from the builder
// It lets the user have the graph and its entrypoint which is used to traverse the graph
#[derive(Clone)]
pub struct FunctionBody {
    pub graph: Graph,
    pub entry: NodeId,
    pub entry_control: NodeOutputId,
    pub entry_memory: NodeOutputId
}

impl FunctionBody {
    pub fn new_invalid() -> Self {
        Self {
            graph: Graph::new(),
            entry: NodeId::reserved_value(),
            entry_control: NodeOutputId::reserved_value(),
            entry_memory: NodeOutputId::reserved_value()
        }
    }

    pub fn preorder(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(&self.graph, self.entry)
    }
}


// The builder just needs to know how to add nodes to the graph and there will extension traits for operations
pub trait Builder {
    fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId;
    fn body(&self) -> &FunctionBody;
    fn body_mut(&mut self) -> &mut FunctionBody;
}


pub struct GraphBuilder<'a>(pub &'a mut FunctionBody);

impl Builder for GraphBuilder<'_> {
    fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        self.0.graph.create_node(kind, inputs, output_kinds)
    }

    fn body(&self) -> &FunctionBody {
        self.0
    }

    fn body_mut(&mut self) -> &mut FunctionBody {
        self.0
    }
}

pub trait BuilderExt: Builder {
    fn graph(&self) -> &Graph {
        &self.body().graph
    }

    fn _build_single_output_pure(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_type: NodeOutputType,
    ) -> NodeOutputId {
        let node = self.create_node(kind, inputs, [NodeOutputKind::OutputType(output_type)]);
        self.graph().node_outputs(node)[0]
    }


    fn get_output_type(&self, output_id: NodeOutputId) -> NodeOutputType {
        self
            .graph()
            .output_kind(output_id)
            .as_value()
            .expect("input should be a value")
    }

}

impl<F: Builder + ?Sized> BuilderExt for F {}