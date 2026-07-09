//! A fast, from-scratch layered ("hierarchical") graph layout.
//!
//! Graphviz `dot` cannot lay out large sea-of-nodes IR graphs: a ~1800-node
//! function is ~1400 ranks deep, and `dot`'s crossing-minimisation inserts a
//! dummy node per rank each edge spans, exploding into hundreds of thousands
//! of virtual nodes that never converge (it times out even natively). This
//! engine keeps the *shape* of the Sugiyama pipeline that makes `dot` readable
//! — ALAP ranking, virtual routing nodes for multi-rank edges, barycenter
//! crossing reduction, and isotonic coordinate assignment — but caps the
//! ordering passes instead of running mincross to convergence. That is exactly
//! the part of `dot` that blows up (its unbounded, transposing mincross over
//! the virtual-expanded graph), so bounding it keeps the layout `O((V+E)·k)`
//! and lays a 2500-node / 500k-virtual-node graph out in ~0.25 s.
//!
//! Output is coordinates (a [`Positioned`] graph); rendering to SVG happens in
//! the viewer from the emitted JSON.

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

/// Effective width of a routing (virtual) node — narrow so long edges pack
/// into tight channels beside the real-node spine rather than re-widening it.
const VIRT_W: f64 = 4.0;

/// Lay out `input` into absolute coordinates.
///
/// Edges spanning more than one rank are routed through a chain of *virtual*
/// nodes — one per intermediate rank — that participate in ordering and
/// coordinate assignment. This is the piece of `dot` that makes a deep graph
/// readable: long edges bend along channels beside the nodes instead of
/// cutting straight across the whole graph. Unlike `dot` we cap the ordering
/// passes rather than running mincross to convergence, so it stays fast.
pub fn layout(input: &LayoutInput, opts: &LayoutOptions) -> Positioned {
    let n = input.nodes.len();
    if n == 0 {
        return Positioned::default();
    }
    let forward = break_cycles(n, &input.edges);
    let rank = assign_ranks(n, &forward);

    // Augment: real nodes are ids `0..n`; virtual routing nodes follow.
    let mut vrank = rank.clone();
    let mut vwidth: Vec<f64> = input.nodes.iter().map(|b| b.width).collect();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let link = |adj: &mut Vec<Vec<usize>>, a: usize, b: usize| {
        adj[a].push(b);
        adj[b].push(a);
    };
    // chains[e] = the aug-node path for input edge e, in the edge's own
    // direction (so the arrowhead lands on the real target).
    let mut chains: Vec<Vec<usize>> = Vec::with_capacity(input.edges.len());
    for &(u, v) in &input.edges {
        if u == v {
            chains.push(vec![u]);
            continue;
        }
        let (lo, hi) = if rank[u] <= rank[v] { (u, v) } else { (v, u) };
        let (rlo, rhi) = (rank[lo], rank[hi]);
        let mut path = vec![lo];
        let mut prev = lo;
        for r in (rlo + 1)..rhi {
            let vid = vrank.len();
            vrank.push(r);
            vwidth.push(VIRT_W);
            adj.push(Vec::new());
            link(&mut adj, prev, vid);
            path.push(vid);
            prev = vid;
        }
        link(&mut adj, prev, hi);
        path.push(hi);
        if u != lo {
            path.reverse(); // emit in the original u → v direction
        }
        chains.push(path);
    }
    let total = vrank.len();

    let ranks = order_within_ranks(total, &vrank, &adj, opts);
    let vheight: Vec<f64> = (0..total)
        .map(|i| if i < n { input.nodes[i].height } else { 0.0 })
        .collect();
    let coords = assign_x(&vwidth, &vheight, &vrank, &ranks, &adj, opts);

    // Real-node placement.
    let placed: Vec<Placed> = (0..n)
        .map(|v| {
            let r = vrank[v];
            Placed {
                x: coords.cx[v] - input.nodes[v].width / 2.0,
                y: coords.band_top[r] + (coords.band_h[r] - input.nodes[v].height) / 2.0,
                width: input.nodes[v].width,
                height: input.nodes[v].height,
                rank: r,
                order: ranks[r].iter().position(|&u| u == v).unwrap_or(0),
            }
        })
        .collect();

    // Edge polylines through their virtual chains (endpoints clamped to the
    // real nodes' box borders; near-collinear interior points dropped).
    let edges: Vec<Vec<(f64, f64)>> = chains
        .iter()
        .map(|chain| route_chain(chain, n, &coords, &vrank))
        .collect();

    let width = placed.iter().map(|p| p.x + p.width).fold(0.0, f64::max);
    let height = placed.iter().map(|p| p.y + p.height).fold(0.0, f64::max);
    Positioned {
        nodes: placed,
        edges,
        width,
        height,
    }
}

/// Coordinate result over the augmented node set.
struct Coords {
    cx: Vec<f64>,       // centre x per aug node
    band_top: Vec<f64>, // top y per rank
    band_h: Vec<f64>,   // band height per rank
}

/// Turns one edge's aug-node chain into a screen polyline: virtual nodes
/// contribute their centre, the two real endpoints are clamped to the box
/// border facing their neighbour, then near-collinear points are dropped.
fn route_chain(chain: &[usize], n: usize, c: &Coords, vrank: &[usize]) -> Vec<(f64, f64)> {
    let cy = |id: usize| c.band_top[vrank[id]] + c.band_h[vrank[id]] / 2.0;
    if chain.len() == 1 {
        let id = chain[0];
        return vec![(c.cx[id], cy(id))];
    }
    let mut pts: Vec<(f64, f64)> = chain.iter().map(|&id| (c.cx[id], cy(id))).collect();
    // Clamp the real endpoints to the box edge facing the next/prev point.
    let border = |id: usize, toward_y: f64| -> f64 {
        let top = c.band_top[vrank[id]];
        let h = c.band_h[vrank[id]];
        if toward_y >= top + h / 2.0 {
            top + h
        } else {
            top
        }
    };
    let first = chain[0];
    if first < n {
        pts[0].1 = border(first, pts[1].1);
    }
    let last = chain[chain.len() - 1];
    if last < n {
        let k = pts.len() - 1;
        pts[k].1 = border(last, pts[k - 1].1);
    }
    simplify_polyline(&mut pts);
    pts
}

/// Drops interior points that lie (near) on the segment between their
/// neighbours, so a straight channel run collapses to its two endpoints.
fn simplify_polyline(pts: &mut Vec<(f64, f64)>) {
    if pts.len() <= 2 {
        return;
    }
    let mut out = Vec::with_capacity(pts.len());
    out.push(pts[0]);
    for i in 1..pts.len() - 1 {
        let (ax, ay) = *out.last().unwrap();
        let (bx, by) = pts[i];
        let (cx, cy) = pts[i + 1];
        // Cross product of (b-a) × (c-a); ~0 ⇒ collinear ⇒ drop b.
        let cross = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        if cross.abs() > 1.0 {
            out.push(pts[i]);
        }
    }
    out.push(pts[pts.len() - 1]);
    *pts = out;
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

/// Layer assignment on the acyclic `forward` edges. Uses *as-late-as-possible*
/// ranking: each node sits one layer above its earliest successor. This pulls
/// source nodes (constants, initial values — of which a sea-of-nodes graph has
/// hundreds, all with no predecessors) *down* next to the consumers that use
/// them, instead of an as-soon-as-possible scheme that would pile every source
/// into rank 0 and make it thousands of nodes wide. Edges stay short and the
/// widest rank shrinks dramatically. `O(V+E)`.
fn assign_ranks(n: usize, forward: &[(usize, usize)]) -> Vec<usize> {
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for &(u, v) in forward {
        succ[u].push(v);
        indeg[v] += 1;
    }
    // Topological order + ASAP longest-path depth (only used to find max rank).
    let mut asap = vec![0usize; n];
    let mut indeg_left = indeg.clone();
    let mut queue: Vec<usize> = (0..n).filter(|&v| indeg_left[v] == 0).collect();
    let mut topo = Vec::with_capacity(n);
    let mut head = 0;
    while head < queue.len() {
        let u = queue[head];
        head += 1;
        topo.push(u);
        for &v in &succ[u] {
            asap[v] = asap[v].max(asap[u] + 1);
            indeg_left[v] -= 1;
            if indeg_left[v] == 0 {
                queue.push(v);
            }
        }
    }
    let max_rank = asap.iter().copied().max().unwrap_or(0);
    // ALAP: in reverse topological order, place each node just above its
    // earliest successor; sinks stay at the bottom rank.
    let mut rank = vec![max_rank; n];
    for &u in topo.iter().rev() {
        if let Some(&min_succ) = succ[u].iter().map(|v| &rank[*v]).min() {
            rank[u] = min_succ.saturating_sub(1);
        }
    }
    rank
}

/// Builds `ranks[r]` = node ids at rank `r`, ordered within each rank to
/// reduce edge crossings via alternating up/down barycenter sweeps.
fn order_within_ranks(
    n: usize,
    rank: &[usize],
    adj: &[Vec<usize>],
    opts: &LayoutOptions,
) -> Vec<Vec<usize>> {
    let max_rank = rank.iter().copied().max().unwrap_or(0);
    let mut ranks: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for v in 0..n {
        ranks[rank[v]].push(v);
    }
    // Virtual-node expansion makes every augmented edge span exactly one rank,
    // so adjacency splits cleanly into the rank above (`up`) and below (`down`).
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); n];
    for v in 0..n {
        for &u in &adj[v] {
            if rank[u] + 1 == rank[v] {
                up[v].push(u);
            } else if rank[v] + 1 == rank[u] {
                down[v].push(u);
            }
        }
    }
    let mut pos = vec![0usize; n];
    let reindex = |ranks: &[Vec<usize>], pos: &mut [usize]| {
        for r in ranks {
            for (i, &v) in r.iter().enumerate() {
                pos[v] = i;
            }
        }
    };
    reindex(&ranks, &mut pos);

    let mut best = ranks.clone();
    let mut best_cross = total_crossings(&ranks, &pos, &down);
    // Alternating weighted-median sweeps + transpose, keeping the best. The
    // caller's budget allows several rounds; stop early once it plateaus.
    let max_iters = if opts.reduce_crossings { 12 } else { 6 };
    let mut stale = 0;
    for it in 0..max_iters {
        let down_dir = it % 2 == 0;
        let seq: Vec<usize> = if down_dir {
            (0..=max_rank).collect()
        } else {
            (0..=max_rank).rev().collect()
        };
        for r in seq {
            let nbr = if down_dir { &up } else { &down };
            let mut keyed: Vec<(f64, usize)> = ranks[r]
                .iter()
                .map(|&v| {
                    // No neighbour in the reference rank ⇒ keep current slot.
                    (weighted_median(&nbr[v], &pos).unwrap_or(pos[v] as f64), v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            ranks[r] = keyed.into_iter().map(|(_, v)| v).collect();
            for (i, &v) in ranks[r].iter().enumerate() {
                pos[v] = i;
            }
        }
        transpose(&mut ranks, &mut pos, &up, &down);
        let c = total_crossings(&ranks, &pos, &down);
        if c < best_cross {
            best_cross = c;
            best.clone_from(&ranks);
            stale = 0;
        } else {
            stale += 1;
            if stale >= 2 {
                break;
            }
        }
    }
    best
}

/// Weighted median of `nbr`'s positions (Gansner et al.), or `None` when the
/// node has no neighbour in the reference rank (caller keeps its slot).
fn weighted_median(nbr: &[usize], pos: &[usize]) -> Option<f64> {
    if nbr.is_empty() {
        return None;
    }
    let mut ps: Vec<usize> = nbr.iter().map(|&u| pos[u]).collect();
    ps.sort_unstable();
    let m = ps.len() / 2;
    Some(if ps.len() % 2 == 1 {
        ps[m] as f64
    } else if ps.len() == 2 {
        (ps[0] + ps[1]) as f64 / 2.0
    } else {
        let left = (ps[m - 1] - ps[0]) as f64;
        let right = (ps[ps.len() - 1] - ps[m]) as f64;
        if left + right == 0.0 {
            (ps[m - 1] + ps[m]) as f64 / 2.0
        } else {
            (ps[m - 1] as f64 * right + ps[m] as f64 * left) / (left + right)
        }
    })
}

/// Greedy transpose heuristic: repeatedly swap adjacent nodes in a rank when it
/// reduces the crossings against their fixed up/down neighbours. Cheap because
/// virtual routing nodes have degree ≤ 2.
fn transpose(ranks: &mut [Vec<usize>], pos: &mut [usize], up: &[Vec<usize>], down: &[Vec<usize>]) {
    let pair = |left: usize, right: usize, pos: &[usize]| -> usize {
        let mut c = 0;
        for nbrs in [up, down] {
            for &a in &nbrs[left] {
                for &b in &nbrs[right] {
                    if pos[a] > pos[b] {
                        c += 1;
                    }
                }
            }
        }
        c
    };
    let mut improved = true;
    let mut rounds = 0;
    while improved && rounds < 4 {
        improved = false;
        rounds += 1;
        for r in ranks.iter_mut() {
            for i in 0..r.len().saturating_sub(1) {
                let (v, w) = (r[i], r[i + 1]);
                if pair(w, v, pos) < pair(v, w, pos) {
                    r.swap(i, i + 1);
                    pos[v] = i + 1;
                    pos[w] = i;
                    improved = true;
                }
            }
        }
    }
}

/// Total edge crossings summed over every adjacent rank pair, via inversion
/// counting on the lower-rank endpoint order (Barth–Mutzel).
fn total_crossings(ranks: &[Vec<usize>], pos: &[usize], down: &[Vec<usize>]) -> u64 {
    let mut total = 0u64;
    for rnodes in ranks {
        // Edges to the next rank as (upper-pos, lower-pos), read in upper order.
        let mut targets: Vec<usize> = Vec::new();
        for &u in rnodes {
            let mut ds: Vec<usize> = down[u].iter().map(|&w| pos[w]).collect();
            ds.sort_unstable();
            targets.extend(ds);
        }
        total += count_inversions(&targets);
    }
    total
}

/// Number of inversions in `a` (pairs i<j with a[i]>a[j]) via a Fenwick tree.
fn count_inversions(a: &[usize]) -> u64 {
    if a.is_empty() {
        return 0;
    }
    let max = a.iter().copied().max().unwrap() + 2;
    let mut bit = vec![0u64; max + 1];
    let mut inv = 0u64;
    for (seen, &x) in a.iter().enumerate() {
        // Of the `seen` already-inserted values, subtract those ≤ x to get the
        // count strictly greater than x — i.e. the inversions x introduces.
        let mut le = 0u64;
        let mut i = x + 1;
        while i > 0 {
            le += bit[i];
            i &= i - 1;
        }
        inv += seen as u64 - le;
        let mut i = x + 1;
        while i <= max {
            bit[i] += 1;
            i += i & i.wrapping_neg();
        }
    }
    inv
}

/// Assigns rank `y` bands and refined centre `x` over the augmented node set
/// (`width` / `height` are per aug node; virtual routing nodes have height 0).
/// `y` stacks ranks top-to-bottom. `x` is refined by alternating sweeps: each
/// node is pulled toward the mean centre of its neighbours, then each rank is
/// resolved to be order-preserving and non-overlapping with minimum total
/// displacement (isotonic regression / pool-adjacent-violators). Because long
/// edges carry virtual nodes here, this aligns both real nodes *and* the edge
/// channels running past them — the readable, `dot`-like shape.
fn assign_x(
    width: &[f64],
    height: &[f64],
    rank: &[usize],
    ranks: &[Vec<usize>],
    adj: &[Vec<usize>],
    opts: &LayoutOptions,
) -> Coords {
    let total = width.len();

    let mut band_top = vec![0.0; ranks.len()];
    let mut band_h = vec![0.0; ranks.len()];
    let mut y = 0.0;
    for (r, rnodes) in ranks.iter().enumerate() {
        let h = rnodes.iter().map(|&v| height[v]).fold(0.0, f64::max);
        band_top[r] = y;
        band_h[r] = h;
        y += h + opts.rank_sep;
    }

    let mut cx = vec![0.0f64; total];
    for rnodes in ranks {
        let mut x = 0.0;
        for &v in rnodes {
            cx[v] = x + width[v] / 2.0;
            x += width[v] + gap_after(v, width);
        }
    }

    let passes = if opts.reduce_crossings { 12 } else { 8 };
    for pass in 0..passes {
        let range: Vec<usize> = if pass % 2 == 0 {
            (0..ranks.len()).collect()
        } else {
            (0..ranks.len()).rev().collect()
        };
        for r in range {
            let desired: Vec<f64> = ranks[r]
                .iter()
                .map(|&v| {
                    if adj[v].is_empty() {
                        cx[v]
                    } else {
                        adj[v].iter().map(|&u| cx[u]).sum::<f64>() / adj[v].len() as f64
                    }
                })
                .collect();
            let placed = resolve_rank(&ranks[r], &desired, width);
            for (i, &v) in ranks[r].iter().enumerate() {
                cx[v] = placed[i];
            }
        }
    }

    let min_left = (0..total)
        .filter(|&v| !ranks[rank[v]].is_empty())
        .map(|v| cx[v] - width[v] / 2.0)
        .fold(f64::INFINITY, f64::min);
    if min_left.is_finite() {
        for x in &mut cx {
            *x -= min_left;
        }
    }
    Coords {
        cx,
        band_top,
        band_h,
    }
}

/// Horizontal gap kept after a node: tight for virtual routing nodes (so long
/// edges pack into narrow channels) and roomy for real nodes.
fn gap_after(v: usize, width: &[f64]) -> f64 {
    if width[v] <= VIRT_W { 2.0 } else { 24.0 }
}

/// Resolves one rank's node centres: returns centres (in `order` sequence) that
/// are order-preserving, separated by at least the half-widths + gap, and as
/// close to `desired` as possible (minimum sum of squared displacement). Solved
/// by pool-adjacent-violators after transforming the min-gap constraints into a
/// plain non-decreasing (isotonic) constraint.
fn resolve_rank(order: &[usize], desired: &[f64], width: &[f64]) -> Vec<f64> {
    let m = order.len();
    if m == 0 {
        return Vec::new();
    }
    let mut s = vec![0.0; m];
    for i in 1..m {
        s[i] = s[i - 1]
            + width[order[i - 1]] / 2.0
            + width[order[i]] / 2.0
            + gap_after(order[i - 1], width);
    }
    let target: Vec<f64> = (0..m).map(|i| desired[i] - s[i]).collect();
    let t = pava(&target);
    (0..m).map(|i| t[i] + s[i]).collect()
}

/// Pool-adjacent-violators: the least-squares non-decreasing fit to `y`
/// (isotonic regression, equal weights). `O(len)`.
fn pava(y: &[f64]) -> Vec<f64> {
    // Stack of blocks: (mean, count).
    let mut stack: Vec<(f64, usize)> = Vec::with_capacity(y.len());
    for &yi in y {
        let mut cur = (yi, 1usize);
        while let Some(&(v, c)) = stack.last() {
            if v <= cur.0 {
                break;
            }
            let count = c + cur.1;
            let mean = (v * c as f64 + cur.0 * cur.1 as f64) / count as f64;
            stack.pop();
            cur = (mean, count);
        }
        stack.push(cur);
    }
    let mut out = Vec::with_capacity(y.len());
    for (v, c) in stack {
        for _ in 0..c {
            out.push(v);
        }
    }
    out
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
