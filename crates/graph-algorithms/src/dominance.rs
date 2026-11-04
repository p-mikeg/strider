use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};

use rustc_hash::{FxHashMap, FxHashSet};

/// `Frontiers[x]` is the dominance frontier of `x`; absent means empty.
pub type Frontiers<N> = FxHashMap<N, Vec<N>>;

/// Cytron dominance frontiers: `DF(x)` is the set of vertices `b` where `x`
/// dominates a predecessor of `b` but does not strictly dominate `b`.
/// `preds` yields each vertex's predecessors, unreachable ones included.
///
/// A root with a predecessor reachable from it is in its own frontier. A
/// climb from an unreachable predecessor records `(p, b)` and stops.
#[must_use]
pub fn dominance_frontiers<N, I>(
    doms: &Dominators<N>,
    mut preds: impl FnMut(N) -> I,
) -> Frontiers<N>
where
    N: Copy + Eq + Hash,
    I: IntoIterator<Item = N>,
{
    let mut frontiers: Frontiers<N> = FxHashMap::default();
    // Membership beside the discovery-ordered `Vec`s. Drained by `remove`,
    // not `clear`, which would memset its whole capacity per vertex.
    let mut recorded: FxHashSet<(N, N)> = FxHashSet::default();
    // The pairs `recorded` holds for the vertex being processed.
    let mut climbed: Vec<(N, N)> = Vec::new();
    for (at, &b) in doms.vertices.iter().enumerate() {
        // An earlier `b`'s pairs are never probed again.
        for pair in climbed.drain(..) {
            recorded.remove(&pair);
        }
        let idom_b = (at != 0).then(|| doms.vertices[doms.idom[at] as usize]);
        for p in preds(b) {
            let mut runner = p;
            while Some(runner) != idom_b {
                // An earlier predecessor climbed the same idom chain from
                // here to the same stop, so every pair above is in.
                if !recorded.insert((runner, b)) {
                    break;
                }
                climbed.push((runner, b));
                frontiers.entry(runner).or_default().push(b);
                match doms.immediate_dominator(runner) {
                    Some(next) => runner = next,
                    // The root, or unreachable.
                    None => break,
                }
            }
        }
    }
    frontiers
}

/// Iterated dominance frontier / SSA phi placement (Cytron et al., Fig. 11):
/// a phi for `V` lands at node `R` iff `R` ∈ `IDF(def-sites(V))`.
///
/// Returns, per node, the variables needing a phi there.
#[must_use]
pub fn phi_placement<Var, Node, C, H>(
    frontiers: &Frontiers<Node>,
    def_sites: &HashMap<Var, C, H>,
) -> FxHashMap<Node, FxHashSet<Var>>
where
    Var: Copy + Eq + Hash,
    Node: Copy + Eq + Hash,
    H: BuildHasher,
    for<'a> &'a C: IntoIterator<Item = &'a Node>,
{
    let mut placement: FxHashMap<Node, FxHashSet<Var>> = FxHashMap::default();
    for (&var, defs) in def_sites {
        let sites: FxHashSet<Node> = defs.into_iter().copied().collect();
        // Each node is queued at most once.
        let mut worklist: Vec<Node> = sites.iter().copied().collect();
        let mut placed: FxHashSet<Node> = FxHashSet::default();
        while let Some(x) = worklist.pop() {
            let Some(df) = frontiers.get(&x) else {
                continue;
            };
            for &y in df {
                if placed.insert(y) {
                    placement.entry(y).or_default().insert(var);
                    // The phi is itself a def of `V`.
                    if !sites.contains(&y) {
                        worklist.push(y);
                    }
                }
            }
        }
    }
    placement
}

/// Immediate dominators of the vertices reachable from a root.
pub struct Dominators<N> {
    /// Reachable vertices in depth-first preorder, the root first.
    vertices: Vec<N>,
    /// Each vertex's position in `vertices`.
    number: FxHashMap<N, u32>,
    /// By position; the root's is itself.
    idom: Vec<u32>,
}

impl<N: Copy + Eq + Hash> Dominators<N> {
    /// Every vertex reachable from the root, the root first.
    pub fn vertices(&self) -> &[N] {
        &self.vertices
    }

    /// `None` for the root and for any vertex it does not reach.
    pub fn immediate_dominator(&self, n: N) -> Option<N> {
        let &at = self.number.get(&n)?;
        (at != 0).then(|| self.vertices[self.idom[at as usize] as usize])
    }

    /// The interval numbering of this tree, over positions in
    /// [`Self::vertices`], so siblings come in depth-first discovery order.
    #[must_use]
    pub fn intervals(&self) -> DomIntervals {
        DomIntervals::new(&self.idom)
    }

    /// The tree in preorder, siblings ascending by `N`: a function of the idom
    /// relation alone, not of the walk that found it.
    #[must_use]
    pub fn tree_preorder(&self) -> Vec<N>
    where
        N: Ord,
    {
        let mut children = children(&self.idom);
        let mut order = Vec::with_capacity(self.vertices.len());
        let mut stack = vec![0u32];
        while let Some(v) = stack.pop() {
            order.push(self.vertices[v as usize]);
            let siblings = &mut children[v as usize];
            siblings.sort_unstable_by_key(|&c| self.vertices[c as usize]);
            stack.extend(siblings.iter().rev());
        }
        order
    }
}

const UNLINKED: u32 = u32::MAX;

/// Each vertex's dominator-tree children, ascending by index.
fn children(idom: &[u32]) -> Vec<Vec<u32>> {
    let mut children = vec![Vec::new(); idom.len()];
    for (v, &d) in idom.iter().enumerate().skip(1) {
        children[d as usize].push(v as u32);
    }
    children
}

/// Lengauer-Tarjan with path compression, O(m log n) over the `m` edges
/// `successors` yields from the vertices `root` reaches.
pub fn dominators<N, I>(root: N, mut successors: impl FnMut(N) -> I) -> Dominators<N>
where
    N: Copy + Eq + Hash,
    I: IntoIterator<Item = N>,
{
    let mut vertices = vec![root];
    let mut number: FxHashMap<N, u32> = FxHashMap::default();
    number.insert(root, 0);
    let mut parent = vec![0u32];
    let mut preds: Vec<Vec<u32>> = vec![Vec::new()];
    // Iterative DFS, so `parent` is a true depth-first spanning tree.
    let mut stack: Vec<(u32, Vec<N>, usize)> = vec![(0, successors(root).into_iter().collect(), 0)];
    while let Some((at, succs, next)) = stack.last_mut() {
        let Some(&succ) = succs.get(*next) else {
            stack.pop();
            continue;
        };
        *next += 1;
        let at = *at;
        if let Some(&seen) = number.get(&succ) {
            preds[seen as usize].push(at);
            continue;
        }
        let fresh = u32::try_from(vertices.len()).expect("vertex count fits u32");
        number.insert(succ, fresh);
        vertices.push(succ);
        parent.push(at);
        preds.push(vec![at]);
        stack.push((fresh, successors(succ).into_iter().collect(), 0));
    }

    let idom = immediate_dominators(&parent, |w| std::mem::take(&mut preds[w as usize]));
    Dominators {
        vertices,
        number,
        idom,
    }
}

/// Lengauer-Tarjan over a depth-first preorder numbering: `parent[w]` is `w`'s
/// spanning-tree parent and `preds(w)` yields the preorder numbers of `w`'s
/// predecessors, asked at most once for each `w` and never for the root, so a
/// destructive closure (`std::mem::take`) is a valid one. The result is indexed
/// by preorder number, the root at 0 and its own immediate dominator.
///
/// Every vertex must be numbered, i.e. reachable from the root, and every
/// number `preds` yields must be in range.
#[must_use]
pub fn immediate_dominators<I>(parent: &[u32], mut preds: impl FnMut(u32) -> I) -> Vec<u32>
where
    I: IntoIterator<Item = u32>,
{
    let n = parent.len();
    let mut semi: Vec<u32> = (0..n as u32).collect();
    let mut label: Vec<u32> = (0..n as u32).collect();
    let mut ancestor = vec![UNLINKED; n];
    let mut idom = vec![0u32; n];
    let mut bucket: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut path: Vec<u32> = Vec::new();

    for w in (1..n).rev() {
        for v in preds(w as u32) {
            let u = eval(v, &mut ancestor, &mut label, &semi, &mut path);
            semi[w] = semi[w].min(semi[u as usize]);
        }
        bucket[semi[w] as usize].push(w as u32);
        let p = parent[w];
        ancestor[w] = p;
        for v in std::mem::take(&mut bucket[p as usize]) {
            let u = eval(v, &mut ancestor, &mut label, &semi, &mut path);
            idom[v as usize] = if semi[u as usize] < semi[v as usize] {
                u
            } else {
                p
            };
        }
    }
    for w in 1..n {
        if idom[w] != semi[w] {
            idom[w] = idom[idom[w] as usize];
        }
    }
    idom
}

/// Enter/exit interval numbering of the dominator tree an
/// [`immediate_dominators`] result describes, so [`DomIntervals::dominates`] is
/// O(1).
pub struct DomIntervals {
    enter: Vec<u32>,
    exit: Vec<u32>,
    preorder: Vec<u32>,
}

impl DomIntervals {
    /// `idom` must be non-empty, with the root at index 0.
    #[must_use]
    pub fn new(idom: &[u32]) -> Self {
        let n = idom.len();
        let children = children(idom);
        let mut enter = vec![0u32; n];
        let mut exit = vec![0u32; n];
        let mut clock = 0u32;
        let mut preorder = Vec::with_capacity(n);
        let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
        enter[0] = clock;
        preorder.push(0);
        clock += 1;
        while let Some(top) = stack.last_mut() {
            let (v, next) = *top;
            if let Some(&c) = children[v as usize].get(next) {
                top.1 += 1;
                enter[c as usize] = clock;
                preorder.push(c);
                clock += 1;
                stack.push((c, 0));
            } else {
                exit[v as usize] = clock;
                clock += 1;
                stack.pop();
            }
        }
        Self {
            enter,
            exit,
            preorder,
        }
    }

    /// Reflexive: every vertex dominates itself.
    #[must_use]
    pub fn dominates(&self, a: u32, b: u32) -> bool {
        self.enter[a as usize] <= self.enter[b as usize]
            && self.exit[b as usize] <= self.exit[a as usize]
    }

    /// `v`'s `(enter, exit)` clock, from 0: `a` dominates `b` iff `a`'s
    /// interval encloses `b`'s.
    #[must_use]
    pub fn span(&self, v: u32) -> (u32, u32) {
        (self.enter[v as usize], self.exit[v as usize])
    }

    /// Every vertex in tree preorder, siblings in ascending discovery
    /// position.
    #[must_use]
    pub fn preorder(&self) -> &[u32] {
        &self.preorder
    }
}

/// The vertex of least semidominator on `v`'s linked ancestor path,
/// compressing that path.
fn eval(v: u32, ancestor: &mut [u32], label: &mut [u32], semi: &[u32], path: &mut Vec<u32>) -> u32 {
    if ancestor[v as usize] == UNLINKED {
        return v;
    }
    // Iterative compress: the path runs up to the vertex whose ancestor is the
    // forest root.
    path.clear();
    let mut at = v;
    while ancestor[ancestor[at as usize] as usize] != UNLINKED {
        path.push(at);
        at = ancestor[at as usize];
    }
    for &x in path.iter().rev() {
        let up = ancestor[x as usize] as usize;
        if semi[label[up] as usize] < semi[label[x as usize] as usize] {
            label[x as usize] = label[up];
        }
        ancestor[x as usize] = ancestor[up];
    }
    label[v as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Successor lists by vertex; a vertex not listed has none.
    type Edges = [(u32, &'static [u32])];

    fn succs_of(edges: &Edges, v: u32) -> Vec<u32> {
        edges
            .iter()
            .find(|(n, _)| *n == v)
            .map_or_else(Vec::new, |(_, s)| s.to_vec())
    }

    /// Every source edging into `v`, unreachable ones included.
    fn preds_of(edges: &Edges, v: u32) -> Vec<u32> {
        edges
            .iter()
            .filter(|(_, s)| s.contains(&v))
            .map(|(n, _)| *n)
            .collect()
    }

    /// The dominators from 0 over `edges`, and the frontiers over them.
    fn frontiers_of(edges: &Edges) -> (Dominators<u32>, Frontiers<u32>) {
        let doms = dominators(0u32, |v| succs_of(edges, v));
        let fr = dominance_frontiers(&doms, |v| preds_of(edges, v));
        (doms, fr)
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

    const DIAMOND: &Edges = &[(0, &[1, 2]), (1, &[3]), (2, &[3])];

    /// Diamond: 0 -> {1,2} -> 3.
    #[test]
    fn diamond_frontiers_and_placement() {
        let (_, fr) = frontiers_of(DIAMOND);
        assert_eq!(df_sorted(&fr, 1), vec![3]);
        assert_eq!(df_sorted(&fr, 2), vec![3]);
        assert!(df_sorted(&fr, 3).is_empty());
        assert!(df_sorted(&fr, 0).is_empty());

        // Defined in one arm only, but still needs a phi at the join.
        let place = phi_placement(&fr, &defs(&[('a', &[1])]));
        assert!(place[&3].contains(&'a'));
        assert_eq!(place.len(), 1, "φ only at the join");
    }

    /// Loop: 0 -> 1(head) -> 2(body) -> 1 (back-edge), 1 -> 3(exit).  The
    /// header is its own frontier via the back-edge, so this pins the IDF
    /// fixed point.
    #[test]
    fn loop_frontiers_and_iterated_placement() {
        let (_, fr) = frontiers_of(&[(0, &[1]), (1, &[2, 3]), (2, &[1])]);
        assert_eq!(df_sorted(&fr, 2), vec![1]);
        assert_eq!(df_sorted(&fr, 1), vec![1]);

        // DF(2)={1} -> phi at head; head is then a fresh def, but DF(1)={1} is
        // already placed -> fixed point at one phi.
        let place = phi_placement(&fr, &defs(&[('v', &[2])]));
        assert!(place[&1].contains(&'v'));
        assert_eq!(place.len(), 1);
    }

    /// A self-loop on the root: `DF(root) = {root}` by the definition, since
    /// the root dominates its own predecessor and does not STRICTLY dominate
    /// itself. Strider's CFG keeps the entry's predecessors: a branch back to
    /// the entry address is a self-edge on the entry region.
    #[test]
    fn self_loop_at_root_puts_root_in_its_own_frontier() {
        let (_, fr) = frontiers_of(&[(0, &[0])]);
        assert_eq!(df_sorted(&fr, 0), vec![0]);

        // A variable defined in the loop body needs its phi AT the root.
        let place = phi_placement(&fr, &defs(&[('v', &[0])]));
        assert!(place[&0].contains(&'v'));
    }

    /// The root as a loop header reached from deeper in the loop: every node
    /// on the back-edge path carries the root in its frontier.
    #[test]
    fn back_edge_to_root_frontiers() {
        let (_, fr) = frontiers_of(&[(0, &[1]), (1, &[2]), (2, &[0])]);
        assert_eq!(df_sorted(&fr, 0), vec![0]);
        assert_eq!(df_sorted(&fr, 1), vec![0]);
        assert_eq!(df_sorted(&fr, 2), vec![0]);
    }

    /// The root and an unreachable vertex share the "no idom" answer, so the
    /// root's frontier must not leak onto dead vertices: a predecessor-free
    /// root still has an empty frontier, and the climb from a dead
    /// predecessor still terminates.
    #[test]
    fn unreachable_node_is_not_treated_as_the_root() {
        // 9 is unreachable and edges into the join 3.
        let (_, fr) = frontiers_of(&[(0, &[1, 2]), (1, &[3]), (2, &[3]), (9, &[3])]);
        assert!(
            df_sorted(&fr, 0).is_empty(),
            "a predecessor-free root has an empty frontier"
        );
        // The dead node reaches the join, so it carries it; what matters is
        // that the climb stops rather than looping or ascending into the
        // root's.
        assert_eq!(df_sorted(&fr, 9), vec![3]);
    }

    #[test]
    fn preorder_respects_idom() {
        let (doms, _) = frontiers_of(DIAMOND);
        let order = doms.tree_preorder();
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], 0);
        let pos: FxHashMap<u32, usize> = order.iter().enumerate().map(|(i, &n)| (n, i)).collect();
        for &n in &order[1..] {
            let idom = doms.immediate_dominator(n).expect("reachable");
            assert!(pos[&idom] < pos[&n], "node {n} must follow its idom {idom}");
        }
    }

    /// `0 -> 2, 0 -> 1`: the DFS discovers 2 first. `tree_preorder` visits the
    /// siblings by vertex, `intervals().preorder()` by discovery.
    #[test]
    fn tree_preorder_orders_siblings_by_vertex_not_discovery() {
        let doms = dominators(0u32, |v| match v {
            0 => vec![2, 1],
            _ => Vec::new(),
        });
        assert_eq!(doms.tree_preorder(), [0, 1, 2]);
        let by_discovery: Vec<u32> = doms
            .intervals()
            .preorder()
            .iter()
            .map(|&at| doms.vertices()[at as usize])
            .collect();
        assert_eq!(by_discovery, [0, 2, 1]);
    }

    /// The interval tree over `dominators`' own numbering: the root encloses
    /// every vertex, siblings enclose neither each other nor the join.
    #[test]
    fn intervals_number_the_dominator_tree() {
        let (doms, _) = frontiers_of(DIAMOND);
        let iv = doms.intervals();
        let at = |v: u32| {
            doms.vertices()
                .iter()
                .position(|&x| x == v)
                .expect("reachable") as u32
        };
        assert_eq!(iv.preorder().len(), 4);
        for v in [1, 2, 3] {
            assert!(iv.dominates(at(0), at(v)), "the root dominates {v}");
        }
        assert!(!iv.dominates(at(1), at(3)) && !iv.dominates(at(1), at(2)));
        let (enter, exit) = iv.span(at(0));
        assert_eq!(enter, 0, "the clock starts at 0");
        assert!(exit > enter);
    }

    /// Vertex whose `PartialEq` counts calls, so the frontier build's work is
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

    /// `0 -> {1, 2} ->` each of `joins` joins: both arms carry every join in
    /// their frontier, so each arm's frontier grows to `joins` entries.
    fn frontier_eq_calls(joins: u32) -> u64 {
        let doms = dominators(Counted(0), |n: Counted| match n.0 {
            0 => vec![Counted(1), Counted(2)],
            1 | 2 => (3..3 + joins).map(Counted).collect(),
            _ => Vec::new(),
        });
        EQ_CALLS.with(|c| c.set(0));
        let fr = dominance_frontiers(&doms, |n: Counted| match n.0 {
            0 => Vec::new(),
            1 | 2 => vec![Counted(0)],
            _ => vec![Counted(1), Counted(2)],
        });
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

    /// Chain `0 -> 1 -> .. -> n-1` where every node also edges into the join
    /// `n`: the `if (a) goto err; if (b) goto err;` ladder. `idom(n) = 0`, so
    /// each of the n predecessors would climb the whole chain back to 0, while
    /// the frontier output is only n - 1 entries. Each idom step of a climb is
    /// a hash probe, so the comparisons count the climb.
    fn ladder_eq_calls(n: u32) -> u64 {
        let doms = dominators(Counted(0), |v: Counted| {
            if v.0 + 1 < n {
                vec![Counted(v.0 + 1), Counted(n)]
            } else if v.0 + 1 == n {
                vec![Counted(n)]
            } else {
                Vec::new()
            }
        });
        EQ_CALLS.with(|c| c.set(0));
        let fr = dominance_frontiers(&doms, |b: Counted| {
            if b.0 == n {
                (0..n).map(Counted).collect()
            } else if b.0 == 0 {
                Vec::new()
            } else {
                vec![Counted(b.0 - 1)]
            }
        });
        let entries: usize = fr.values().map(Vec::len).sum();
        assert_eq!(entries, n as usize - 1, "output is linear in n");
        EQ_CALLS.with(std::cell::Cell::get)
    }

    /// The climb must stop as soon as it reaches a pair an earlier
    /// predecessor already recorded: that predecessor walked the identical
    /// idom chain to the identical stop, so everything above is already in.
    /// Without the cut, shared chain segments are re-walked once per
    /// predecessor.
    #[test]
    fn error_ladder_frontier_climb_is_not_quadratic() {
        let small = ladder_eq_calls(200);
        let large = ladder_eq_calls(800);
        assert!(
            large < small * 8,
            "4x the nodes cost {large} comparisons vs {small} against linear output: \
             the climb re-walks shared chain segments"
        );
    }

    /// The frontier `Vec` keeps discovery order; the membership set must not
    /// reorder or drop entries.
    #[test]
    fn frontier_preserves_discovery_order() {
        let (_, fr) = frontiers_of(&[(0, &[1, 2]), (1, &[3, 4]), (2, &[3, 4])]);
        assert_eq!(fr[&1], vec![3, 4]);
        assert_eq!(fr[&2], vec![3, 4]);
    }

    #[test]
    fn multiple_variables_share_a_join() {
        let (_, fr) = frontiers_of(DIAMOND);
        let place = phi_placement(&fr, &defs(&[('a', &[1]), ('b', &[2])]));
        assert_eq!(place[&3].len(), 2);
        assert!(place[&3].contains(&'a') && place[&3].contains(&'b'));
        assert_eq!(place.len(), 1, "both φs land only at the join");
    }
}
