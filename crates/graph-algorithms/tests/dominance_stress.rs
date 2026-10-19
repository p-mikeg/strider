//! The oracles here share NO code with the module under test: dominator sets
//! come from the textbook iterative fixpoint `Dom(n) = {n} ∪ ⋂ Dom(pred)`, from
//! the deletion definition (`oracle`) and from petgraph's
//! `simple_fast`; DF / IDF from their definitions.

mod common;

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use common::cases::{self, Case};
use graph_algorithms::dominance::{
    DomIntervals, Dominators, Frontiers, dominance_frontiers, dominators, immediate_dominators,
    phi_placement,
};
use graph_algorithms::oracle;
use petgraph::graph::NodeIndex;
use proptest::prelude::*;
use rustc_hash::FxHashMap;

/// Deterministic splitmix-ish PRNG, so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        // LCG step, then xorshift the high bits down. The raw LCG's low bits
        // are far too regular for the modulo in `below`.
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z ^ (z >> 27)
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % u64::from(n)) as u32
    }
}

/// A random CFG over `n` nodes (0 = entry) in which every node is reachable:
/// a spanning tree plus `extra` arbitrary edges, self-loops allowed.
fn random_cfg(rng: &mut Rng, n: u32, extra: u32) -> HashMap<u32, Vec<u32>> {
    let mut edges: HashSet<(u32, u32)> = HashSet::new();
    for v in 1..n {
        let u = rng.below(v); // parent strictly earlier -> acyclic tree, all reachable
        edges.insert((u, v));
    }
    for _ in 0..extra {
        let u = rng.below(n);
        let v = rng.below(n);
        // Strider's CFG builder routes a branch back to the entry address as an
        // edge into the entry region, so back-edges into node 0 are in scope.
        edges.insert((u, v));
    }
    let mut preds: HashMap<u32, Vec<u32>> = (0..n).map(|x| (x, Vec::new())).collect();
    for (u, v) in edges {
        preds.get_mut(&v).unwrap().push(u);
    }
    preds
}

/// ORACLE: dominator sets by the iterative data-flow fixpoint, then idoms.
fn oracle_dom_and_idom(
    n: u32,
    preds: &HashMap<u32, Vec<u32>>,
) -> (HashMap<u32, HashSet<u32>>, HashMap<u32, u32>) {
    let all: HashSet<u32> = (0..n).collect();
    let mut dom: HashMap<u32, HashSet<u32>> = HashMap::new();
    dom.insert(0, HashSet::from([0]));
    for x in 1..n {
        dom.insert(x, all.clone()); // start universal, shrink to fixpoint
    }
    let mut changed = true;
    while changed {
        changed = false;
        for x in 1..n {
            let mut inter: Option<HashSet<u32>> = None;
            for &p in &preds[&x] {
                let dp = &dom[&p];
                inter = Some(match inter {
                    None => dp.clone(),
                    Some(acc) => acc.intersection(dp).copied().collect(),
                });
            }
            let mut new = inter.unwrap_or_default();
            new.insert(x);
            if new != dom[&x] {
                dom.insert(x, new);
                changed = true;
            }
        }
    }
    // The closest strict dominator is the one with the largest Dom set.
    let mut idom: HashMap<u32, u32> = HashMap::new();
    for x in 1..n {
        let best = dom[&x]
            .iter()
            .filter(|&&d| d != x)
            .max_by_key(|&&d| dom[&d].len());
        if let Some(&d) = best {
            idom.insert(x, d);
        }
    }
    (dom, idom)
}

/// ORACLE: `DF(x)` by definition, as a brute-force O(n³) scan.
fn oracle_df(
    n: u32,
    preds: &HashMap<u32, Vec<u32>>,
    dom: &HashMap<u32, HashSet<u32>>,
) -> HashMap<u32, HashSet<u32>> {
    let dominates = |x: u32, y: u32| dom[&y].contains(&x); // x ∈ Dom(y)
    let strictly_dominates = |x: u32, y: u32| x != y && dominates(x, y);
    let mut df: HashMap<u32, HashSet<u32>> = (0..n).map(|x| (x, HashSet::new())).collect();
    for b in 0..n {
        for &p in &preds[&b] {
            for x in 0..n {
                if dominates(x, p) && !strictly_dominates(x, b) {
                    df.get_mut(&x).unwrap().insert(b);
                }
            }
        }
    }
    df
}

/// ORACLE: iterated dominance frontier as the fixpoint `J = DF(defs ∪ J)`.
fn oracle_idf(df: &HashMap<u32, HashSet<u32>>, defs: &HashSet<u32>) -> HashSet<u32> {
    let mut j: HashSet<u32> = HashSet::new();
    loop {
        let mut next: HashSet<u32> = HashSet::new();
        for &x in defs.iter().chain(j.iter()) {
            if let Some(s) = df.get(&x) {
                next.extend(s.iter().copied());
            }
        }
        if next == j {
            return j;
        }
        j = next;
    }
}

fn as_set(v: &[u32]) -> HashSet<u32> {
    v.iter().copied().collect()
}

#[test]
fn dominance_frontiers_match_definition_over_random_cfgs() {
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for iter in 0..600u32 {
        let n = 3 + rng.below(11); // 3..=13 nodes
        let extra = rng.below(2 * n); // up to ~2n extra edges (dense, irreducible)
        let preds = random_cfg(&mut rng, n, extra);
        let (dom, idom) = oracle_dom_and_idom(n, &preds);

        let succs = succs_of(n, &preds);
        let doms = dominators(0u32, |v| succs.get(&v).cloned().unwrap_or_default());
        let df_impl = dominance_frontiers(&doms, |v| preds.get(&v).cloned().unwrap_or_default());
        // Same DF in the oracle's representation, for the IDF fixpoint below.
        let df_std: HashMap<u32, HashSet<u32>> = df_impl
            .iter()
            .map(|(&k, v)| (k, v.iter().copied().collect()))
            .collect();
        let df_oracle = oracle_df(n, &preds, &dom);
        for x in 0..n {
            let got = df_impl.get(&x).map(|v| as_set(v)).unwrap_or_default();
            assert_eq!(
                got, df_oracle[&x],
                "iter {iter}: DF({x}) mismatch (n={n}, extra={extra})\npreds={preds:?}\nidom={idom:?}"
            );
        }

        let order = doms.tree_preorder();
        assert_eq!(order[0], 0, "iter {iter}: preorder must start at root");
        let pos: HashMap<u32, usize> = order.iter().enumerate().map(|(i, &nn)| (nn, i)).collect();
        assert_eq!(
            pos.len(),
            order.len(),
            "iter {iter}: preorder has duplicates"
        );
        for (&node, &id) in &idom {
            assert!(
                pos[&id] < pos[&node],
                "iter {iter}: node {node} must follow its idom {id}"
            );
        }

        // Both sides run over the SAME impl DF map, isolating the worklist
        // logic from any DF bug.
        for _ in 0..4 {
            let defs: HashSet<u32> = (0..n).filter(|_| rng.below(3) == 0).collect();
            let defs: HashSet<u32> = if defs.is_empty() {
                HashSet::from([rng.below(n)])
            } else {
                defs
            };
            let dm: HashMap<(), Vec<u32>> = HashMap::from([((), defs.iter().copied().collect())]);
            let place = phi_placement(&df_impl, &dm);
            let phi_nodes: HashSet<u32> = place
                .iter()
                .filter(|(_, vars)| vars.contains(&()))
                .map(|(&nn, _)| nn)
                .collect();
            let idf_oracle = oracle_idf(&df_std, &defs);
            assert_eq!(
                phi_nodes, idf_oracle,
                "iter {iter}: IDF mismatch for defs={defs:?}\ndf={df_impl:?}"
            );
        }
    }
}

/// Unreachable nodes (idom = None) must be skipped, not panic and not appear.
#[test]
fn unreachable_nodes_are_skipped() {
    // 0 -> 1 -> 2 reachable; 3 -> 4 is an island with no edge in from {0,1,2}.
    let preds: HashMap<u32, Vec<u32>> = HashMap::from([
        (0, vec![]),
        (1, vec![0]),
        (2, vec![1]),
        (3, vec![]), // island root, not reachable from 0
        (4, vec![3]),
    ]);
    let succs = succs_of(5, &preds);
    let doms = dominators(0u32, |v| succs.get(&v).cloned().unwrap_or_default());
    let df = dominance_frontiers(&doms, |v| preds.get(&v).cloned().unwrap_or_default());
    // A straight-line chain has empty frontiers everywhere.
    for x in 0..5 {
        assert!(
            df.get(&x).is_none_or(|v| v.is_empty()),
            "DF({x}) must be empty"
        );
    }
    let order = doms.tree_preorder();
    assert_eq!(
        order,
        vec![0, 1, 2],
        "preorder excludes the unreachable island"
    );
}

/// Against the fixpoint oracle, on dense irreducible graphs plus an island of
/// vertices the root never reaches, and edges back into the root.
#[test]
fn dominators_match_the_fixpoint_oracle_over_random_cfgs() {
    let mut rng = Rng(0x0dd0_face_5eed_cafe);
    for iter in 0..1_000u32 {
        let n = 2 + rng.below(14);
        let extra = rng.below(3 * n);
        let preds = random_cfg(&mut rng, n, extra);
        let (_, idom) = oracle_dom_and_idom(n, &preds);
        let island = n + rng.below(3);
        let mut succs: HashMap<u32, Vec<u32>> = HashMap::new();
        for (&v, ps) in &preds {
            for &p in ps {
                succs.entry(p).or_default().push(v);
            }
        }
        // Island vertices reach into the graph but nothing reaches them.
        for x in n..island {
            succs.entry(x).or_default().push(rng.below(n));
        }
        let got = dominators(0u32, |v| succs.get(&v).cloned().unwrap_or_default());
        assert_eq!(got.vertices()[0], 0, "iter {iter}: root first");
        assert_eq!(
            got.vertices().len(),
            n as usize,
            "iter {iter}: reachable set"
        );
        assert_eq!(got.immediate_dominator(0), None, "iter {iter}: root");
        for v in 1..island {
            assert_eq!(
                got.immediate_dominator(v),
                idom.get(&v).copied(),
                "iter {iter}: idom({v}) (n={n})\npreds={preds:?}"
            );
        }
    }
}

/// Depth-first preorder from 0: `(num, vertex, parent)`, as a caller that owns
/// its own numbering hands it to [`immediate_dominators`].
fn preorder_dfs(n: u32, succs: &HashMap<u32, Vec<u32>>) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    const NONE: u32 = u32::MAX;
    let mut num = vec![NONE; n as usize];
    let mut vertex = vec![0u32];
    let mut parent = vec![NONE];
    num[0] = 0;
    let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
    while let Some(top) = stack.last_mut() {
        let (v, next) = *top;
        let Some(&w) = succs.get(&v).and_then(|s| s.get(next)) else {
            stack.pop();
            continue;
        };
        top.1 += 1;
        if num[w as usize] == NONE {
            num[w as usize] = u32::try_from(vertex.len()).unwrap();
            parent.push(num[v as usize]);
            vertex.push(w);
            stack.push((w, 0));
        }
    }
    (num, vertex, parent)
}

fn succs_of(n: u32, preds: &HashMap<u32, Vec<u32>>) -> HashMap<u32, Vec<u32>> {
    let mut succs: HashMap<u32, Vec<u32>> = (0..n).map(|x| (x, Vec::new())).collect();
    for (&v, ps) in preds {
        for &p in ps {
            succs.get_mut(&p).unwrap().push(v);
        }
    }
    succs
}

/// The preorder-numbered entry point, driven from a caller-owned DFS, answers
/// what the node-keyed [`dominators`] answers.
#[test]
fn immediate_dominators_over_a_caller_owned_preorder_matches_the_oracle() {
    let mut rng = Rng(0x1dea_5eed_f00d_b00b);
    for iter in 0..1_000u32 {
        let n = 2 + rng.below(14);
        let extra = rng.below(3 * n);
        let preds = random_cfg(&mut rng, n, extra);
        let (_, idom) = oracle_dom_and_idom(n, &preds);
        let succs = succs_of(n, &preds);
        let (num, vertex, parent) = preorder_dfs(n, &succs);
        assert_eq!(vertex.len(), n as usize, "iter {iter}: all reachable");
        let got = immediate_dominators(&parent, |w| {
            preds[&vertex[w as usize]]
                .iter()
                .map(|&p| num[p as usize])
                .collect::<Vec<u32>>()
        });
        assert_eq!(got[0], 0, "iter {iter}: the root dominates itself");
        for w in 1..n as usize {
            assert_eq!(
                vertex[got[w] as usize], idom[&vertex[w]],
                "iter {iter}: idom({}) (n={n})\npreds={preds:?}",
                vertex[w]
            );
        }
    }
}

/// The interval numbering answers exactly the reflexive ancestor relation of
/// the idom array it was built from.
#[test]
fn dom_intervals_answer_the_idom_chain() {
    let mut rng = Rng(0xfeed_bead_0bad_c0de);
    for iter in 0..2_000u32 {
        let n = 1 + rng.below(20) as usize;
        // `idom[v] < v` for `v > 0`: a valid preorder-numbered tree.
        let idom: Vec<u32> = (0..n)
            .map(|v| if v == 0 { 0 } else { rng.below(v as u32) })
            .collect();
        let intervals = DomIntervals::new(&idom);
        for a in 0..n as u32 {
            for b in 0..n as u32 {
                let mut at = b;
                let mut climbed = at == a;
                while !climbed && at != 0 {
                    at = idom[at as usize];
                    climbed = at == a;
                }
                assert_eq!(
                    intervals.dominates(a, b),
                    climbed,
                    "iter {iter}: dominates({a}, {b}) over {idom:?}"
                );
            }
        }
    }
}

const CASES: u32 = if cfg!(debug_assertions) { 256 } else { 8192 };

/// The vertex types the checks run over.
trait Id: Copy + Eq + Hash + std::fmt::Debug {
    fn at(i: u32) -> Self;
    fn ix(self) -> usize;
}

impl Id for u32 {
    fn at(i: u32) -> Self {
        i
    }
    fn ix(self) -> usize {
        self as usize
    }
}

/// A vertex whose `Hash` counts calls: every map probe the dominance code
/// makes hashes once, so the count is its work, independent of timing. The
/// probe that reaches `HASH_BUDGET` panics, so superlinear work fails fast
/// rather than exhausting time or memory.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Counted(u32);

thread_local! {
    static HASHES: Cell<u64> = const { Cell::new(0) };
    static HASH_BUDGET: Cell<u64> = const { Cell::new(u64::MAX) };
}

impl Hash for Counted {
    fn hash<H: Hasher>(&self, h: &mut H) {
        let probes = HASHES.with(Cell::get) + 1;
        let budget = HASH_BUDGET.with(Cell::get);
        assert!(probes < budget, "hash probes reach the budget of {budget}");
        HASHES.with(|c| c.set(probes));
        self.0.hash(h);
    }
}

impl Id for Counted {
    fn at(i: u32) -> Self {
        Self(i)
    }
    fn ix(self) -> usize {
        self.0 as usize
    }
}

/// The frontiers lift computes.
fn frontiers<N: Id>(doms: &Dominators<N>, preds: &[Vec<u32>]) -> Frontiers<N> {
    dominance_frontiers(doms, |v: N| preds[v.ix()].iter().map(|&p| N::at(p)))
}

/// The preorder strider-ir walks: siblings in depth-first discovery order.
fn preorder_by_discovery<N: Id>(doms: &Dominators<N>) -> Vec<N> {
    doms.intervals()
        .preorder()
        .iter()
        .map(|&at| doms.vertices()[at as usize])
        .collect()
}

/// `(enter, exit)` of each reachable vertex, from 1 as strider-ir stores
/// them.
fn spans<N: Id>(doms: &Dominators<N>) -> FxHashMap<N, (u32, u32)> {
    let intervals = doms.intervals();
    doms.vertices()
        .iter()
        .enumerate()
        .map(|(at, &v)| {
            let (enter, exit) = intervals.span(at as u32);
            (v, (enter + 1, exit + 1))
        })
        .collect()
}

/// Every dominance answer rooted at 0, against the definitions and petgraph.
fn check_dominance(c: &Case) {
    let succs = c.succs();
    let preds = oracle::preds_of(&succs);
    let dom = oracle::dom_matrix(&succs, 0);
    let reach = dom[0].clone();
    let idom = oracle::idoms(&dom, 0);

    let mut asked = vec![0u32; succs.len()];
    let doms = dominators(0u32, |v| {
        asked[v as usize] += 1;
        succs[v as usize].clone()
    });
    let (discovery, parent) = oracle::dfs_tree(&succs, 0);
    assert_eq!(doms.vertices(), discovery.as_slice(), "vertices, {c:?}");
    for (v, (&n, &r)) in asked.iter().zip(&reach).enumerate() {
        assert_eq!(n, u32::from(r), "successors({v}) asked {n} times, {c:?}");
    }
    let pg = cases::petgraph_of(c, &vec![true; succs.len()]);
    let simple = petgraph::algo::dominators::simple_fast(&pg, NodeIndex::new(0));
    for (v, &want) in idom.iter().enumerate() {
        assert_eq!(doms.immediate_dominator(v as u32), want, "idom({v}), {c:?}");
        let theirs = simple
            .immediate_dominator(NodeIndex::new(v))
            .map(|d| d.index() as u32);
        assert_eq!(theirs, want, "petgraph idom({v}), {c:?}");
    }

    let mut num = vec![u32::MAX; succs.len()];
    for (i, &v) in discovery.iter().enumerate() {
        num[v as usize] = i as u32;
    }
    let raw = immediate_dominators(&parent, |w| {
        preds[discovery[w as usize] as usize]
            .iter()
            .filter(|&&p| reach[p as usize])
            .map(|&p| num[p as usize])
            .collect::<Vec<u32>>()
    });
    for (w, &v) in discovery.iter().enumerate().skip(1) {
        assert_eq!(
            Some(discovery[raw[w] as usize]),
            idom[v as usize],
            "raw idom({v}), {c:?}"
        );
    }
    let intervals = DomIntervals::new(&raw);
    for &a in &discovery {
        for &b in &discovery {
            let got = intervals.dominates(num[a as usize], num[b as usize]);
            assert_eq!(
                got, dom[a as usize][b as usize],
                "intervals: {a} dom {b}, {c:?}"
            );
        }
    }

    let df = oracle::frontiers(&succs, &dom, 0);
    let fr = frontiers(&doms, &preds);
    for (v, want) in df.iter().enumerate() {
        let got = fr.get(&(v as u32)).cloned().unwrap_or_default();
        let set: BTreeSet<u32> = got.iter().copied().collect();
        assert_eq!(set.len(), got.len(), "DF({v}) repeats in {got:?}, {c:?}");
        assert_eq!(&set, want, "DF({v}), {c:?}");
    }
    let sites: HashMap<usize, Vec<u32>> = c.defs.iter().cloned().enumerate().collect();
    let placed = phi_placement(&fr, &sites);
    for (var, defs) in c.defs.iter().enumerate() {
        let got: BTreeSet<u32> = placed
            .iter()
            .filter(|(_, vars)| vars.contains(&var))
            .map(|(&v, _)| v)
            .collect();
        assert_eq!(
            got,
            oracle::iterated_frontier(&df, defs),
            "phis of {defs:?}, {c:?}"
        );
    }

    let mut at_discovery = vec![usize::MAX; succs.len()];
    for (i, &v) in discovery.iter().enumerate() {
        at_discovery[v as usize] = i;
    }
    let by_discovery = oracle::tree_preorder(&idom, 0, |v| at_discovery[v as usize]);
    assert_eq!(
        preorder_by_discovery(&doms),
        by_discovery,
        "discovery preorder, {c:?}"
    );
    let ascending = oracle::tree_preorder(&idom, 0, |v| v as usize);
    assert_eq!(doms.tree_preorder(), ascending, "ascending preorder, {c:?}");
    let span = spans(&doms);
    for (a, row) in dom.iter().enumerate() {
        assert_eq!(
            span.contains_key(&(a as u32)),
            reach[a],
            "span of {a}, {c:?}"
        );
        let Some(&(ae, ax)) = span.get(&(a as u32)) else {
            continue;
        };
        assert!(1 <= ae && ae < ax, "span of {a} is ({ae}, {ax}), {c:?}");
        for (b, &d) in row.iter().enumerate() {
            let Some(&(be, bx)) = span.get(&(b as u32)) else {
                continue;
            };
            let encloses = ae <= be && bx <= ax;
            assert_eq!(encloses, d, "span({a}) encloses span({b}), {c:?}");
            let nested = encloses || (be <= ae && ax <= bx);
            assert!(
                nested || ax < be || bx < ae,
                "spans of {a} and {b} overlap, {c:?}"
            );
        }
    }
}

/// Deep random graphs: everything against the definitions while the cubic
/// oracle stays cheap, past that discovery order, one successor query per
/// vertex and idoms against petgraph.
fn check_large(c: &Case) {
    if c.n <= 200 {
        check_dominance(c);
        return;
    }
    let n = c.n;
    let succs = c.succs();
    let reach = oracle::reachable(&succs, &[0], None);
    let mut asked = vec![0u32; succs.len()];
    let doms = dominators(0u32, |v| {
        asked[v as usize] += 1;
        succs[v as usize].clone()
    });
    let (discovery, _) = oracle::dfs_tree(&succs, 0);
    assert!(doms.vertices() == discovery.as_slice(), "vertices, n {n}");
    for (v, (&k, &r)) in asked.iter().zip(&reach).enumerate() {
        assert_eq!(k, u32::from(r), "successors({v}) asked {k} times, n {n}");
    }
    let pg = cases::petgraph_of(c, &vec![true; succs.len()]);
    let simple = petgraph::algo::dominators::simple_fast(&pg, NodeIndex::new(0));
    for v in 0..n {
        let theirs = simple
            .immediate_dominator(NodeIndex::new(v as usize))
            .map(|d| d.index() as u32);
        assert_eq!(doms.immediate_dominator(v), theirs, "idom({v}), n {n}");
    }
    check_tree_numbering(&doms, n);
}

/// The tree numberings against the idoms, in one pass each: `tree_preorder`
/// is a preorder of the tree with siblings ascending, every span nests in its
/// idom's, and `intervals().preorder()` is the order of `enter`, with sibling
/// spans disjoint and siblings ascending by position, which is discovery order.
fn check_tree_numbering(doms: &Dominators<u32>, n: u32) {
    let order = doms.tree_preorder();
    assert_eq!(order.len(), doms.vertices().len(), "preorder length, n {n}");
    assert_eq!(order[0], 0, "preorder starts at the root, n {n}");
    let mut path = vec![0u32];
    let mut last_child = vec![None::<u32>; n as usize];
    for &v in &order[1..] {
        let idom = doms.immediate_dominator(v).expect("reachable");
        while path.last() != Some(&idom) {
            assert!(
                path.pop().is_some(),
                "{v} visited off its idom's path, n {n}"
            );
        }
        if let Some(prev) = last_child[idom as usize] {
            assert!(prev < v, "siblings {prev} then {v} under {idom}, n {n}");
        }
        last_child[idom as usize] = Some(v);
        path.push(v);
    }

    let iv = doms.intervals();
    let mut at = vec![u32::MAX; n as usize];
    for (i, &v) in doms.vertices().iter().enumerate() {
        at[v as usize] = i as u32;
    }
    assert_eq!(
        iv.preorder().len(),
        doms.vertices().len(),
        "intervals, n {n}"
    );
    for &v in &doms.vertices()[1..] {
        let idom = doms.immediate_dominator(v).expect("reachable");
        let (ve, vx) = iv.span(at[v as usize]);
        let (de, dx) = iv.span(at[idom as usize]);
        assert!(
            de < ve && vx < dx,
            "span of {v} ({ve}, {vx}) outside its idom {idom}'s ({de}, {dx}), n {n}"
        );
    }

    // Walked in `preorder()` with a stack of still-open spans: enters strictly
    // increase, and once the spans closed before an entry are popped, the top
    // of the stack is its idom and strictly encloses it. So every earlier
    // sibling has closed before a later one enters: sibling spans are
    // disjoint, and the numbering is a DFS interval numbering of the tree.
    // Siblings enter at ascending positions.
    let mut open: Vec<(u32, u32, u32)> = Vec::new();
    let mut last_sibling = vec![None::<u32>; doms.vertices().len()];
    for &pos in iv.preorder() {
        let (e, x) = iv.span(pos);
        assert!(e < x, "span of position {pos} is ({e}, {x}), n {n}");
        if let Some(&(_, prev, _)) = open.last() {
            assert!(prev < e, "preorder enters {pos} at {e} after {prev}, n {n}");
        }
        while let Some(&(_, _, top_x)) = open.last()
            && top_x < e
        {
            open.pop();
        }
        let v = doms.vertices()[pos as usize];
        match doms.immediate_dominator(v) {
            None => assert!(open.is_empty(), "root {v} entered inside a span, n {n}"),
            Some(idom) => {
                let &(top, top_e, top_x) = open
                    .last()
                    .unwrap_or_else(|| panic!("{v} entered outside every span, n {n}"));
                assert_eq!(
                    top, at[idom as usize],
                    "{v} enters inside position {top}, not its idom {idom}'s, n {n}"
                );
                assert!(
                    top_e < e && x < top_x,
                    "span ({e}, {x}) of {v} overlaps its idom's ({top_e}, {top_x}), n {n}"
                );
                if let Some(prev) = last_sibling[top as usize] {
                    assert!(
                        prev < pos,
                        "siblings at positions {prev} then {pos} under {idom}, n {n}"
                    );
                }
                last_sibling[top as usize] = Some(pos);
            }
        }
        open.push((pos, e, x));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn dominance_matches_its_definitions(c in cases::arb_case(10)) {
        check_dominance(&c);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    #[cfg_attr(debug_assertions, ignore = "release-only scale test")]
    fn dominance_matches_on_deep_random_graphs(
        n in prop_oneof![50u32..=200, 201u32..=2000],
        seed in any::<u64>(),
    ) {
        check_large(&cases::large_case(n, seed));
    }
}

#[test]
fn dominance_matches_its_definitions_on_every_named_shape() {
    for (name, c) in cases::shapes() {
        if c.n > 0 {
            println!("shape {name}");
            check_dominance(&c);
        }
    }
}

/// The numbering checks past the cubic oracle: a wide star discovered in
/// reverse vertex order, so discovery and vertex order disagree for every
/// sibling pair, and deep random graphs. Debug builds otherwise check the
/// numbering at 200 vertices at most.
#[test]
fn tree_numbering_holds_past_the_oracle() {
    const N: u32 = 1_000;
    let star: Vec<(u32, u32)> = (1..N).rev().map(|v| (0, v)).collect();
    let succs = Case::new(N, &star, &[0]).succs();
    let doms = dominators(0u32, |v| succs[v as usize].clone());
    check_tree_numbering(&doms, N);
    for seed in 1..=8 {
        let c = cases::large_case(1_500, seed);
        let succs = c.succs();
        let doms = dominators(0u32, |v| succs[v as usize].clone());
        check_tree_numbering(&doms, c.n);
    }
}

/// The loop oracles on the named shapes, against answers worked by hand.
#[test]
fn loop_oracles_hold_on_the_named_shapes() {
    let mut loops: HashMap<&str, Vec<(u32, Vec<u32>)>> = HashMap::new();
    for (name, c) in cases::shapes() {
        if c.n == 0 {
            continue;
        }
        let succs = c.succs();
        let reducible = oracle::is_reducible(&succs, 0);
        assert_eq!(reducible, name != "irreducible", "shape {name}");
        let dom = oracle::dom_matrix(&succs, 0);
        let found = (0..c.n)
            .filter_map(|h| {
                let body = oracle::natural_loop(&succs, &dom, h)?;
                Some((h, (0..c.n).filter(|&v| body[v as usize]).collect()))
            })
            .collect();
        loops.insert(name, found);
    }
    assert_eq!(loops["diamond"], []);
    assert_eq!(loops["irreducible"], []);
    assert_eq!(loops["self_loop"], [(0, vec![0])]);
    assert_eq!(loops["back_edge_to_root"], [(0, vec![0, 1, 2])]);
    assert_eq!(loops["root_on_a_pure_cycle"], [(0, vec![0, 1, 2])]);
    assert_eq!(loops["nested_loops"], [(1, vec![1, 2, 3]), (2, vec![2, 3])]);
    assert_eq!(loops["sibling_loops"], [(1, vec![1, 2]), (3, vec![3, 4])]);
}

/// A chain, an error ladder (each rung also edges into one sink), a star,
/// and a cycle back to vertex 1.
fn scale_succs(shape: usize, n: u32, v: u32) -> Vec<u32> {
    match shape {
        0 => (v + 1 < n).then_some(v + 1).into_iter().collect(),
        1 if v + 2 < n => vec![v + 1, n - 1],
        1 if v + 2 == n => vec![n - 1],
        2 if v == 0 => (1..n).collect(),
        3 => vec![if v + 1 < n { v + 1 } else { 1 }],
        _ => Vec::new(),
    }
}

/// Successors asked once per vertex, and hash probes linear in the graph,
/// across the dominators, frontiers, placement and tree walks.
#[test]
#[cfg_attr(debug_assertions, ignore = "release-only scale test")]
fn dominance_work_is_linear_on_million_vertex_graphs() {
    const N: u32 = 1_000_000;
    // (idom of N - 1, frontier entries, phis for one definition at 1)
    let expect: [(u32, usize, &[u32]); 4] = [
        (N - 2, 0, &[]),
        (0, N as usize - 2, &[N - 1]),
        (0, 0, &[]),
        (N - 2, N as usize - 1, &[1]),
    ];
    for (shape, &(idom, entries, phis)) in expect.iter().enumerate() {
        let mut preds: Vec<Vec<u32>> = vec![Vec::new(); N as usize];
        let mut edges = 0u64;
        for v in 0..N {
            for w in scale_succs(shape, N, v) {
                preds[w as usize].push(v);
                edges += 1;
            }
        }
        HASHES.with(|c| c.set(0));
        HASH_BUDGET.with(|c| c.set(64 * (u64::from(N) + edges)));
        let mut asked = 0u64;
        let doms = dominators(Counted(0), |v: Counted| {
            asked += 1;
            assert!(asked <= u64::from(N), "shape {shape}: a vertex asked twice");
            scale_succs(shape, N, v.0).into_iter().map(Counted)
        });
        // Each result is reduced to the numbers asserted below and dropped,
        // so the peak holds one of them at a time.
        let fr = frontiers(&doms, &preds);
        let fr_entries: usize = fr.values().map(Vec::len).sum();
        let placed = phi_placement(&fr, &HashMap::from([(0u8, vec![Counted(1)])]));
        drop(fr);
        let mut got: Vec<u32> = placed.keys().map(|v| v.0).collect();
        drop(placed);
        let order_len = doms.tree_preorder().len();
        let span_len = spans(&doms).len();
        assert_eq!(
            asked,
            u64::from(N),
            "shape {shape}: successors asked once each"
        );
        assert_eq!(
            doms.immediate_dominator(Counted(N - 1)),
            Some(Counted(idom)),
            "shape {shape}"
        );
        assert_eq!(fr_entries, entries, "shape {shape}");
        got.sort_unstable();
        assert_eq!(got, phis, "shape {shape}");
        assert_eq!(
            (order_len, span_len),
            (N as usize, N as usize),
            "shape {shape}"
        );
    }
    HASH_BUDGET.with(|c| c.set(u64::MAX));
}
