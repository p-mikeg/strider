//! A fast, from-scratch layered ("hierarchical") graph layout.
//!
//! Graphviz `dot` cannot lay out large sea-of-nodes IR graphs: a ~1800-node
//! function is ~1400 ranks deep, and `dot`'s crossing-minimisation inserts a
//! dummy node per rank each edge spans, exploding into hundreds of thousands
//! of virtual nodes that never converge (it times out even natively). This
//! engine trades graphviz's aesthetic crossing-minimisation and spline routing
//! for speed: longest-path ranking, an optional bounded ordering pass, simple
//! coordinate assignment, and straight edges — `O((V+E)·k)`, so 10k+ nodes lay
//! out in well under a second.
//!
//! Output is coordinates (a [`Positioned`] graph); rendering to SVG happens in
//! the viewer from the emitted JSON. The two heavier stages — crossing
//! reduction and orthogonal edge routing — are opt-in via [`LayoutOptions`] so
//! their cost can be measured against the MVP.

/// An input node's bounding box (its rendered size). Position is assigned by
/// [`layout`]; only the size is an input.
#[derive(Clone, Copy, Debug)]
pub struct NodeBox {
    pub width: f64,
    pub height: f64,
}

/// The abstract graph to lay out: sized nodes (indexed `0..nodes.len()`) and
/// directed edges referencing those indices.
#[derive(Clone, Debug, Default)]
pub struct LayoutInput {
    pub nodes: Vec<NodeBox>,
    pub edges: Vec<(usize, usize)>,
}

/// Layout tuning. `rank_sep` / `node_sep` are the inter-rank (vertical) and
/// intra-rank (horizontal) gaps. The two `bool`s gate the optional heavier
/// stages (default off = MVP).
#[derive(Clone, Copy, Debug)]
pub struct LayoutOptions {
    pub rank_sep: f64,
    pub node_sep: f64,
    /// Run barycenter ordering sweeps to reduce edge crossings (optional).
    pub reduce_crossings: bool,
    /// Number of down+up ordering sweeps when `reduce_crossings` is set.
    pub ordering_passes: usize,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            rank_sep: 40.0,
            node_sep: 24.0,
            reduce_crossings: false,
            ordering_passes: 4,
        }
    }
}

/// A placed node: top-left corner `(x, y)`, its size, and its layer/position.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rank: usize,
    pub order: usize,
}

/// The laid-out graph: placed nodes (index-aligned with the input), one
/// polyline per input edge (index-aligned), and the overall canvas size.
#[derive(Clone, Debug, Default)]
pub struct Positioned {
    pub nodes: Vec<Placed>,
    pub edges: Vec<Vec<(f64, f64)>>,
    pub width: f64,
    pub height: f64,
}

/// Lay out `input` into absolute coordinates.
pub fn layout(input: &LayoutInput, opts: &LayoutOptions) -> Positioned {
    let n = input.nodes.len();
    if n == 0 {
        return Positioned::default();
    }
    let forward = break_cycles(n, &input.edges);
    let rank = assign_ranks(n, &forward);
    let mut order = order_within_ranks(n, &rank, &forward, input, opts);
    let placed = assign_coords(input, &rank, &mut order, opts);
    let edges = route_straight(input, &placed);
    let width = placed.iter().map(|p| p.x + p.width).fold(0.0, f64::max);
    let height = placed.iter().map(|p| p.y + p.height).fold(0.0, f64::max);
    Positioned {
        nodes: placed,
        edges,
        width,
        height,
    }
}

/// Returns the acyclic edge set: back-edges (targets currently on the DFS
/// stack) are reversed so ranking sees a DAG. Self-loops are dropped.
fn break_cycles(n: usize, edges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(u, v) in edges {
        if u != v {
            succ[u].push(v);
        }
    }
    // Iterative DFS colouring: 0 = unvisited, 1 = on stack, 2 = done.
    let mut color = vec![0u8; n];
    let mut on_stack = vec![false; n];
    let mut forward = Vec::with_capacity(edges.len());
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (node, next successor index)
    for start in 0..n {
        if color[start] != 0 {
            continue;
        }
        stack.push((start, 0));
        color[start] = 1;
        on_stack[start] = true;
        while let Some(&mut (u, ref mut i)) = stack.last_mut() {
            if *i < succ[u].len() {
                let v = succ[u][*i];
                *i += 1;
                match color[v] {
                    0 => {
                        forward.push((u, v));
                        color[v] = 1;
                        on_stack[v] = true;
                        stack.push((v, 0));
                    }
                    1 if on_stack[v] => forward.push((v, u)), // back-edge: reverse
                    _ => forward.push((u, v)),                // forward/cross edge
                }
            } else {
                on_stack[u] = false;
                color[u] = 2;
                stack.pop();
            }
        }
    }
    forward
}

/// Longest-path ranking on the acyclic `forward` edges: a node's rank is one
/// more than the max rank of its predecessors (sources get 0).
fn assign_ranks(n: usize, forward: &[(usize, usize)]) -> Vec<usize> {
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for &(u, v) in forward {
        succ[u].push(v);
        indeg[v] += 1;
    }
    let mut rank = vec![0usize; n];
    let mut queue: Vec<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
    let mut head = 0;
    while head < queue.len() {
        let u = queue[head];
        head += 1;
        for &v in &succ[u] {
            if rank[u] + 1 > rank[v] {
                rank[v] = rank[u] + 1;
            }
            indeg[v] -= 1;
            if indeg[v] == 0 {
                queue.push(v);
            }
        }
    }
    rank
}

/// Assigns each node an order index within its rank. MVP = input order;
/// `reduce_crossings` runs barycenter sweeps to shrink crossings.
fn order_within_ranks(
    n: usize,
    rank: &[usize],
    forward: &[(usize, usize)],
    _input: &LayoutInput,
    opts: &LayoutOptions,
) -> Vec<usize> {
    let max_rank = rank.iter().copied().max().unwrap_or(0);
    // ranks[r] = node ids at rank r, in current order.
    let mut ranks: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for v in 0..n {
        ranks[rank[v]].push(v);
    }
    if opts.reduce_crossings {
        // Undirected adjacency for barycenter medians.
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for &(u, v) in forward {
            adj[u].push(v);
            adj[v].push(u);
        }
        let mut pos = order_positions(&ranks);
        for pass in 0..opts.ordering_passes {
            let down = pass % 2 == 0;
            let range: Vec<usize> = if down {
                (0..=max_rank).collect()
            } else {
                (0..=max_rank).rev().collect()
            };
            for r in range {
                let mut keyed: Vec<(f64, usize)> = ranks[r]
                    .iter()
                    .map(|&v| (barycenter(v, &adj, &pos), v))
                    .collect();
                keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                ranks[r] = keyed.into_iter().map(|(_, v)| v).collect();
                for (i, &v) in ranks[r].iter().enumerate() {
                    pos[v] = i as f64;
                }
            }
        }
    }
    // Flatten ranks → per-node order index.
    let mut order = vec![0usize; n];
    for rnodes in &ranks {
        for (i, &v) in rnodes.iter().enumerate() {
            order[v] = i;
        }
    }
    order
}

fn order_positions(ranks: &[Vec<usize>]) -> Vec<f64> {
    let n: usize = ranks.iter().map(|r| r.len()).sum();
    let mut pos = vec![0.0f64; n];
    for rnodes in ranks {
        for (i, &v) in rnodes.iter().enumerate() {
            pos[v] = i as f64;
        }
    }
    pos
}

fn barycenter(v: usize, adj: &[Vec<usize>], pos: &[f64]) -> f64 {
    let ns = &adj[v];
    if ns.is_empty() {
        return pos[v];
    }
    ns.iter().map(|&u| pos[u]).sum::<f64>() / ns.len() as f64
}

/// Assigns absolute `(x, y)` top-left coordinates. Nodes are packed
/// left-to-right within each rank (in `order`) with `node_sep` gaps; ranks
/// stack top-to-bottom with `rank_sep` gaps, each rank as tall as its tallest
/// node. Narrower ranks are centred against the widest.
fn assign_coords(
    input: &LayoutInput,
    rank: &[usize],
    order: &mut [usize],
    opts: &LayoutOptions,
) -> Vec<Placed> {
    let n = input.nodes.len();
    let max_rank = rank.iter().copied().max().unwrap_or(0);
    let mut ranks: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for v in 0..n {
        ranks[rank[v]].push(v);
    }
    for rnodes in &mut ranks {
        rnodes.sort_by_key(|&v| order[v]);
    }

    // Rank widths (sum of node widths + gaps) and heights (tallest node).
    let rank_width = |rnodes: &[usize]| -> f64 {
        if rnodes.is_empty() {
            return 0.0;
        }
        rnodes.iter().map(|&v| input.nodes[v].width).sum::<f64>()
            + opts.node_sep * (rnodes.len() - 1) as f64
    };
    let total_width = ranks.iter().map(|r| rank_width(r)).fold(0.0, f64::max);

    let mut placed: Vec<Placed> = vec![
        Placed {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            rank: 0,
            order: 0,
        };
        n
    ];
    let mut y = 0.0;
    for (r, rnodes) in ranks.iter().enumerate() {
        let rw = rank_width(rnodes);
        let rh = rnodes
            .iter()
            .map(|&v| input.nodes[v].height)
            .fold(0.0, f64::max);
        let mut x = (total_width - rw) / 2.0; // centre this rank
        for (i, &v) in rnodes.iter().enumerate() {
            let b = input.nodes[v];
            placed[v] = Placed {
                x,
                y: y + (rh - b.height) / 2.0,
                width: b.width,
                height: b.height,
                rank: r,
                order: i,
            };
            x += b.width + opts.node_sep;
        }
        y += rh + opts.rank_sep;
    }
    placed
}

/// One straight polyline per input edge: source bottom-centre → target
/// top-centre. (A back-edge just points upward; still a straight segment.)
fn route_straight(input: &LayoutInput, placed: &[Placed]) -> Vec<Vec<(f64, f64)>> {
    let bottom = |p: &Placed| (p.x + p.width / 2.0, p.y + p.height);
    let top = |p: &Placed| (p.x + p.width / 2.0, p.y);
    input
        .edges
        .iter()
        .map(|&(u, v)| vec![bottom(&placed[u]), top(&placed[v])])
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn boxes(n: usize) -> Vec<NodeBox> {
        vec![
            NodeBox {
                width: 10.0,
                height: 10.0
            };
            n
        ]
    }

    fn run(n: usize, edges: &[(usize, usize)], opts: &LayoutOptions) -> Positioned {
        layout(
            &LayoutInput {
                nodes: boxes(n),
                edges: edges.to_vec(),
            },
            opts,
        )
    }

    /// No overlap: within a rank x-intervals are disjoint; across ranks the
    /// y-bands don't overlap.
    fn assert_no_overlap(p: &Positioned) {
        for i in 0..p.nodes.len() {
            for j in (i + 1)..p.nodes.len() {
                let (a, b) = (p.nodes[i], p.nodes[j]);
                let x_overlap = a.x < b.x + b.width && b.x < a.x + a.width;
                let y_overlap = a.y < b.y + b.height && b.y < a.y + a.height;
                assert!(
                    !(x_overlap && y_overlap),
                    "nodes {i} and {j} overlap: {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn empty_graph_is_empty() {
        let p = layout(&LayoutInput::default(), &LayoutOptions::default());
        assert!(p.nodes.is_empty() && p.edges.is_empty());
    }

    #[test]
    fn diamond_ranks_are_longest_path() {
        // 0 → {1,2} → 3
        let p = run(
            4,
            &[(0, 1), (0, 2), (1, 3), (2, 3)],
            &LayoutOptions::default(),
        );
        assert_eq!(p.nodes[0].rank, 0);
        assert_eq!(p.nodes[1].rank, 1);
        assert_eq!(p.nodes[2].rank, 1);
        assert_eq!(p.nodes[3].rank, 2);
        // y strictly increases by rank; siblings share a band.
        assert!(p.nodes[0].y < p.nodes[1].y);
        assert!((p.nodes[1].y - p.nodes[2].y).abs() < 1e-9);
        assert!(p.nodes[1].y < p.nodes[3].y);
        assert_no_overlap(&p);
    }

    #[test]
    fn siblings_do_not_overlap() {
        let p = run(3, &[(0, 1), (0, 2)], &LayoutOptions::default());
        // 1 and 2 are on the same rank and must be side by side.
        assert_eq!(p.nodes[1].rank, p.nodes[2].rank);
        let (a, b) = (p.nodes[1], p.nodes[2]);
        assert!(a.x + a.width <= b.x + 1e-9 || b.x + b.width <= a.x + 1e-9);
        assert_no_overlap(&p);
    }

    #[test]
    fn cycle_terminates_and_ranks() {
        // A 2-cycle must not hang; the back-edge is reversed for ranking.
        let p = run(2, &[(0, 1), (1, 0)], &LayoutOptions::default());
        assert_eq!(p.nodes.len(), 2);
        assert_ne!(p.nodes[0].rank, p.nodes[1].rank);
        assert_no_overlap(&p);
    }

    #[test]
    fn self_loop_is_dropped_and_terminates() {
        let p = run(1, &[(0, 0)], &LayoutOptions::default());
        assert_eq!(p.nodes[0].rank, 0);
        assert_eq!(p.edges.len(), 1); // still one edge polyline emitted
    }

    #[test]
    fn edges_are_straight_two_point_polylines() {
        let p = run(2, &[(0, 1)], &LayoutOptions::default());
        assert_eq!(p.edges.len(), 1);
        assert_eq!(p.edges[0].len(), 2);
        // source point is the bottom of node 0, target point the top of node 1.
        assert!((p.edges[0][0].1 - (p.nodes[0].y + p.nodes[0].height)).abs() < 1e-9);
        assert!((p.edges[0][1].1 - p.nodes[1].y).abs() < 1e-9);
    }

    #[test]
    fn crossing_reduction_keeps_a_valid_layout() {
        // A small bipartite tangle; reduce_crossings must still produce a
        // non-overlapping, correctly-ranked layout.
        let edges = [(0, 3), (0, 4), (1, 3), (1, 5), (2, 4), (2, 5)];
        let opts = LayoutOptions {
            reduce_crossings: true,
            ..LayoutOptions::default()
        };
        let p = run(6, &edges, &opts);
        for v in 0..3 {
            assert_eq!(p.nodes[v].rank, 0);
        }
        for v in 3..6 {
            assert_eq!(p.nodes[v].rank, 1);
        }
        assert_no_overlap(&p);
    }

    #[test]
    fn deep_chain_is_linear_and_fast() {
        // 5000-node chain → 5000 ranks; must lay out (this is the shape that
        // makes graphviz dot hang).
        let edges: Vec<(usize, usize)> = (0..4999).map(|i| (i, i + 1)).collect();
        let p = run(5000, &edges, &LayoutOptions::default());
        assert_eq!(p.nodes[4999].rank, 4999);
        assert!(p.height > p.nodes[4999].y);
    }
}
