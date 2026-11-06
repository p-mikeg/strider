use std::hash::Hash;

use cranelift_entity::{EntityRef, SecondaryMap};
use smallvec::SmallVec;

use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, ValueId};

/// A dominator tree numbered by pre/post interval, so [`Self::dominates`] is
/// O(1).
pub struct DominatorTree<K: EntityRef> {
    /// `(0, 0)` for a vertex outside the tree.
    span: SecondaryMap<K, (u32, u32)>,
    preorder: Vec<K>,
}

impl<K: EntityRef + Hash> DominatorTree<K> {
    /// Over the vertices `succs` reaches from `root`.
    fn compute<I: IntoIterator<Item = K>>(root: K, succs: impl FnMut(K) -> I) -> Self {
        let doms = graph_algorithms::dominance::dominators(root, succs);
        let intervals = doms.intervals();
        let vertices = doms.vertices();
        let mut span: SecondaryMap<K, (u32, u32)> = SecondaryMap::new();
        for (at, &vertex) in vertices.iter().enumerate() {
            let (enter, exit) = intervals.span(at as u32);
            // From 1, so `(0, 0)` stays free to mean "outside the tree".
            span[vertex] = (enter + 1, exit + 1);
        }
        let preorder = intervals
            .preorder()
            .iter()
            .map(|&at| vertices[at as usize])
            .collect();
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

    /// `v`'s `(pre, post)` clock, `None` outside the tree: `a` dominates `b`
    /// iff `a`'s span encloses `b`'s, and spans otherwise never overlap.
    pub fn span(&self, v: K) -> Option<(u32, u32)> {
        self.contains(v).then(|| self.span[v])
    }

    /// True when every path from the root to `b` passes through `a`. Reflexive
    /// even outside the tree; otherwise `false` when either vertex is outside.
    pub fn dominates(&self, a: K, b: K) -> bool {
        let ((a_pre, a_post), (b_pre, b_post)) = (self.span[a], self.span[b]);
        a == b || (a_pre != 0 && b_pre != 0 && a_pre <= b_pre && b_post <= a_post)
    }

    /// [`Self::dominates`], `None` when either vertex is absent from the tree
    /// (as every non-control node is).
    pub fn dominance_verdict(&self, a: K, b: K) -> Option<bool> {
        (self.contains(a) && self.contains(b)).then(|| self.dominates(a, b))
    }
}

/// Dominators of the control subgraph.
pub fn control_dominator_tree(function: &Function) -> DominatorTree<NodeId> {
    let graph = function.graph();
    DominatorTree::compute(function.entry(), |n| crate::walk::cfg_succs(graph, n))
}

/// Dominators of the edge-split control graph. Querying with
/// [`CtrlKey::Node`] keys answers node dominance identically to
/// [`control_dominator_tree`].
pub fn control_edge_dominator_tree(function: &Function) -> DominatorTree<CtrlKey> {
    let graph = function.graph();
    DominatorTree::compute(CtrlKey::Node(function.entry()), |k| split_succs(graph, k))
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

/// [`cfg_succs`](crate::walk::cfg_succs)'s two stages unfolded: node to its
/// `Control` outputs stops at [`CtrlKey::Edge`], and output to its consumers
/// resumes from it.
fn split_succs(graph: &Graph, a: CtrlKey) -> SmallVec<[CtrlKey; 2]> {
    match a {
        CtrlKey::Node(node) => crate::walk::cfg_outputs(graph, node)
            .map(CtrlKey::Edge)
            .collect(),
        CtrlKey::Edge(value) => {
            let succs: SmallVec<[CtrlKey; 2]> = graph
                .value_uses(value)
                .map(|(succ, _)| CtrlKey::Node(succ))
                .collect();
            // Valid IR gives each control edge exactly one consumer.
            debug_assert_eq!(
                succs.len(),
                1,
                "control edge {value:?} has {} consumers, expected exactly one",
                succs.len()
            );
            succs
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::IRBuilderExt;
    use crate::node::NodeKind;
    use crate::test_cfgs::{self, empty_builder};
    use crate::{FunctionBuilder, IRViewer};
    use cranelift_entity::EntityRef;
    use petgraph::visit::{GraphBase, IntoNeighbors};
    use proptest::prelude::*;
    use strider_ir_test_utils::graph_oracle;

    /// Only `Control`-kind edges are visible.
    #[derive(Clone, Copy)]
    struct ControlFlowView<'a>(&'a Function);

    impl ControlFlowView<'_> {
        /// Every control node but `Switch` fans out to at most two.
        fn succs(self, a: NodeId) -> SmallVec<[NodeId; 2]> {
            crate::walk::cfg_succs(self.0.graph(), a).collect()
        }
    }

    /// [`ControlFlowView`] over the edge-split control graph.
    #[derive(Clone, Copy)]
    struct ControlSplitView<'a>(&'a Function);

    impl ControlSplitView<'_> {
        fn succs(self, a: CtrlKey) -> SmallVec<[CtrlKey; 2]> {
            split_succs(self.0.graph(), a)
        }
    }

    // petgraph's `simple_fast` is the independent oracle for the interval trees.
    impl GraphBase for ControlFlowView<'_> {
        type NodeId = NodeId;
        type EdgeId = (NodeId, NodeId);
    }

    // petgraph requires the impl on `&G` so the receiver is Copy.
    impl<'a> IntoNeighbors for &'a ControlFlowView<'a> {
        type Neighbors = smallvec::IntoIter<[NodeId; 2]>;

        fn neighbors(self, a: NodeId) -> Self::Neighbors {
            (*self).succs(a).into_iter()
        }
    }

    impl petgraph::visit::Visitable for ControlFlowView<'_> {
        type Map = rustc_hash::FxHashSet<NodeId>;

        fn visit_map(&self) -> Self::Map {
            Self::Map::default()
        }

        fn reset_map(&self, map: &mut Self::Map) {
            map.clear();
        }
    }

    impl GraphBase for ControlSplitView<'_> {
        type NodeId = CtrlKey;
        type EdgeId = (CtrlKey, CtrlKey);
    }

    impl<'a> IntoNeighbors for &'a ControlSplitView<'a> {
        type Neighbors = smallvec::IntoIter<[CtrlKey; 2]>;

        fn neighbors(self, a: CtrlKey) -> Self::Neighbors {
            (*self).succs(a).into_iter()
        }
    }

    impl petgraph::visit::Visitable for ControlSplitView<'_> {
        type Map = rustc_hash::FxHashSet<CtrlKey>;

        fn visit_map(&self) -> Self::Map {
            Self::Map::default()
        }

        fn reset_map(&self, map: &mut Self::Map) {
            map.clear();
        }
    }

    fn control_dominators(function: &Function) -> petgraph::algo::dominators::Dominators<NodeId> {
        petgraph::algo::dominators::simple_fast(&ControlFlowView(function), function.entry())
    }

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
            &ControlSplitView(function),
            CtrlKey::Node(function.entry()),
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
    fn diamond() -> crate::Result<Function> {
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
        let view = ControlFlowView(&f);
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

        let doms = simple_fast(&ControlFlowView(&f), entry);

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
    fn empty_true_arm() -> crate::Result<(Function, ValueId, NodeId, NodeId)> {
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
    fn diamond_loop_and_empty_arm() -> crate::Result<Function> {
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

            // Guards against both trees vacuously answering `false`.
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
            let view = ControlSplitView(&f);
            let plain = ControlFlowView(&f);

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
    fn irreducible_with_orphan() -> crate::Result<Function> {
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

    /// The interval trees answer every query exactly as petgraph's idom chain
    /// walk: over every pair of nodes, and over every pair of node and value
    /// keys of the split graph.
    fn check_against_chain_walk(f: &Function) {
        let chain = control_dominators(f);
        let tree = control_dominator_tree(f);
        let nodes: Vec<NodeId> = f.graph().all_node_ids().collect();
        for &a in &nodes {
            for &b in &nodes {
                assert_eq!(
                    tree.dominance_verdict(a, b),
                    dominance_verdict(&chain, a, b),
                    "node verdict({a:?}, {b:?})"
                );
                assert_eq!(
                    tree.dominates(a, b),
                    dominates(&chain, a, b),
                    "node dominates({a:?}, {b:?})"
                );
            }
        }

        let chain = control_edge_dominators(f);
        let tree = control_edge_dominator_tree(f);
        let keys: Vec<CtrlKey> = nodes
            .iter()
            .map(|&n| CtrlKey::Node(n))
            .chain(f.graph().all_value_ids().map(CtrlKey::Edge))
            .collect();
        for &a in &keys {
            for &b in &keys {
                assert_eq!(
                    tree.dominance_verdict(a, b),
                    dominance_verdict(&chain, a, b),
                    "split verdict({a:?}, {b:?})"
                );
                assert_eq!(
                    tree.dominates(a, b),
                    dominates(&chain, a, b),
                    "split dominates({a:?}, {b:?})"
                );
            }
        }
    }

    /// `if (c) goto err;` `n` times: every rung also edges into one shared
    /// error region.
    fn error_ladder(n: usize) -> crate::Result<Function> {
        let mut b = empty_builder()?;
        let rungs: Vec<_> = (0..n)
            .map(|_| b.create_region_all())
            .collect::<crate::Result<_>>()?;
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

    const CASES: u32 = if cfg!(debug_assertions) { 128 } else { 4096 };
    /// Regions and switch arms per draw: release draws deeper trees.
    const SHAPE: (usize, usize) = if cfg!(debug_assertions) {
        (10, 4)
    } else {
        (48, 12)
    };

    /// One tree against the deletion definition of dominance, including the
    /// exact preorder: siblings in depth-first discovery order.
    fn check_tree<K: EntityRef + Hash + std::fmt::Debug>(
        succs: &[Vec<u32>],
        root: u32,
        tree: &DominatorTree<K>,
        key: fn(usize) -> K,
    ) {
        let dom = graph_oracle::dom_matrix(succs, root);
        let reach = &dom[root as usize];
        let (discovery, _) = graph_oracle::dfs_tree(succs, root);
        let mut at = vec![usize::MAX; succs.len()];
        for (i, &v) in discovery.iter().enumerate() {
            at[v as usize] = i;
        }
        let idom = graph_oracle::idoms(&dom, root);
        let want: Vec<K> = graph_oracle::tree_preorder(&idom, root, |v| at[v as usize])
            .into_iter()
            .map(|v| key(v as usize))
            .collect();
        assert_eq!(tree.preorder(), want.as_slice(), "preorder");
        for (a, row) in dom.iter().enumerate() {
            let ka = key(a);
            assert_eq!(tree.span(ka).is_some(), reach[a], "{ka:?} in the tree");
            for (b, &d) in row.iter().enumerate() {
                let kb = key(b);
                let verdict = (reach[a] && reach[b]).then_some(d);
                assert_eq!(
                    tree.dominance_verdict(ka, kb),
                    verdict,
                    "verdict({ka:?}, {kb:?})"
                );
                assert_eq!(
                    tree.dominates(ka, kb),
                    a == b || d,
                    "dominates({ka:?}, {kb:?})"
                );
                if let (Some((a_pre, a_post)), Some((b_pre, b_post))) =
                    (tree.span(ka), tree.span(kb))
                    && !d
                    && !dom[b][a]
                {
                    assert!(
                        a_post < b_pre || b_post < a_pre,
                        "spans of unrelated {ka:?} and {kb:?} overlap"
                    );
                }
            }
        }
    }

    /// Builds both trees with counting successor closures: each vertex in the
    /// tree is asked for its successors exactly once, any other never.
    fn assert_each_vertex_asked_once(f: &Function) {
        let view = ControlFlowView(f);
        let mut asked: SecondaryMap<NodeId, u32> = SecondaryMap::new();
        let tree = DominatorTree::compute(f.entry(), |n| {
            asked[n] += 1;
            view.succs(n)
        });
        for n in f.graph().all_node_ids() {
            assert_eq!(asked[n], u32::from(tree.contains(n)), "{n:?} asked");
        }
        let view = ControlSplitView(f);
        let mut asked: SecondaryMap<CtrlKey, u32> = SecondaryMap::new();
        let tree = DominatorTree::compute(CtrlKey::Node(f.entry()), |k| {
            asked[k] += 1;
            view.succs(k)
        });
        let g = f.graph();
        for k in g
            .all_node_ids()
            .map(CtrlKey::Node)
            .chain(g.all_value_ids().map(CtrlKey::Edge))
        {
            assert_eq!(asked[k], u32::from(tree.contains(k)), "{k:?} asked");
        }
    }

    /// Both trees of `f`, and the successor calls that built them.
    fn check_trees(f: &Function) {
        assert_each_vertex_asked_once(f);
        let root = u32::try_from(f.entry().index()).unwrap();
        check_tree(
            &test_cfgs::control_succs(f),
            root,
            &control_dominator_tree(f),
            NodeId::new,
        );
        let root = u32::try_from(CtrlKey::Node(f.entry()).index()).unwrap();
        check_tree(
            &test_cfgs::split_succs(f),
            root,
            &control_edge_dominator_tree(f),
            CtrlKey::new,
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(CASES))]

        #[test]
        fn dominator_trees_match_the_definition(
            terms in test_cfgs::arb_terms(SHAPE.0, SHAPE.1),
        ) {
            if let Some(b) = test_cfgs::lower(&terms) {
                check_trees(b.function());
            }
        }

        #[test]
        fn dominator_trees_match_the_chain_walk(terms in test_cfgs::arb_terms(10, 4)) {
            if let Some(b) = test_cfgs::lower(&terms) {
                check_against_chain_walk(b.function());
            }
        }
    }

    #[test]
    fn dominator_trees_of_the_fixtures_match_the_definition() {
        for f in [
            diamond().expect("diamond builds"),
            empty_true_arm().expect("empty_true_arm builds").0,
            diamond_loop_and_empty_arm().expect("combined fixture builds"),
            irreducible_with_orphan().expect("irreducible fixture builds"),
        ] {
            check_trees(&f);
            check_against_chain_walk(&f);
        }
    }

    /// A 100,000-region chain, a 50,000-arm join and a 50,000-rung error
    /// ladder: no recursion, one successor call per vertex, and the answers a
    /// path and a fan-in must give.
    #[test]
    #[cfg_attr(debug_assertions, ignore = "release-only scale test")]
    fn dominator_trees_scale_to_a_chain_a_join_and_a_ladder() {
        let f = test_cfgs::chain(100_000)
            .and_then(FunctionBuilder::build)
            .expect("chain builds");
        assert_each_vertex_asked_once(&f);
        let tree = control_dominator_tree(&f);
        let regions: Vec<NodeId> = tree
            .preorder()
            .iter()
            .copied()
            .filter(|&n| matches!(f.node_kind(n), NodeKind::Region))
            .collect();
        assert_eq!(regions.len(), 100_000);
        for pair in regions.windows(2) {
            assert!(tree.dominates(pair[0], pair[1]) && !tree.dominates(pair[1], pair[0]));
        }
        let split = control_edge_dominator_tree(&f);
        assert!(split.dominates(CtrlKey::Node(regions[0]), CtrlKey::Node(regions[99_999])));

        let f = test_cfgs::wide_join(50_000).expect("join builds");
        assert_each_vertex_asked_once(&f);
        let tree = control_dominator_tree(&f);
        let join = f
            .graph()
            .all_node_ids()
            .find(|&n| {
                matches!(f.node_kind(n), NodeKind::Region)
                    && f.graph().node_inputs(n).into_iter().count() == 50_000
            })
            .expect("the join");
        let regions: Vec<NodeId> = tree
            .preorder()
            .iter()
            .copied()
            .filter(|&n| matches!(f.node_kind(n), NodeKind::Region))
            .collect();
        assert_eq!(regions.len(), 50_002);
        assert!(tree.dominates(regions[0], join));
        for &arm in regions.iter().skip(1).filter(|&&n| n != join) {
            assert!(!tree.dominates(arm, join));
        }

        let f = error_ladder(50_000).expect("ladder builds");
        assert_each_vertex_asked_once(&f);
        let tree = control_dominator_tree(&f);
        let err = f
            .graph()
            .all_node_ids()
            .find(|&n| {
                matches!(f.node_kind(n), NodeKind::Region)
                    && f.graph().node_inputs(n).into_iter().count() == 50_000
            })
            .expect("the error region");
        let regions: Vec<NodeId> = tree
            .preorder()
            .iter()
            .copied()
            .filter(|&n| matches!(f.node_kind(n), NodeKind::Region))
            .collect();
        assert_eq!(regions.len(), 50_002);
        assert!(tree.dominates(regions[0], err));
        for &rung in regions.iter().skip(1).filter(|&&n| n != err) {
            assert!(!tree.dominates(rung, err));
        }
    }
}
