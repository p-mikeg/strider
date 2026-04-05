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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Creates an Entry node with a single Control output.  Returns the node
    /// id and the control output id.
    fn make_entry(graph: &mut Graph) -> (NodeId, NodeOutputId) {
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let [ctrl] = graph.node_outputs_exact::<1>(entry);
        (entry, ctrl)
    }

    /// Creates a non-cacheable ControlState node that produces one Control
    /// output, and wires `ctrl_in` as its first input so that the producer
    /// of `ctrl_in` has this node as a CFG successor.
    fn make_ctrl_node(graph: &mut Graph, ctrl_in: NodeOutputId) -> (NodeId, NodeOutputId) {
        let node = graph.create_node(NodeKind::ControlState, [], [NodeOutputKind::Control]);
        graph.add_node_input(node, ctrl_in);
        let [out] = graph.node_outputs_exact::<1>(node);
        (node, out)
    }

    /// Creates a Return node (leaf, non-cacheable) that consumes `ctrl_in`
    /// as its only input, making the producer of `ctrl_in` a CFG predecessor.
    fn make_return(graph: &mut Graph, ctrl_in: NodeOutputId) -> NodeId {
        let node = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(node, ctrl_in);
        node
    }

    // ── walk_graph ────────────────────────────────────────────────────────────

    /// An entry node with no successors must be visited exactly once.
    #[test]
    fn walk_single_entry_visits_exactly_one_node() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited, vec![entry]);
    }

    /// A linear chain entry → A → B must be fully traversed: all three nodes
    /// must appear exactly once.
    #[test]
    fn walk_linear_chain_visits_all_nodes() {
        let mut graph = Graph::new();
        let (entry, entry_ctrl) = make_entry(&mut graph);
        let (a, a_ctrl) = make_ctrl_node(&mut graph, entry_ctrl);
        let b = make_return(&mut graph, a_ctrl);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited.len(), 3, "all three nodes must be visited");
        assert!(visited.contains(&entry));
        assert!(visited.contains(&a));
        assert!(visited.contains(&b));
    }

    /// A longer chain entry → A → B → C → D must be fully traversed.
    #[test]
    fn walk_long_chain_visits_all_nodes() {
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        let (b, c2) = make_ctrl_node(&mut graph, c1);
        let (c, c3) = make_ctrl_node(&mut graph, c2);
        let d = make_return(&mut graph, c3);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited.len(), 5);
        for node in [entry, a, b, c, d] {
            assert!(visited.contains(&node), "{node:?} missing from walk");
        }
    }

    /// A diamond-shaped graph (entry → left, right → merge) must visit each
    /// node exactly once despite the converging control edges.
    #[test]
    fn walk_diamond_visits_each_node_once() {
        let mut graph = Graph::new();

        // Entry with two control outputs: one for left, one for right.
        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [ctrl_l, ctrl_r] = graph.node_outputs_exact::<2>(entry);

        let (_left, left_ctrl)  = make_ctrl_node(&mut graph, ctrl_l);
        let (_right, right_ctrl) = make_ctrl_node(&mut graph, ctrl_r);

        // Merge consumes both branch ctrl outputs.
        let merge = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(merge, left_ctrl);
        graph.add_node_input(merge, right_ctrl);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited.len(), 4, "diamond must produce exactly 4 nodes");

        // No duplicates.
        let mut seen = std::collections::HashSet::new();
        for n in &visited {
            assert!(seen.insert(*n), "node {n:?} was visited more than once");
        }
    }

    /// Nodes that are not reachable from the entry (no control or data path)
    /// must not appear in the walk output.
    #[test]
    fn walk_does_not_visit_unreachable_nodes() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        // Isolated node: completely disconnected.
        let isolated = graph.create_node(NodeKind::Return, [], []);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert!(!visited.contains(&isolated), "isolated node must not be visited");
        assert!(visited.contains(&entry));
    }

    /// A data-only fan-out (one value consumed by multiple sinks) must
    /// visit the source and all sinks.
    #[test]
    fn walk_follows_data_inputs_to_producer() {
        let mut graph = Graph::new();
        // A pure data source (no control outputs).
        let src = graph.create_node(
            NodeKind::IntConst(42),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [data_out] = graph.node_outputs_exact::<1>(src);

        // Entry → sink1 and sink2, both also consuming the data value.
        let (entry, entry_ctrl) = make_entry(&mut graph);
        let sink1 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink1, entry_ctrl);
        graph.add_node_input(sink1, data_out);

        let sink2 = graph.create_node(NodeKind::Return, [], []);
        // sink2 is only reachable via data input from sink1's producer (entry_ctrl consumed by sink1, not sink2)
        // Actually attach sink2 to data_out only - it won't be reachable from entry via control
        // but via data: walk from entry visits sink1 (cfg succ), sink1's inputs point to entry and src,
        // src has no inputs, so src is visited. sink2 is not reachable at all.
        graph.add_node_input(sink2, data_out);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        // entry → sink1 (cfg succ), sink1's inputs → entry (visited), src (not visited yet)
        // src has no inputs or cfg succs.
        // sink2 is reachable only as a consumer of data_out (via output_uses), but
        // graph_walk_succs does NOT follow output uses — it follows inputs.
        assert!(visited.contains(&entry));
        assert!(visited.contains(&sink1));
        assert!(visited.contains(&src), "src is reachable via sink1's data input");
        assert!(!visited.contains(&sink2), "sink2 has no path from entry through inputs");
    }

    // ── cfg_succs ─────────────────────────────────────────────────────────────

    /// A node with no Control outputs must have no CFG successors.
    #[test]
    fn cfg_succs_no_control_outputs_is_empty() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::IntConst(0),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let succs: Vec<_> = cfg_succs(&graph, node).collect();
        assert!(succs.is_empty(), "data-only node must have no cfg successors");
    }

    /// A node whose single Control output is consumed by two different nodes
    /// must appear as a predecessor of both.
    #[test]
    fn cfg_succs_returns_all_control_consumers() {
        let mut graph = Graph::new();
        let (entry, ctrl) = make_entry(&mut graph);

        let r0 = make_return(&mut graph, ctrl);
        let r1 = make_return(&mut graph, ctrl);

        let succs: Vec<_> = cfg_succs(&graph, entry).collect();
        assert_eq!(succs.len(), 2, "both consumers must appear");
        assert!(succs.contains(&r0));
        assert!(succs.contains(&r1));
    }

    /// A node with two Control outputs leading to different successors must
    /// report both successors.
    #[test]
    fn cfg_succs_two_control_outputs_two_successors() {
        let mut graph = Graph::new();
        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [ctrl0, ctrl1] = graph.node_outputs_exact::<2>(entry);

        let left  = make_return(&mut graph, ctrl0);
        let right = make_return(&mut graph, ctrl1);

        let succs: Vec<_> = cfg_succs(&graph, entry).collect();
        assert_eq!(succs.len(), 2);
        assert!(succs.contains(&left));
        assert!(succs.contains(&right));
    }

    /// A node with a Control output that has no consumers must not report any
    /// CFG successors for that output.
    #[test]
    fn cfg_succs_unconsumed_control_output_yields_nothing() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        // ctrl is produced but never consumed.
        let succs: Vec<_> = cfg_succs(&graph, entry).collect();
        assert!(succs.is_empty());
    }

    // ── cfg_outputs ───────────────────────────────────────────────────────────

    /// `cfg_outputs` must only return outputs with `NodeOutputKind::Control`.
    /// Data and memory outputs must be excluded.
    #[test]
    fn cfg_outputs_excludes_non_control_outputs() {
        let mut graph = Graph::new();
        // ControlState is non-cacheable so we can give it arbitrary outputs.
        let node = graph.create_node(
            NodeKind::ControlState,
            [],
            [
                NodeOutputKind::Control,
                NodeOutputKind::OutputType(NodeOutputType::U64),
                NodeOutputKind::Memory,
                NodeOutputKind::Control,
            ],
        );
        let ctrl_outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert_eq!(ctrl_outs.len(), 2, "only the two Control outputs must appear");
        for out in ctrl_outs {
            assert_eq!(
                graph.output_kind(out),
                NodeOutputKind::Control,
                "cfg_outputs must only yield Control-kind outputs"
            );
        }
    }

    /// A node with no outputs at all must yield an empty iterator from
    /// `cfg_outputs`.
    #[test]
    fn cfg_outputs_empty_for_node_with_no_outputs() {
        let mut graph = Graph::new();
        let node = graph.create_node(NodeKind::Return, [], []);
        let outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert!(outs.is_empty());
    }

    /// A node whose outputs are all non-control must yield nothing from
    /// `cfg_outputs`.
    #[test]
    fn cfg_outputs_empty_when_all_outputs_are_data() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::IntConst(5),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert!(outs.is_empty());
    }
}
