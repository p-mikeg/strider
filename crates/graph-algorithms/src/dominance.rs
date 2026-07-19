//! The graph-theory half of Cytron et al.'s SSA construction: dominance
//! frontiers, dominator-tree preorder, iterated-DF phi placement.
//!
//! The immediate-dominator relation is an INPUT ([`DomTree::immediate_dominator`]);
//! nothing here computes dominators. Callers supply them from petgraph's
//! `simple_fast`, a hand-authored tree, or wherever else.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};

use rustc_hash::{FxHashMap, FxHashSet};

/// `Frontiers[x]` is the dominance frontier of `x`. An absent key means an
/// empty frontier.
pub type Frontiers<N> = FxHashMap<N, Vec<N>>;

pub trait DomTree {
    type Node: Copy + Eq + Hash;

    /// Order is unspecified; determinism is the implementor's to provide.
    fn nodes(&self) -> impl Iterator<Item = Self::Node> + '_;

    fn predecessors(&self, n: Self::Node) -> impl Iterator<Item = Self::Node> + '_;

    /// `None` for the root and for any node unreachable from it.
    fn immediate_dominator(&self, n: Self::Node) -> Option<Self::Node>;
}

/// Cytron dominance frontiers: `DF(x)` is the set of nodes `b` where `x`
/// dominates a predecessor of `b` but does not strictly dominate `b`.
#[must_use]
pub fn dominance_frontiers<G: DomTree>(g: &G) -> Frontiers<G::Node> {
    let mut frontiers: Frontiers<G::Node> = FxHashMap::default();
    for b in g.nodes() {
        let Some(idom_b) = g.immediate_dominator(b) else {
            // Root, or unreachable from it: contributes nothing.
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
                    // `runner` is unreachable from the root: an edge from a
                    // dead region into a live join. Dead nodes carry no live
                    // definition to reconcile, so stop climbing.
                    None => break,
                }
            }
        }
    }
    frontiers
}

/// Dominator-tree preorder from `root`: every node appears after its immediate
/// dominator. Nodes unreachable from `root` are excluded.
///
/// Only the after-idom invariant is load-bearing. Sibling order happens to
/// follow [`DomTree::nodes`] order; don't depend on it.
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

/// Maps each SSA variable to the nodes that define it.
///
/// Blanket-implemented for `HashMap<Var, C>` under any hasher and any
/// iterable `C`, so callers pass their native def-site map to
/// [`phi_placement`] with no adapter.
pub trait DefSites {
    type Var: Copy + Eq + Hash;
    type Node: Copy + Eq + Hash;
    fn vars(&self) -> impl Iterator<Item = Self::Var> + '_;
    /// Empty if `v` is unknown.
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

/// Iterated dominance frontier / SSA phi placement (Cytron et al., Fig. 11):
/// a phi for `V` lands at node `R` iff `R ∈ IDF(def-sites(V))`.
///
/// Returns, per node, the set of variables needing a phi there.
#[must_use]
pub fn phi_placement<D: DefSites>(
    frontiers: &Frontiers<D::Node>,
    def_sites: &D,
) -> FxHashMap<D::Node, FxHashSet<D::Var>> {
    let mut placement: FxHashMap<D::Node, FxHashSet<D::Var>> = FxHashMap::default();
    for var in def_sites.vars() {
        let sites: FxHashSet<D::Node> = def_sites.def_nodes(var).collect();
        // `placed` bounds the worklist: each node is queued at most once, which
        // is what makes the iteration terminate on cyclic graphs.
        let mut worklist: Vec<D::Node> = sites.iter().copied().collect();
        let mut placed: FxHashSet<D::Node> = FxHashSet::default();
        while let Some(x) = worklist.pop() {
            let Some(df) = frontiers.get(&x) else {
                continue;
            };
            for &y in df {
                if placed.insert(y) {
                    placement.entry(y).or_default().insert(var);
                    // The placed phi is itself a definition of `V`, so explore
                    // its frontier too, unless it was an original def-site
                    // and is already seeded.
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

    /// Explicit preds + explicit idoms, so the DF/IDF/preorder logic is
    /// exercised with no other graph algorithm in the loop.
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

    fn defs(pairs: &[(char, &[u32])]) -> FxHashMap<char, Vec<u32>> {
        pairs
            .iter()
            .map(|(v, sites)| (*v, sites.to_vec()))
            .collect()
    }

    /// Diamond: 0 -> {1,2} -> 3.
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

        // Defined in one arm only, but still needs a phi at the join.
        let place = phi_placement(&fr, &defs(&[('a', &[1])]));
        assert!(place[&3].contains(&'a'));
        assert_eq!(place.len(), 1, "φ only at the join");
    }

    /// Loop: 0 -> 1(head) -> 2(body) -> 1 (back-edge), 1 -> 3(exit).  The header is
    /// its own frontier via the back-edge, so this pins the IDF fixed point.
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

        // DF(2)={1} -> phi at head; head is then a fresh def, but DF(1)={1} is
        // already placed -> fixed point at one phi.
        let place = phi_placement(&fr, &defs(&[('v', &[2])]));
        assert!(place[&1].contains(&'v'));
        assert_eq!(place.len(), 1);
    }

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
