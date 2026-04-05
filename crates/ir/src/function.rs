use std::collections::HashMap;
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
    pub entry_memory: NodeOutputId,
    /// Maps each `Call` node to the ordered list of clobbered varnodes whose
    /// values appear as the Call's outputs at indices 2, 3, 4, … (after the
    /// Control and Memory outputs).
    pub call_clobbered: HashMap<NodeId, Box<[rsleigh::Vn]>>,
}

impl FunctionGraph {
    pub fn new_invalid() -> Self {
        Self {
            graph: Graph::new(),
            entry: NodeId::reserved_value(),
            entry_control: NodeOutputId::reserved_value(),
            entry_memory: NodeOutputId::reserved_value(),
            call_clobbered: HashMap::new(),
        }
    }
}

pub struct BuiltFunctionGraph {
    pub graph: Graph,
    pub entry: NodeId,
    pub variables: PrimaryMap<VarId, rsleigh::Vn>,
    pub call_clobbered: HashMap<NodeId, Box<[rsleigh::Vn]>>,
}

impl BuiltFunctionGraph {
    pub fn preorder(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(&self.graph, self.entry)
    }

    /// Iterates over **every** node id in the graph, including nodes that are
    /// not reachable from the entry via the control-flow or data-dependency
    /// chains (e.g. `Store` nodes whose memory output is not consumed by any
    /// node visible from `preorder`).
    pub fn all_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.graph.nodes.keys()
    }

    pub fn dot_dumper<'a, R: rsleigh::MemReader>(&'a self, sleigh: &'a rsleigh::Sleigh<R>) -> crate::dot::GraphDotDumper<'a, R> {
        GraphDotDumper {
            entry: self.entry,
            graph: &self.graph,
            sleigh,
            call_clobbered: &self.call_clobbered,
        }
    }
}