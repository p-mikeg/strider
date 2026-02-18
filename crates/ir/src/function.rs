use crate::dot::GraphDotDumper;
use crate::graph::Graph;
use crate::node::{NodeId, NodeOutputId};
use cranelift_entity::packed_option::ReservedValue;
use cranelift_entity::{PrimaryMap};
use crate::builder::VarId;


#[derive(Clone)]
pub struct FunctionGraph {
    pub graph: Graph,
    pub entry: NodeId,
    pub entry_control: NodeOutputId,
    pub entry_memory: NodeOutputId
}

impl FunctionGraph {
    pub fn new_invalid() -> Self {
        Self {
            graph: Graph::new(),
            entry: NodeId::reserved_value(),
            entry_control: NodeOutputId::reserved_value(),
            entry_memory: NodeOutputId::reserved_value()
        }
    }
}

pub struct BuiltFunctionGraph {
    pub graph: Graph,
    pub entry: NodeId,
    pub variables: PrimaryMap<VarId, rsleigh::Vn>,
}

impl BuiltFunctionGraph {
    pub fn preorder(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(&self.graph, self.entry)
    }

    pub fn dot_dumper<'a, R: rsleigh::MemReader>(&'a self, sleigh: &'a rsleigh::Sleigh<R>) -> crate::dot::GraphDotDumper<'a, R> {
        GraphDotDumper { entry: self.entry, graph: &self.graph, sleigh}
    }
}