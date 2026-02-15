use core::{iter, ops::ControlFlow};

use entity_utils::set::DenseEntitySet;

use crate::{
    node::{NodeId, NodeOutputId},
    graph::{Graph},
};

pub type PreOrder<G> = graphwalk::PreOrder<G, DenseEntitySet<NodeId>>;
#[derive(Clone, Copy)]
pub struct GraphWalkSuccs<'a>(&'a Graph);

impl<'a> GraphWalkSuccs<'a> {
    #[inline]
    pub fn new(graph: &'a Graph) -> Self {
        Self(graph)
    }
}

pub fn graph_walk_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    // Visit all inputs so we don't cause cases where uses are traversed without their corresponding
    // defs. Users that want to treat regions with no control inputs as dead should do so
    // themselves.
    graph
        .node_inputs(node)
        .into_iter()
        .map(move |input| graph.output_definition(input).0)
        .chain(
            // Walk forward only along control outputs.
            cfg_succs(graph, node),
        )
}


pub fn cfg_outputs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeOutputId> + '_ {
    graph
        .node_outputs(node)
        .into_iter()
        .filter(|&output| graph.output_kind(output).is_control())
}

pub fn cfg_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    cfg_outputs(graph, node)
        .flat_map(|output| graph.output_uses(output))
        .map(|(succ_node, _succ_input_idx)| succ_node)
}

impl graphwalk::GraphRef for GraphWalkSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        graph_walk_succs(self.0, node).try_for_each(f)
    }
}

pub type GraphWalk<'a> = PreOrder<GraphWalkSuccs<'a>>;

/// Walks all nodes reachable in `graph` from `entry` in an unspecified order.
///
/// Note that "reachable" nodes here include dead CFG inputs.
///
/// `entry` is guaranteed to be the last node returned if it has no inputs (as should be the case
/// with every well-formed graph).
pub fn walk_graph(graph: &Graph, entry: NodeId) -> GraphWalk<'_> {
    PreOrder::new(GraphWalkSuccs::new(graph), iter::once(entry))
}
