use petgraph::visit::{GraphBase, IntoNeighbors, Visitable};
use rustc_hash::FxHashSet;

use crate::function::Function;
use crate::node::{NodeId, ValueId};

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
    type Neighbors = std::vec::IntoIter<NodeId>;

    fn neighbors(self, a: NodeId) -> Self::Neighbors {
        crate::walk::cfg_succs(self.function.graph(), a)
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl Visitable for ControlFlowView<'_> {
    type Map = FxHashSet<NodeId>;

    fn visit_map(&self) -> Self::Map {
        FxHashSet::default()
    }

    fn reset_map(&self, map: &mut Self::Map) {
        map.clear();
    }
}

/// Cooper-Harvey-Kennedy dominators of the control subgraph.
pub fn control_dominators(function: &Function) -> petgraph::algo::dominators::Dominators<NodeId> {
    let entry = function.entry();
    petgraph::algo::dominators::simple_fast(&ControlFlowView::new(function), entry)
}

/// True when every path from `doms`'s entry to `b` passes through `a`; a node
/// trivially dominates itself.
pub fn dominates<N: Copy + Eq + std::hash::Hash>(
    doms: &petgraph::algo::dominators::Dominators<N>,
    a: N,
    b: N,
) -> bool {
    if a == b {
        return true;
    }
    doms.dominators(b).is_some_and(|mut it| it.any(|d| d == a))
}

/// [`dominates`], three-valued: `None` when either vertex is absent from
/// `doms`, so a caller negating the answer does not turn "cannot say" into
/// "yes". Kinds with no control edge (`Load`, `Store`, arithmetic) are never in
/// the tree.
pub fn dominance_verdict<N: Copy + Eq + std::hash::Hash>(
    doms: &petgraph::algo::dominators::Dominators<N>,
    a: N,
    b: N,
) -> Option<bool> {
    if doms.dominators(a).is_none() || doms.dominators(b).is_none() {
        return None;
    }
    Some(dominates(doms, a, b))
}

/// A vertex of the edge-split control graph. Dominance over `Edge(v)` is edge
/// dominance over `v` in the ordinary CFG.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CtrlKey {
    Node(NodeId),
    /// A `Control`-kind output value.
    Edge(ValueId),
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
    type Neighbors = std::vec::IntoIter<CtrlKey>;

    fn neighbors(self, a: CtrlKey) -> Self::Neighbors {
        let graph = self.function.graph();
        match a {
            // Stage 1, stopping at the output.
            CtrlKey::Node(node) => crate::walk::cfg_outputs(graph, node)
                .map(CtrlKey::Edge)
                .collect::<Vec<_>>()
                .into_iter(),
            // Stage 2, resuming from the output.
            CtrlKey::Edge(value) => {
                let succs: Vec<CtrlKey> = graph
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

/// Dominators of the edge-split control graph. Querying with
/// [`CtrlKey::Node`] keys answers node dominance identically to
/// [`control_dominators`].
pub fn control_edge_dominators(
    function: &Function,
) -> petgraph::algo::dominators::Dominators<CtrlKey> {
    // The entry key must be `CtrlKey::Node(function.entry())`; a mismatch
    // yields an empty tree that silently answers `false` to every query.
    petgraph::algo::dominators::simple_fast(
        &ControlSplitView::new(function),
        CtrlKey::Node(function.entry()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::IRBuilderExt;
    use crate::node::NodeKind;
    use crate::{FunctionBuilder, IRViewer};
    use cranelift_entity::EntityRef;
    use petgraph::visit::IntoNeighbors;

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
}
