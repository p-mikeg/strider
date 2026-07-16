//! Dominance frontiers, dominator-tree preorder, and iterated-dominance-frontier
//! (SSA phi placement) — the graph-theory half of Cytron et al.'s pruned-SSA
//! construction, extracted from any concrete CFG.
//!
//! The immediate-dominator relation is an INPUT here ([`DomTree::immediate_dominator`]):
//! this module never computes dominators itself (a caller supplies them —
//! petgraph's `simple_fast`, a hand-authored test tree, …).  Given idoms +
//! predecessors it derives:
//!
//! * [`dominance_frontiers`] — for every node, the set where its dominance stops
//!   (the classic Cytron `DF`).
//! * [`dominator_tree_preorder`] — a dom-tree preorder (every node after its
//!   idom), the visitation order an SSA renaming walk needs.
//! * [`phi_placement`] — the iterated dominance frontier: given each variable's
//!   definition nodes, the set of variables that need a φ at each node.
//!
//! Everything is generic over an opaque `Node: Copy + Eq + Hash`, so it is
//! unit-testable on tiny hand-built graphs with no CFG/IR types in scope (see
//! this module's tests).  A concrete graph implements [`DomTree`] to plug in.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};

use rustc_hash::{FxHashMap, FxHashSet};

/// Per-node dominance-frontier sets: `Frontiers[x]` is the dominance frontier of
/// node `x` — the nodes where a definition in `x` first stops dominating.  The
/// output of [`dominance_frontiers`] and the `frontiers` input to
/// [`phi_placement`]; an absent key means an empty frontier.
pub type Frontiers<N> = FxHashMap<N, Vec<N>>;

/// A graph whose immediate-dominator relation is already known — the minimal
/// surface [`dominance_frontiers`] / [`dominator_tree_preorder`] need.
///
/// Implementors expose the node set, the predecessor relation, and the
/// precomputed immediate dominator of each node.
pub trait DomTree {
    /// Opaque node identifier.
    type Node: Copy + Eq + Hash;

    /// Every node of the graph (order unspecified; determinism, if wanted, is
    /// the implementor's to provide).
    fn nodes(&self) -> impl Iterator<Item = Self::Node> + '_;

    /// The direct predecessors of `n` (control-flow: the nodes with an edge
    /// INTO `n`).
    fn predecessors(&self, n: Self::Node) -> impl Iterator<Item = Self::Node> + '_;

    /// The immediate dominator of `n`, or `None` for the entry/root and for any
    /// node unreachable from it.
    fn immediate_dominator(&self, n: Self::Node) -> Option<Self::Node>;
}

/// Cytron dominance frontiers: `DF(x)` is the set of nodes `b` where `x`
/// dominates a predecessor of `b` but does not strictly dominate `b` — i.e.
/// where a definition in `x` first stops dominating and a φ may be needed.
///
/// For every node `b` with an immediate dominator, walk from each predecessor
/// `p` of `b` up the idom chain until reaching `idom(b)`, adding `b` to the
/// frontier of every node on the way.  A node absent from the returned map has
/// an empty frontier.
#[must_use]
pub fn dominance_frontiers<G: DomTree>(g: &G) -> Frontiers<G::Node> {
    let mut frontiers: Frontiers<G::Node> = FxHashMap::default();
    for b in g.nodes() {
        let Some(idom_b) = g.immediate_dominator(b) else {
            // Entry (no idom) or a node unreachable from it: contributes nothing.
            continue;
        };
        for p in g.predecessors(b) {
            let mut runner = p;
            while runner != idom_b {
                let df = frontiers.entry(runner).or_default();
                if !df.contains(&b) {
                    df.push(b);
                }
                match g.immediate_dominator(runner) {
                    Some(next) => runner = next,
                    // `runner` is unreachable from the root (an edge from a dead
                    // region into a live join) — stop; dead nodes carry no live
                    // definition to reconcile.
                    None => break,
                }
            }
        }
    }
    frontiers
}

/// A dominator-tree preorder starting at `root`: every node appears after its
/// immediate dominator.  Nodes unreachable from `root` (no idom, not the root)
/// are excluded.
///
/// The idom relation is inverted into a children map, then walked depth-first
/// from `root`.  Sibling order follows the reverse of [`DomTree::nodes`] order
/// (so an ascending-id `nodes()` yields ascending-id siblings); only the
/// after-idom invariant is load-bearing.
#[must_use]
pub fn dominator_tree_preorder<G: DomTree>(g: &G, root: G::Node) -> Vec<G::Node> {
    let mut children: FxHashMap<G::Node, Vec<G::Node>> = FxHashMap::default();
    for n in g.nodes() {
        if let Some(idom) = g.immediate_dominator(n) {
            children.entry(idom).or_default().push(n);
        }
    }
    let mut preorder = Vec::new();
    let mut stack = vec![root];
    while let Some(r) = stack.pop() {
        preorder.push(r);
        if let Some(ch) = children.get(&r) {
            // Reverse so children come off the stack in `nodes()` order.
            stack.extend(ch.iter().rev().copied());
        }
    }
    preorder
}

/// A mapping from each SSA variable to the graph nodes that DEFINE it — the
/// input to iterated-DF φ placement.
///
/// Blanket-implemented for the usual def-site containers — `HashMap<Var,
/// HashSet<Node>>` and `HashMap<Var, Vec<Node>>` under any hasher, so an
/// `FxHashMap<V, FxHashSet<N>>` satisfies it directly — so a caller passes its
/// native def-site map to [`phi_placement`] with no adapter.
pub trait DefSites {
    /// The SSA variable identifier.
    type Var: Copy + Eq + Hash;
    /// The graph node identifier (a CFG region, a basic block, …).
    type Node: Copy + Eq + Hash;
    /// Every variable that has at least one definition.
    fn vars(&self) -> impl Iterator<Item = Self::Var> + '_;
    /// The nodes that define `v` (empty if `v` is unknown).
    fn def_nodes(&self, v: Self::Var) -> impl Iterator<Item = Self::Node> + '_;
}

impl<V, N, C, H> DefSites for HashMap<V, C, H>
where
    V: Copy + Eq + Hash,
    N: Copy + Eq + Hash + 'static,
    H: BuildHasher,
    for<'a> &'a C: IntoIterator<Item = &'a N>,
{
    type Var = V;
    type Node = N;
    fn vars(&self) -> impl Iterator<Item = V> + '_ {
        self.keys().copied()
    }
    fn def_nodes(&self, v: V) -> impl Iterator<Item = N> + '_ {
        self.get(&v).into_iter().flatten().copied()
    }
}

/// Iterated dominance frontier / SSA φ placement (Cytron et al., Fig. 11).
///
/// A φ for variable `V` is placed at node `R` iff `R ∈ IDF(def-sites(V))`.
/// Standard worklist form: seed the worklist with `V`'s definition nodes; for
/// each node `X` popped, place a φ at every node in `DF(X)` not yet holding one;
/// a freshly-placed φ is itself a definition, so its node re-enters the worklist
/// (the "iterated" step) unless it was already an original def-site.
///
/// `frontiers` is the output of [`dominance_frontiers`]; `def_sites` maps each
/// variable to its defining nodes (see [`DefSites`]).  Returns, per node, the
/// set of variables needing a φ there.
#[must_use]
pub fn phi_placement<D: DefSites>(
    frontiers: &Frontiers<D::Node>,
    def_sites: &D,
) -> FxHashMap<D::Node, FxHashSet<D::Var>> {
    let mut placement: FxHashMap<D::Node, FxHashSet<D::Var>> = FxHashMap::default();
    for var in def_sites.vars() {
        let sites: FxHashSet<D::Node> = def_sites.def_nodes(var).collect();
        // Worklist seeded with the variable's definition nodes; `placed` tracks
        // where a φ already sits so each node is processed once.
        let mut worklist: Vec<D::Node> = sites.iter().copied().collect();
        let mut placed: FxHashSet<D::Node> = FxHashSet::default();
        while let Some(x) = worklist.pop() {
            let Some(df) = frontiers.get(&x) else {
                continue;
            };
            for &y in df {
                if placed.insert(y) {
                    placement.entry(y).or_default().insert(var);
                    // A newly-placed φ is a fresh definition of `V`, so its
                    // node's frontier must be explored too — unless it was
                    // already an original def-site (already seeded), which this
                    // guard avoids re-queuing.
                    if !sites.contains(&y) {
                        worklist.push(y);
                    }
                }
            }
        }
    }
    placement
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A hand-authored dominator tree over `u32` nodes: explicit predecessor
    /// lists + explicit idoms, so the DF/IDF/preorder logic is exercised with
    /// no graph-algorithm of its own in the loop.
    struct Mock {
        nodes: Vec<u32>,
        preds: FxHashMap<u32, Vec<u32>>,
        idom: FxHashMap<u32, u32>,
    }

    impl DomTree for Mock {
        type Node = u32;
        fn nodes(&self) -> impl Iterator<Item = u32> + '_ {
            self.nodes.iter().copied()
        }
        fn predecessors(&self, n: u32) -> impl Iterator<Item = u32> + '_ {
            self.preds.get(&n).into_iter().flatten().copied()
        }
        fn immediate_dominator(&self, n: u32) -> Option<u32> {
            self.idom.get(&n).copied()
        }
    }

    fn mock(nodes: &[u32], preds: &[(u32, &[u32])], idom: &[(u32, u32)]) -> Mock {
        Mock {
            nodes: nodes.to_vec(),
            preds: preds.iter().map(|(n, p)| (*n, p.to_vec())).collect(),
            idom: idom.iter().copied().collect(),
        }
    }

    fn df_sorted(fr: &Frontiers<u32>, n: u32) -> Vec<u32> {
        let mut v = fr.get(&n).cloned().unwrap_or_default();
        v.sort_unstable();
        v
    }

    /// Build a [`DefSites`] map (`var → defining nodes`) for the φ-placement tests.
    fn defs(pairs: &[(char, &[u32])]) -> FxHashMap<char, Vec<u32>> {
        pairs
            .iter()
            .map(|(v, sites)| (*v, sites.to_vec()))
            .collect()
    }

    /// Diamond: 0 → {1,2} → 3.  A def in either arm needs a φ at the join 3;
    /// the join's own frontier is empty.
    #[test]
    fn diamond_frontiers_and_placement() {
        let g = mock(
            &[0, 1, 2, 3],
            &[(1, &[0]), (2, &[0]), (3, &[1, 2])],
            &[(1, 0), (2, 0), (3, 0)],
        );
        let fr = dominance_frontiers(&g);
        assert_eq!(df_sorted(&fr, 1), vec![3]);
        assert_eq!(df_sorted(&fr, 2), vec![3]);
        assert!(df_sorted(&fr, 3).is_empty());
        assert!(df_sorted(&fr, 0).is_empty());

        // A variable defined only in arm 1 still needs a φ at the join.
        let place = phi_placement(&fr, &defs(&[('a', &[1])]));
        assert!(place[&3].contains(&'a'));
        assert_eq!(place.len(), 1, "φ only at the join");
    }

    /// Loop: 0 → 1(head) → 2(body) → 1 (back-edge), 1 → 3(exit).  A def in the
    /// body needs a φ at the header; the header is its own frontier (back-edge
    /// join), and the IDF fixed-point places exactly one φ there.
    #[test]
    fn loop_frontiers_and_iterated_placement() {
        let g = mock(
            &[0, 1, 2, 3],
            &[(1, &[0, 2]), (2, &[1]), (3, &[1])],
            &[(1, 0), (2, 1), (3, 1)],
        );
        let fr = dominance_frontiers(&g);
        assert_eq!(df_sorted(&fr, 2), vec![1]);
        assert_eq!(df_sorted(&fr, 1), vec![1]);

        // Def in body(2): DF(2)={1} → φ at head; head is a fresh def whose
        // DF(1)={1} is already placed → fixed point, one φ.
        let place = phi_placement(&fr, &defs(&[('v', &[2])]));
        assert!(place[&1].contains(&'v'));
        assert_eq!(place.len(), 1);
    }

    /// Preorder: root first, and every node strictly after its idom.
    #[test]
    fn preorder_respects_idom() {
        let g = mock(
            &[0, 1, 2, 3],
            &[(1, &[0]), (2, &[0]), (3, &[1, 2])],
            &[(1, 0), (2, 0), (3, 0)],
        );
        let order = dominator_tree_preorder(&g, 0);
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], 0);
        let pos: FxHashMap<u32, usize> = order.iter().enumerate().map(|(i, &n)| (n, i)).collect();
        for (&n, &idom) in &g.idom {
            assert!(pos[&idom] < pos[&n], "node {n} must follow its idom {idom}");
        }
    }

    /// Multiple variables place independently; a node can carry φs for several.
    #[test]
    fn multiple_variables_share_a_join() {
        let g = mock(
            &[0, 1, 2, 3],
            &[(1, &[0]), (2, &[0]), (3, &[1, 2])],
            &[(1, 0), (2, 0), (3, 0)],
        );
        let fr = dominance_frontiers(&g);
        let place = phi_placement(&fr, &defs(&[('a', &[1]), ('b', &[2])]));
        assert_eq!(place[&3].len(), 2);
        assert!(place[&3].contains(&'a') && place[&3].contains(&'b'));
        assert_eq!(place.len(), 1, "both φs land only at the join");
    }
}
