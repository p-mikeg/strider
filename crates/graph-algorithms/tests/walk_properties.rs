//! Walks against brute-force reachability, cycle and root-order oracles.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use common::cases::{self, Case};
use common::oracle;
use common::{Graph, NodeId};
use graph_algorithms::walk::{PostOrder, PreOrder, WalkPhase};
use proptest::prelude::*;

const CASES: u32 = if cfg!(debug_assertions) { 256 } else { 8192 };

fn ids(roots: &[u32]) -> Vec<NodeId> {
    roots.iter().map(|&r| NodeId::from_u32(r)).collect()
}

/// Brackets, tree parents, coverage, edge classes, topological order on a
/// DAG, root order.
fn check_postorder(c: &Case) {
    let g = Graph::from_edges(c.n, &c.edges);
    let succs = c.succs();
    let reach = oracle::reachable(&succs, &c.roots, None);
    let mut walk = PostOrder::new(&g, ids(&c.roots));
    let events: Vec<(WalkPhase, u32)> = std::iter::from_fn(|| walk.next_event())
        .map(|(p, v)| (p, v.as_u32()))
        .collect();
    let mut pre = vec![usize::MAX; reach.len()];
    let mut post = vec![usize::MAX; reach.len()];
    let mut open: Vec<u32> = Vec::new();
    for (i, &(phase, v)) in events.iter().enumerate() {
        let at = v as usize;
        match phase {
            WalkPhase::Pre => {
                match open.last() {
                    None => assert!(c.roots.contains(&v), "Pre({v}) of a non-root, {c:?}"),
                    Some(&u) => assert!(
                        succs[u as usize].contains(&v),
                        "Pre({v}) under {u}, which has no edge to it, {c:?}"
                    ),
                }
                assert_eq!(pre[at], usize::MAX, "Pre({v}) twice in {events:?}");
                pre[at] = i;
                open.push(v);
            }
            WalkPhase::Post => {
                assert_eq!(post[at], usize::MAX, "Post({v}) twice in {events:?}");
                post[at] = i;
                assert_eq!(
                    open.pop(),
                    Some(v),
                    "Post({v}) closes another Pre in {events:?}"
                );
            }
        }
    }
    assert!(open.is_empty(), "an unclosed Pre in {events:?}");
    for (v, &r) in reach.iter().enumerate() {
        assert_eq!(
            pre[v] != usize::MAX,
            r,
            "node {v} entered iff reachable, {c:?}"
        );
        assert_eq!(
            post[v] != usize::MAX,
            r,
            "node {v} left iff reachable, {c:?}"
        );
    }
    let posts: Vec<u32> = PostOrder::new(&g, ids(&c.roots))
        .map(NodeId::as_u32)
        .collect();
    let from_events: Vec<u32> = events
        .iter()
        .filter(|(p, _)| *p == WalkPhase::Post)
        .map(|&(_, v)| v)
        .collect();
    assert_eq!(posts, from_events, "the iterator is the Post events, {c:?}");
    for &(u, v) in &c.edges {
        let (u, v) = (u as usize, v as usize);
        if !reach[u] {
            continue;
        }
        let back = u == v || (pre[v] < pre[u] && post[u] < post[v]);
        assert!(
            post[v] < post[u] || back,
            "edge {u}->{v} neither finished nor back, {c:?}"
        );
        if back {
            assert!(
                oracle::reaches(&succs, v as u32, u as u32),
                "back edge {u}->{v} on no cycle, {c:?}"
            );
        }
    }
    let cyclic = reach
        .iter()
        .enumerate()
        .any(|(v, &r)| r && oracle::reaches(&succs, v as u32, v as u32));
    let pg = cases::petgraph_of(c, &reach);
    assert_eq!(
        petgraph::algo::toposort(&pg, None).is_err(),
        cyclic,
        "cycle verdict, {c:?}"
    );
    if !cyclic {
        let mut rank = vec![usize::MAX; reach.len()];
        for (i, &v) in posts.iter().rev().enumerate() {
            rank[v as usize] = i;
        }
        for &(u, v) in &c.edges {
            if reach[u as usize] {
                assert!(
                    rank[u as usize] < rank[v as usize],
                    "RPO puts {v} before {u}, {c:?}"
                );
            }
        }
    }
    for (i, &u) in c.roots.iter().enumerate() {
        let later = &c.roots[i + 1..];
        if later
            .iter()
            .any(|&w| w == u || oracle::reaches(&succs, w, u))
        {
            continue;
        }
        for &v in later {
            assert!(
                post[v as usize] < post[u as usize],
                "root {u} after later root {v} in the RPO, {c:?}"
            );
        }
    }
}

/// Coverage, predecessor order, root order.
fn check_preorder(c: &Case) {
    let g = Graph::from_edges(c.n, &c.edges);
    let succs = c.succs();
    let preds = oracle::preds_of(&succs);
    let reach = oracle::reachable(&succs, &c.roots, None);
    let mut walk = PreOrder::new(&g, ids(&c.roots));
    let order: Vec<u32> = walk.by_ref().map(NodeId::as_u32).collect();
    let visited = walk.into_visited();
    let mut pos = vec![usize::MAX; reach.len()];
    for (i, &v) in order.iter().enumerate() {
        assert_eq!(pos[v as usize], usize::MAX, "{v} twice in {order:?}");
        pos[v as usize] = i;
    }
    for (v, &r) in reach.iter().enumerate() {
        assert_eq!(
            pos[v] != usize::MAX,
            r,
            "node {v} yielded iff reachable, {c:?}"
        );
        assert_eq!(
            visited.contains(NodeId::from_u32(v as u32)),
            r,
            "visited({v}), {c:?}"
        );
    }
    if let Some(&last) = c.roots.last() {
        assert_eq!(
            order.first(),
            Some(&last),
            "the last root comes first, {c:?}"
        );
    }
    for (i, &v) in order.iter().enumerate() {
        let fed = preds[v as usize].iter().any(|&p| pos[p as usize] < i);
        assert!(
            c.roots.contains(&v) || fed,
            "{v} has no earlier predecessor in {order:?}"
        );
    }
    for (i, &u) in c.roots.iter().enumerate() {
        let later = &c.roots[i + 1..];
        if later
            .iter()
            .any(|&w| w == u || oracle::reaches(&succs, w, u))
        {
            continue;
        }
        for &v in later {
            assert!(
                pos[v as usize] < pos[u as usize],
                "root {u} before later root {v}, {c:?}"
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn walks_match_their_definitions(c in cases::arb_case(10)) {
        check_postorder(&c);
        check_preorder(&c);
    }
}

#[test]
fn walks_match_their_definitions_on_every_named_shape() {
    for (name, c) in cases::shapes() {
        println!("shape {name}");
        check_postorder(&c);
        check_preorder(&c);
    }
}

/// A later root's walk claims an earlier root it reaches.
#[test]
fn a_later_root_claims_an_earlier_root_it_reaches() {
    let g = Graph::from_edges(3, &[(2, 0)]);
    let pre: Vec<u32> = PreOrder::new(&g, ids(&[0, 1, 2]))
        .map(NodeId::as_u32)
        .collect();
    assert_eq!(pre, [2, 0, 1]);
    let mut rpo: Vec<u32> = PostOrder::new(&g, ids(&[0, 1, 2]))
        .map(NodeId::as_u32)
        .collect();
    rpo.reverse();
    assert_eq!(rpo, [1, 2, 0]);
}

/// Each walk asks every reached node for its successors exactly once, on a
/// chain, a star and a cycle of a million nodes.
#[test]
#[cfg_attr(debug_assertions, ignore = "release-only scale test")]
fn walks_ask_each_node_once_on_million_node_graphs() {
    const N: u32 = 1_000_000;
    let chain: Vec<(u32, u32)> = (1..N).map(|v| (v - 1, v)).collect();
    let star: Vec<(u32, u32)> = (1..N).map(|v| (0, v)).collect();
    let cycle: Vec<(u32, u32)> = (0..N).map(|v| (v, (v + 1) % N)).collect();
    for edges in [chain, star, cycle] {
        let g = Graph::from_edges(N, &edges);
        g.set_succ_budget(2 * u64::from(N));
        assert_eq!(PostOrder::new(&g, ids(&[0])).count(), N as usize);
        assert_eq!(g.succ_calls(), u64::from(N));
        assert_eq!(PreOrder::new(&g, ids(&[0])).count(), N as usize);
        assert_eq!(g.succ_calls(), 2 * u64::from(N));
    }
}
