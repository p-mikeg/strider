//! The layout's graph pieces against their definitions, on index graphs.

use smallvec::SmallVec;
use strider_ir_test_utils::graph_oracle;

use super::{DepthFirst, LoopForest, NONE};
use crate::test_support::Rng;

const CASES: usize = if cfg!(debug_assertions) { 400 } else { 20_000 };

/// Up to `max` nodes, node 0 the root with no edge into it (as
/// `InitialMemory`), every node reachable: a random tree plus extra edges.
fn random_graph(rng: &mut Rng, max: usize) -> Vec<Vec<u32>> {
    let n = 1 + rng.below(max);
    let mut succs = vec![Vec::new(); n];
    for v in 1..n {
        let u = rng.below(v);
        succs[u].push(v as u32);
    }
    if n > 1 {
        for _ in 0..rng.below(2 * n) {
            let u = rng.below(n);
            let v = 1 + rng.below(n - 1);
            succs[u].push(v as u32);
        }
    }
    succs
}

/// A structured graph: sequences, two-way branches and loops nested up to
/// four deep, left from the latch or from both header and latch; one graph
/// in four gains an extra edge, which may make it irreducible.
fn nested_graph(rng: &mut Rng) -> Vec<Vec<u32>> {
    fn fresh(succs: &mut Vec<Vec<u32>>, pred: u32) -> u32 {
        succs.push(Vec::new());
        let v = succs.len() as u32 - 1;
        succs[pred as usize].push(v);
        v
    }
    fn block(rng: &mut Rng, succs: &mut Vec<Vec<u32>>, from: u32, depth: u32) -> u32 {
        let mut at = from;
        for _ in 0..=rng.below(3) {
            at = match rng.below(5) {
                0 | 1 if depth < 4 => {
                    let header = fresh(succs, at);
                    let latch = block(rng, succs, header, depth + 1);
                    succs[latch as usize].push(header);
                    if rng.below(2) == 0 {
                        latch
                    } else {
                        let exit = fresh(succs, header);
                        succs[latch as usize].push(exit);
                        exit
                    }
                }
                2 if depth < 4 => {
                    let split = fresh(succs, at);
                    let left = block(rng, succs, split, depth + 1);
                    let right = block(rng, succs, split, depth + 1);
                    let join = fresh(succs, left);
                    succs[right as usize].push(join);
                    join
                }
                _ => fresh(succs, at),
            };
        }
        at
    }
    let mut succs = vec![Vec::new()];
    block(rng, &mut succs, 0, 0);
    let n = succs.len();
    if n > 1 && rng.below(4) == 0 {
        let u = rng.below(n);
        let v = 1 + rng.below(n - 1);
        succs[u].push(v as u32);
    }
    succs
}

fn small(lists: &[Vec<u32>]) -> Vec<SmallVec<[u32; 2]>> {
    lists.iter().map(|l| l.iter().copied().collect()).collect()
}

/// What one checked graph exercised.
#[derive(Default, Debug)]
struct Tally {
    graphs: usize,
    irreducible: usize,
    looped: usize,
    /// A loop inside another loop.
    nested: usize,
    /// Two loops with distinct headers, neither inside the other.
    siblings: usize,
    self_loop: usize,
    multi_edge: usize,
    /// A loop with an exit edge from a node other than its header.
    exit_from_body: usize,
    deepest: usize,
    largest: usize,
}

/// Checks one graph and records its shape.
fn check(succs: &[Vec<u32>], tally: &mut Tally) {
    let n = succs.len();
    tally.graphs += 1;
    tally.largest = tally.largest.max(n);
    let preds = graph_oracle::preds_of(succs);
    let (s_succs, s_preds) = (small(succs), small(&preds));
    let dfs = DepthFirst::from_root(&s_succs);
    let (order, parent) = graph_oracle::dfs_tree(succs, 0);
    assert_eq!(dfs.vertex, order, "{succs:?}");
    for (v, &d) in dfs.num.iter().enumerate() {
        assert_eq!(order[d as usize], v as u32, "{succs:?}");
    }
    assert_eq!(&dfs.parent[1..], &parent[1..], "{succs:?}");
    for a in 0..n as u32 {
        for b in 0..n as u32 {
            let mut at = b;
            let mut above = at == a;
            while !above && at != 0 {
                at = parent[at as usize];
                above = at == a;
            }
            assert_eq!(dfs.is_ancestor(a, b), above, "{a} above {b} in {succs:?}");
        }
    }
    if !graph_oracle::is_reducible(succs, 0) {
        tally.irreducible += 1;
        return;
    }
    let dom = graph_oracle::dom_matrix(succs, 0);
    let forest = LoopForest::new(&s_preds, &dfs);
    let bodies: Vec<Option<Vec<bool>>> = (0..n as u32)
        .map(|h| graph_oracle::natural_loop(succs, &dom, h))
        .collect();
    let size = |h: usize| {
        bodies[h]
            .as_ref()
            .map_or(0, |b| b.iter().filter(|&&x| x).count())
    };
    for (x, body) in bodies.iter().enumerate() {
        assert_eq!(
            forest.is_header[x],
            body.is_some(),
            "header {x} in {succs:?}"
        );
        let innermost = (0..n)
            .filter(|&h| h != x && bodies[h].as_ref().is_some_and(|b| b[x]))
            .min_by_key(|&h| size(h));
        assert_eq!(
            forest.parent[x],
            innermost.map_or(NONE, |h| h as u32),
            "loop of {x} in {succs:?}"
        );
    }
    let (order, block_end) = forest
        .nested_order(&s_preds, &dfs, 0)
        .expect("a reducible graph has a nested order");
    let mut pos = vec![NONE; n];
    for (p, &v) in order.iter().enumerate() {
        assert_eq!(pos[v as usize], NONE, "{v} placed twice in {order:?}");
        pos[v as usize] = p as u32;
    }
    assert!(!pos.contains(&NONE), "every node placed: {order:?}");
    for (u, ss) in succs.iter().enumerate() {
        for &v in ss {
            if !dom[v as usize][u] {
                assert!(
                    pos[u] < pos[v as usize],
                    "forward edge {u}->{v} runs back in {order:?}"
                );
            }
        }
    }
    for (v, d) in graph_oracle::idoms(&dom, 0).iter().enumerate() {
        if let Some(d) = d {
            assert!(pos[*d as usize] < pos[v], "idom {d} after {v} in {order:?}");
        }
    }
    for (h, body) in bodies.iter().enumerate() {
        let Some(body) = body else {
            continue;
        };
        let block: Vec<bool> = pos
            .iter()
            .map(|&p| pos[h] <= p && p <= block_end[h])
            .collect();
        assert_eq!(
            &block, body,
            "loop of {h} is not one block from it in {order:?}"
        );
    }

    let headers: Vec<usize> = (0..n).filter(|&h| bodies[h].is_some()).collect();
    tally.looped += usize::from(!headers.is_empty());
    let depth = (0..n)
        .map(|x| {
            headers
                .iter()
                .filter(|&&h| bodies[h].as_ref().unwrap()[x])
                .count()
        })
        .max()
        .unwrap_or(0);
    tally.deepest = tally.deepest.max(depth);
    tally.nested += usize::from(depth >= 2);
    let inside = |a: usize, b: usize| bodies[b].as_ref().unwrap()[a];
    tally.siblings += usize::from(headers.iter().any(|&a| {
        headers
            .iter()
            .any(|&b| a != b && !inside(a, b) && !inside(b, a))
    }));
    tally.self_loop += usize::from((0..n).any(|v| succs[v].contains(&(v as u32))));
    tally.multi_edge += usize::from(succs.iter().any(|ss| {
        let mut sorted = ss.clone();
        sorted.sort_unstable();
        sorted.windows(2).any(|w| w[0] == w[1])
    }));
    tally.exit_from_body += usize::from(headers.iter().any(|&h| {
        let body = bodies[h].as_ref().unwrap();
        (0..n).any(|u| u != h && body[u] && succs[u].iter().any(|&v| !body[v as usize]))
    }));
}

#[test]
fn layout_pieces_match_their_definitions() {
    let named: [&[(u32, u32)]; 5] = [
        &[(0, 1), (1, 2), (2, 3), (3, 2), (3, 1), (1, 4)],
        &[(0, 1), (1, 2), (2, 1), (1, 3), (3, 4), (4, 3), (3, 5)],
        &[(0, 1), (1, 1), (1, 2)],
        &[(0, 1), (1, 2), (2, 3), (3, 1), (2, 4)],
        &[(0, 1), (1, 2), (1, 3), (2, 4), (3, 4), (4, 1), (4, 5)],
    ];
    for edges in named {
        let n = edges.iter().map(|&(a, b)| a.max(b) + 1).max().unwrap_or(1) as usize;
        let mut succs = vec![Vec::new(); n];
        for &(a, b) in edges {
            succs[a as usize].push(b);
        }
        let mut tally = Tally::default();
        check(&succs, &mut tally);
        assert_eq!(tally.looped, 1, "{edges:?} is reducible with a loop");
    }
    let mut rng = Rng(0x5eed_1a70);
    let mut tally = Tally::default();
    for i in 0..CASES {
        let succs = match i % 3 {
            0 => random_graph(&mut rng, 9),
            1 => random_graph(&mut rng, 20),
            _ => nested_graph(&mut rng),
        };
        check(&succs, &mut tally);
    }
    let floor = CASES / 50;
    let Tally {
        irreducible,
        looped,
        nested,
        siblings,
        self_loop,
        multi_edge,
        exit_from_body,
        deepest,
        ..
    } = tally;
    assert!(
        [
            irreducible,
            looped,
            nested,
            siblings,
            self_loop,
            multi_edge,
            exit_from_body
        ]
        .iter()
        .all(|&c| c >= floor)
            && deepest >= 4,
        "too few shapes, floor {floor}: {tally:?}"
    );
}
