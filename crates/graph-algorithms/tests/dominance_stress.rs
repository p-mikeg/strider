//! The oracles here share NO code with the module under test: dominator sets
//! come from the deletion definition (`oracle`) and petgraph's `simple_fast`;
//! DF / IDF from their definitions.

mod common;

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

use common::cases::{self, Case};
use graph_algorithms::dominance::{
    DomIntervals, Dominators, Frontiers, dominance_frontiers, dominators, immediate_dominators,
    phi_placement, phi_placement_from_frontiers,
};
use graph_algorithms::oracle;
use petgraph::graph::NodeIndex;
use proptest::prelude::*;
use rustc_hash::FxHashMap;

/// `a` dominates `b`: `a`'s interval encloses `b`'s.
fn encloses(intervals: &DomIntervals, a: u32, b: u32) -> bool {
    let ((a_in, a_out), (b_in, b_out)) = (intervals.span(a), intervals.span(b));
    a_in <= b_in && b_out <= a_out
}

/// The interval numbering answers exactly the reflexive ancestor relation of
/// the idom array it was built from.
#[test]
fn dom_intervals_answer_the_idom_chain() {
    proptest!(|(raw in proptest::collection::vec(any::<u32>(), 1..=20))| {
        // `idom[v] < v` for `v > 0`: a valid preorder-numbered tree.
        let idom: Vec<u32> = (0..raw.len() as u32)
            .map(|v| if v == 0 { 0 } else { raw[v as usize] % v })
            .collect();
        let intervals = DomIntervals::new(&idom);
        for a in 0..idom.len() as u32 {
            for b in 0..idom.len() as u32 {
                let mut at = b;
                let mut climbed = at == a;
                while !climbed && at != 0 {
                    at = idom[at as usize];
                    climbed = at == a;
                }
                prop_assert_eq!(
                    encloses(&intervals, a, b),
                    climbed,
                    "dominates({}, {}) over {:?}", a, b, &idom
                );
            }
        }
    });
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

/// The placement lift computes, as `(node, var)` pairs.
fn placement<N: Id + Ord>(
    doms: &Dominators<N>,
    preds: &[Vec<u32>],
    defs: &[Vec<u32>],
) -> BTreeSet<(N, usize)> {
    let sites: HashMap<usize, Vec<N>> = defs
        .iter()
        .map(|d| d.iter().map(|&v| N::at(v)).collect())
        .enumerate()
        .collect();
    let placed = phi_placement(doms, |v: N| preds[v.ix()].iter().map(|&p| N::at(p)), &sites);
    pairs(&placed)
}

fn pairs<N: Id + Ord>(placed: &FxHashMap<N, rustc_hash::FxHashSet<usize>>) -> BTreeSet<(N, usize)> {
    placed
        .iter()
        .flat_map(|(&v, vars)| vars.iter().map(move |&var| (v, var)))
        .collect()
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
            let got = encloses(&intervals, num[a as usize], num[b as usize]);
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
    let placed = phi_placement_from_frontiers(&fr, &sites);
    assert_eq!(
        placement(&doms, &preds, &c.defs),
        pairs(&placed),
        "placement against the frontier placement, {c:?}"
    );
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
    let preds = oracle::preds_of(&succs);
    let sites: HashMap<usize, Vec<u32>> = c.defs.iter().cloned().enumerate().collect();
    let placed = phi_placement_from_frontiers(&frontiers(&doms, &preds), &sites);
    assert_eq!(
        placement(&doms, &preds, &c.defs),
        pairs(&placed),
        "placement against the frontier placement, n {n}"
    );
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
/// sibling pair, and deep random graphs. Debug builds otherwise check it on
/// ten-vertex graphs and the named shapes.
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
/// across the dominators, placement and tree walks.
#[test]
#[cfg_attr(debug_assertions, ignore = "release-only scale test")]
fn dominance_work_is_linear_on_million_vertex_graphs() {
    const N: u32 = 1_000_000;
    // (idom of N - 1, phis for one definition at 1)
    let expect: [(u32, &[u32]); 4] = [(N - 2, &[]), (0, &[N - 1]), (0, &[]), (N - 2, &[1])];
    for (shape, &(idom, phis)) in expect.iter().enumerate() {
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
        let placed = phi_placement(
            &doms,
            |v: Counted| preds[v.0 as usize].iter().map(|&p| Counted(p)),
            &HashMap::from([(0u8, vec![Counted(1)])]),
        );
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

/// Many small random graphs, irreducible and with unreachable predecessors
/// and defs among them: the placement matches the frontier one.
#[test]
fn placement_matches_the_frontier_placement_on_random_graphs() {
    for seed in 1..=4_000u64 {
        let c = cases::large_case(2 + (seed % 60) as u32, seed);
        let succs = c.succs();
        let preds = oracle::preds_of(&succs);
        let doms = dominators(0u32, |v| succs[v as usize].clone());
        let sites: HashMap<usize, Vec<u32>> = c.defs.iter().cloned().enumerate().collect();
        let placed = phi_placement_from_frontiers(&frontiers(&doms, &preds), &sites);
        assert_eq!(
            placement(&doms, &preds, &c.defs),
            pairs(&placed),
            "seed {seed}, {c:?}"
        );
    }
}

/// A back-edge ladder: a chain `L_0 .. L_{n-1}`, then a run of blocks each
/// branching back to one `L_i`. `DF(L_k)` holds `L_0 ..= L_k`, so the
/// frontiers total `n^2 / 2` entries; the placement probes linearly.
#[test]
fn placement_is_linear_on_a_back_edge_ladder() {
    const N: u32 = 50_000;
    // `L_i = i`, the test `N`, the back-gotos `N + 1 + i`, the exit `2N + 1`.
    let succs = |v: u32| -> Vec<u32> {
        match v {
            _ if v < N => vec![v + 1],
            _ if v == N => vec![N + 1, 2 * N + 1],
            _ if v < 2 * N => vec![v - N - 1, v + 1],
            _ if v == 2 * N => vec![N - 1, 2 * N + 1],
            _ => Vec::new(),
        }
    };
    let mut preds: Vec<Vec<u32>> = vec![Vec::new(); 2 * N as usize + 2];
    let mut edges = 0u64;
    for v in 0..2 * N + 2 {
        for w in succs(v) {
            preds[w as usize].push(v);
            edges += 1;
        }
    }
    let doms = dominators(Counted(0), |v: Counted| succs(v.0).into_iter().map(Counted));
    HASHES.with(|c| c.set(0));
    HASH_BUDGET.with(|c| c.set(64 * (u64::from(2 * N + 2) + edges)));
    let placed = phi_placement(
        &doms,
        |v: Counted| preds[v.0 as usize].iter().map(|&p| Counted(p)),
        &HashMap::from([(0u8, (0..N).map(Counted).collect::<Vec<_>>())]),
    );
    HASH_BUDGET.with(|c| c.set(u64::MAX));
    let mut got: Vec<u32> = placed.keys().map(|v| v.0).collect();
    got.sort_unstable();
    assert_eq!(got, (0..N).collect::<Vec<_>>());
}
