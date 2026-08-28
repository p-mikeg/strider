use core::iter;
use core::ops::ControlFlow;

pub use entity_utils::set::DenseEntitySet;

use crate::IRViewer;
use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueId};

mod cast;
pub use cast::{CastMask, cast_mask_of};

pub type NodeIdSet = DenseEntitySet<NodeId>;

/// The CFG skeleton: only `Control` edges are followed, so the result holds
/// control-flow nodes alone.
pub fn cfg_reachable(graph: &Graph, entry: NodeId) -> DenseEntitySet<NodeId> {
    // The walk's visited set IS the answer, so the yielded items are dropped.
    let mut walk = PreOrder::new(CfgSuccs(graph), iter::once(entry));
    walk.by_ref().for_each(drop);
    walk.into_visited()
}

/// Control-reachable from `entry` and unable to reach a terminator: an
/// exit-free control cycle plus everything that only reaches it, which
/// `validate` rejects and the lifter seats an `Unreachable` sink on.
///
/// A dangling control output counts as an exit, so an already-malformed
/// function reports that malformation rather than this one.
pub fn stranded_nodes(graph: &Graph, entry: NodeId) -> NodeIdSet {
    let cfg = cfg_reachable(graph, entry);

    let mut escapes = NodeIdSet::new();
    let mut work: Vec<NodeId> = Vec::new();
    for node in &cfg {
        let dangling =
            cfg_outputs(graph, node).any(|value| graph.value_uses(value).next().is_none());
        if graph.node_kind(node).is_terminator() || dangling {
            escapes.insert(node);
            work.push(node);
        }
    }
    close_over_control_preds(graph, &mut escapes, work, |_| false, Some(&cfg));

    let mut stranded = NodeIdSet::new();
    for node in cfg.iter().filter(|&node| !escapes.contains(node)) {
        stranded.insert(node);
    }
    stranded
}

/// Grows `reached` backward over control inputs from `work`.
///
/// `skip_edge` drops an edge that must not be followed (a branch arm proven
/// dead); `universe`, when given, bounds membership.
pub fn close_over_control_preds(
    graph: &Graph,
    reached: &mut NodeIdSet,
    mut work: Vec<NodeId>,
    skip_edge: impl Fn(ValueId) -> bool,
    universe: Option<&NodeIdSet>,
) {
    while let Some(node) = work.pop() {
        for value in graph.node_inputs(node) {
            if !graph.value_kind(value).is_control() || skip_edge(value) {
                continue;
            }
            let pred = graph.value_definition(value).0;
            if universe.is_none_or(|u| u.contains(pred)) && reached.insert(pred) {
                work.push(pred);
            }
        }
    }
}

pub type PreOrder<G> = graph_algorithms::walk::PreOrder<G>;

pub type PostOrder<G> = graph_algorithms::walk::PostOrder<G>;

/// Successors follow data inputs backward and control edges forward.
#[derive(Clone, Copy)]
pub struct GraphWalkSuccs<'a>(&'a Graph);

impl<'a> GraphWalkSuccs<'a> {
    #[inline]
    pub(crate) fn new(graph: &'a Graph) -> Self {
        Self(graph)
    }
}

/// Two disjoint sets: every data predecessor (walking value / memory /
/// dispatch edges BACKWARD, so each def precedes its uses) and every CFG
/// successor (walking control edges FORWARD).
///
/// Dead CFG inputs still show up while they hang off a live node as data.
pub(crate) fn graph_walk_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .node_inputs(node)
        .into_iter()
        .map(move |value| graph.value_definition(value).0)
        .chain(cfg_succs(graph, node))
}

pub fn cfg_outputs(graph: &Graph, node: NodeId) -> impl Iterator<Item = ValueId> + '_ {
    graph
        .node_outputs(node)
        .iter()
        .copied()
        .filter(|&output| graph.value_kind(output).is_control())
}

pub(crate) fn cfg_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    cfg_outputs(graph, node)
        .flat_map(|output| graph.value_uses(output))
        .map(|(succ_node, _succ_input_idx)| succ_node)
}

/// Forward def-use successors, unrestricted by liveness.
fn def_use_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .node_outputs(node)
        .iter()
        .flat_map(move |output| graph.value_uses(*output))
        .map(|(succ, _use_idx)| succ)
}

impl graph_algorithms::walk::GraphRef for GraphWalkSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        graph_walk_succs(self.0, node).try_for_each(f)
    }
}

/// Forward control edges only.
#[derive(Clone, Copy)]
struct CfgSuccs<'a>(&'a Graph);

impl graph_algorithms::walk::GraphRef for CfgSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        cfg_succs(self.0, node).try_for_each(f)
    }
}

pub type GraphWalk<'a> = PreOrder<GraphWalkSuccs<'a>>;

/// Pre-order from `entry`, so `entry` is yielded FIRST. Order is otherwise
/// unspecified, and "reachable" includes dead CFG inputs.
pub(crate) fn walk_graph(graph: &Graph, entry: NodeId) -> GraphWalk<'_> {
    PreOrder::new(GraphWalkSuccs::new(graph), iter::once(entry))
}

pub type DefUsePostorder<'a> = PostOrder<DefUseSuccs<'a>>;

/// The liveness-unrestricted counterpart of [`DefUseSuccs`]: a post-order
/// from some roots reaches every transitive consumer, dead ones included.
#[derive(Clone, Copy)]
pub struct RawDefUseSuccs<'a>(&'a Graph);

impl<'a> RawDefUseSuccs<'a> {
    #[inline]
    pub fn new(graph: &'a Graph) -> Self {
        Self(graph)
    }
}

impl graph_algorithms::walk::GraphRef for RawDefUseSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        def_use_succs(self.0, node).try_for_each(f)
    }
}

/// Forward def-use edges restricted to a precomputed live set.
#[derive(Clone, Copy)]
pub struct DefUseSuccs<'a> {
    graph: &'a Graph,
    live_nodes: &'a DenseEntitySet<NodeId>,
}

impl<'a> DefUseSuccs<'a> {
    #[inline]
    pub fn new(graph: &'a Graph, live_nodes: &'a DenseEntitySet<NodeId>) -> Self {
        Self { graph, live_nodes }
    }
}

impl graph_algorithms::walk::GraphRef for DefUseSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        def_use_succs(self.graph, node)
            .filter(|&succ| self.live_nodes.contains(succ))
            .try_for_each(f)
    }
}

/// A walk's reachable set plus its input-less `roots`.
#[derive(Debug, Clone)]
pub struct GraphWalkInfo {
    /// Input-less source nodes: `Entry`, constants, `InitialVar`,
    /// `InitialMemory`.
    pub roots: Vec<NodeId>,
    pub live_nodes: DenseEntitySet<NodeId>,
}

impl GraphWalkInfo {
    /// Walks the mixed backward-data plus forward-control relation.
    pub fn compute_full(graph: &Graph, entry: NodeId) -> Self {
        let mut walk = walk_graph(graph, entry);
        let roots: Vec<NodeId> = walk
            .by_ref()
            .filter(|&n| graph.node_inputs(n).is_empty())
            .collect();

        Self {
            roots,
            live_nodes: walk.into_visited(),
        }
    }

    /// Every node is yielded after all of its consumers.
    pub fn postorder<'a>(&'a self, graph: &'a Graph) -> DefUsePostorder<'a> {
        PostOrder::new(
            DefUseSuccs::new(graph, &self.live_nodes),
            self.roots.iter().copied(),
        )
    }

    /// Every producer strictly before its consumers, roots first.
    pub fn reverse_postorder(&self, graph: &Graph) -> Vec<NodeId> {
        let mut rpo: Vec<_> = self.postorder(graph).collect();
        rpo.reverse();
        rpo
    }
}

/// The `InitialMemory` root plus every node that consumes and re-produces a
/// `Memory` token. `Load` is the exception: it consumes but produces none, so
/// it is always a leaf.
fn is_memory_chain_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::InitialMemory
            | NodeKind::Store(_)
            | NodeKind::Load(_)
            | NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::MemPhi
    )
}

/// Consumers of `node`'s `Memory` output that are themselves chain kinds.
fn mem_succs(function: &Function, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    function
        .memory_output_of(node)
        .ok()
        .into_iter()
        .flat_map(move |mem_value| function.value_uses(mem_value))
        .map(|(consumer, _slot)| consumer)
        .filter(|&consumer| is_memory_chain_kind(function.node_kind(consumer)))
}

/// The forward memory-token chain.
#[derive(Clone, Copy)]
struct MemorySuccs<'a>(&'a Function);

impl graph_algorithms::walk::GraphRef for MemorySuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        mem_succs(self.0, node).try_for_each(f)
    }
}

/// The memory-touching nodes reachable forward from `function`'s
/// `InitialMemory` root, in pre-order, the root included.
///
/// Consumers that merely read the final token without touching memory (a
/// `Return`'s memory input, an `IndirectBranch`'s memory slot) are excluded.
/// Empty when no `InitialMemory` is reachable from `entry`.
///
/// The walk follows structural use-lists, so on a NON-compacted graph the
/// result can include a memory op that is not itself reachable from `entry`,
/// e.g. a dead `Store` still consuming the live token.
pub fn memory_reachable(function: &Function, entry: NodeId) -> Vec<NodeId> {
    let graph = function.graph();
    let live = GraphWalkInfo::compute_full(graph, entry).live_nodes;
    let Some(root) = function
        .reachable_kind_iter(&live)
        .find(|(_, k)| matches!(k, NodeKind::InitialMemory))
        .map(|(n, _)| n)
    else {
        return Vec::new();
    };

    // The walk's visited tracker dedups and breaks cycles, such as a
    // loop-header `MemPhi` back-edge.
    PreOrder::new(MemorySuccs(function), iter::once(root)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeKind, ValueKind, ValueType};
    use cranelift_entity::EntityRef;

    fn make_entry(graph: &mut Graph) -> (NodeId, ValueId) {
        let entry = graph.create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        (entry, ctrl)
    }

    /// Wires `ctrl_value` as the Region's first input, making its producer a
    /// CFG predecessor.
    fn make_ctrl_node(graph: &mut Graph, ctrl_value: ValueId) -> (NodeId, ValueId) {
        let node = graph.create_node(NodeKind::Region, [], [ValueKind::Control]);
        graph.add_node_input(node, ctrl_value);
        let [value] = graph.node_outputs_exact::<1>(node).unwrap();
        (node, value)
    }

    fn make_return(graph: &mut Graph, ctrl_value: ValueId) -> NodeId {
        let node = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(node, ctrl_value);
        node
    }

    /// A self-looping Region reaches no terminator, and neither does the
    /// `Entry` feeding it.
    #[test]
    fn stranded_nodes_reports_an_exit_free_cycle_and_its_predecessors() {
        let mut graph = Graph::new();
        let (entry, ctrl) = make_entry(&mut graph);
        let (region, region_ctrl) = make_ctrl_node(&mut graph, ctrl);
        graph.add_node_input(region, region_ctrl);

        let stranded = stranded_nodes(&graph, entry);
        assert!(stranded.contains(region), "the self-loop reaches no exit");
        assert!(stranded.contains(entry), "Entry only reaches the self-loop");
    }

    #[test]
    fn stranded_nodes_is_empty_when_every_node_reaches_a_terminator() {
        let mut graph = Graph::new();
        let (entry, ctrl) = make_entry(&mut graph);
        let (region, region_ctrl) = make_ctrl_node(&mut graph, ctrl);
        graph.add_node_input(region, region_ctrl);
        make_return(&mut graph, region_ctrl);

        assert!(stranded_nodes(&graph, entry).iter().next().is_none());
    }

    /// A dangling control output is its own validation error; counting it as
    /// an exit keeps this out of an already-malformed function.
    #[test]
    fn stranded_nodes_treats_a_dangling_control_output_as_an_exit() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        assert!(stranded_nodes(&graph, entry).iter().next().is_none());
    }

    /// An entry node with no successors must be visited exactly once.
    #[test]
    fn walk_single_entry_visits_exactly_one_node() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited, vec![entry]);
    }

    /// A linear chain must be fully traversed, each node exactly once.
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

    /// Converging control edges must not cause a second visit.
    #[test]
    fn walk_diamond_visits_each_node_once() {
        let mut graph = Graph::new();

        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [ValueKind::Control, ValueKind::Control],
        );
        let [ctrl_l, ctrl_r] = graph.node_outputs_exact::<2>(entry).unwrap();

        let (_left, left_ctrl) = make_ctrl_node(&mut graph, ctrl_l);
        let (_right, right_ctrl) = make_ctrl_node(&mut graph, ctrl_r);

        let merge = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(merge, left_ctrl);
        graph.add_node_input(merge, right_ctrl);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited.len(), 4, "diamond must produce exactly 4 nodes");

        let mut seen = std::collections::HashSet::new();
        for n in &visited {
            assert!(seen.insert(*n), "node {n:?} was visited more than once");
        }
    }

    #[test]
    fn walk_does_not_visit_unreachable_nodes() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        let isolated = graph.create_node(NodeKind::Return, [], []);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert!(
            !visited.contains(&isolated),
            "isolated node must not be visited"
        );
        assert!(visited.contains(&entry));
    }

    #[test]
    fn walk_follows_data_inputs_to_producer() {
        let mut graph = Graph::new();
        let src = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [data_value] = graph.node_outputs_exact::<1>(src).unwrap();

        let (entry, entry_ctrl) = make_entry(&mut graph);
        let sink1 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink1, entry_ctrl);
        graph.add_node_input(sink1, data_value);

        let sink2 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink2, data_value);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        // src is reached backward through sink1's inputs. sink2 only CONSUMES
        // data_value, and the walk follows inputs, not output uses, so it
        // stays unreachable.
        assert!(visited.contains(&entry));
        assert!(visited.contains(&sink1));
        assert!(
            visited.contains(&src),
            "src is reachable via sink1's data input"
        );
        assert!(
            !visited.contains(&sink2),
            "sink2 has no path from entry through inputs"
        );
    }

    /// A node with no Control outputs must have no CFG successors.
    #[test]
    fn cfg_succs_no_control_outputs_is_empty() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let succs: Vec<_> = cfg_succs(&graph, node).collect();
        assert!(
            succs.is_empty(),
            "data-only node must have no cfg successors"
        );
    }

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

    #[test]
    fn cfg_succs_two_control_outputs_two_successors() {
        let mut graph = Graph::new();
        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [ValueKind::Control, ValueKind::Control],
        );
        let [ctrl0, ctrl1] = graph.node_outputs_exact::<2>(entry).unwrap();

        let left = make_return(&mut graph, ctrl0);
        let right = make_return(&mut graph, ctrl1);

        let succs: Vec<_> = cfg_succs(&graph, entry).collect();
        assert_eq!(succs.len(), 2);
        assert!(succs.contains(&left));
        assert!(succs.contains(&right));
    }

    #[test]
    fn cfg_succs_unconsumed_control_output_yields_nothing() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        let succs: Vec<_> = cfg_succs(&graph, entry).collect();
        assert!(succs.is_empty());
    }

    /// Data and memory outputs must be excluded.
    #[test]
    fn cfg_outputs_excludes_non_control_outputs() {
        let mut graph = Graph::new();
        // Region is non-cacheable, so it accepts arbitrary outputs here.
        let node = graph.create_node(
            NodeKind::Region,
            [],
            [
                ValueKind::Control,
                ValueKind::Typed(ValueType::I64),
                ValueKind::Memory,
                ValueKind::Control,
            ],
        );
        let ctrl_outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert_eq!(
            ctrl_outs.len(),
            2,
            "only the two Control outputs must appear"
        );
        for value in ctrl_outs {
            assert_eq!(
                graph.value_kind(value),
                ValueKind::Control,
                "cfg_outputs must only yield Control-kind outputs"
            );
        }
    }

    #[test]
    fn cfg_outputs_empty_for_node_with_no_outputs() {
        let mut graph = Graph::new();
        let node = graph.create_node(NodeKind::Return, [], []);
        let outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert!(outs.is_empty());
    }

    #[test]
    fn cfg_outputs_empty_when_all_outputs_are_data() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert!(outs.is_empty());
    }

    /// Both operands must precede the Add, and the seed comes last.
    #[test]
    fn rpo_emits_operands_before_consumer() {
        let mut graph = Graph::new();
        let a = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [a_value] = graph.node_outputs_exact::<1>(a).unwrap();
        let c = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(4_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [c_value] = graph.node_outputs_exact::<1>(c).unwrap();
        let add = graph.create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [a_value, c_value],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [_add_value] = graph.node_outputs_exact::<1>(add).unwrap();

        let order: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, add).reverse_postorder(&graph);

        assert_eq!(
            order.len(),
            3,
            "rpo must visit each cone node once: {order:?}"
        );
        let pos = |n: NodeId| order.iter().position(|&x| x == n).unwrap();
        assert!(pos(a) < pos(add), "first IntConst must precede Add");
        assert!(pos(c) < pos(add), "second IntConst must precede Add");
        assert_eq!(order[2], add, "seed (Add) is emitted last");
    }

    /// A shared operand is visited once.
    #[test]
    fn rpo_visits_shared_operand_once() {
        let mut graph = Graph::new();
        let c = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [c_value] = graph.node_outputs_exact::<1>(c).unwrap();
        let add = graph.create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [c_value, c_value],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [_add_value] = graph.node_outputs_exact::<1>(add).unwrap();

        let order: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, add).reverse_postorder(&graph);
        assert_eq!(
            order,
            vec![c, add],
            "shared operand visited once, before Add"
        );
    }

    fn int_const(graph: &mut Graph, v: u64) -> (NodeId, ValueId) {
        let n = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new((v) as usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [out] = graph.node_outputs_exact::<1>(n).unwrap();
        (n, out)
    }

    fn int_bin(
        graph: &mut Graph,
        op: crate::IntBinaryOp,
        l: ValueId,
        r: ValueId,
    ) -> (NodeId, ValueId) {
        let n = graph.create_node(
            NodeKind::IntBinaryOp(op),
            [l, r],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [out] = graph.node_outputs_exact::<1>(n).unwrap();
        (n, out)
    }

    #[test]
    fn compute_full_records_roots_and_live_set() {
        let mut graph = Graph::new();
        let (k, kv) = int_const(&mut graph, 9);
        let neg = graph.create_node(
            NodeKind::IntUnaryOp(crate::IntUnaryOp::Neg),
            [kv],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [negv] = graph.node_outputs_exact::<1>(neg).unwrap();
        let (add, _addv) = int_bin(&mut graph, crate::IntBinaryOp::Add, kv, negv);

        let info = GraphWalkInfo::compute_full(&graph, add);
        assert_eq!(
            info.roots,
            vec![k],
            "only the input-less IntConst is a root"
        );
        for n in [k, neg, add] {
            assert!(info.live_nodes.contains(n), "{n:?} must be live");
        }
    }

    /// Two consts feeding two ops that both feed a sink: every operand must
    /// strictly precede each consuming op along EVERY path.
    #[test]
    fn rpo_is_strict_defs_before_uses_on_a_diamond() {
        let mut graph = Graph::new();
        let (k1, k1v) = int_const(&mut graph, 1);
        let (k2, k2v) = int_const(&mut graph, 2);
        let (left, lv) = int_bin(&mut graph, crate::IntBinaryOp::Add, k1v, k2v);
        let (right, rv) = int_bin(&mut graph, crate::IntBinaryOp::Mul, k1v, k2v);
        let (sink, _sv) = int_bin(&mut graph, crate::IntBinaryOp::Add, lv, rv);

        let order =
            crate::walk::GraphWalkInfo::compute_full(&graph, sink).reverse_postorder(&graph);
        assert_eq!(order.len(), 5, "each node once: {order:?}");
        let pos = |n: NodeId| order.iter().position(|&x| x == n).unwrap();
        for op in [left, right] {
            assert!(pos(k1) < pos(op), "k1 before {op:?}: {order:?}");
            assert!(pos(k2) < pos(op), "k2 before {op:?}: {order:?}");
            assert!(pos(op) < pos(sink), "{op:?} before sink: {order:?}");
        }
        assert_eq!(*order.last().unwrap(), sink, "sink (sole consumer) is last");
    }

    /// A back-edge must terminate and visit each node once, roots first.
    /// Built from non-cacheable `Region` nodes, since cacheable data nodes
    /// reject post-hoc input edits.
    #[test]
    fn rpo_terminates_and_dedups_on_a_cycle() {
        use std::collections::HashSet;
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        let (a, a_ctrl) = make_ctrl_node(&mut graph, e_ctrl);
        let (b, b_ctrl) = make_ctrl_node(&mut graph, a_ctrl);
        // A also consumes B's control, closing the cycle.
        graph.add_node_input(a, b_ctrl);

        let order =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        let unique: HashSet<NodeId> = order.iter().copied().collect();
        assert_eq!(
            order.len(),
            unique.len(),
            "no node visited twice despite the cycle: {order:?}"
        );
        for n in [entry, a, b] {
            assert!(
                unique.contains(&n),
                "{n:?} missing despite the cycle: {order:?}"
            );
        }
        assert_eq!(
            order.first(),
            Some(&entry),
            "input-less root (entry) first: {order:?}"
        );
    }

    /// The raw relation must reach a consumer that is NOT in the live set.
    #[test]
    fn raw_def_use_postorder_reaches_dead_consumer() {
        let mut graph = Graph::new();
        // `Neg` is the dead consumer: reachable from the const through
        // def-use, but absent from every live set here.
        let (k, kv) = int_const(&mut graph, 3);
        let neg = graph.create_node(
            NodeKind::IntUnaryOp(crate::IntUnaryOp::Neg),
            [kv],
            [ValueKind::Typed(ValueType::I64)],
        );

        let empty: DenseEntitySet<NodeId> = DenseEntitySet::new();
        let filtered: Vec<NodeId> =
            PostOrder::new(DefUseSuccs::new(&graph, &empty), std::iter::once(k)).collect();
        assert_eq!(filtered, vec![k], "filtered walk stays at the root");

        let raw: Vec<NodeId> =
            PostOrder::new(RawDefUseSuccs::new(&graph), std::iter::once(k)).collect();
        assert!(
            raw.contains(&neg),
            "raw walk must reach the dead consumer: {raw:?}"
        );
        assert!(raw.contains(&k), "raw walk includes the root: {raw:?}");
        let pos = |n: NodeId| raw.iter().position(|&x| x == n).unwrap();
        assert!(
            pos(neg) < pos(k),
            "post-order yields the consumer before the producer"
        );
    }

    /// Global RPO must put `entry` first and visit each node exactly once.
    #[test]
    fn rpo_entry_first_visits_each_once() {
        use std::collections::HashSet;
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        let data = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [data_value] = graph.node_outputs_exact::<1>(data).unwrap();
        let b = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(b, c1);
        graph.add_node_input(b, data_value);

        let order: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);

        assert_eq!(
            order.first(),
            Some(&entry),
            "RPO must start at entry: {order:?}"
        );
        let unique: HashSet<NodeId> = order.iter().copied().collect();
        assert_eq!(
            order.len(),
            unique.len(),
            "no node visited twice: {order:?}"
        );
        for n in [entry, a, b, data] {
            assert!(unique.contains(&n), "{n:?} missing from RPO: {order:?}");
        }
    }

    /// A kind filter must preserve the relative RPO order of what survives.
    #[test]
    fn reverse_postorder_filter_kind_yields_only_matching_in_order() {
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        let (b, c2) = make_ctrl_node(&mut graph, c1);
        let _ret = make_return(&mut graph, c2);

        let regions: Vec<NodeId> = crate::walk::GraphWalkInfo::compute_full(&graph, entry)
            .reverse_postorder(&graph)
            .into_iter()
            .filter(|&n| matches!(graph.node_kind(n), NodeKind::Region))
            .collect();
        assert_eq!(
            regions,
            vec![a, b],
            "only Regions, earlier before later: {regions:?}"
        );
    }

    #[test]
    fn reverse_postorder_filter_excludes_unreachable() {
        let mut graph = Graph::new();
        let (entry, _c0) = make_entry(&mut graph);
        let isolated = graph.create_node(NodeKind::Return, [], []);

        let order: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        assert!(order.contains(&entry));
        assert!(
            !order.contains(&isolated),
            "unreachable node must be excluded"
        );
    }

    /// Two calls on one graph must yield the identical order.
    #[test]
    fn reverse_postorder_filter_is_deterministic_entry_first() {
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        let a = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [a_value] = graph.node_outputs_exact::<1>(a).unwrap();
        let c = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(4_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [c_value] = graph.node_outputs_exact::<1>(c).unwrap();
        let add = graph.create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [a_value, c_value],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [add_value] = graph.node_outputs_exact::<1>(add).unwrap();
        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, e_ctrl);
        graph.add_node_input(ret, add_value);

        let order1: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        let order2: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        assert_eq!(order1, order2, "RPO must be deterministic");
        assert_eq!(order1[0], entry, "entry first: {order1:?}");
        for n in [entry, a, c, add, ret] {
            assert_eq!(
                order1.iter().filter(|&&x| x == n).count(),
                1,
                "{n:?} must appear exactly once: {order1:?}"
            );
        }
    }

    /// The no-duplicate-visit invariant on a less regular shape than the
    /// single / linear / diamond cases above.
    #[test]
    fn walk_visits_no_node_more_than_once() {
        use std::collections::HashSet;
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        let (a, a_ctrl) = make_ctrl_node(&mut graph, e_ctrl);
        let (b, b_ctrl) = make_ctrl_node(&mut graph, a_ctrl);
        let data = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [data_value] = graph.node_outputs_exact::<1>(data).unwrap();
        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, b_ctrl);
        graph.add_node_input(ret, data_value);

        let visited: Vec<NodeId> = walk_graph(&graph, entry).collect();
        let unique: HashSet<NodeId> = visited.iter().copied().collect();
        assert_eq!(
            visited.len(),
            unique.len(),
            "walk_graph must visit each node at most once: visited={visited:?}"
        );
        for nid in [entry, a, b, data, ret] {
            assert!(unique.contains(&nid), "missing {nid:?}");
        }
    }

    /// Over a control diamond, post-order must visit each node once, cover
    /// exactly the reachable set, and put the lone root last. Converging
    /// control edges must not duplicate the join.
    #[test]
    fn postorder_on_control_diamond_visits_each_node_once() {
        let mut graph = Graph::new();
        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [ValueKind::Control, ValueKind::Control],
        );
        let [ctrl_l, ctrl_r] = graph.node_outputs_exact::<2>(entry).unwrap();
        let (left, left_ctrl) = make_ctrl_node(&mut graph, ctrl_l);
        let (right, right_ctrl) = make_ctrl_node(&mut graph, ctrl_r);
        let merge = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(merge, left_ctrl);
        graph.add_node_input(merge, right_ctrl);

        let info = GraphWalkInfo::compute_full(&graph, entry);
        let order: Vec<NodeId> = info.postorder(&graph).collect();
        assert_eq!(
            order.len(),
            4,
            "diamond postorder yields exactly 4 nodes: {order:?}"
        );
        for n in [entry, left, right, merge] {
            assert_eq!(
                order.iter().filter(|&&x| x == n).count(),
                1,
                "{n:?} must appear exactly once: {order:?}"
            );
        }
        assert_eq!(
            *order.last().unwrap(),
            entry,
            "the lone root (entry) comes last in post-order"
        );
    }

    /// A mid-graph seed reaches only its transitive data operands, never its
    /// consumers or the spine.
    #[test]
    fn walk_from_mid_graph_node_reaches_only_its_cone() {
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        let (k1, k1v) = int_const(&mut graph, 1);
        let (k2, k2v) = int_const(&mut graph, 2);
        let (add, addv) = int_bin(&mut graph, crate::IntBinaryOp::Add, k1v, k2v);
        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, e_ctrl);
        graph.add_node_input(ret, addv);

        use cranelift_entity::EntityRef;
        let mut cone: Vec<NodeId> = walk_graph(&graph, add).collect();
        cone.sort_unstable_by_key(|n| n.index());
        let mut expected = vec![add, k1, k2];
        expected.sort_unstable_by_key(|n| n.index());
        assert_eq!(
            cone, expected,
            "walk_from(add) covers exactly {{add, k1, k2}}: not the Return \
             consumer, not the entry spine"
        );
        assert!(!cone.contains(&ret) && !cone.contains(&entry));
    }

    /// Duplicated per in-crate test module on purpose: a dev-dep on
    /// `strider-ir-test-utils` would double-compile a DIFFERENT
    /// `FunctionBuilder` under `cargo test`.
    fn builder_with_region() -> crate::Result<crate::FunctionBuilder> {
        let mut b = crate::FunctionBuilder::new(
            vec![],
            strider_target::BuiltCallingConvention::default(),
            strider_target::Endianness::Little,
        )?;
        let r = b.create_region_all()?;
        b.set_entry_region_all(r)?;
        b.set_region(r);
        Ok(b)
    }

    /// The InitialMemory -> Store -> Load chain must come back whole, and a
    /// pure-arithmetic node with no memory edge must not.
    #[test]
    fn memory_reachable_covers_the_store_load_chain() {
        use crate::IRBuilderExt;

        let mut b = builder_with_region().unwrap();

        let space = rsleigh::VnSpace::RAM;
        let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
        let data = b.build_int_const(7u64, ValueType::I32).unwrap();
        b.build_store(addr, data, space).unwrap();
        b.build_load(addr, space, ValueType::I32).unwrap();

        // Must not appear in the result.
        let (arith, _) = int_bin(
            b.function_mut().graph_mut(),
            crate::IntBinaryOp::Add,
            addr,
            data,
        );

        // Without a terminator consuming the region's final memory token,
        // nothing control-reachable walks backward into the memory chain, so
        // the root-finding pass would never see `InitialMemory`.
        b.build_return(None, &[]).unwrap();

        let entry = b.entry();
        let f = b.function();
        let mem = memory_reachable(f, entry);

        for &n in &mem {
            let k = f.node_kind(n);
            assert!(
                matches!(
                    k,
                    NodeKind::InitialMemory
                        | NodeKind::Store(_)
                        | NodeKind::Load(_)
                        | NodeKind::Call
                        | NodeKind::CallOther { .. }
                        | NodeKind::MemPhi
                ),
                "non-memory-touching node {n:?} ({k:?}) must not appear in memory_reachable"
            );
        }
        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::InitialMemory)),
            "InitialMemory root must be included: {mem:?}"
        );
        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::Store(_))),
            "the Store must be reachable: {mem:?}"
        );
        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::Load(_))),
            "the Load must be reachable: {mem:?}"
        );
        assert!(
            !mem.contains(&arith),
            "a pure-arithmetic node with no memory edge must not appear: {mem:?}"
        );
    }

    /// With no memory ops at all, entry setup still wires the entry region's
    /// `MemPhi` to `InitialMemory`, so the result is the two-node chain rather
    /// than empty.
    #[test]
    fn memory_reachable_finds_the_entry_mem_phi_with_no_memory_ops() {
        let mut b = builder_with_region().unwrap();
        b.build_return(None, &[]).unwrap();
        let entry = b.entry();
        let f = b.function();
        let mem = memory_reachable(f, entry);

        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::InitialMemory)),
            "InitialMemory root must be included: {mem:?}"
        );
        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::MemPhi)),
            "the entry region's MemPhi must be reachable: {mem:?}"
        );
        for &n in &mem {
            assert!(
                matches!(f.node_kind(n), NodeKind::InitialMemory | NodeKind::MemPhi),
                "no Store/Load/Call exists yet non-memory node {n:?} appeared: {mem:?}"
            );
        }
    }

    /// The walk must terminate and dedup on a genuinely cyclic memory chain:
    /// `r0` branches to header `r1`, which branches to body `r2` or exit
    /// `r3`; `r2` branches back to `r1`.
    #[test]
    fn memory_reachable_terminates_on_a_loop_header_mem_phi_cycle() {
        use crate::IRBuilderExt;
        use std::collections::HashSet;

        let mut b = crate::FunctionBuilder::new(
            vec![],
            strider_target::BuiltCallingConvention::default(),
            strider_target::Endianness::Little,
        )
        .unwrap();

        let r0 = b.create_region_all().unwrap();
        b.set_entry_region_all(r0).unwrap();
        b.set_region(r0);

        let r1 = b.create_region_all().unwrap();
        b.build_branch(r1).unwrap();

        b.set_region(r1);
        let r2 = b.create_region_all().unwrap(); // loop body
        let r3 = b.create_region_all().unwrap(); // exit
        let cond = b.build_int_const(1u64, ValueType::I1).unwrap();
        b.build_if(cond, r2, r3).unwrap();

        // The back-edge wires r2's MemPhi output as r1's second predecessor.
        b.set_region(r2);
        b.build_branch(r1).unwrap();

        b.set_region(r3);
        b.build_return(None, &[]).unwrap();

        let entry = b.entry();
        let f = b.function();

        // Reaching the assertions below IS the termination proof.
        let mem = memory_reachable(f, entry);

        assert!(
            mem.iter()
                .filter(|&&n| matches!(f.node_kind(n), NodeKind::MemPhi))
                .count()
                >= 2,
            "both the loop-header MemPhi (r1) and the loop-body MemPhi (r2) \
             forming the back-edge must be included: {mem:?}"
        );
        let unique: HashSet<NodeId> = mem.iter().copied().collect();
        assert_eq!(
            mem.len(),
            unique.len(),
            "no node visited twice despite the back-edge cycle: {mem:?}"
        );
    }
}
