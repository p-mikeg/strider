use std::hash::Hash;

use cranelift_entity::{EntityRef, SecondaryMap};
use entity_utils::DenseEntitySet;
use petgraph::visit::{GraphBase, IntoNeighbors, VisitMap, Visitable};
use rustc_hash::FxHashSet;
use smallvec::SmallVec;

use crate::function::Function;
use crate::node::{NodeId, ValueId};

/// [`DenseEntitySet`] as a petgraph visit map; the trait is foreign to both.
#[derive(Default)]
pub(crate) struct NodeVisitMap(DenseEntitySet<NodeId>);

impl VisitMap<NodeId> for NodeVisitMap {
    fn visit(&mut self, a: NodeId) -> bool {
        self.0.insert(a)
    }

    fn is_visited(&self, a: &NodeId) -> bool {
        self.0.contains(*a)
    }

    fn unvisit(&mut self, a: NodeId) -> bool {
        self.0.remove(a)
    }
}

/// Only `Control`-kind edges are visible.
#[derive(Clone, Copy)]
pub(crate) struct ControlFlowView<'a> {
    function: &'a Function,
}

impl<'a> ControlFlowView<'a> {
    pub(crate) fn new(function: &'a Function) -> Self {
        Self { function }
    }
}

impl GraphBase for ControlFlowView<'_> {
    type NodeId = NodeId;
    type EdgeId = (NodeId, NodeId);
}

// petgraph requires the impl on `&G` so the receiver is Copy.
impl<'a> IntoNeighbors for &'a ControlFlowView<'a> {
    // Every control node but `Switch` fans out to at most two.
    type Neighbors = smallvec::IntoIter<[NodeId; 2]>;

    fn neighbors(self, a: NodeId) -> Self::Neighbors {
        crate::walk::cfg_succs(self.function.graph(), a)
            .collect::<SmallVec<[NodeId; 2]>>()
            .into_iter()
    }
}

impl Visitable for ControlFlowView<'_> {
    type Map = NodeVisitMap;

    fn visit_map(&self) -> Self::Map {
        NodeVisitMap::default()
    }

    fn reset_map(&self, map: &mut Self::Map) {
        map.0.clear();
    }
}

/// Cooper-Harvey-Kennedy dominators of the control subgraph, quadratic on a
/// long `if (c) goto err;` ladder; [`control_dominator_tree`] is not.
pub fn control_dominators(function: &Function) -> petgraph::algo::dominators::Dominators<NodeId> {
    let entry = function.entry();
    petgraph::algo::dominators::simple_fast(&ControlFlowView::new(function), entry)
}

/// A dominator tree numbered by pre/post interval, so [`Self::dominates`] is
/// O(1).
pub struct DominatorTree<K: EntityRef> {
    /// `(0, 0)` for a vertex outside the tree.
    span: SecondaryMap<K, (u32, u32)>,
    preorder: Vec<K>,
}

impl<K: EntityRef + Hash> DominatorTree<K> {
    /// Over the vertices `graph` reaches from `root`.
    fn compute<G>(graph: G, root: K) -> Self
    where
        G: IntoNeighbors<NodeId = K>,
    {
        let doms = graph_algorithms::dominance::dominators(root, |v| graph.neighbors(v));
        let mut edges: Vec<(K, K)> = doms
            .vertices()
            .iter()
            .filter_map(|&v| doms.immediate_dominator(v).map(|d| (d, v)))
            .collect();
        edges.sort_unstable_by_key(|&(d, v)| (d.index(), v.index()));
        let mut span: SecondaryMap<K, (u32, u32)> = SecondaryMap::new();
        let mut preorder = Vec::new();
        let mut clock = 1u32;
        let mut stack = vec![(root, false)];
        while let Some((vertex, done)) = stack.pop() {
            if done {
                span[vertex].1 = clock;
            } else {
                span[vertex].0 = clock;
                preorder.push(vertex);
                stack.push((vertex, true));
                let lo = edges.partition_point(|&(d, _)| d.index() < vertex.index());
                stack.extend(
                    edges[lo..]
                        .iter()
                        .take_while(|&&(d, _)| d == vertex)
                        .map(|&(_, child)| (child, false)),
                );
            }
            clock += 1;
        }
        Self { span, preorder }
    }

    pub(crate) fn root(&self) -> K {
        self.preorder[0]
    }

    /// Vertices in dominator-tree pre-order, so a vertex follows every vertex
    /// that dominates it.
    pub(crate) fn preorder(&self) -> &[K] {
        &self.preorder
    }

    pub(crate) fn contains(&self, v: K) -> bool {
        self.span[v].0 != 0
    }

    /// True when every path from the root to `b` passes through `a`. Reflexive
    /// for every vertex, one absent from the tree included; otherwise `false`
    /// when either vertex is absent.
    pub fn dominates(&self, a: K, b: K) -> bool {
        let ((a_pre, a_post), (b_pre, b_post)) = (self.span[a], self.span[b]);
        a == b || (a_pre != 0 && b_pre != 0 && a_pre <= b_pre && b_post <= a_post)
    }

    /// [`Self::dominates`], three-valued: `None` when either vertex is absent,
    /// so a caller negating the answer does not turn "cannot say" into "yes".
    /// Kinds with no control edge (`Load`, `Store`, arithmetic) are never in
    /// the tree.
    pub fn dominance_verdict(&self, a: K, b: K) -> Option<bool> {
        (self.contains(a) && self.contains(b)).then(|| self.dominates(a, b))
    }
}

/// [`control_dominators`] as a [`DominatorTree`].
pub fn control_dominator_tree(function: &Function) -> DominatorTree<NodeId> {
    DominatorTree::compute(&ControlFlowView::new(function), function.entry())
}

/// Dominators of the edge-split control graph. Querying with
/// [`CtrlKey::Node`] keys answers node dominance identically to
/// [`control_dominator_tree`].
pub fn control_edge_dominator_tree(function: &Function) -> DominatorTree<CtrlKey> {
    // The entry key must be `CtrlKey::Node(function.entry())`; a mismatch
    // yields an empty tree that silently answers `false` to every query.
    DominatorTree::compute(
        &ControlSplitView::new(function),
        CtrlKey::Node(function.entry()),
    )
}

/// A vertex of the edge-split control graph. Dominance over `Edge(v)` is edge
/// dominance over `v` in the ordinary CFG.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CtrlKey {
    Node(NodeId),
    /// A `Control`-kind output value.
    Edge(ValueId),
}

/// A node's index is even, an edge's odd.
impl EntityRef for CtrlKey {
    fn new(index: usize) -> Self {
        if index.is_multiple_of(2) {
            Self::Node(NodeId::new(index / 2))
        } else {
            Self::Edge(ValueId::new(index / 2))
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Node(n) => 2 * n.index(),
            Self::Edge(v) => 2 * v.index() + 1,
        }
    }
}

/// [`ControlFlowView`] with [`cfg_succs`](crate::walk::cfg_succs)'s two stages
/// unfolded: node to its `Control` outputs stops at [`CtrlKey::Edge`], and
/// output to its consumers resumes from it.
#[derive(Clone, Copy)]
pub(crate) struct ControlSplitView<'a> {
    function: &'a Function,
}

impl<'a> ControlSplitView<'a> {
    pub(crate) fn new(function: &'a Function) -> Self {
        Self { function }
    }
}

impl GraphBase for ControlSplitView<'_> {
    type NodeId = CtrlKey;
    type EdgeId = (CtrlKey, CtrlKey);
}

impl<'a> IntoNeighbors for &'a ControlSplitView<'a> {
    type Neighbors = smallvec::IntoIter<[CtrlKey; 2]>;

    fn neighbors(self, a: CtrlKey) -> Self::Neighbors {
        let graph = self.function.graph();
        match a {
            // Stage 1, stopping at the output.
            CtrlKey::Node(node) => crate::walk::cfg_outputs(graph, node)
                .map(CtrlKey::Edge)
                .collect::<SmallVec<[CtrlKey; 2]>>()
                .into_iter(),
            // Stage 2, resuming from the output.
            CtrlKey::Edge(value) => {
                let succs: SmallVec<[CtrlKey; 2]> = graph
                    .value_uses(value)
                    .map(|(succ, _)| CtrlKey::Node(succ))
                    .collect();
                debug_assert_eq!(
                    succs.len(),
                    1,
                    "control edge {value:?} has {} consumers; every control edge \
                     has exactly one.  Zero is a dangling control path: the \
                     validator rejects it as `UnusedControlOutput`, since every \
                     control edge must reach a terminator (`Return` / \
                     `IndirectBranch` / `Unreachable`).  It would also make this \
                     edge-split vertex a DEAD END, so `simple_fast` would treat \
                     everything past it as unreachable and every dominance query \
                     beyond it would silently answer `false` instead of failing.",
                    succs.len()
                );
                succs.into_iter()
            }
        }
    }
}

impl Visitable for ControlSplitView<'_> {
    type Map = FxHashSet<CtrlKey>;

    fn visit_map(&self) -> Self::Map {
        FxHashSet::default()
    }

    fn reset_map(&self, map: &mut Self::Map) {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::IRBuilderExt;
    use crate::node::NodeKind;
    use crate::{FunctionBuilder, IRViewer};
    use cranelift_entity::EntityRef;
    use petgraph::visit::IntoNeighbors;

    /// The idom chain-walk answer [`DominatorTree::dominates`] must match.
    fn dominates<N: Copy + Eq + std::hash::Hash>(
        doms: &petgraph::algo::dominators::Dominators<N>,
        a: N,
        b: N,
    ) -> bool {
        a == b || doms.dominators(b).is_some_and(|mut it| it.any(|d| d == a))
    }

    fn dominance_verdict<N: Copy + Eq + std::hash::Hash>(
        doms: &petgraph::algo::dominators::Dominators<N>,
        a: N,
        b: N,
    ) -> Option<bool> {
        (doms.dominators(a).is_some() && doms.dominators(b).is_some())
            .then(|| dominates(doms, a, b))
    }

    fn control_edge_dominators(
        function: &Function,
    ) -> petgraph::algo::dominators::Dominators<CtrlKey> {
        petgraph::algo::dominators::simple_fast(
            &ControlSplitView::new(function),
            CtrlKey::Node(function.entry()),
        )
    }

    fn empty_builder() -> crate::error::Result<FunctionBuilder> {
        FunctionBuilder::new(
            vec![],
            strider_target::BuiltCallingConvention::default(),
            strider_target::Endianness::Little,
        )
    }

    /// Builds a diamond-shaped CFG:
    ///
    /// ```text
    ///       Entry
    ///         |
    ///     Region A  (branch)
    ///       If(cond)
    ///      /        \
    ///  Region B   Region C
    ///      \        /
    ///       Region D  (join)
    ///         |
    ///       Return
    /// ```
    fn diamond() -> crate::error::Result<Function> {
        let mut b = empty_builder()?;

        let region_a = b.create_region_all()?;
        let region_b = b.create_region_all()?;
        let region_c = b.create_region_all()?;
        let region_d = b.create_region_all()?;

        b.set_entry_region_all(region_a)?;

        // A: branch to B on true, C on false.
        b.set_region(region_a);
        b.set_lift_addr(Some(0x1000));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, region_b, region_c)?;
        b.set_lift_addr(None);

        b.set_region(region_b);
        b.set_lift_addr(Some(0x1010));
        b.build_branch(region_d)?;
        b.set_lift_addr(None);

        b.set_region(region_c);
        b.set_lift_addr(Some(0x1020));
        b.build_branch(region_d)?;
        b.set_lift_addr(None);

        b.set_region(region_d);
        b.set_lift_addr(Some(0x1030));
        b.build_function_return()?;
        b.set_lift_addr(None);

        b.build()
    }

    #[test]
    fn control_view_neighbors_are_control_successors() {
        let f = diamond().expect("diamond() should build without errors");
        let view = ControlFlowView::new(&f);
        let if_node = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::If))
            .expect("diamond CFG must contain an If node");
        let succ: std::collections::BTreeSet<_> =
            view.neighbors(if_node).map(|n| n.index()).collect();
        assert_eq!(
            succ.len(),
            2,
            "If has exactly two control successors, got {succ:?}"
        );
    }

    #[test]
    fn simple_fast_join_idom_is_branch_region() {
        use petgraph::algo::dominators::simple_fast;

        let f = diamond().expect("diamond() should build without errors");
        let entry = f.entry();

        let doms = simple_fast(&ControlFlowView::new(&f), entry);

        let if_node = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::If))
            .expect("diamond must have an If node");

        // The join is the unique Region with more than one control input.
        let join_region_node = f
            .graph()
            .all_node_ids()
            .filter(|&n| matches!(f.node_kind(n), NodeKind::Region))
            .find(|&n| {
                f.graph()
                    .node_inputs(n)
                    .into_iter()
                    .filter(|&v| f.graph().value_kind(v).is_control())
                    .count()
                    > 1
            })
            .expect("diamond must have a join region with 2 control inputs");

        let idom = doms
            .immediate_dominator(join_region_node)
            .expect("join region must have an immediate dominator");
        assert_eq!(
            idom, if_node,
            "join region's idom should be the If node (every path through \
             the diamond must pass through If before reaching the join), \
             got {idom:?}"
        );

        assert!(
            dominates(&doms, if_node, join_region_node),
            "If node should dominate the join region"
        );
        assert!(
            !dominates(&doms, join_region_node, if_node),
            "join region should NOT dominate the If node"
        );

        assert!(
            dominates(&doms, entry, join_region_node),
            "entry should dominate join region"
        );
        assert!(
            dominates(&doms, entry, if_node),
            "entry should dominate If node"
        );
    }

    /// `if (c) {} else { X }` with an EMPTY true arm, then a join and a tail:
    ///
    /// ```text
    ///       Entry
    ///         |
    ///     Region A
    ///       If(cond)
    ///      /        \
    ///  (true edge)  Region E   (the else block: non-empty)
    ///      \        /
    ///       Region J  (join: the true edge's DIRECT target)
    ///         |
    ///       Region T  (tail: after the merge, reachable via BOTH arms)
    ///         |
    ///       Return
    /// ```
    ///
    /// Returns `(function, true_edge_value, region_j, region_t)`.
    fn empty_true_arm() -> crate::error::Result<(Function, ValueId, NodeId, NodeId)> {
        let mut b = empty_builder()?;

        let region_a = b.create_region_all()?;
        let region_e = b.create_region_all()?;
        let region_j = b.create_region_all()?;
        let region_t = b.create_region_all()?;

        b.set_entry_region_all(region_a)?;

        // True goes straight to the join (the empty arm), false to the else.
        b.set_region(region_a);
        b.set_lift_addr(Some(0x2000));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, region_j, region_e)?;
        b.set_lift_addr(None);

        b.set_region(region_e);
        b.set_lift_addr(Some(0x2010));
        b.build_branch(region_j)?;
        b.set_lift_addr(None);

        b.set_region(region_j);
        b.set_lift_addr(Some(0x2020));
        b.build_branch(region_t)?;
        b.set_lift_addr(None);

        b.set_region(region_t);
        b.set_lift_addr(Some(0x2030));
        b.build_function_return()?;
        b.set_lift_addr(None);

        let f = b.build()?;

        let if_node = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::If))
            .expect("must contain an If node");
        // The If's first Control output is the true edge.
        let [true_edge, _false_edge] = f.graph().node_outputs_exact::<2>(if_node).unwrap();

        // With an empty true arm the true edge's sole consumer is the join,
        // and the tail is the join's sole control successor. Derived from the
        // graph because the builder's `RegionId`s are not `NodeId`s.
        let join = f
            .graph()
            .value_uses(true_edge)
            .next()
            .expect("the true edge has a consumer")
            .0;
        let tail = crate::walk::cfg_succs(f.graph(), join)
            .next()
            .expect("the join has a control successor");

        Ok((f, true_edge, join, tail))
    }

    /// All three shapes that could break the node/split dominance
    /// correspondence, in one function:
    ///
    /// ```text
    ///       Entry
    ///         |
    ///     Region A
    ///       If(c)          (the diamond)
    ///      /      \
    ///  Region B  Region C
    ///      \      /
    ///     Region D  (join)
    ///       If(c)          (the empty arm: true runs straight into the join)
    ///      /      \
    ///  (true)   Region E
    ///      \      /
    ///     Region G  (join: the true edge's DIRECT target)
    ///       If(c)          (the guarded loop's guard)
    ///      /      \
    ///  Region H   \        (loop header: preds = the guard's edge + the latch)
    ///    If(c)     \
    ///    /    \     \
    /// Region L  \    \     (latch: back-edge to H)
    ///    |       \    \
    ///    +--> H   \    \
    ///              \    |
    ///              Region X  (exit: reachable from the guard AND the loop)
    ///                |
    ///              Return
    /// ```
    fn diamond_loop_and_empty_arm() -> crate::error::Result<Function> {
        let mut b = empty_builder()?;

        let (a, c_b, c_c, d) = (
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
        );
        let (e, g, h, l, x) = (
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
        );

        b.set_entry_region_all(a)?;

        b.set_region(a);
        b.set_lift_addr(Some(0x3000));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, c_b, c_c)?;
        b.set_lift_addr(None);

        // The diamond's arms, both into D.
        for (region, addr) in [(c_b, 0x3010), (c_c, 0x3020)] {
            b.set_region(region);
            b.set_lift_addr(Some(addr));
            b.build_branch(d)?;
            b.set_lift_addr(None);
        }

        // The empty-arm branch: true goes straight to the join G.
        b.set_region(d);
        b.set_lift_addr(Some(0x3030));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, g, e)?;
        b.set_lift_addr(None);

        // The non-empty else arm.
        b.set_region(e);
        b.set_lift_addr(Some(0x3040));
        b.build_branch(g)?;
        b.set_lift_addr(None);

        // The loop guard: enter the loop, or skip to the exit.
        b.set_region(g);
        b.set_lift_addr(Some(0x3050));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, h, x)?;
        b.set_lift_addr(None);

        // The loop header: preds are the guard's edge and L's back edge.
        b.set_region(h);
        b.set_lift_addr(Some(0x3060));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, l, x)?;
        b.set_lift_addr(None);

        // The latch, back-edge to the header.
        b.set_region(l);
        b.set_lift_addr(Some(0x3070));
        b.build_branch(h)?;
        b.set_lift_addr(None);

        b.set_region(x);
        b.set_lift_addr(Some(0x3080));
        b.build_function_return()?;
        b.set_lift_addr(None);

        b.build()
    }

    /// The subsumption property, over every ordered pair of control-reachable
    /// nodes.
    #[test]
    fn split_dominance_subsumes_node_dominance() {
        for (name, f) in [
            ("diamond", diamond().expect("diamond builds")),
            (
                "empty_true_arm",
                empty_true_arm().expect("empty_true_arm builds").0,
            ),
            (
                "diamond_loop_and_empty_arm",
                diamond_loop_and_empty_arm().expect("combined fixture builds"),
            ),
        ] {
            let node_doms = control_dominators(&f);
            let split = control_edge_dominators(&f);

            let reachable = crate::walk::cfg_reachable(f.graph(), f.entry());
            let nodes: Vec<NodeId> = f
                .graph()
                .all_node_ids()
                .filter(|&n| reachable.contains(n))
                .collect();

            // A fixture that walked no nodes would pass vacuously.
            assert!(
                nodes.len() >= 4,
                "{name}: fixture must have control-reachable nodes to compare, got {}",
                nodes.len()
            );

            let mut agreed_true = 0usize;
            for &a in &nodes {
                for &b in &nodes {
                    let via_nodes = dominates(&node_doms, a, b);
                    let via_split = dominates(&split, CtrlKey::Node(a), CtrlKey::Node(b));
                    assert_eq!(
                        via_nodes, via_split,
                        "{name}: node tree and split tree disagree on \
                         dominates({a:?}, {b:?}): {via_nodes} vs {via_split}"
                    );
                    if via_nodes {
                        agreed_true += 1;
                    }
                }
            }

            // Guards a vacuous pass where both trees answer `false` for
            // everything, e.g. an entry-key mismatch yielding an empty tree:
            // every node dominates itself and the entry dominates all.
            assert!(
                agreed_true >= 2 * nodes.len() - 1,
                "{name}: expected at least the reflexive pairs plus the entry's \
                 row to hold, got {agreed_true} true pairs over {} nodes",
                nodes.len()
            );
        }
    }

    /// `Node(n) -> Edge(v) -> Node(c)` must compose to exactly `cfg_succs(n)`.
    #[test]
    fn split_view_composes_to_cfg_succs() {
        for (name, f) in [
            ("diamond", diamond().expect("diamond builds")),
            (
                "empty_true_arm",
                empty_true_arm().expect("empty_true_arm builds").0,
            ),
        ] {
            let view = ControlSplitView::new(&f);
            let plain = ControlFlowView::new(&f);

            for node in f.graph().all_node_ids() {
                // The split view and `cfg_succs` are only consulted on
                // control-reachable nodes.
                if !crate::walk::cfg_reachable(f.graph(), f.entry()).contains(node) {
                    continue;
                }

                let composed: std::collections::BTreeSet<usize> = view
                    .neighbors(CtrlKey::Node(node))
                    .flat_map(|edge| view.neighbors(edge))
                    .map(|k| match k {
                        CtrlKey::Node(n) => n.index(),
                        CtrlKey::Edge(_) => panic!("Edge -> Edge must be impossible"),
                    })
                    .collect();

                let direct: std::collections::BTreeSet<usize> =
                    crate::walk::cfg_succs(f.graph(), node)
                        .map(|n| n.index())
                        .collect();

                assert_eq!(
                    composed, direct,
                    "{name}: split view Node->Edge->Node must compose to exactly \
                     cfg_succs for {node:?}"
                );

                // And to what the un-split view reports, which is the graph
                // `Dominates` answers from.
                let plain_succs: std::collections::BTreeSet<usize> =
                    plain.neighbors(node).map(|n| n.index()).collect();
                assert_eq!(
                    composed, plain_succs,
                    "{name}: split view must agree with ControlFlowView for {node:?}"
                );
            }
        }
    }

    /// With an empty true arm the true edge runs straight into the join, so
    /// the join dominates the tail, yet the tail is reachable through both
    /// arms and the true EDGE does not dominate it.
    #[test]
    fn edge_dominates_is_false_past_a_join_with_an_empty_arm() {
        let (f, true_edge, region_j, region_t) = empty_true_arm().expect("empty_true_arm builds");

        let split = control_edge_dominators(&f);
        let node_doms = control_dominators(&f);

        let consumer = f
            .graph()
            .value_uses(true_edge)
            .next()
            .expect("true edge has a consumer")
            .0;
        assert_eq!(
            consumer, region_j,
            "with an empty true arm the true edge's consumer is the join itself"
        );
        assert!(
            dominates(&node_doms, consumer, region_t),
            "the join DOES dominate the tail, so a node-dominance proxy \
             wrongly claims the tail is inside the true block"
        );

        assert!(
            !dominates(&split, CtrlKey::Edge(true_edge), CtrlKey::Node(region_t)),
            "the tail is past the merge and reachable through BOTH arms, so the \
             true EDGE must not dominate it"
        );
        assert!(
            !dominates(&split, CtrlKey::Edge(true_edge), CtrlKey::Node(region_j)),
            "the join is reachable through the false arm too, so the true edge \
             does not dominate it either"
        );
    }

    /// A node genuinely inside a non-empty arm IS edge-dominated by that arm's
    /// edge; the relation must not be vacuously false.
    #[test]
    fn edge_dominates_is_true_inside_a_non_empty_arm() {
        let f = diamond().expect("diamond builds");
        let split = control_edge_dominators(&f);

        let if_node = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::If))
            .expect("diamond has an If");
        let [true_edge, false_edge] = f.graph().node_outputs_exact::<2>(if_node).unwrap();

        let true_block = f.graph().value_uses(true_edge).next().unwrap().0;
        let false_block = f.graph().value_uses(false_edge).next().unwrap().0;

        assert!(
            dominates(&split, CtrlKey::Edge(true_edge), CtrlKey::Node(true_block)),
            "the true block is in the true block"
        );
        assert!(
            !dominates(&split, CtrlKey::Edge(true_edge), CtrlKey::Node(false_block)),
            "the false block is NOT in the true block"
        );
        assert!(
            dominates(
                &split,
                CtrlKey::Edge(false_edge),
                CtrlKey::Node(false_block)
            ),
            "the false block is in the false block"
        );

        // The join is reachable from both arms, so neither edge dominates it.
        let join = f
            .graph()
            .all_node_ids()
            .filter(|&n| matches!(f.node_kind(n), NodeKind::Region))
            .find(|&n| {
                f.graph()
                    .node_inputs(n)
                    .into_iter()
                    .filter(|&v| f.graph().value_kind(v).is_control())
                    .count()
                    > 1
            })
            .expect("diamond has a join");
        assert!(
            !dominates(&split, CtrlKey::Edge(true_edge), CtrlKey::Node(join)),
            "the join is past the merge: no branch edge dominates it"
        );
        assert!(!dominates(
            &split,
            CtrlKey::Edge(false_edge),
            CtrlKey::Node(join)
        ));
    }

    /// An edge trivially dominates itself over the zero-length path.
    #[test]
    fn edge_dominates_itself_via_zero_length_path() {
        let f = diamond().expect("diamond builds");
        let split = control_edge_dominators(&f);
        let if_node = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::If))
            .expect("diamond has an If");
        let [true_edge, _] = f.graph().node_outputs_exact::<2>(if_node).unwrap();

        assert!(
            dominates(&split, CtrlKey::Edge(true_edge), CtrlKey::Edge(true_edge)),
            "an edge dominates itself over the zero-length path (the direct case)"
        );
        // The trap: an edge does NOT dominate the If that produces it, so
        // testing edge-against-producer instead of edge-against-edge would
        // break exactly the direct case.
        assert!(
            !dominates(&split, CtrlKey::Edge(true_edge), CtrlKey::Node(if_node)),
            "an edge cannot dominate its own producer; the If precedes it"
        );
    }

    /// A two-entry loop `B <-> C` entered from both arms of `A`, and an orphan
    /// loop `U <-> V` that no path from the entry reaches, exiting into `C`:
    ///
    /// ```text
    ///       Entry
    ///         |
    ///     Region A
    ///       If(c)
    ///      /     \
    ///  Region B <-> Region C <- Region V <-> Region U   (orphans)
    ///      \       /
    ///      Region X
    ///         |
    ///       Return
    /// ```
    fn irreducible_with_orphan() -> crate::error::Result<Function> {
        let mut b = empty_builder()?;
        let (a, rb, rc, x, u, v) = (
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
        );
        b.set_entry_region_all(a)?;
        b.set_lift_addr(Some(0x4000));
        for (region, t, f) in [(a, rb, rc), (rb, rc, x), (rc, rb, x), (v, u, rc)] {
            b.set_region(region);
            let cond = b.build_boolean_const(true);
            b.build_if(cond, t, f)?;
        }
        b.set_region(u);
        b.build_branch(v)?;
        b.set_region(x);
        b.build_function_return()?;
        b.build()
    }

    /// A CFG of 2 to 9 regions with terminators drawn from `seed`: loops,
    /// irreducible edges and orphan regions. `None` when the draw is not a
    /// valid function, e.g. an exit-free cycle.
    fn random_cfg(seed: u64) -> Option<Function> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        let mut b = empty_builder().ok()?;
        let n = 2 + next(8);
        let regions: Vec<_> = (0..n)
            .map(|_| b.create_region_all())
            .collect::<crate::error::Result<_>>()
            .ok()?;
        b.set_entry_region_all(regions[0]).ok()?;
        b.set_lift_addr(Some(0x5000));
        for &region in &regions {
            b.set_region(region);
            let pick = regions[next(n) as usize];
            let other = regions[next(n) as usize];
            match next(10) {
                0 => b.build_function_return().ok()?,
                1 => b.build_unreachable().ok()?,
                2 | 3 => b.build_branch(pick).ok()?,
                4 => {
                    let address = b.build_int_const(0u64, crate::node::ValueType::I64).ok()?;
                    let third = regions[next(n) as usize];
                    b.build_switch(address, &[(pick, 0), (other, 1), (third, 2)])
                        .ok()?;
                }
                _ => {
                    let cond = b.build_boolean_const(true);
                    b.build_if(cond, pick, other).ok()?;
                }
            }
        }
        b.build().ok()
    }

    /// The interval trees answer every query exactly as the idom chain walk:
    /// over every pair of nodes, and over every pair of node and value keys of
    /// the split graph.
    #[test]
    fn dominator_tree_matches_chain_walk() {
        let mut fixtures = vec![
            diamond().expect("diamond builds"),
            empty_true_arm().expect("empty_true_arm builds").0,
            diamond_loop_and_empty_arm().expect("combined fixture builds"),
            irreducible_with_orphan().expect("irreducible fixture builds"),
        ];
        let hand_built = fixtures.len();
        fixtures.extend((0..400).filter_map(random_cfg));
        assert!(
            fixtures.len() >= hand_built + 100,
            "too few random CFGs built: {}",
            fixtures.len() - hand_built
        );

        for (i, f) in fixtures.iter().enumerate() {
            let chain = control_dominators(f);
            let tree = control_dominator_tree(f);
            let nodes: Vec<NodeId> = f.graph().all_node_ids().collect();
            let mut held = 0usize;
            for &a in &nodes {
                for &b in &nodes {
                    let verdict = tree.dominance_verdict(a, b);
                    assert_eq!(
                        verdict,
                        dominance_verdict(&chain, a, b),
                        "fixture {i}: node verdict({a:?}, {b:?})"
                    );
                    assert_eq!(
                        tree.dominates(a, b),
                        dominates(&chain, a, b),
                        "fixture {i}: node dominates({a:?}, {b:?})"
                    );
                    held += usize::from(verdict == Some(true));
                }
            }
            assert!(held >= 2, "fixture {i}: vacuous node tree");

            let chain = control_edge_dominators(f);
            let tree = control_edge_dominator_tree(f);
            let keys: Vec<CtrlKey> = nodes
                .iter()
                .map(|&n| CtrlKey::Node(n))
                .chain(f.graph().all_value_ids().map(CtrlKey::Edge))
                .collect();
            let mut held = 0usize;
            for &a in &keys {
                for &b in &keys {
                    let verdict = tree.dominance_verdict(a, b);
                    assert_eq!(
                        verdict,
                        dominance_verdict(&chain, a, b),
                        "fixture {i}: split verdict({a:?}, {b:?})"
                    );
                    assert_eq!(
                        tree.dominates(a, b),
                        dominates(&chain, a, b),
                        "fixture {i}: split dominates({a:?}, {b:?})"
                    );
                    held += usize::from(verdict == Some(true));
                }
            }
            assert!(held >= 3, "fixture {i}: vacuous split tree");
        }
    }

    /// `if (c) goto err;` `n` times: every rung also edges into one shared
    /// error region.
    fn error_ladder(n: usize) -> crate::error::Result<Function> {
        let mut b = empty_builder()?;
        let rungs: Vec<_> = (0..n)
            .map(|_| b.create_region_all())
            .collect::<crate::error::Result<_>>()?;
        let (err, done) = (b.create_region_all()?, b.create_region_all()?);
        b.set_entry_region_all(rungs[0])?;
        b.set_lift_addr(Some(0x6000));
        for (i, &rung) in rungs.iter().enumerate() {
            b.set_region(rung);
            let cond = b.build_boolean_const(true);
            b.build_if(cond, err, rungs.get(i + 1).copied().unwrap_or(done))?;
        }
        for region in [err, done] {
            b.set_region(region);
            b.build_function_return()?;
        }
        b.build()
    }

    #[test]
    fn dominator_tree_build_is_near_linear_on_an_error_ladder() {
        fn build_tree(n: usize) -> std::time::Duration {
            let f = error_ladder(n).expect("ladder builds");
            let start = std::time::Instant::now();
            let tree = control_dominator_tree(&f);
            let elapsed = start.elapsed();
            assert!(tree.contains(f.entry()));
            elapsed
        }
        build_tree(500);
        let small = build_tree(1_000);
        let large = build_tree(16_000);
        // Linear would be 16x; quadratic 256x.
        assert!(
            large.as_secs_f64() < small.as_secs_f64() * 40.0,
            "16x the rungs cost {:.1}x the build ({small:?} -> {large:?})",
            large.as_secs_f64() / small.as_secs_f64(),
        );
    }
}
