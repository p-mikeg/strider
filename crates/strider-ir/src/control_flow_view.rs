//! A petgraph view over the IR's CONTROL subgraph: control nodes
//! (Entry/Region/If/Call/CallOther/Return/IndirectBranch) connected by forward
//! control edges only (no data, no Phi back-edges), so
//! `petgraph::algo::dominators::simple_fast` can compute dominators directly.

use petgraph::visit::{
    GraphBase, IntoNeighbors, IntoNodeIdentifiers, NodeCount, Visitable,
};
use rustc_hash::FxHashSet;

use crate::function::Function;
use crate::node::{NodeId, NodeKind};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Returns `true` if this node kind participates in the control subgraph.
///
/// Exhaustive pattern covers every `NodeKind` variant so a future addition
/// is a compile error here, forcing an explicit decision.
#[inline]
fn is_control_node(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Entry
            | NodeKind::Region
            | NodeKind::If
            | NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::Return
            | NodeKind::IndirectBranch
    )
}

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
pub struct ControlFlowView<'a> {
    function: &'a Function,
}

impl<'a> ControlFlowView<'a> {
    /// Creates a view over `function`'s control subgraph.
    pub fn new(function: &'a Function) -> Self {
        Self { function }
    }

    /// Returns the forward control successors of `node`: every consumer of
    /// each `Control`-typed output of `node`.
    fn control_successors(&self, node: NodeId) -> Vec<NodeId> {
        let g = self.function.graph();
        let mut out = Vec::new();
        for &val in g.node_outputs(node) {
            if g.value_kind(val).is_control() {
                for (consumer, _slot) in g.value_uses(val) {
                    out.push(consumer);
                }
            }
        }
        out
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
        self.control_successors(a).into_iter()
    }
}

impl<'a> IntoNodeIdentifiers for &'a ControlFlowView<'a> {
    type NodeIdentifiers = std::vec::IntoIter<NodeId>;

    fn node_identifiers(self) -> Self::NodeIdentifiers {
        let g = self.function.graph();
        let ids: Vec<NodeId> = g
            .all_node_ids()
            .filter(|&n| is_control_node(g.node_kind(n)))
            .collect();
        ids.into_iter()
    }
}

impl NodeCount for &ControlFlowView<'_> {
    fn node_count(&self) -> usize {
        let g = self.function.graph();
        g.all_node_ids()
            .filter(|&n| is_control_node(g.node_kind(n)))
            .count()
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
///
/// # Panics
///
/// Panics if `function` has no entry node.
pub fn control_dominators(
    function: &Function,
) -> petgraph::algo::dominators::Dominators<NodeId> {
    let entry = function
        .entry()
        .expect("control_dominators: entry must be set");
    petgraph::algo::dominators::simple_fast(&ControlFlowView::new(function), entry)
}

/// Returns `true` if node `a` dominates node `b` in the control subgraph
/// (i.e. every path from the entry to `b` passes through `a`).
///
/// A node trivially dominates itself.
pub fn dominates(
    doms: &petgraph::algo::dominators::Dominators<NodeId>,
    a: NodeId,
    b: NodeId,
) -> bool {
    if a == b {
        return true;
    }
    doms.dominators(b)
        .is_some_and(|mut it| it.any(|d| d == a))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::IRBuilderExt;
    use crate::node::NodeKind;
    use crate::{FunctionBuilder, IRViewer};
    use cranelift_entity::EntityRef;
    use petgraph::visit::{IntoNeighbors, IntoNodeIdentifiers};

    /// Build a minimal `FunctionBuilder` with no tracked variables and
    /// Little-endian, using the default calling convention.
    fn empty_builder() -> crate::error::Result<FunctionBuilder> {
        FunctionBuilder::new(
            vec![],
            &strider_target::BuiltCallingConvention::default(),
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
        let region_a = b.create_region()?;
        let region_b = b.create_region()?;
        let region_c = b.create_region()?;
        let region_d = b.create_region()?;

        // Wire entry → region A.
        b.set_entry_region(region_a)?;

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

    // ── control_view_lists_only_control_nodes ─────────────────────────────────

    #[test]
    fn control_view_lists_only_control_nodes() {
        let f = diamond().expect("diamond() should build without errors");
        let view = ControlFlowView::new(&f);
        for n in view.node_identifiers() {
            assert!(
                matches!(
                    f.node_kind(n),
                    NodeKind::Entry
                        | NodeKind::Region
                        | NodeKind::If
                        | NodeKind::Return
                        | NodeKind::Call
                        | NodeKind::CallOther { .. }
                        | NodeKind::IndirectBranch
                ),
                "view node {n:?} is not a control node: {:?}",
                f.node_kind(n)
            );
        }
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
        let entry = f.entry().expect("entry must be set after build");

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
}
