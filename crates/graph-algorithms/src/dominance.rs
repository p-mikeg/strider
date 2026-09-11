use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};

use rustc_hash::{FxHashMap, FxHashSet};

/// `Frontiers[x]` is the dominance frontier of `x`. An absent key means an
/// empty frontier.
pub type Frontiers<N> = FxHashMap<N, Vec<N>>;

/// The immediate-dominator relation as an INPUT: this crate consumes it and
/// never computes it. `strider-lift` supplies petgraph's `simple_fast`.
pub trait DomTree {
    type Node: Copy + Eq + Hash;

    /// Each node exactly once. Order is unspecified; determinism is the
    /// implementor's to provide.
    fn nodes(&self) -> impl Iterator<Item = Self::Node> + '_;

    fn predecessors(&self, n: Self::Node) -> impl Iterator<Item = Self::Node> + '_;

    /// `None` for the root and for any node unreachable from it.
    fn immediate_dominator(&self, n: Self::Node) -> Option<Self::Node>;
}

/// Cytron dominance frontiers: `DF(x)` is the set of nodes `b` where `x`
/// dominates a predecessor of `b` but does not strictly dominate `b`.
///
/// `root` is required because [`DomTree::immediate_dominator`] returns `None`
/// for both the root and unreachable nodes. The root's stop condition is the
/// virtual node ABOVE it, so a climb from one of its predecessors passes
/// through the root itself: a root with a predecessor REACHABLE from it is in
/// its own frontier. An unreachable predecessor shares the `None` encoding, so
/// its climb records the one pair `(p, b)` and then dies.
#[must_use]
pub fn dominance_frontiers<G: DomTree>(g: &G, root: G::Node) -> Frontiers<G::Node> {
    let mut frontiers: Frontiers<G::Node> = FxHashMap::default();
    // Membership runs beside the `Vec`s, which keep discovery order: a wide
    // join fans one frontier out to every join, so scanning one for the
    // duplicate check is quadratic in its own length.
    let mut recorded: FxHashSet<(G::Node, G::Node)> = FxHashSet::default();
    // Reused across nodes, and drained by `remove` rather than `clear`: the set
    // never shrinks its allocation, so a `clear` after one long climb costs a
    // memset over that capacity for every later node, which is quadratic in the
    // node count on a graph with one deep chain.
    let mut climbed: Vec<(G::Node, G::Node)> = Vec::new();
    for b in g.nodes() {
        // Every pair recorded below carries this `b`, so an earlier `b`'s
        // pairs are never probed again; dropping them caps the set at the
        // pairs of one node's climb.
        for pair in climbed.drain(..) {
            recorded.remove(&pair);
        }
        let idom_b = g.immediate_dominator(b);
        if idom_b.is_none() && b != root {
            // Unreachable from the root: contributes nothing.
            continue;
        }
        for p in g.predecessors(b) {
            let mut runner = p;
            while Some(runner) != idom_b {
                // Already recorded means an earlier predecessor climbed from
                // here up the same idom chain and stopped where this climb
                // would: at `idom_b`, or at a node with no idom. Either way
                // every pair above is in too. Re-walking a chain of joins is
                // quadratic.
                if !recorded.insert((runner, b)) {
                    break;
                }
                climbed.push((runner, b));
                frontiers.entry(runner).or_default().push(b);
                match g.immediate_dominator(runner) {
                    Some(next) => runner = next,
                    // No idom: `runner` is the root (nothing above it), or is
                    // unreachable and carries no live definition to reconcile.
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
    debug_assert!(
        g.immediate_dominator(root).is_none(),
        "root must have no immediate dominator: a node inside the tree walks \
         its own subtree and silently drops everything outside it"
    );
    let mut children: FxHashMap<G::Node, Vec<G::Node>> = FxHashMap::default();
    for n in g.nodes() {
        if let Some(idom) = g.immediate_dominator(n) {
            children.entry(idom).or_default().push(n);
        }
    }
    let mut preorder = Vec::new();
    // A well-formed idom relation pushes every node once, so this only bites
    // on a malformed one: a repeat in `nodes()`, or an idom cycle, which
    // without the dedup grows `preorder` and `stack` without bound.
    let mut seen: FxHashSet<G::Node> = FxHashSet::default();
    seen.insert(root);
    let mut stack = vec![root];
    while let Some(r) = stack.pop() {
        preorder.push(r);
        if let Some(ch) = children.get(&r) {
            // Reverse so children come off the stack in `nodes()` order.
            stack.extend(ch.iter().rev().copied().filter(|c| seen.insert(*c)));
        }
    }
    preorder
}

/// Maps each SSA variable to the nodes that define it. Blanket-implemented for
/// `HashMap<Var, C>` over any iterable `C`, so a caller passes its native
/// def-site map straight in.
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
/// a phi for `V` lands at node `R` iff `R` ∈ `IDF(def-sites(V))`.
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
        // `placed` queues each node at most once, which is what terminates
        // the iteration on a cyclic graph.
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
        let fr = dominance_frontiers(&g, 0);
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
        let fr = dominance_frontiers(&g, 0);
        assert_eq!(df_sorted(&fr, 2), vec![1]);
        assert_eq!(df_sorted(&fr, 1), vec![1]);

        // DF(2)={1} -> phi at head; head is then a fresh def, but DF(1)={1} is
        // already placed -> fixed point at one phi.
        let place = phi_placement(&fr, &defs(&[('v', &[2])]));
        assert!(place[&1].contains(&'v'));
        assert_eq!(place.len(), 1);
    }

    /// A self-loop on the root: `DF(root) = {root}` by the definition, since the
    /// root dominates its own predecessor and does not STRICTLY dominate itself.
    /// Strider's CFG keeps the entry's predecessors: a branch back to the
    /// entry address is a self-edge on the entry region.
    #[test]
    fn self_loop_at_root_puts_root_in_its_own_frontier() {
        let g = mock(&[0], &[(0, &[0])], &[]);
        let fr = dominance_frontiers(&g, 0);
        assert_eq!(df_sorted(&fr, 0), vec![0]);

        // A variable defined in the loop body needs its phi AT the root.
        let place = phi_placement(&fr, &defs(&[('v', &[0])]));
        assert!(place[&0].contains(&'v'));
    }

    /// The root as a loop header reached from deeper in the loop: every node on
    /// the back-edge path carries the root in its frontier.
    #[test]
    fn back_edge_to_root_frontiers() {
        let g = mock(
            &[0, 1, 2],
            &[(1, &[0]), (2, &[1]), (0, &[2])],
            &[(1, 0), (2, 1)],
        );
        let fr = dominance_frontiers(&g, 0);
        assert_eq!(df_sorted(&fr, 0), vec![0]);
        assert_eq!(df_sorted(&fr, 1), vec![0]);
        assert_eq!(df_sorted(&fr, 2), vec![0]);
    }

    /// The root and an unreachable node share the `immediate_dominator == None`
    /// encoding, so the root's frontier must not leak onto dead nodes: a
    /// predecessor-free root still has an empty frontier, and the climb from a
    /// dead predecessor still terminates.
    #[test]
    fn unreachable_node_is_not_treated_as_the_root() {
        // 9 is unreachable and edges into the join 3.
        let g = mock(
            &[0, 1, 2, 3, 9],
            &[(1, &[0]), (2, &[0]), (3, &[1, 2, 9])],
            &[(1, 0), (2, 0), (3, 0)],
        );
        let fr = dominance_frontiers(&g, 0);
        assert!(
            df_sorted(&fr, 0).is_empty(),
            "a predecessor-free root has an empty frontier"
        );
        // The dead node reaches the join, so it carries it; what matters is that
        // the climb stops rather than looping or ascending into the root's.
        assert_eq!(df_sorted(&fr, 9), vec![3]);
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

    /// Node whose `PartialEq` counts calls, so the frontier build's work is
    /// measurable without timing.
    #[derive(Clone, Copy, Debug)]
    struct Counted(u32);

    thread_local! {
        static EQ_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    impl PartialEq for Counted {
        fn eq(&self, other: &Self) -> bool {
            EQ_CALLS.with(|c| c.set(c.get() + 1));
            self.0 == other.0
        }
    }
    impl Eq for Counted {}
    impl Hash for Counted {
        fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
            self.0.hash(h);
        }
    }

    struct WideJoin {
        joins: u32,
    }

    /// `0 -> {1, 2} ->` each of `joins` joins: both arms carry every join in
    /// their frontier, so each arm's frontier grows to `joins` entries.
    impl DomTree for WideJoin {
        type Node = Counted;
        fn nodes(&self) -> impl Iterator<Item = Counted> + '_ {
            (0..3 + self.joins).map(Counted)
        }
        fn predecessors(&self, n: Counted) -> impl Iterator<Item = Counted> + '_ {
            let preds: &'static [u32] = match n.0 {
                0 => &[],
                1 | 2 => &[0],
                _ => &[1, 2],
            };
            preds.iter().copied().map(Counted)
        }
        fn immediate_dominator(&self, n: Counted) -> Option<Counted> {
            (n.0 != 0).then_some(Counted(0))
        }
    }

    fn frontier_eq_calls(joins: u32) -> u64 {
        EQ_CALLS.with(|c| c.set(0));
        let fr = dominance_frontiers(&WideJoin { joins }, Counted(0));
        assert_eq!(fr[&Counted(1)].len(), joins as usize);
        EQ_CALLS.with(std::cell::Cell::get)
    }

    /// The duplicate check must be a set probe, not a linear scan of the
    /// frontier built so far. Counting comparisons rather than timing keeps
    /// this deterministic.
    #[test]
    fn wide_join_frontier_build_is_not_quadratic() {
        let small = frontier_eq_calls(400);
        let large = frontier_eq_calls(3200);
        assert!(
            large < small * 16,
            "8x the joins cost {large} comparisons vs {small}: \
             a linear duplicate scan, not an O(1) membership check"
        );
    }

    thread_local! {
        static IDOM_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// Chain `0 -> 1 -> .. -> n-1` where every node also edges into the join
    /// `n`: the `if (a) goto err; if (b) goto err;` ladder. `idom(n) = 0`, so
    /// each of the n predecessors climbs the whole chain back to 0, while the
    /// frontier output is only n entries.
    struct ErrorLadder {
        n: u32,
    }

    impl DomTree for ErrorLadder {
        type Node = u32;
        fn nodes(&self) -> impl Iterator<Item = u32> + '_ {
            0..=self.n
        }
        fn predecessors(&self, b: u32) -> impl Iterator<Item = u32> + '_ {
            let preds: Vec<u32> = if b == self.n {
                (0..self.n).collect()
            } else if b == 0 {
                vec![]
            } else {
                vec![b - 1]
            };
            preds.into_iter()
        }
        fn immediate_dominator(&self, n: u32) -> Option<u32> {
            IDOM_CALLS.with(|c| c.set(c.get() + 1));
            if n == 0 {
                None
            } else if n == self.n {
                Some(0)
            } else {
                Some(n - 1)
            }
        }
    }

    fn ladder_idom_calls(n: u32) -> u64 {
        IDOM_CALLS.with(|c| c.set(0));
        let fr = dominance_frontiers(&ErrorLadder { n }, 0);
        let entries: usize = fr.values().map(Vec::len).sum();
        assert_eq!(entries, n as usize - 1, "output is linear in n");
        IDOM_CALLS.with(std::cell::Cell::get)
    }

    /// The climb must stop as soon as it reaches a pair an earlier predecessor
    /// already recorded: that predecessor walked the identical idom chain to
    /// the identical stop, so everything above is already in. Without the cut,
    /// shared chain segments are re-walked once per predecessor.
    #[test]
    fn error_ladder_frontier_climb_is_not_quadratic() {
        let small = ladder_idom_calls(200);
        let large = ladder_idom_calls(800);
        assert!(
            large < small * 8,
            "4x the nodes cost {large} idom calls vs {small} against linear output: \
             the climb re-walks shared chain segments"
        );
    }

    /// The frontier `Vec` keeps discovery order; the membership set must not
    /// reorder or drop entries.
    #[test]
    fn frontier_preserves_discovery_order() {
        let g = mock(
            &[0, 1, 2, 3, 4],
            &[(1, &[0]), (2, &[0]), (3, &[1, 2]), (4, &[1, 2])],
            &[(1, 0), (2, 0), (3, 0), (4, 0)],
        );
        let fr = dominance_frontiers(&g, 0);
        assert_eq!(fr[&1], vec![3, 4]);
        assert_eq!(fr[&2], vec![3, 4]);
    }

    /// [`DomTree::nodes`] must yield each node once: the climb's dedup is
    /// per-`b`, so a repeated `b` is walked again and its frontier entries
    /// appear twice.
    #[test]
    fn duplicate_nodes_duplicate_frontier_entries() {
        let g = mock(
            &[0, 1, 2, 3, 3],
            &[(1, &[0]), (2, &[0]), (3, &[1, 2])],
            &[(1, 0), (2, 0), (3, 0)],
        );
        let fr = dominance_frontiers(&g, 0);
        assert_eq!(fr[&1], vec![3, 3]);
        assert_eq!(fr[&2], vec![3, 3]);
    }

    #[test]
    fn multiple_variables_share_a_join() {
        let g = mock(
            &[0, 1, 2, 3],
            &[(1, &[0]), (2, &[0]), (3, &[1, 2])],
            &[(1, 0), (2, 0), (3, 0)],
        );
        let fr = dominance_frontiers(&g, 0);
        let place = phi_placement(&fr, &defs(&[('a', &[1]), ('b', &[2])]));
        assert_eq!(place[&3].len(), 2);
        assert!(place[&3].contains(&'a') && place[&3].contains(&'b'));
        assert_eq!(place.len(), 1, "both φs land only at the join");
    }
}
