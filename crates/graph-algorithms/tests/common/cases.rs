//! Generated and named graphs for the walk and dominance properties.

use petgraph::graph::{DiGraph, NodeIndex};
use proptest::prelude::*;

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
/// unreachable vertices, irreducible and rootless cycles all arise.
pub fn arb_case(max_n: u32) -> impl Strategy<Value = Case> {
    (1..=max_n)
        .prop_flat_map(|n| {
            (
                Just(n),
                prop::collection::vec((0..n, 0..n), 0..=3 * n as usize),
                prop::collection::vec(0..n, 0..=3),
                prop::collection::vec(prop::collection::vec(0..n, 0..=4), 3),
            )
        })
        .prop_map(|(n, edges, roots, defs)| Case {
            n,
            edges,
            roots,
            defs,
        })
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
