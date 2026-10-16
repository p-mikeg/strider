//! Generated and named graphs for the walk and dominance properties.

use petgraph::graph::{DiGraph, NodeIndex};
use proptest::prelude::*;
use strider_ir_test_utils::Rng;

/// Vertices `0..n`; dominance roots at 0, walks at `roots`.
#[derive(Clone, Debug)]
pub struct Case {
    pub n: u32,
    pub edges: Vec<(u32, u32)>,
    pub roots: Vec<u32>,
    /// Definition sites, one list per variable.
    pub defs: Vec<Vec<u32>>,
}

impl Case {
    pub fn new(n: u32, edges: &[(u32, u32)], roots: &[u32]) -> Self {
        let defs = if n == 0 {
            Vec::new()
        } else {
            vec![(0..n).step_by(2).collect(), vec![n - 1], Vec::new()]
        };
        Self {
            n,
            edges: edges.to_vec(),
            roots: roots.to_vec(),
            defs,
        }
    }

    pub fn succs(&self) -> Vec<Vec<u32>> {
        let mut succs = vec![Vec::new(); self.n as usize];
        for &(a, b) in &self.edges {
            succs[a as usize].push(b);
        }
        succs
    }
}

/// Up to `max_n` vertices and `3 n` edges: self-loops, duplicate edges,
/// unreachable vertices, irreducible and rootless cycles all arise. Vertex 0
/// gets an out-edge in three cases of four, so most graphs reach past it.
pub fn arb_case(max_n: u32) -> impl Strategy<Value = Case> {
    (1..=max_n)
        .prop_flat_map(|n| {
            (
                Just(n),
                prop::collection::vec((0..n, 0..n), 0..=3 * n as usize),
                prop::option::weighted(0.75, (0..n, any::<prop::sample::Index>())),
                prop::collection::vec(0..n, 0..=3),
                prop::collection::vec(prop::collection::vec(0..n, 0..=4), 3),
            )
        })
        .prop_map(|(n, mut edges, from_root, roots, defs)| {
            if let Some((to, at)) = from_root {
                edges.insert(at.index(edges.len() + 1), (0, to));
            }
            Case {
                n,
                edges,
                roots,
                defs,
            }
        })
}

/// `n` vertices, most reachable from 0: a spanning tree of the first `k`,
/// half its parents the previous vertex so the depth-first tree runs deep,
/// then extra edges (back edges, self-loops, duplicates) among them, and an
/// unreachable tail of `n - k` vertices with its own edges, some into the
/// reachable part. Vertices are relabelled at random with 0 fixed and the
/// edge order shuffled, so neither ids nor order follow the construction.
pub fn large_case(n: u32, seed: u64) -> Case {
    let mut rng = Rng(seed | 1);
    let mut below = |bound: u32| rng.below(bound as usize) as u32;
    let k = n - below(n / 10 + 1);
    let mut edges = Vec::new();
    for v in 1..k {
        let parent = if below(2) == 0 { v - 1 } else { below(v) };
        edges.push((parent, v));
    }
    for _ in 0..below(2 * k) {
        edges.push((below(k), below(k)));
    }
    for _ in 0..below(k / 20 + 1) {
        let v = below(k);
        edges.push((v, v));
    }
    for _ in 0..below(k / 10 + 1) {
        let e = edges[below(edges.len() as u32) as usize];
        edges.push(e);
    }
    if k < n {
        for _ in 0..2 * (n - k) {
            edges.push((k + below(n - k), k + below(n - k)));
            edges.push((k + below(n - k), below(k)));
        }
    }
    let mut label: Vec<u32> = (0..n).collect();
    for i in (2..n as usize).rev() {
        let j = 1 + below(i as u32) as usize;
        label.swap(i, j);
    }
    for i in (1..edges.len()).rev() {
        let j = below(i as u32 + 1) as usize;
        edges.swap(i, j);
    }
    let edges: Vec<(u32, u32)> = edges
        .iter()
        .map(|&(a, b)| (label[a as usize], label[b as usize]))
        .collect();
    let defs = (0..3)
        .map(|_| (0..below(5)).map(|_| below(n)).collect())
        .collect();
    Case {
        n,
        edges,
        roots: vec![0],
        defs,
    }
}

/// Every shape the owner named, so none depends on the generator's luck.
pub fn shapes() -> Vec<(&'static str, Case)> {
    let complete: Vec<(u32, u32)> = (0..6)
        .flat_map(|i| (i + 1..6).map(move |j| (i, j)))
        .collect();
    let star_out: Vec<(u32, u32)> = (1..8).map(|i| (0, i)).collect();
    let star_in: Vec<(u32, u32)> = (1..7).flat_map(|i| [(0, i), (i, 7)]).collect();
    vec![
        ("empty", Case::new(0, &[], &[])),
        ("single", Case::new(1, &[], &[0])),
        ("self_loop", Case::new(1, &[(0, 0)], &[0])),
        (
            "chain",
            Case::new(5, &[(0, 1), (1, 2), (2, 3), (3, 4)], &[0]),
        ),
        (
            "duplicate_edges",
            Case::new(3, &[(0, 1), (0, 1), (1, 2), (1, 2), (0, 2), (0, 2)], &[0]),
        ),
        (
            "unreachable_component",
            Case::new(5, &[(0, 1), (2, 3), (3, 2), (3, 1)], &[0]),
        ),
        (
            "diamond",
            Case::new(4, &[(0, 1), (0, 2), (1, 3), (2, 3)], &[0]),
        ),
        (
            "ladder",
            Case::new(
                8,
                &[
                    (0, 1),
                    (0, 2),
                    (1, 3),
                    (2, 3),
                    (3, 4),
                    (3, 5),
                    (4, 6),
                    (5, 6),
                    (6, 7),
                ],
                &[0],
            ),
        ),
        (
            "error_ladder",
            Case::new(
                6,
                &[
                    (0, 1),
                    (1, 2),
                    (2, 3),
                    (3, 4),
                    (0, 5),
                    (1, 5),
                    (2, 5),
                    (3, 5),
                    (4, 5),
                ],
                &[0],
            ),
        ),
        (
            "nested_loops",
            Case::new(5, &[(0, 1), (1, 2), (2, 3), (3, 2), (3, 1), (1, 4)], &[0]),
        ),
        (
            "sibling_loops",
            Case::new(
                6,
                &[(0, 1), (1, 2), (2, 1), (1, 3), (3, 4), (4, 3), (3, 5)],
                &[0],
            ),
        ),
        (
            "irreducible",
            Case::new(4, &[(0, 1), (0, 2), (1, 2), (2, 1), (1, 3), (2, 3)], &[0]),
        ),
        (
            "back_edge_to_root",
            Case::new(4, &[(0, 1), (1, 2), (2, 0), (2, 3)], &[0]),
        ),
        (
            "root_on_a_pure_cycle",
            Case::new(3, &[(0, 1), (1, 2), (2, 0)], &[0]),
        ),
        (
            "pure_cycle_without_entry",
            Case::new(4, &[(0, 1), (2, 3), (3, 2)], &[0]),
        ),
        ("complete_dag", Case::new(6, &complete, &[0])),
        ("star_out", Case::new(8, &star_out, &[0])),
        ("star_in", Case::new(8, &star_in, &[0])),
        (
            "discovery_not_ascending",
            Case::new(3, &[(0, 2), (0, 1)], &[0]),
        ),
        (
            "multi_root",
            Case::new(6, &[(0, 1), (1, 2), (3, 4), (4, 2), (5, 5)], &[0, 3, 5]),
        ),
        (
            "later_root_reaches_earlier",
            Case::new(3, &[(2, 0)], &[0, 1, 2]),
        ),
        ("duplicate_root", Case::new(2, &[], &[1, 0, 1])),
    ]
}

/// `case` as a petgraph graph, keeping only edges between kept vertices.
pub fn petgraph_of(case: &Case, keep: &[bool]) -> DiGraph<(), ()> {
    let mut g: DiGraph<(), ()> = DiGraph::new();
    let nodes: Vec<NodeIndex> = (0..case.n).map(|_| g.add_node(())).collect();
    for &(a, b) in &case.edges {
        if keep[a as usize] && keep[b as usize] {
            g.add_edge(nodes[a as usize], nodes[b as usize], ());
        }
    }
    g
}
