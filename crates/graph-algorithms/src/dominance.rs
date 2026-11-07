use std::collections::{BinaryHeap, HashMap};
use std::hash::{BuildHasher, Hash};

use rustc_hash::{FxHashMap, FxHashSet};

/// `Frontiers[x]` is the dominance frontier of `x`; absent means empty.
#[cfg(feature = "test-util")]
pub type Frontiers<N> = FxHashMap<N, Vec<N>>;

/// Cytron dominance frontiers: `DF(x)` is the set of vertices `b` where `x`
/// dominates a predecessor of `b` but does not strictly dominate `b`.
/// `preds` yields each vertex's predecessors, unreachable ones included.
///
/// A root with a predecessor reachable from it is in its own frontier. A
/// climb from an unreachable predecessor records `(p, b)` and stops.
#[cfg(feature = "test-util")]
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

/// Iterated dominance frontier / SSA phi placement: a phi for `V` lands at
/// node `R` iff `R` is in `IDF(def-sites(V))`. `preds` yields each vertex's
/// predecessors, unreachable ones included; an unreachable def's frontier is
/// its reachable successors.
///
/// Sreedhar-Gao over the DJ graph: each def's dominator subtree is walked
/// deepest def first, and a join edge into a node no deeper than the def is
/// in the IDF. `O(N log N + E)` per variable; no frontier is built.
///
/// Returns, per node, the variables needing a phi there.
#[must_use]
pub fn phi_placement<Var, Node, C, H, I>(
    doms: &Dominators<Node>,
    mut preds: impl FnMut(Node) -> I,
    def_sites: &HashMap<Var, C, H>,
) -> FxHashMap<Node, FxHashSet<Var>>
where
    Var: Copy + Eq + Hash,
    Node: Copy + Eq + Hash,
    H: BuildHasher,
    for<'a> &'a C: IntoIterator<Item = &'a Node>,
    I: IntoIterator<Item = Node>,
{
    let n = doms.vertices.len();
    let children = children(&doms.idom);
    let mut level = vec![0u32; n];
    for v in 1..n {
        level[v] = level[doms.idom[v] as usize] + 1;
    }
    // Join edges `p -> b`, `p` not `b`'s idom, by position. One from an
    // unreachable `p` is keyed by `p`: its frontier holds every such `b`.
    let mut joins: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut unreached_joins: FxHashMap<Node, Vec<u32>> = FxHashMap::default();
    for (b, &node) in doms.vertices.iter().enumerate() {
        for p in preds(node) {
            match doms.number.get(&p) {
                Some(&at) if b != 0 && doms.idom[b] == at => {}
                Some(&at) => joins[at as usize].push(b as u32),
                None => unreached_joins.entry(p).or_default().push(b as u32),
            }
        }
    }

    let mut placement: FxHashMap<Node, FxHashSet<Var>> = FxHashMap::default();
    // Per-variable marks: equal to `stamp` means set for this variable.
    let (mut is_def, mut visited, mut placed) = (vec![0u32; n], vec![0u32; n], vec![0u32; n]);
    let mut roots: BinaryHeap<(u32, u32)> = BinaryHeap::new();
    let mut stack: Vec<u32> = Vec::new();
    // IDF candidates not yet placed.
    let mut found: Vec<u32> = Vec::new();
    for (stamp, (&var, defs)) in (1u32..).zip(def_sites) {
        for d in defs {
            if let Some(&at) = doms.number.get(d) {
                is_def[at as usize] = stamp;
                roots.push((level[at as usize], at));
            } else if let Some(bs) = unreached_joins.get(d) {
                found.extend(bs);
            }
        }
        loop {
            for b in found.drain(..) {
                let b_ix = b as usize;
                if placed[b_ix] != stamp {
                    placed[b_ix] = stamp;
                    placement
                        .entry(doms.vertices[b_ix])
                        .or_default()
                        .insert(var);
                    // The phi is itself a def of `V`.
                    if is_def[b_ix] != stamp {
                        roots.push((level[b_ix], b));
                    }
                }
            }
            let Some((root_level, root)) = roots.pop() else {
                break;
            };
            if visited[root as usize] == stamp {
                continue;
            }
            visited[root as usize] = stamp;
            stack.push(root);
            while let Some(y) = stack.pop() {
                let y = y as usize;
                found.extend(
                    joins[y]
                        .iter()
                        .filter(|&&b| level[b as usize] <= root_level),
                );
                for &c in &children[y] {
                    if visited[c as usize] != stamp {
                        visited[c as usize] = stamp;
                        stack.push(c);
                    }
                }
            }
        }
    }
    placement
}

/// [`phi_placement`] from materialised frontiers (Cytron et al., Fig. 11),
/// quadratic where frontiers overlap.
#[cfg(feature = "test-util")]
#[must_use]
pub fn phi_placement_from_frontiers<Var, Node, C, H>(
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
/// `successors` yields from the vertices `root` reaches. `successors` is
/// called once per reached vertex.
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
    // Each frame resumes its own successor list, so `parent` is a true DFS
    // tree at any depth.
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

/// Lengauer-Tarjan over a DFS preorder numbering: `parent[w]` is `w`'s tree
/// parent; `preds(w)` yields `w`'s predecessors' numbers, in range, and is asked
/// at most once per `w`, never for the root. Indexed by preorder number; the
/// root is 0 and its own immediate dominator. Every vertex must be reachable.
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
/// [`immediate_dominators`] result describes, so dominance is an O(1)
/// [`DomIntervals::span`] comparison.
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

    const DIAMOND: &Edges = &[(0, &[1, 2]), (1, &[3]), (2, &[3])];

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
        let doms = dominators(0u32, |v| succs_of(DIAMOND, v));
        let iv = doms.intervals();
        let at = |v: u32| {
            doms.vertices()
                .iter()
                .position(|&x| x == v)
                .expect("reachable") as u32
        };
        let dominates = |a, b| {
            let ((a_in, a_out), (b_in, b_out)) = (iv.span(at(a)), iv.span(at(b)));
            a_in <= b_in && b_out <= a_out
        };
        assert_eq!(iv.preorder().len(), 4);
        for v in [1, 2, 3] {
            assert!(dominates(0, v), "the root dominates {v}");
        }
        assert!(!dominates(1, 3) && !dominates(1, 2));
        let (enter, exit) = iv.span(at(0));
        assert_eq!(enter, 0, "the clock starts at 0");
        assert!(exit > enter);
    }
}
