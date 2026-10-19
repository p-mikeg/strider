//! Walks against brute-force reachability, cycle and root-order oracles.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use common::cases::{self, Case};
use common::{Graph, NodeId};
use graph_algorithms::oracle;
use graph_algorithms::walk::{PostOrder, PreOrder, WalkPhase, acyclic_postorder};
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

/// `Err` exactly when a cycle is reachable from the roots, naming a reachable
/// node on one; `Ok` is the walk's own post-order, each reachable node once,
/// after every successor. The cycle verdict is petgraph's over the reachable
/// part, checked against brute force while that stays cheap.
fn check_acyclic_postorder(c: &Case) {
    let g = Graph::from_edges(c.n, &c.edges);
    let succs = c.succs();
    let reach = oracle::reachable(&succs, &c.roots, None);
    let cyclic = petgraph::algo::toposort(&cases::petgraph_of(c, &reach), None).is_err();
    if c.n <= 200 {
        let brute = reach
            .iter()
            .enumerate()
            .any(|(v, &r)| r && oracle::reaches(&succs, v as u32, v as u32));
        assert_eq!(brute, cyclic, "petgraph's cycle verdict, {c:?}");
    }
    match acyclic_postorder(&g, ids(&c.roots)) {
        Err(node) => {
            let v = node.as_u32();
            assert!(cyclic, "Err({v}) with no reachable cycle, {c:?}");
            assert!(reach[v as usize], "Err({v}) is unreachable, {c:?}");
            assert!(
                succs[v as usize]
                    .iter()
                    .any(|&s| oracle::reaches(&succs, s, v)),
                "Err({v}) has no successor on a cycle back to it, {c:?}"
            );
        }
        Ok(order) => {
            assert!(!cyclic, "Ok on a reachable cycle, {c:?}");
            let order: Vec<u32> = order.into_iter().map(NodeId::as_u32).collect();
            let posts: Vec<u32> = PostOrder::new(&g, ids(&c.roots))
                .map(NodeId::as_u32)
                .collect();
            assert_eq!(order, posts, "the walk's post-order, {c:?}");
            let mut pos = vec![usize::MAX; reach.len()];
            for (i, &v) in order.iter().enumerate() {
                assert_eq!(pos[v as usize], usize::MAX, "{v} twice in {order:?}");
                pos[v as usize] = i;
            }
            for (v, &r) in reach.iter().enumerate() {
                assert_eq!(pos[v] != usize::MAX, r, "{v} ordered iff reachable, {c:?}");
            }
            for &(u, v) in &c.edges {
                if reach[u as usize] {
                    assert!(
                        pos[v as usize] < pos[u as usize],
                        "{v} after its predecessor {u}, {c:?}"
                    );
                }
            }
        }
    }
}

/// `n` vertices under a hidden random order whose first vertex is the root
/// (label 0): each later one of a reachable prefix hangs off an earlier one,
/// half the time the one just before, so chains run deep; about as many extra
/// forward edges again, duplicates among them; and an unreachable tail whose
/// edges point into the prefix. A DAG, labels and edge order shuffled. `back`
/// adds one edge from a reachable vertex to the root, which reaches it: one
/// cycle.
fn deep_case(n: u32, seed: u64, back: bool) -> Case {
    let mut state = seed | 1;
    let mut below = |bound: u32| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % u64::from(bound)) as u32
    };
    let k = n - below(n / 10 + 1);
    let mut label: Vec<u32> = (0..n).collect();
    for i in (2..n as usize).rev() {
        let j = 1 + below(i as u32) as usize;
        label.swap(i, j);
    }
    let mut edges = Vec::new();
    for i in 1..k {
        let parent = if below(2) == 0 { i - 1 } else { below(i) };
        edges.push((label[parent as usize], label[i as usize]));
    }
    for _ in 0..k {
        let (a, b) = (below(k), below(k));
        if a < b {
            edges.push((label[a as usize], label[b as usize]));
        }
    }
    for i in k..n {
        edges.push((label[i as usize], label[below(k) as usize]));
    }
    if back {
        edges.push((label[1 + below(k - 1) as usize], label[0]));
    }
    for i in (1..edges.len()).rev() {
        let j = below(i as u32 + 1) as usize;
        edges.swap(i, j);
    }
    Case {
        n,
        edges,
        roots: vec![label[0]],
        defs: Vec::new(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn walks_match_their_definitions(c in cases::arb_case(10)) {
        check_postorder(&c);
        check_preorder(&c);
        check_acyclic_postorder(&c);
    }
}

#[test]
fn walks_match_their_definitions_on_every_named_shape() {
    for (name, c) in cases::shapes() {
        println!("shape {name}");
        check_postorder(&c);
        check_preorder(&c);
        check_acyclic_postorder(&c);
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    #[cfg_attr(debug_assertions, ignore = "release-only scale test")]
    fn acyclic_postorder_matches_on_deep_graphs(
        n in 50u32..=2000,
        seed in any::<u64>(),
        back in any::<bool>(),
    ) {
        let c = deep_case(n, seed, back);
        check_acyclic_postorder(&c);
        let g = Graph::from_edges(c.n, &c.edges);
        prop_assert_eq!(acyclic_postorder(&g, ids(&c.roots)).is_err(), back);
    }
}

/// Successors asked twice per reached node, once by the walk and once for
/// the back-edge check: a chain and a star of a million nodes order, and a
/// million-node cycle is refused.
#[test]
#[cfg_attr(debug_assertions, ignore = "release-only scale test")]
fn acyclic_postorder_is_linear_on_million_node_graphs() {
    const N: u32 = 1_000_000;
    let chain: Vec<(u32, u32)> = (1..N).map(|v| (v - 1, v)).collect();
    let star: Vec<(u32, u32)> = (1..N).map(|v| (0, v)).collect();
    let cycle: Vec<(u32, u32)> = (0..N).map(|v| (v, (v + 1) % N)).collect();
    for edges in [chain, star] {
        let g = Graph::from_edges(N, &edges);
        g.set_succ_budget(2 * u64::from(N));
        let order = acyclic_postorder(&g, ids(&[0])).expect("acyclic");
        assert_eq!(order.len(), N as usize);
        assert_eq!(order.last(), Some(&NodeId::from_u32(0)), "the root last");
        assert_eq!(g.succ_calls(), 2 * u64::from(N));
    }
    let g = Graph::from_edges(N, &cycle);
    g.set_succ_budget(2 * u64::from(N));
    assert_eq!(
        acyclic_postorder(&g, ids(&[0])),
        Err(NodeId::from_u32(N - 1))
    );
}
