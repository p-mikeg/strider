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
}

impl FunctionGraph {
    pub fn new_invalid() -> Self {
        Self {
            graph: Graph::new(),
            entry: NodeId::reserved_value(),
            entry_control: NodeOutputId::reserved_value(),
            entry_memory: NodeOutputId::reserved_value(),
        }
    }
}

pub struct BuiltFunctionGraph {
    pub graph: Graph,
    pub entry: NodeId,
    pub variables: PrimaryMap<VarId, rsleigh::Vn>,
    /// Ordered list of varnodes clobbered by every `Call` node.
    /// The i-th clobbered output of any Call (output index `i + 2`) corresponds
    /// to `call_clobbered[i]`.  The list is the same for all calls.
    pub call_clobbered: Box<[rsleigh::Vn]>,
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