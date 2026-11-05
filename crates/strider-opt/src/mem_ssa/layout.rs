//! The memory graph laid out for a nearest-clobber climb.
//!
//! A location's nearest clobber at a memory value is its nearest dominator,
//! itself included, that clobbers it or is a `MemPhi` whose region (the defs
//! reaching it without passing its immediate dominator) holds a clobber;
//! `InitialMemory` when there is none.
//!
//! Positions are a topological order in which every natural loop is one
//! contiguous block opened by its header, so a clobber-free position range
//! passes every dominator it covers at once. An irreducible loop is one block
//! opened by its DFS-first node; inside it the climb steps one dominator at a
//! time.

use cranelift_entity::SecondaryMap;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

const NONE: u32 = u32::MAX;

#[cfg(test)]
thread_local! {
    /// Climb steps, hops, region-walk visits and candidate asks.
    pub(crate) static CLIMB_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn step() {
    CLIMB_STEPS.with(|c| c.set(c.get() + 1));
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Shape {
    Initial,
    Phi,
    /// A `Store`, `Call` or `CallOther`: one memory input, one memory output.
    Def,
}

/// Answers the defs of one position range for one probed location.
pub(crate) trait ClobberProbe {
    /// The exact verdict for a def.
    fn clobbers(&mut self, function: &Function, def: NodeId) -> bool;

    /// The highest position in `lo..=hi` holding a def that may clobber. A
    /// superset of the defs [`Self::clobbers`] accepts is enough.
    fn candidate(&mut self, lo: u32, hi: u32) -> Option<u32>;
}

/// One location's memoised climb results, by position. Valid while no def's
/// verdict for the location changes.
#[derive(Default)]
pub(crate) struct Answers {
    nearest: FxHashMap<u32, u32>,
    dirty: FxHashMap<u32, bool>,
    /// What a range scan learned below each def it passed or stopped at.
    below: FxHashMap<u32, Below>,
}

/// The defs at and below a position, as far as a scan has decided them.
#[derive(Clone, Copy)]
enum Below {
    /// The highest def at or below it that clobbers.
    Clobber(u32),
    /// No def from this position up to it clobbers; below is undecided.
    CleanFrom(u32),
}

pub(crate) struct MemLayout {
    slot: SecondaryMap<NodeId, u32>,
    node: Vec<NodeId>,
    shape: Vec<Shape>,
    preds: Vec<SmallVec<[u32; 2]>>,
    /// `InitialMemory` is its own immediate dominator.
    idom: Vec<u32>,
    pos: Positions,
}

/// What a position means beyond its order: a range of them is a set of defs
/// the climb can pass at once.
pub(crate) struct Positions {
    /// Skip pointer up the dominator tree.
    dom_jump: Vec<u32>,
    /// Innermost loop header whose block holds the node, the node itself
    /// excluded; `NONE` at top level.
    encl: Vec<u32>,
    /// Skip pointer up the `encl` chain of a header.
    loop_jump: Vec<u32>,
    /// Last position of a header's block; the node itself otherwise.
    end: Vec<u32>,
    /// In an irreducible loop's block.
    tangled: Vec<bool>,
    /// For a tangled def, the first of the run of tangled defs at consecutive
    /// positions, each the only predecessor of the next, that ends at it.
    run_start: Vec<u32>,
}

impl MemLayout {
    /// The layout of every memory def reachable from `InitialMemory`.
    pub(crate) fn build(function: &Function) -> Self {
        let graph = Discovered::collect(function);
        let n = graph.node.len();
        let dfs = DepthFirst::from_root(&graph.succs);
        let idom_dfs = graph_algorithms::dominance::immediate_dominators(&dfs.parent, |w| {
            graph.preds[dfs.vertex[w as usize] as usize]
                .iter()
                .map(|&p| dfs.num[p as usize])
                .collect::<SmallVec<[u32; 2]>>()
        });
        let loops = LoopForest::new(&graph.preds, &dfs);
        let (order, block_end) = loops.nested_order(&graph.preds, &dfs);

        let mut pos = vec![NONE; n];
        for (p, &v) in order.iter().enumerate() {
            pos[v as usize] = p as u32;
        }
        let mut slot = SecondaryMap::with_default(NONE);
        let mut node = Vec::with_capacity(n);
        let mut shape = Vec::with_capacity(n);
        let mut preds: Vec<SmallVec<[u32; 2]>> = Vec::with_capacity(n);
        let mut idom = Vec::with_capacity(n);
        for (p, &v) in order.iter().enumerate() {
            let v = v as usize;
            slot[graph.node[v]] = p as u32;
            node.push(graph.node[v]);
            shape.push(graph.shape[v]);
            preds.push(graph.preds[v].iter().map(|&u| pos[u as usize]).collect());
            idom.push(pos[dfs.vertex[idom_dfs[dfs.num[v] as usize] as usize] as usize]);
        }
        debug_assert_eq!(pos[0], 0, "InitialMemory leads");

        let positions = {
            // The top level, `n` in the forest, is `NONE` here.
            let at = |v: u32| pos.get(v as usize).copied().unwrap_or(NONE);
            // Preorder reaches an enclosing header first.
            let mut tangled_node = vec![false; n];
            for &v in &dfs.vertex {
                let v = v as usize;
                let p = loops.parent[v];
                tangled_node[v] = loops.irreducible[v] || (p != NONE && tangled_node[p as usize]);
            }
            let mut encl = Vec::with_capacity(n);
            let mut loop_jump = Vec::with_capacity(n);
            let mut end = Vec::with_capacity(n);
            let mut tangled = Vec::with_capacity(n);
            for (p, &v) in order.iter().enumerate() {
                let v = v as usize;
                encl.push(at(loops.parent[v]));
                loop_jump.push(at(loops.jump[v]));
                end.push(if loops.is_header[v] {
                    block_end[v]
                } else {
                    p as u32
                });
                tangled.push(tangled_node[v]);
            }
            let mut run_start: Vec<u32> = Vec::with_capacity(n);
            for p in 0..n {
                let def = |p: usize| tangled[p] && shape[p] == Shape::Def;
                let linked = p > 0 && def(p) && def(p - 1) && preds[p][..] == [p as u32 - 1];
                run_start.push(if linked { run_start[p - 1] } else { p as u32 });
            }

            // Preorder reaches a dominator first.
            let mut dom_depth = vec![0u32; n];
            let mut dom_jump = vec![NONE; n];
            for &v in &dfs.vertex {
                let x = pos[v as usize] as usize;
                let parent = idom[x] as usize;
                if parent == x {
                    dom_jump[x] = x as u32;
                } else {
                    dom_depth[x] = dom_depth[parent] + 1;
                    dom_jump[x] = skip(&dom_depth, &dom_jump, parent as u32);
                }
            }

            Positions {
                dom_jump,
                encl,
                loop_jump,
                end,
                tangled,
                run_start,
            }
        };
        Self {
            slot,
            node,
            shape,
            preds,
            idom,
            pos: positions,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_reducible(&self) -> bool {
        !self.pos.tangled.contains(&true)
    }

    pub(crate) fn len(&self) -> usize {
        self.node.len()
    }

    pub(crate) fn position(&self, node: NodeId) -> Option<u32> {
        let p = self.slot[node];
        (p != NONE).then_some(p)
    }

    pub(crate) fn node_at(&self, pos: u32) -> NodeId {
        self.node[pos as usize]
    }

    pub(crate) fn shape_at(&self, pos: u32) -> Shape {
        self.shape[pos as usize]
    }

    /// The nearest clobber backward from `start`'s memory output, or `None`
    /// when `start` is not in the layout.
    pub(crate) fn nearest_clobber(
        &self,
        function: &Function,
        start: NodeId,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> Option<NodeId> {
        // Unreachable from `InitialMemory`: the caller bounds it.
        let start = self.position(start)?;
        Some(self.node_at(self.climb(function, start, probe, answers)))
    }

    /// Whether merge `phi`'s region holds a clobber, over the defs that reach
    /// it without passing its immediate dominator.
    fn region_dirty_walked(
        &self,
        function: &Function,
        phi: u32,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> bool {
        if let Some(&d) = answers.dirty.get(&phi) {
            return d;
        }
        let stop = self.idom[phi as usize];
        let mut work = vec![phi];
        let mut seen = FxHashSet::from_iter([phi]);
        let dirty = 'walk: loop {
            let Some(y) = work.pop() else {
                break false;
            };
            for &p in &self.preds[y as usize] {
                #[cfg(test)]
                step();
                if p == stop || !seen.insert(p) {
                    continue;
                }
                let mut next = p;
                if self.shape[p as usize] == Shape::Def {
                    // The region holds `p`'s run down to `stop`.
                    let lo = self.pos.run_start[p as usize].max(stop + 1);
                    if self
                        .highest_clobber(function, probe, answers, lo, p)
                        .is_some()
                    {
                        break 'walk true;
                    }
                    if lo != p && !seen.insert(lo) {
                        continue;
                    }
                    next = lo;
                }
                work.push(next);
            }
        };
        answers.dirty.insert(phi, dirty);
        dirty
    }

    fn climb(
        &self,
        function: &Function,
        start: u32,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> u32 {
        // Each frame above the first climbs an entry arm of the merge below it,
        // as far as that merge's immediate dominator; arms precede their
        // merge, so the stack ends. Frames share one path from their `base`.
        let mut path: SmallVec<[u32; 8]> = SmallVec::new();
        let mut frames = vec![Climb::new(start, None, 0)];
        loop {
            let frame = frames.last_mut().expect("the first frame ends the loop");
            match self.climb_frame(function, frame, &mut path, probe, answers) {
                Step::Answer(answer) => {
                    for p in path.drain(frame.base..) {
                        answers.nearest.insert(p, answer);
                    }
                    frames.pop();
                    if frames.is_empty() {
                        return answer;
                    }
                }
                // What it passed stays on the path, to share the merge's
                // immediate dominator's answer if the merge is clean.
                Step::Bounded => {
                    frames.pop();
                    let merge = frames.last_mut().expect("the first frame is unbounded");
                    merge.arm = merge.arm.map(|(i, mark)| (i + 1, mark));
                }
                Step::Arm(u) => {
                    let stop = self.idom[frame.x as usize];
                    frames.push(Climb::new(u, Some(stop), path.len()));
                }
            }
        }
    }

    /// Runs one frame to its answer, past its bound, or to an entry arm of the
    /// merge it stands on whose answer is not known yet.
    fn climb_frame(
        &self,
        function: &Function,
        frame: &mut Climb,
        path: &mut SmallVec<[u32; 8]>,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> Step {
        let Climb { x, stop, arm, .. } = frame;
        let past = |x: u32| stop.is_some_and(|s| x <= s);
        loop {
            if let Some((first, mark)) = *arm {
                let dirty = match self.arms_dirty(*x, first, answers) {
                    Ok(dirty) => dirty,
                    Err((i, u)) => {
                        *arm = Some((i, mark));
                        return Step::Arm(u);
                    }
                };
                answers.dirty.insert(*x, dirty);
                *arm = None;
                if dirty {
                    // The arms' positions are not below the merge.
                    path.truncate(mark);
                    return Step::Answer(*x);
                }
                // The arms passed to the immediate dominator share its answer.
                *x = self.idom[*x as usize];
            }
            if past(*x) {
                return Step::Bounded;
            }
            #[cfg(test)]
            step();
            if let Some(&a) = answers.nearest.get(x) {
                return Step::Answer(a);
            }
            path.push(*x);
            if self.pos.tangled[*x as usize] {
                let stop = match self.shape[*x as usize] {
                    Shape::Initial => true,
                    Shape::Phi => self.region_dirty_walked(function, *x, probe, answers),
                    Shape::Def => {
                        // The run's defs are `x`'s nearest dominators.
                        let lo = self.pos.run_start[*x as usize];
                        if let Some(c) = self.highest_clobber(function, probe, answers, lo, *x) {
                            return Step::Answer(c);
                        }
                        *x = self.idom[lo as usize];
                        continue;
                    }
                };
                if stop {
                    return Step::Answer(*x);
                }
                *x = self.idom[*x as usize];
                continue;
            }
            // Every def reaching `x` without passing a dominator inside the
            // same loop lies in `floor..=end[x]`.
            let enc = self.pos.encl[*x as usize];
            let floor = if enc == NONE { 0 } else { enc + 1 };
            let Some(c) =
                self.highest_clobber(function, probe, answers, floor, self.pos.end[*x as usize])
            else {
                if enc == NONE {
                    // `InitialMemory` leads the order.
                    return Step::Answer(0);
                }
                *x = self.outermost_clean_loop(function, enc, probe, answers);
                continue;
            };
            let a = self.topmost_dominator_above(*x, c);
            if a != *x {
                *x = a;
                if self.pos.tangled[a as usize] {
                    continue;
                }
                if past(*x) {
                    return Step::Bounded;
                }
                if let Some(&a) = answers.nearest.get(x) {
                    return Step::Answer(a);
                }
                path.push(*x);
            }
            match self.shape[*x as usize] {
                Shape::Initial => return Step::Answer(*x),
                Shape::Phi => {
                    // The jump stops where the next dominator is at or below
                    // `c`, so the region may hold `c`.
                    debug_assert!(self.idom[*x as usize] <= c, "a merge passed by the jump");
                    // Every def in a loop block is in its header's region.
                    let block_dirty = self.pos.end[*x as usize] > *x && c > *x;
                    match answers
                        .dirty
                        .get(x)
                        .copied()
                        .or(block_dirty.then_some(true))
                    {
                        Some(dirty) => {
                            answers.dirty.insert(*x, dirty);
                            if dirty {
                                return Step::Answer(*x);
                            }
                            *x = self.idom[*x as usize];
                        }
                        None => *arm = Some((0, path.len())),
                    }
                }
                Shape::Def => {
                    if c == *x {
                        return Step::Answer(c);
                    }
                    *x = self.idom[*x as usize];
                }
            }
        }
    }

    /// Whether some entry arm of merge `phi`, from the `first`-th, has its
    /// nearest clobber laid out above `phi`'s immediate dominator. `Err` names
    /// the first arm whose answer is not known yet, with its index.
    fn arms_dirty(&self, phi: u32, first: usize, answers: &Answers) -> Result<bool, (usize, u32)> {
        let stop = self.idom[phi as usize];
        for (i, &u) in self.preds[phi as usize].iter().enumerate().skip(first) {
            if u >= phi || u == stop {
                continue;
            }
            match answers.nearest.get(&u) {
                Some(&n) => {
                    if n > stop {
                        return Ok(true);
                    }
                }
                None => return Err((i, u)),
            }
        }
        Ok(false)
    }

    /// The outermost loop header from `header` outward whose block past the
    /// header is free of clobbers, or `header` itself.
    fn outermost_clean_loop(
        &self,
        function: &Function,
        header: u32,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> u32 {
        let pos = &self.pos;
        farthest(
            header,
            &pos.loop_jump,
            |h| pos.encl[h as usize],
            |h| {
                self.highest_clobber(function, probe, answers, h + 1, pos.end[h as usize])
                    .is_none()
            },
        )
    }

    /// The topmost dominator of `x` laid out above `c`.
    fn topmost_dominator_above(&self, x: u32, c: u32) -> u32 {
        farthest(x, &self.pos.dom_jump, |a| self.idom[a as usize], |a| a > c)
    }

    /// The highest position in `lo..=hi` whose def clobbers. A def rejected
    /// once is never asked again.
    fn highest_clobber(
        &self,
        function: &Function,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
        lo: u32,
        hi: u32,
    ) -> Option<u32> {
        if lo > hi {
            return None;
        }
        let mut hi = hi;
        let mut passed: SmallVec<[u32; 8]> = SmallVec::new();
        let found = loop {
            #[cfg(test)]
            step();
            // Nothing between a candidate and `hi` may clobber.
            let Some(c) = probe.candidate(lo, hi) else {
                break Below::CleanFrom(lo);
            };
            debug_assert!((lo..=hi).contains(&c), "candidate {c} outside {lo}..={hi}");
            passed.push(c);
            let below = answers.below.get(&c).copied().unwrap_or_else(|| {
                if self.shape[c as usize] == Shape::Def
                    && probe.clobbers(function, self.node[c as usize])
                {
                    Below::Clobber(c)
                } else {
                    Below::CleanFrom(c)
                }
            });
            match below {
                Below::CleanFrom(t) if t > lo => hi = t - 1,
                _ => break below,
            }
        };
        for p in passed {
            answers.below.insert(p, found);
        }
        match found {
            Below::Clobber(c) if c >= lo => Some(c),
            _ => None,
        }
    }
}

/// The farthest ancestor of `x` that `ok` accepts, up `parent` with the skip
/// pointers `jump`, or `x` itself. `ok` accepts every node between `x` and
/// any ancestor it accepts. `NONE` and a self-pointer end a chain.
fn farthest(
    mut x: u32,
    jump: &[u32],
    parent: impl Fn(u32) -> u32,
    mut ok: impl FnMut(u32) -> bool,
) -> u32 {
    loop {
        #[cfg(test)]
        step();
        let j = jump[x as usize];
        if j != NONE && j != x && ok(j) {
            x = j;
            continue;
        }
        let p = parent(x);
        if p != NONE && p != x && ok(p) {
            x = p;
            continue;
        }
        return x;
    }
}

/// The skip pointer of a child of `parent`.
fn skip(depth: &[u32], jump: &[u32], parent: u32) -> u32 {
    let p = parent as usize;
    let j = jump[p] as usize;
    let jj = jump[j] as usize;
    if depth[p] - depth[j] == depth[j] - depth[jj] {
        jj as u32
    } else {
        parent
    }
}

/// One open climb of [`MemLayout::climb`].
struct Climb {
    /// Where the climb stands.
    x: u32,
    /// For an arm's climb, its merge's immediate dominator, where it ends.
    stop: Option<u32>,
    /// Where the positions this frame passed start on the shared path.
    base: usize,
    /// On a merge its arms decide: the first unchecked arm and the path's
    /// length at the merge.
    arm: Option<(usize, usize)>,
}

impl Climb {
    fn new(x: u32, stop: Option<u32>, base: usize) -> Self {
        Self {
            x,
            stop,
            base,
            arm: None,
        }
    }
}

/// Where a [`Climb`] frame stopped.
enum Step {
    Answer(u32),
    /// Past its bound with no clobber found.
    Bounded,
    /// At a merge needing this entry arm's answer.
    Arm(u32),
}

/// The memory defs reachable from `InitialMemory`, in discovery order.
struct Discovered {
    node: Vec<NodeId>,
    shape: Vec<Shape>,
    preds: Vec<SmallVec<[u32; 2]>>,
    succs: Vec<SmallVec<[u32; 2]>>,
}

impl Discovered {
    fn collect(function: &Function) -> Self {
        let graph = function.graph();
        let mut initials = graph
            .all_node_ids()
            .filter(|&n| matches!(function.node_kind(n), NodeKind::InitialMemory));
        let initial = initials.next().expect("a function has an InitialMemory");
        debug_assert!(
            initials.next().is_none(),
            "InitialMemory is cacheable, so a function holds one"
        );
        let mut index: SecondaryMap<NodeId, u32> = SecondaryMap::with_default(NONE);
        let mut node = vec![initial];
        index[initial] = 0;
        let mut i = 0;
        while i < node.len() {
            let out = function
                .memory_output_of(node[i])
                .expect("a memory def has a memory output");
            for (user, slot) in graph.value_uses(out) {
                if index[user] == NONE && consumes_memory_at(function, user, slot) {
                    index[user] = node.len() as u32;
                    node.push(user);
                }
            }
            i += 1;
        }
        let n = node.len();
        let mut shape = Vec::with_capacity(n);
        let mut preds: Vec<SmallVec<[u32; 2]>> = Vec::with_capacity(n);
        let mut succs: Vec<SmallVec<[u32; 2]>> = vec![SmallVec::new(); n];
        for (v, &id) in node.iter().enumerate() {
            let (s, inputs): (Shape, SmallVec<[ValueId; 2]>) = match function.node_kind(id) {
                NodeKind::InitialMemory => (Shape::Initial, SmallVec::new()),
                // An arm fed from outside the reach is on an edge no path
                // takes.
                NodeKind::MemPhi => (
                    Shape::Phi,
                    function
                        .phi_data_inputs(id)
                        .filter(|&arm| index[function.producer(arm)] != NONE)
                        .collect(),
                ),
                _ => (
                    Shape::Def,
                    function.memory_input_of(id).into_iter().collect(),
                ),
            };
            shape.push(s);
            let mut ps = SmallVec::new();
            for input in inputs {
                let u = index[function.producer(input)];
                debug_assert_ne!(u, NONE, "a reached def is fed from the reach");
                ps.push(u);
                succs[u as usize].push(v as u32);
            }
            preds.push(ps);
        }
        Self {
            node,
            shape,
            preds,
            succs,
        }
    }
}

/// Whether `user` reads memory at `slot` and produces memory of its own.
fn consumes_memory_at(function: &Function, user: NodeId, slot: u32) -> bool {
    let slot = slot as usize;
    match function.node_kind(user) {
        kind @ NodeKind::MemPhi => slot >= kind.input_head_len(),
        NodeKind::Store(_) => slot == NodeKind::ACCESS_MEM_SLOT,
        NodeKind::Call { .. } | NodeKind::CallOther { .. } => slot == NodeKind::CTRL_MEM_SLOT,
        _ => false,
    }
}

/// Preorder numbering from node 0 over successors.
struct DepthFirst {
    /// Node to preorder number.
    num: Vec<u32>,
    /// Preorder number to node.
    vertex: Vec<u32>,
    /// By preorder number.
    parent: Vec<u32>,
    /// Subtree size, by preorder number.
    size: Vec<u32>,
}

impl DepthFirst {
    fn from_root(succs: &[SmallVec<[u32; 2]>]) -> Self {
        let n = succs.len();
        let mut num = vec![NONE; n];
        let mut vertex = Vec::with_capacity(n);
        let mut parent = Vec::with_capacity(n);
        let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
        num[0] = 0;
        vertex.push(0);
        parent.push(NONE);
        while let Some(top) = stack.last_mut() {
            let (v, next) = *top;
            if let Some(&w) = succs[v as usize].get(next) {
                top.1 += 1;
                if num[w as usize] == NONE {
                    num[w as usize] = vertex.len() as u32;
                    parent.push(num[v as usize]);
                    vertex.push(w);
                    stack.push((w, 0));
                }
            } else {
                stack.pop();
            }
        }
        let mut size = vec![1u32; vertex.len()];
        for d in (1..vertex.len()).rev() {
            let p = parent[d] as usize;
            size[p] += size[d];
        }
        Self {
            num,
            vertex,
            parent,
            size,
        }
    }

    /// Whether preorder number `a` is an ancestor of `b`, itself included.
    fn is_ancestor(&self, a: u32, b: u32) -> bool {
        a <= b && b < a + self.size[a as usize]
    }
}

/// Loops, found by collapsing each header's body into it innermost first
/// (Havlak). A header is the DFS-first node of its loop; the loop is
/// irreducible when some body node is entered from outside the header's DFS
/// subtree.
struct LoopForest {
    /// Innermost header whose loop holds the node, the node excluded.
    parent: Vec<u32>,
    is_header: Vec<bool>,
    irreducible: Vec<bool>,
    /// Loop depth of a header, by node, the top level `n` at depth 0.
    depth: Vec<u32>,
    /// Skip pointer up the headers enclosing a header, by node; `n` otherwise.
    jump: Vec<u32>,
}

impl LoopForest {
    fn new(preds: &[SmallVec<[u32; 2]>], dfs: &DepthFirst) -> Self {
        let n = preds.len();
        let mut rep: Vec<u32> = (0..n as u32).collect();
        let mut parent = vec![NONE; n];
        let mut is_header = vec![false; n];
        let mut irreducible = vec![false; n];
        // A collapsed header's entries from outside its subtree, which its
        // enclosing loops see as its own predecessors.
        let mut entries: Vec<SmallVec<[u32; 2]>> = vec![SmallVec::new(); n];
        let mut mark = vec![NONE; n];
        let mut body: Vec<u32> = Vec::new();
        for &w in dfs.vertex.iter().rev() {
            body.clear();
            let dw = dfs.num[w as usize];
            for &v in &preds[w as usize] {
                if v == w {
                    is_header[w as usize] = true;
                } else if dfs.is_ancestor(dw, dfs.num[v as usize]) {
                    let r = find(&mut rep, v);
                    if r != w && mark[r as usize] != w {
                        mark[r as usize] = w;
                        body.push(r);
                    }
                }
            }
            if body.is_empty() {
                continue;
            }
            is_header[w as usize] = true;
            let mut i = 0;
            let mut outside: SmallVec<[u32; 2]> = SmallVec::new();
            while i < body.len() {
                let x = body[i];
                i += 1;
                for &y in preds[x as usize].iter().chain(&entries[x as usize]) {
                    let r = find(&mut rep, y);
                    if r == w || mark[r as usize] == w {
                        continue;
                    }
                    if !dfs.is_ancestor(dw, dfs.num[r as usize]) {
                        irreducible[w as usize] = true;
                        outside.push(y);
                        continue;
                    }
                    mark[r as usize] = w;
                    body.push(r);
                }
            }
            entries[w as usize] = outside;
            for &x in &body {
                parent[x as usize] = w;
                rep[x as usize] = w;
            }
        }
        // Parents before children: preorder reaches an enclosing header first.
        let top = n as u32;
        let mut depth = vec![0u32; n + 1];
        let mut jump = vec![top; n + 1];
        for &v in &dfs.vertex {
            if is_header[v as usize] {
                let p = match parent[v as usize] {
                    NONE => top,
                    p => p,
                };
                depth[v as usize] = depth[p as usize] + 1;
                jump[v as usize] = skip(&depth, &jump, p);
            }
        }
        Self {
            parent,
            is_header,
            irreducible,
            depth,
            jump,
        }
    }

    /// Nodes in position order, and each header's last position. Each level
    /// (the top, or one loop body with inner loops collapsed into their
    /// headers) is ordered topologically, an inner loop's block spliced in
    /// where its header falls. An edge entering an irreducible loop away from
    /// its header counts at the innermost level holding both ends.
    fn nested_order(&self, preds: &[SmallVec<[u32; 2]>], dfs: &DepthFirst) -> (Vec<u32>, Vec<u32>) {
        let n = preds.len();
        let top = n as u32;
        let level = |x: u32| match self.parent.get(x as usize) {
            Some(&p) if p != NONE => p,
            _ => top,
        };
        let depth = &self.depth;
        let ancestor_at =
            |h: u32, d: u32| farthest(h, &self.jump, level, |a| depth[a as usize] >= d);
        let common = |a: u32, b: u32| {
            let d = depth[a as usize].min(depth[b as usize]);
            let (mut a, mut b) = (ancestor_at(a, d), ancestor_at(b, d));
            while a != b {
                (a, b) = (level(a), level(b));
            }
            a
        };
        // The representative of `x` at the level inside loop `l`.
        let rep_in = |l: u32, x: u32, lx: u32| {
            if lx == l {
                x
            } else {
                ancestor_at(lx, depth[l as usize] + 1)
            }
        };

        // Edges between the representatives of one level: a node at its own
        // level, or the header of the inner loop holding it. A header opens
        // its own level as the source and sits in its parent's as a member.
        let mut member_succs: Vec<SmallVec<[u32; 2]>> = vec![SmallVec::new(); n];
        let mut source_succs: Vec<SmallVec<[u32; 2]>> = vec![SmallVec::new(); n];
        let mut indegree = vec![0u32; n];
        for (v, ps) in preds.iter().enumerate() {
            let v = v as u32;
            for &u in ps {
                if dfs.is_ancestor(dfs.num[v as usize], dfs.num[u as usize]) {
                    continue;
                }
                let lv = level(v);
                // A header's out-edges leave from inside its own loop.
                let lu = if self.is_header[u as usize] {
                    u
                } else {
                    level(u)
                };
                let l = common(lu, lv);
                let rv = rep_in(l, v, lv);
                indegree[rv as usize] += 1;
                if u == l {
                    source_succs[u as usize].push(rv);
                } else {
                    member_succs[rep_in(l, u, lu) as usize].push(rv);
                }
            }
        }

        let mut order = Vec::with_capacity(n);
        let mut block_end = vec![NONE; n];
        // One frame per open level: its header (`top` at the root, opened by
        // `InitialMemory`) and its members ready to place.
        let mut frames: Vec<(u32, Vec<u32>)> = vec![(top, vec![0])];
        while let Some((lvl, ready)) = frames.last_mut() {
            let lvl = *lvl;
            let Some(r) = ready.pop() else {
                if lvl != top {
                    block_end[lvl as usize] = order.len() as u32 - 1;
                }
                frames.pop();
                continue;
            };
            let succs = if r == lvl {
                order.push(r);
                &source_succs[r as usize]
            } else {
                &member_succs[r as usize]
            };
            for &s in succs {
                indegree[s as usize] -= 1;
                if indegree[s as usize] == 0 {
                    ready.push(s);
                }
            }
            if r != lvl {
                if self.is_header[r as usize] {
                    frames.push((r, vec![r]));
                } else {
                    order.push(r);
                }
            }
        }
        assert_eq!(order.len(), n, "a reducible graph has a nested order");
        (order, block_end)
    }
}

fn find(rep: &mut [u32], x: u32) -> u32 {
    let mut root = x;
    while rep[root as usize] != root {
        root = rep[root as usize];
    }
    let mut y = x;
    while rep[y as usize] != root {
        let next = rep[y as usize];
        rep[y as usize] = root;
        y = next;
    }
    root
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod layout_tests;
