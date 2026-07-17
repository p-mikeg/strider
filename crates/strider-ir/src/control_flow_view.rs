//! A petgraph view over the IR's CONTROL subgraph: control nodes
//! (Entry/Region/If/Call/CallOther/Return/IndirectBranch) connected by forward
//! control edges only (no data, no Phi back-edges), so
//! `petgraph::algo::dominators::simple_fast` can compute dominators directly.

use petgraph::visit::{GraphBase, IntoNeighbors, Visitable};
use rustc_hash::FxHashSet;

use crate::function::Function;
use crate::node::{NodeId, ValueId};

// ── ControlFlowView ───────────────────────────────────────────────────────────

/// A petgraph-compatible view over the IR's CONTROL subgraph.
///
/// Presents only control nodes (`Entry`, `Region`, `If`, `Call`, `CallOther`,
/// `Return`, `IndirectBranch`) connected by forward control edges (outputs
/// whose [`crate::node::ValueKind`] is `Control`).  Data edges, `PhiToken`
/// edges, and `Memory` edges are all invisible.
///
/// The view holds a shared borrow of the [`Function`] and implements the
/// petgraph visitor traits on `&ControlFlowView<'_>` (a `Copy` reference, as
/// petgraph's `GraphRef` requirement demands).
#[derive(Clone, Copy)]
pub(crate) struct ControlFlowView<'a> {
    function: &'a Function,
}

impl<'a> ControlFlowView<'a> {
    /// Creates a view over `function`'s control subgraph.
    pub(crate) fn new(function: &'a Function) -> Self {
        Self { function }
    }
}

// ── petgraph trait impls ──────────────────────────────────────────────────────

impl GraphBase for ControlFlowView<'_> {
    type NodeId = NodeId;
    type EdgeId = (NodeId, NodeId);
}

// petgraph requires `IntoNeighbors for &G` (a shared reference so it is Copy).
impl<'a> IntoNeighbors for &'a ControlFlowView<'a> {
    type Neighbors = std::vec::IntoIter<NodeId>;

    fn neighbors(self, a: NodeId) -> Self::Neighbors {
        // Forward control successors of `a`: every consumer of each
        // `Control`-typed output of `a`.
        crate::walk::cfg_succs(self.function.graph(), a)
            .collect::<Vec<_>>()
            .into_iter()
    }
}

/// The visit map for `ControlFlowView`: an `FxHashSet<NodeId>` (avoids the
/// dense-array overhead; `NodeId` is `Hash + Eq`).
impl Visitable for ControlFlowView<'_> {
    type Map = FxHashSet<NodeId>;

    fn visit_map(&self) -> Self::Map {
        FxHashSet::default()
    }

    fn reset_map(&self, map: &mut Self::Map) {
        map.clear();
    }
}

// ── public helpers ────────────────────────────────────────────────────────────

/// Computes Cooper–Harvey–Kennedy dominators of the control subgraph.
pub fn control_dominators(function: &Function) -> petgraph::algo::dominators::Dominators<NodeId> {
    let entry = function.entry();
    petgraph::algo::dominators::simple_fast(&ControlFlowView::new(function), entry)
}

/// Returns `true` if `a` dominates `b` in whichever graph `doms` was computed
/// over (i.e. every path from that graph's entry to `b` passes through `a`).
///
/// A node trivially dominates itself.
///
/// Generic over the dominator-tree's node type so the same relation serves both
/// [`control_dominators`] (keyed by [`NodeId`]) and [`control_edge_dominators`]
/// (keyed by [`CtrlKey`], where it additionally expresses EDGE dominance).
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

// ── ControlSplitView ──────────────────────────────────────────────────────────

/// A vertex of the EDGE-SPLIT control graph: either an IR control node or one
/// of its outgoing control edges, reified as a vertex of its own.
///
/// The split graph is the classic edge-splitting construction — but it needs no
/// synthetic vertices, because this IR's control edges are ALREADY first-class:
/// each control edge is a [`ValueId`] with exactly one consumer.  So dominance
/// over `Edge(v)` is exactly EDGE dominance over `v` in the ordinary CFG.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CtrlKey {
    /// An IR control node.
    Node(NodeId),
    /// A control edge (a `Control`-kind output value).
    Edge(ValueId),
}

/// A petgraph-compatible view over the IR's control subgraph with every control
/// edge SPLIT into a vertex.
///
/// This is [`ControlFlowView`]'s relation with
/// [`cfg_succs`](crate::walk::cfg_succs)'s two-stage composition unfolded:
/// stage 1 (node → its `Control` outputs) stops at [`CtrlKey::Edge`], and stage
/// 2 (output → its consumers) resumes from it.  Composing the two therefore
/// reproduces `cfg_succs` exactly — the property
/// `split_view_composes_to_cfg_succs` pins.
#[derive(Clone, Copy)]
pub(crate) struct ControlSplitView<'a> {
    function: &'a Function,
}

impl<'a> ControlSplitView<'a> {
    /// Creates a view over `function`'s edge-split control subgraph.
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
            // Stage 1 of `cfg_succs`, stopping at the output.
            CtrlKey::Node(node) => crate::walk::cfg_outputs(graph, node)
                .map(CtrlKey::Edge)
                .collect::<Vec<_>>()
                .into_iter(),
            // Stage 2 of `cfg_succs`, resuming from the output.  A control edge
            // has exactly one consumer in well-formed IR — that is what makes
            // this a true edge split rather than a hyper-edge.
            CtrlKey::Edge(value) => {
                let succs: Vec<CtrlKey> = graph
                    .value_uses(value)
                    .map(|(succ, _)| CtrlKey::Node(succ))
                    .collect();
                debug_assert_eq!(
                    succs.len(),
                    1,
                    "control edge {value:?} has {} consumers; every control edge \
                     has exactly one.  Zero is a dangling control path — the \
                     validator rejects it as `UnusedControlOutput`, since every \
                     control edge must reach a terminator (`Return` / \
                     `IndirectBranch` / `Unreachable`) — and it would make this \
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

/// The visit map for `ControlSplitView`: an `FxHashSet<CtrlKey>`, mirroring
/// [`ControlFlowView`]'s.
impl Visitable for ControlSplitView<'_> {
    type Map = FxHashSet<CtrlKey>;

    fn visit_map(&self) -> Self::Map {
        FxHashSet::default()
    }

    fn reset_map(&self, map: &mut Self::Map) {
        map.clear();
    }
}

/// Computes dominators of the EDGE-SPLIT control subgraph.
///
/// Costlier to build than [`control_dominators`] (the split graph has roughly
/// twice the vertices, hence longer dominator chains), so callers keep it lazy.
///
/// It SUBSUMES [`control_dominators`]: querying it with [`CtrlKey::Node`] keys
/// answers node dominance identically, because edge-splitting preserves paths
/// 1:1 (see `split_dominance_subsumes_node_dominance`).  A caller needing both
/// relations therefore builds only this one.
///
/// The entry key is `CtrlKey::Node(function.entry())`; a mismatch would yield an
/// empty dominator tree, silently making every query `false`.
pub fn control_edge_dominators(
    function: &Function,
) -> petgraph::algo::dominators::Dominators<CtrlKey> {
    petgraph::algo::dominators::simple_fast(
        &ControlSplitView::new(function),
        CtrlKey::Node(function.entry()),
    )
}

/// Returns `true` if the control edge `edge` dominates `node` — i.e. every path
/// from the entry to `node` traverses that EDGE.
///
/// This is the real relation `dominated_by_branch` wants.  It is strictly
/// stronger than `dominates(consumer(edge), node)`: dominating the edge's
/// TARGET only implies traversing the edge when the edge is the target's sole
/// way in.  For `if (c) {} else { X }`, the true edge runs straight into the
/// join, so the join dominates everything after it while the true edge does not.
///
/// `doms` must come from [`control_edge_dominators`].
pub fn edge_dominates(
    doms: &petgraph::algo::dominators::Dominators<CtrlKey>,
    edge: ValueId,
    node: NodeId,
) -> bool {
    dominates(doms, CtrlKey::Edge(edge), CtrlKey::Node(node))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::IRBuilderExt;
    use crate::node::NodeKind;
    use crate::{FunctionBuilder, IRViewer};
    use cranelift_entity::EntityRef;
    use petgraph::visit::IntoNeighbors;

    /// Build a minimal `FunctionBuilder` with no tracked variables and
    /// Little-endian, using the default calling convention.
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
    ///
    /// Returns the completed [`Function`].
    fn diamond() -> crate::error::Result<Function> {
        let mut b = empty_builder()?;

        // Create all four regions.
        let region_a = b.create_region_all()?;
        let region_b = b.create_region_all()?;
        let region_c = b.create_region_all()?;
        let region_d = b.create_region_all()?;

        // Wire entry → region A.
        b.set_entry_region_all(region_a)?;

        // In region A: emit a boolean constant as the branch condition,
        // then branch to B (true) and C (false).
        b.set_region(region_a);
        b.set_lift_addr(Some(0x1000));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, region_b, region_c)?;
        b.set_lift_addr(None);

        // Region B: unconditional branch to D.
        b.set_region(region_b);
        b.set_lift_addr(Some(0x1010));
        b.build_branch(region_d)?;
        b.set_lift_addr(None);

        // Region C: unconditional branch to D.
        b.set_region(region_c);
        b.set_lift_addr(Some(0x1020));
        b.build_branch(region_d)?;
        b.set_lift_addr(None);

        // Region D: return.
        b.set_region(region_d);
        b.set_lift_addr(Some(0x1030));
        b.build_function_return()?;
        b.set_lift_addr(None);

        b.build()
    }

    // ── control_view_neighbors_are_control_successors ─────────────────────────

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

    // ── simple_fast_join_idom_is_branch_region ────────────────────────────────

    #[test]
    fn simple_fast_join_idom_is_branch_region() {
        use petgraph::algo::dominators::simple_fast;

        let f = diamond().expect("diamond() should build without errors");
        let entry = f.entry();

        let doms = simple_fast(&ControlFlowView::new(&f), entry);

        // Identify the If node: it is the branching point with two control
        // successors.  In the diamond CFG:
        //   Entry → Region A → If → {Region B, Region C} → Region D → Return
        // The If node is the immediate dominator of the join Region D, because
        // every path from Entry to D must pass through If.
        let if_node = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::If))
            .expect("diamond must have an If node");

        // Find the join region D: the unique Region node with >1 incoming
        // control edges.  Each `build_branch(D)` wires one Control edge into D.
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

        // The immediate dominator of the join (Region D) is the If node —
        // every path Entry→D passes through If (not through Region A directly).
        let idom = doms
            .immediate_dominator(join_region_node)
            .expect("join region must have an immediate dominator");
        assert_eq!(
            idom, if_node,
            "join region's idom should be the If node (every path through \
             the diamond must pass through If before reaching the join), \
             got {idom:?}"
        );

        // Also verify the dominates() helper: If dominates the join, but not
        // vice versa.
        assert!(
            dominates(&doms, if_node, join_region_node),
            "If node should dominate the join region"
        );
        assert!(
            !dominates(&doms, join_region_node, if_node),
            "join region should NOT dominate the If node"
        );

        // The entry node dominates everything.
        assert!(
            dominates(&doms, entry, join_region_node),
            "entry should dominate join region"
        );
        assert!(
            dominates(&doms, entry, if_node),
            "entry should dominate If node"
        );
    }

    // ── ControlSplitView ──────────────────────────────────────────────────────

    /// Builds `if (c) {} else { X }` — an EMPTY true arm — then a join and a
    /// tail:
    ///
    /// ```text
    ///       Entry
    ///         |
    ///     Region A
    ///       If(cond)
    ///      /        \
    ///  (true edge)  Region E   (the else block: non-empty)
    ///      \        /
    ///       Region J  (join — the true edge's DIRECT target)
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

        // Region A: branch — TRUE goes straight to the join (empty arm),
        // FALSE goes to the else block.
        b.set_region(region_a);
        b.set_lift_addr(Some(0x2000));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, region_j, region_e)?;
        b.set_lift_addr(None);

        // Region E (the else block): branch to the join.
        b.set_region(region_e);
        b.set_lift_addr(Some(0x2010));
        b.build_branch(region_j)?;
        b.set_lift_addr(None);

        // Region J (join): branch to the tail.
        b.set_region(region_j);
        b.set_lift_addr(Some(0x2020));
        b.build_branch(region_t)?;
        b.set_lift_addr(None);

        // Region T (tail): return.
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
        // The If's first Control output is the TRUE edge.
        let [true_edge, _false_edge] = f.graph().node_outputs_exact::<2>(if_node).unwrap();

        // With an empty true arm the true edge's sole consumer IS the join; the
        // tail is the join's sole control successor.  (Derived from the graph
        // rather than the builder's `RegionId`s, which are not `NodeId`s.)
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

    /// Builds a fixture combining ALL THREE shapes that could break the
    /// node/split dominance correspondence, in one function:
    ///
    /// ```text
    ///       Entry
    ///         |
    ///     Region A
    ///       If(c)          ── the DIAMOND
    ///      /      \
    ///  Region B  Region C
    ///      \      /
    ///     Region D  (join)
    ///       If(c)          ── the EMPTY ARM: true runs straight into the join
    ///      /      \
    ///  (true)   Region E
    ///      \      /
    ///     Region G  (join — the true edge's DIRECT target)
    ///       If(c)          ── the GUARDED LOOP's guard
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

        // A: the diamond's branch.
        b.set_region(a);
        b.set_lift_addr(Some(0x3000));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, c_b, c_c)?;
        b.set_lift_addr(None);

        // B / C: the diamond's arms, both into D.
        for (region, addr) in [(c_b, 0x3010), (c_c, 0x3020)] {
            b.set_region(region);
            b.set_lift_addr(Some(addr));
            b.build_branch(d)?;
            b.set_lift_addr(None);
        }

        // D: the EMPTY-ARM branch — true goes straight to the join G.
        b.set_region(d);
        b.set_lift_addr(Some(0x3030));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, g, e)?;
        b.set_lift_addr(None);

        // E: the non-empty else arm.
        b.set_region(e);
        b.set_lift_addr(Some(0x3040));
        b.build_branch(g)?;
        b.set_lift_addr(None);

        // G: the loop GUARD — enter the loop, or skip straight to the exit.
        b.set_region(g);
        b.set_lift_addr(Some(0x3050));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, h, x)?;
        b.set_lift_addr(None);

        // H: the loop header — two preds (the guard's edge, and L's back edge).
        b.set_region(h);
        b.set_lift_addr(Some(0x3060));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, l, x)?;
        b.set_lift_addr(None);

        // L: the latch — back-edge to the header.
        b.set_region(l);
        b.set_lift_addr(Some(0x3070));
        b.build_branch(h)?;
        b.set_lift_addr(None);

        // X: the exit.
        b.set_region(x);
        b.set_lift_addr(Some(0x3080));
        b.build_function_return()?;
        b.set_lift_addr(None);

        b.build()
    }

    /// THE subsumption property, pinned directly: the edge-split dominator tree
    /// answers NODE dominance exactly as the node tree does, for EVERY ordered
    /// pair of control-reachable nodes.
    ///
    /// This is what lets [`ConstraintEval`](../../../strider_pattern) keep ONE
    /// tree.  Edge-splitting inserts a vertex on every edge, so paths correspond
    /// 1:1: `Entry→…→b` maps to `Entry→…→Node(b)` with `Edge(v)` vertices
    /// interleaved, and `Node(a)` lies on the split path IFF `a` lies on the
    /// original.  Dominance is a statement about ALL paths, so the two agree.
    ///
    /// If this ever diverges, the split tree is not a conservative extension of
    /// the node tree and everything built on it is suspect — not just the
    /// single-tree cleanup.
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

            // Guards against a vacuous pass where BOTH trees answered `false`
            // everywhere (e.g. an entry-key mismatch yielding an empty tree):
            // every node dominates itself, and the entry dominates every node.
            assert!(
                agreed_true >= 2 * nodes.len() - 1,
                "{name}: expected at least the reflexive pairs plus the entry's \
                 row to hold, got {agreed_true} true pairs over {} nodes",
                nodes.len()
            );
        }
    }

    /// THE load-bearing property: `Node(n) -> Edge(v) -> Node(c)` in the split
    /// view must compose to EXACTLY `cfg_succs(n)` for every reachable node.
    ///
    /// If the two views ever disagreed about the CFG, `Dominates` (which reads
    /// the node tree) and `DominatedByBranch` (which reads the split tree) would
    /// start answering from different graphs — silently.
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
                // Restrict to the control-reachable nodes: the split view and
                // `cfg_succs` are only ever consulted there.
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

                // …and to exactly what the un-split view reports, which is the
                // graph `Dominates` answers from.
                let plain_succs: std::collections::BTreeSet<usize> =
                    plain.neighbors(node).map(|n| n.index()).collect();
                assert_eq!(
                    composed, plain_succs,
                    "{name}: split view must agree with ControlFlowView for {node:?}"
                );
            }
        }
    }

    /// The bug: with an EMPTY true arm, the true edge runs straight into the
    /// join, so the join dominates the tail — but the tail is reachable through
    /// BOTH arms, so the true edge does NOT dominate it.
    ///
    /// The old node-dominance proxy `dominates(consumer(edge), node)` answers
    /// TRUE here (a silent false positive); `edge_dominates` answers FALSE.
    #[test]
    fn edge_dominates_is_false_past_a_join_with_an_empty_arm() {
        let (f, true_edge, region_j, region_t) = empty_true_arm().expect("empty_true_arm builds");

        let split = control_edge_dominators(&f);
        let node_doms = control_dominators(&f);

        // The proxy the old code used: the true edge's consumer IS the join.
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
            "the join DOES dominate the tail — which is exactly why the old \
             node-dominance proxy wrongly claimed the tail was in the true block"
        );

        // The real relation.
        assert!(
            !edge_dominates(&split, true_edge, region_t),
            "the tail is past the merge and reachable through BOTH arms, so the \
             true EDGE must not dominate it"
        );
        assert!(
            !edge_dominates(&split, true_edge, region_j),
            "the join is reachable through the false arm too, so the true edge \
             does not dominate it either"
        );
    }

    /// Regression: a node genuinely inside a non-empty arm IS edge-dominated by
    /// that arm's edge — the relation must not be vacuously false.
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
            edge_dominates(&split, true_edge, true_block),
            "the true block is in the true block"
        );
        assert!(
            !edge_dominates(&split, true_edge, false_block),
            "the false block is NOT in the true block"
        );
        assert!(
            edge_dominates(&split, false_edge, false_block),
            "the false block is in the false block"
        );

        // The join is reachable from both arms: neither edge dominates it.
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
            !edge_dominates(&split, true_edge, join),
            "the join is past the merge: no branch edge dominates it"
        );
        assert!(!edge_dominates(&split, false_edge, join));
    }

    /// An edge trivially dominates itself — the zero-length path.  This is what
    /// makes the DIRECT case of `phi_input_from_edge` work as plain edge-vs-edge
    /// dominance, with no `==` special case.
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
            "an edge dominates itself (zero-length path) — the direct case"
        );
        // THE TRAP: the edge does NOT dominate the If that PRODUCES it.  Testing
        // edge-against-producer(c_i) instead of edge-against-edge would break
        // exactly the direct case.
        assert!(
            !edge_dominates(&split, true_edge, if_node),
            "an edge cannot dominate its own producer — the If precedes it"
        );
    }
}
