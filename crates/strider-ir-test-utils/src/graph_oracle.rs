//! Brute-force graph definitions over index graphs, where `succs[v]` lists
//! `v`'s successors in edge order. Quadratic or worse: small graphs only.

use std::collections::BTreeSet;

/// `succs` inverted, duplicate edges kept.
pub fn preds_of(succs: &[Vec<u32>]) -> Vec<Vec<u32>> {
    let mut preds = vec![Vec::new(); succs.len()];
    for (u, ss) in succs.iter().enumerate() {
        for &v in ss {
            preds[v as usize].push(u as u32);
        }
    }
    preds
}

/// What `roots` reach without entering `removed`.
pub fn reachable(succs: &[Vec<u32>], roots: &[u32], removed: Option<u32>) -> Vec<bool> {
    let mut seen = vec![false; succs.len()];
    let mut stack: Vec<u32> = roots
        .iter()
        .copied()
        .filter(|&r| Some(r) != removed)
        .collect();
    while let Some(v) = stack.pop() {
        if std::mem::replace(&mut seen[v as usize], true) {
            continue;
        }
        stack.extend(
            succs[v as usize]
                .iter()
                .copied()
                .filter(|&w| Some(w) != removed),
        );
    }
    seen
}

/// Whether a path of one edge or more runs from `a` to `b`.
pub fn reaches(succs: &[Vec<u32>], a: u32, b: u32) -> bool {
    reachable(succs, &succs[a as usize], None)[b as usize]
}

/// `dom[d][v]`: `v` is reachable from `root` and every path to it passes `d`.
/// Reflexive on reachable vertices; an unreachable `d` dominates nothing.
pub fn dom_matrix(succs: &[Vec<u32>], root: u32) -> Vec<Vec<bool>> {
    let reach = reachable(succs, &[root], None);
    (0..succs.len())
        .map(|d| {
            let without = reachable(succs, &[root], Some(d as u32));
            reach
                .iter()
                .zip(&without)
                .enumerate()
                .map(|(v, (&r, &w))| r && (v == d || !w))
                .collect()
        })
        .collect()
}

/// The strict dominator that every other strict dominator dominates; `None`
/// for `root` and for unreachable vertices.
pub fn idoms(dom: &[Vec<bool>], root: u32) -> Vec<Option<u32>> {
    (0..dom.len())
        .map(|v| {
            if v == root as usize || !dom[root as usize][v] {
                return None;
            }
            let strict: Vec<usize> = (0..dom.len()).filter(|&d| d != v && dom[d][v]).collect();
            let closest: Vec<usize> = strict
                .iter()
                .copied()
                .filter(|&d| strict.iter().all(|&o| dom[o][d]))
                .collect();
            assert_eq!(
                closest.len(),
                1,
                "vertex {v} has one closest strict dominator"
            );
            Some(closest[0] as u32)
        })
        .collect()
}

/// `b` is in `DF(x)` when `x` dominates a predecessor of `b` and does not
/// strictly dominate `b`. An unreachable `x` dominates nothing; its frontier
/// is the reachable vertices it has an edge into.
pub fn frontiers(succs: &[Vec<u32>], dom: &[Vec<bool>], root: u32) -> Vec<BTreeSet<u32>> {
    let reach = &dom[root as usize];
    let mut df = vec![BTreeSet::new(); succs.len()];
    for (p, ss) in succs.iter().enumerate() {
        for &b in ss {
            if !reach[b as usize] {
                continue;
            }
            if !reach[p] {
                df[p].insert(b);
                continue;
            }
            for (x, row) in dom.iter().enumerate() {
                if row[p] && (x == b as usize || !row[b as usize]) {
                    df[x].insert(b);
                }
            }
        }
    }
    df
}

/// The least `J` with `J = DF(defs ∪ J)`.
pub fn iterated_frontier(df: &[BTreeSet<u32>], defs: &[u32]) -> BTreeSet<u32> {
    let mut j = BTreeSet::new();
    loop {
        let next: BTreeSet<u32> = defs
            .iter()
            .chain(&j)
            .flat_map(|&x| df[x as usize].iter().copied())
            .collect();
        if next == j {
            return j;
        }
        j = next;
    }
}

/// Depth-first preorder from `root`, successors taken in edge order, and each
/// vertex's tree parent by preorder number (the root's is 0).
pub fn dfs_tree(succs: &[Vec<u32>], root: u32) -> (Vec<u32>, Vec<u32>) {
    let mut num = vec![u32::MAX; succs.len()];
    let mut order = vec![root];
    let mut parent = vec![0];
    num[root as usize] = 0;
    let mut stack = vec![(root, 0usize)];
    while let Some(top) = stack.last_mut() {
        let (v, i) = *top;
        let Some(&w) = succs[v as usize].get(i) else {
            stack.pop();
            continue;
        };
        top.1 += 1;
        if num[w as usize] == u32::MAX {
            num[w as usize] = order.len() as u32;
            parent.push(num[v as usize]);
            order.push(w);
            stack.push((w, 0));
        }
    }
    (order, parent)
}

/// Preorder of the tree `idom` describes, from `root`, siblings by `key`.
pub fn tree_preorder(idom: &[Option<u32>], root: u32, key: impl Fn(u32) -> usize) -> Vec<u32> {
    let mut children: Vec<Vec<u32>> = vec![Vec::new(); idom.len()];
    for (v, &d) in idom.iter().enumerate() {
        if let Some(d) = d {
            children[d as usize].push(v as u32);
        }
    }
    let mut order = Vec::new();
    let mut stack = vec![root];
    while let Some(v) = stack.pop() {
        order.push(v);
        let mut kids = std::mem::take(&mut children[v as usize]);
        kids.sort_by_key(|&k| std::cmp::Reverse(key(k)));
        stack.extend(kids);
    }
    order
}

/// `h` plus every vertex `h` dominates that reaches a back edge into `h`
/// without passing `h`; `None` when no edge into `h` comes from a vertex `h`
/// dominates.
pub fn natural_loop(succs: &[Vec<u32>], dom: &[Vec<bool>], h: u32) -> Option<Vec<bool>> {
    let preds = preds_of(succs);
    let row = &dom[h as usize];
    let latches: Vec<u32> = preds[h as usize]
        .iter()
        .copied()
        .filter(|&u| row[u as usize])
        .collect();
    if latches.is_empty() {
        return None;
    }
    let mut body = vec![false; succs.len()];
    body[h as usize] = true;
    let mut stack = latches;
    while let Some(v) = stack.pop() {
        if !std::mem::replace(&mut body[v as usize], true) {
            stack.extend(
                preds[v as usize]
                    .iter()
                    .copied()
                    .filter(|&p| row[p as usize]),
            );
        }
    }
    Some(body)
}

/// No cycle survives dropping every edge into a dominator of its source.
pub fn is_reducible(succs: &[Vec<u32>], root: u32) -> bool {
    let dom = dom_matrix(succs, root);
    let forward: Vec<Vec<u32>> = succs
        .iter()
        .enumerate()
        .map(|(u, ss)| {
            ss.iter()
                .copied()
                .filter(|&v| !dom[v as usize][u])
                .collect()
        })
        .collect();
    (0..succs.len() as u32).all(|v| !dom[root as usize][v as usize] || !reaches(&forward, v, v))
}
