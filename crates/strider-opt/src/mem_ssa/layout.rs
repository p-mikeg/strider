//! The memory graph laid out for a nearest-clobber climb.
//!
//! The nearest clobber of a location at a memory value is its nearest
//! dominator, itself included, that clobbers the location or is a `MemPhi`
//! whose region holds a clobber, and `InitialMemory` when there is none.  The
//! region of a phi is every def that reaches it without passing its immediate
//! dominator.  A clean region passes the dominator's answer through, since
//! every arm reaches the same dominator or cycles back to the phi; a dirty one
//! is the boundary, since some arm then names a def that does not dominate the
//! merge.  On a reducible graph this is the answer of the path walk in the
//! parent module, except where that walk re-reads a loop node its first visit
//! resolved under an ancestor no longer open and answers the node itself.
//!
//! Positions are a topological order in which every natural loop is one
//! contiguous block opened by its header.  A dominator precedes what it
//! dominates, and every def that reaches `x` without passing a dominator `a`
//! lies above `a` and at most at the end of the outermost loop holding `x` but
//! not `a`.  A position range with no clobber in it passes every dominator it
//! covers at once.

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};
use strider_ir::{Function, IRViewer};

const NONE: u32 = u32::MAX;

#[cfg(test)]
thread_local! {
    /// Climb iterations, region-walk visits and candidate asks, the unit the
    /// scaling tests count beside the def verdicts.
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

    /// The highest position in `lo..=hi` holding a def that may clobber.  A
    /// superset of the defs [`Self::clobbers`] accepts is enough.
    fn candidate(&mut self, layout: &MemLayout, lo: u32, hi: u32) -> Option<u32>;
}

/// One location's memoised climb results, by position.  Valid while no def's
/// verdict for the location changes.
#[derive(Default)]
pub(crate) struct Answers {
    nearest: FxHashMap<u32, u32>,
    dirty: FxHashMap<u32, bool>,
}

pub(crate) struct MemLayout {
    slot: SecondaryMap<NodeId, u32>,
    node: Vec<NodeId>,
    shape: Vec<Shape>,
    preds: Vec<SmallVec<[u32; 2]>>,
    /// `InitialMemory` is its own immediate dominator.
    idom: Vec<u32>,
    /// Skip pointer up the dominator tree, `O(log depth)` hops to any
    /// ancestor.
    dom_jump: Vec<u32>,
    /// Innermost loop header whose block holds the node, the node itself
    /// excluded; `NONE` at top level.
    encl: Vec<u32>,
    /// Skip pointer up the `encl` chain of a header.
    loop_jump: Vec<u32>,
    /// Last position of a header's block; the node itself otherwise.
    end: Vec<u32>,
    /// Lowest position of the run of defs ending at the node, each the memory
    /// input of the next and laid out next to it.
    chain_bottom: Vec<u32>,
    initial: u32,
}

impl MemLayout {
    /// The layout of every memory def reachable from `InitialMemory`, or
    /// `None` when the graph is outside what the climb answers: no unique
    /// `InitialMemory`, a def fed from outside that reach, or an irreducible
    /// loop.
    pub(crate) fn build(function: &Function) -> Option<Self> {
        let graph = Discovered::collect(function)?;
        let n = graph.node.len();
        let dfs = DepthFirst::from_root(&graph.succs);
        let idom_dfs = dominators(&graph.preds, &dfs);
        let dom_order = DomNumbering::new(&idom_dfs);
        // A retreating edge whose target does not dominate its source enters a
        // loop somewhere other than its header.
        for (v, preds) in graph.preds.iter().enumerate() {
            for &u in preds {
                let (dv, du) = (dfs.num[v], dfs.num[u as usize]);
                if dfs.is_ancestor(dv, du) && !dom_order.dominates(dv, du) {
                    return None;
                }
            }
        }
        let loops = LoopForest::new(&graph.preds, &dfs);
        let (order, block_end) = loops.nested_order(&graph.preds, &dfs, graph.initial)?;

        let mut pos = vec![NONE; n];
        for (p, &v) in order.iter().enumerate() {
            pos[v as usize] = p as u32;
        }
        let at = |v: u32| if v == NONE { NONE } else { pos[v as usize] };
        let mut slot = SecondaryMap::with_default(NONE);
        let mut node = Vec::with_capacity(n);
        let mut shape = Vec::with_capacity(n);
        let mut preds: Vec<SmallVec<[u32; 2]>> = Vec::with_capacity(n);
        let mut idom = Vec::with_capacity(n);
        let mut encl = Vec::with_capacity(n);
        let mut end = Vec::with_capacity(n);
        for (p, &v) in order.iter().enumerate() {
            let v = v as usize;
            slot[graph.node[v]] = p as u32;
            node.push(graph.node[v]);
            shape.push(graph.shape[v]);
            preds.push(graph.preds[v].iter().map(|&u| pos[u as usize]).collect());
            idom.push(pos[dfs.vertex[idom_dfs[dfs.num[v] as usize] as usize] as usize]);
            encl.push(at(loops.parent[v]));
            end.push(if loops.is_header[v] {
                block_end[v]
            } else {
                p as u32
            });
        }

        // Parents before children: preorder reaches a dominator first, and an
        // enclosing header too.
        let mut dom_depth = vec![0u32; n];
        let mut dom_jump = vec![NONE; n];
        let mut loop_depth = vec![0u32; n];
        let mut loop_jump = vec![NONE; n];
        for &v in &dfs.vertex {
            let x = pos[v as usize] as usize;
            let parent = idom[x] as usize;
            if parent == x {
                dom_jump[x] = x as u32;
            } else {
                dom_depth[x] = dom_depth[parent] + 1;
                dom_jump[x] = skip(&dom_depth, &dom_jump, parent as u32);
            }
            if loops.is_header[v as usize] {
                let parent = encl[x];
                if parent == NONE {
                    loop_depth[x] = 1;
                    loop_jump[x] = NONE;
                } else {
                    loop_depth[x] = loop_depth[parent as usize] + 1;
                    let j = loop_jump[parent as usize];
                    let jj = if j == NONE {
                        NONE
                    } else {
                        loop_jump[j as usize]
                    };
                    let depth = |h: u32| if h == NONE { 0 } else { loop_depth[h as usize] };
                    loop_jump[x] = if depth(parent) - depth(j) == depth(j) - depth(jj) && j != NONE
                    {
                        jj
                    } else {
                        parent
                    };
                }
            }
        }

        let mut chain_bottom = vec![NONE; n];
        for p in 0..n {
            let linked = p > 0
                && shape[p] == Shape::Def
                && shape[p - 1] == Shape::Def
                && preds[p].as_slice() == [p as u32 - 1];
            chain_bottom[p] = if linked {
                chain_bottom[p - 1]
            } else {
                p as u32
            };
        }

        Some(Self {
            slot,
            initial: pos[graph.initial as usize],
            node,
            shape,
            preds,
            idom,
            dom_jump,
            encl,
            loop_jump,
            end,
            chain_bottom,
        })
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
        let start = self.position(start)?;
        Some(self.node_at(self.climb(function, start, probe, answers)))
    }

    fn is_header(&self, x: u32) -> bool {
        self.end[x as usize] > x || self.preds[x as usize].contains(&x)
    }

    fn climb(
        &self,
        function: &Function,
        start: u32,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> u32 {
        let mut path: SmallVec<[u32; 8]> = SmallVec::new();
        let mut known = Known::default();
        let mut x = start;
        let answer = 'climb: loop {
            #[cfg(test)]
            step();
            if let Some(&a) = answers.nearest.get(&x) {
                break a;
            }
            path.push(x);
            // Every def reaching `x` without passing a dominator inside the
            // same loop lies in `floor..=end[x]`.
            let enc = self.encl[x as usize];
            let floor = if enc == NONE { 0 } else { enc + 1 };
            let Some(c) = known.highest(self, function, probe, floor, self.end[x as usize]) else {
                if enc == NONE {
                    break self.initial;
                }
                x = self.outermost_clean_loop(function, enc, probe, &mut known);
                continue;
            };
            let a = self.topmost_dominator_above(x, c);
            if a != x {
                x = a;
                if let Some(&a) = answers.nearest.get(&x) {
                    break a;
                }
                path.push(x);
            }
            match self.shape[x as usize] {
                Shape::Initial => break x,
                Shape::Phi => {
                    let clean = if self.end[x as usize] > x {
                        // Every def in the loop block is in the region.
                        if c > x {
                            break 'climb x;
                        }
                        self.entry_clean(function, x, c, probe, answers)
                    } else {
                        !self.dirty(function, x, c, probe, answers)
                    };
                    if !clean {
                        break x;
                    }
                    x = self.idom[x as usize];
                }
                Shape::Def => {
                    if (self.chain_bottom[x as usize]..=x).contains(&c) {
                        break c;
                    }
                    x = self.idom[self.chain_bottom[x as usize] as usize];
                }
            }
        };
        for p in path {
            answers.nearest.insert(p, answer);
        }
        answer
    }

    /// The outermost loop header from `header` outward whose whole block past
    /// the header is free of clobbers, or `header` itself.  The climb may move
    /// there: every def reaching the start without passing it lies in that
    /// block.
    fn outermost_clean_loop(
        &self,
        function: &Function,
        header: u32,
        probe: &mut dyn ClobberProbe,
        known: &mut Known,
    ) -> u32 {
        let mut h = header;
        let mut clean = |h: u32, probe: &mut dyn ClobberProbe| {
            known
                .highest(self, function, probe, h + 1, self.end[h as usize])
                .is_none()
        };
        loop {
            let j = self.loop_jump[h as usize];
            if j != NONE && clean(j, probe) {
                h = j;
                continue;
            }
            let p = self.encl[h as usize];
            if p != NONE && clean(p, probe) {
                h = p;
                continue;
            }
            return h;
        }
    }

    /// The topmost dominator of `x` laid out above `c`.
    fn topmost_dominator_above(&self, x: u32, c: u32) -> u32 {
        let mut a = x;
        loop {
            let j = self.dom_jump[a as usize];
            if j != a && j > c {
                a = j;
                continue;
            }
            let p = self.idom[a as usize];
            if p != a && p > c {
                a = p;
                continue;
            }
            return a;
        }
    }

    /// Whether no def reaching loop header `header` from outside its block
    /// clobbers.  `below` is the highest clobber at or below `header`.
    fn entry_clean(
        &self,
        function: &Function,
        header: u32,
        below: u32,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> bool {
        if below < self.idom[header as usize] {
            return true;
        }
        !self.region_dirty(function, header, probe, answers)
    }

    /// Whether merge `phi`'s region holds a clobber.  `below` is the highest
    /// clobber at or below `phi`.
    fn dirty(
        &self,
        function: &Function,
        phi: u32,
        below: u32,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> bool {
        if let Some(&d) = answers.dirty.get(&phi) {
            return d;
        }
        if below < self.idom[phi as usize] {
            answers.dirty.insert(phi, false);
            return false;
        }
        self.region_dirty(function, phi, probe, answers)
    }

    /// Walks the region of `phi` from its arms outside its own block, taking
    /// each enclosed loop and each merge already answered whole.
    fn region_dirty(
        &self,
        function: &Function,
        phi: u32,
        probe: &mut dyn ClobberProbe,
        answers: &mut Answers,
    ) -> bool {
        #[derive(Clone, Copy)]
        struct Frame {
            phi: u32,
            stop: u32,
            base: usize,
            id: u32,
        }
        let entry_arms = |p: u32| {
            self.preds[p as usize]
                .iter()
                .copied()
                .filter(move |&u| u < p)
        };
        let mut frames = vec![Frame {
            phi,
            stop: self.idom[phi as usize],
            base: 0,
            id: 0,
        }];
        let mut next_id = 1;
        let mut work: Vec<u32> = entry_arms(phi).collect();
        // Per frame: a region nested in another is walked again under its own.
        let mut seen: FxHashMap<u32, u32> = FxHashMap::default();
        let dirty = loop {
            let frame = *frames.last().expect("the outer frame ends the loop");
            if work.len() == frame.base {
                frames.pop();
                // A header's body is the climb's to check, so only a merge's
                // verdict is whole.
                if self.end[frame.phi as usize] == frame.phi {
                    answers.dirty.insert(frame.phi, false);
                }
                if frames.is_empty() {
                    break false;
                }
                work.push(self.idom[frame.phi as usize]);
                continue;
            }
            let y = work.pop().expect("above the frame base");
            #[cfg(test)]
            step();
            if y == frame.stop || y == frame.phi || seen.insert(y, frame.id) == Some(frame.id) {
                continue;
            }
            if let Some(h) = self.outermost_loop_apart(y, frame.phi, frame.stop) {
                // A whole loop of the region: its block is all in it.
                if self
                    .highest_clobber(function, probe, h, self.end[h as usize])
                    .is_some()
                {
                    break true;
                }
                if h == y || seen.insert(h, frame.id) != Some(frame.id) {
                    work.extend(entry_arms(h));
                }
                continue;
            }
            match self.shape[y as usize] {
                Shape::Initial => {}
                Shape::Phi => match answers.dirty.get(&y) {
                    Some(true) => break true,
                    Some(false) => work.push(self.idom[y as usize]),
                    None => {
                        if self
                            .highest_clobber(function, probe, self.idom[y as usize] + 1, y)
                            .is_none()
                        {
                            answers.dirty.insert(y, false);
                            work.push(self.idom[y as usize]);
                        } else {
                            frames.push(Frame {
                                phi: y,
                                stop: self.idom[y as usize],
                                base: work.len(),
                                id: next_id,
                            });
                            next_id += 1;
                            work.extend(entry_arms(y));
                        }
                    }
                },
                Shape::Def => {
                    let bottom = self.chain_bottom[y as usize];
                    let stop = frame.stop;
                    let reaches_stop = (bottom..=y).contains(&stop);
                    let lowest = if reaches_stop { stop + 1 } else { bottom };
                    if self.highest_clobber(function, probe, lowest, y).is_some() {
                        break true;
                    }
                    if !reaches_stop {
                        work.push(self.idom[bottom as usize]);
                    }
                }
            }
        };
        if dirty {
            // Every open region contains the one the clobber was found in.
            for frame in frames {
                if self.end[frame.phi as usize] == frame.phi {
                    answers.dirty.insert(frame.phi, true);
                }
            }
        }
        dirty
    }

    /// The outermost loop whose block holds `y` but neither `phi` nor `stop`.
    fn outermost_loop_apart(&self, y: u32, phi: u32, stop: u32) -> Option<u32> {
        let apart = |h: u32| {
            let block = h..=self.end[h as usize];
            !block.contains(&phi) && !block.contains(&stop)
        };
        let mut h = if self.is_header(y) {
            y
        } else {
            self.encl[y as usize]
        };
        if h == NONE || !apart(h) {
            return None;
        }
        loop {
            let j = self.loop_jump[h as usize];
            if j != NONE && apart(j) {
                h = j;
                continue;
            }
            let p = self.encl[h as usize];
            if p != NONE && apart(p) {
                h = p;
                continue;
            }
            return Some(h);
        }
    }

    /// The highest position in `lo..=hi` whose def clobbers.
    fn highest_clobber(
        &self,
        function: &Function,
        probe: &mut dyn ClobberProbe,
        lo: u32,
        hi: u32,
    ) -> Option<u32> {
        if lo > hi {
            return None;
        }
        let mut hi = hi;
        loop {
            #[cfg(test)]
            step();
            let c = probe.candidate(self, lo, hi)?;
            debug_assert!((lo..=hi).contains(&c), "candidate {c} outside {lo}..={hi}");
            if self.shape[c as usize] == Shape::Def
                && probe.clobbers(function, self.node[c as usize])
            {
                return Some(c);
            }
            if c == lo {
                return None;
            }
            hi = c - 1;
        }
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

/// The last range asked and its highest clobber, reused by a narrower ask.
#[derive(Default)]
struct Known {
    last: Option<(u32, u32, Option<u32>)>,
}

impl Known {
    fn highest(
        &mut self,
        layout: &MemLayout,
        function: &Function,
        probe: &mut dyn ClobberProbe,
        lo: u32,
        hi: u32,
    ) -> Option<u32> {
        if let Some((l, h, c)) = self.last
            && l == lo
            && hi <= h
            && c.is_none_or(|c| c <= hi)
        {
            return c;
        }
        let c = layout.highest_clobber(function, probe, lo, hi);
        self.last = Some((lo, hi, c));
        c
    }
}

/// The memory defs reachable from `InitialMemory`, in discovery order.
struct Discovered {
    node: Vec<NodeId>,
    shape: Vec<Shape>,
    preds: Vec<SmallVec<[u32; 2]>>,
    succs: Vec<SmallVec<[u32; 2]>>,
    initial: u32,
}

impl Discovered {
    fn collect(function: &Function) -> Option<Self> {
        let graph = function.graph();
        let mut initials = graph
            .all_node_ids()
            .filter(|&n| matches!(function.node_kind(n), NodeKind::InitialMemory));
        let initial = initials.next()?;
        if initials.next().is_some() {
            return None;
        }
        let mut index: SecondaryMap<NodeId, u32> = SecondaryMap::with_default(NONE);
        let mut node = vec![initial];
        index[initial] = 0;
        let mut i = 0;
        while i < node.len() {
            let out = memory_output(function, node[i]);
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
                NodeKind::MemPhi => (Shape::Phi, function.phi_data_inputs(id).collect()),
                _ => (
                    Shape::Def,
                    function.memory_input_of(id).into_iter().collect(),
                ),
            };
            shape.push(s);
            let mut ps = SmallVec::new();
            for input in inputs {
                let u = index[function.producer(input)];
                if u == NONE {
                    return None;
                }
                ps.push(u);
                succs[u as usize].push(v as u32);
            }
            preds.push(ps);
        }
        Some(Self {
            node,
            shape,
            preds,
            succs,
            initial: 0,
        })
    }
}

fn memory_output(function: &Function, node: NodeId) -> ValueId {
    function
        .node_outputs(node)
        .iter()
        .copied()
        .find(|&v| function.graph().value_kind(v) == ValueKind::Memory)
        .expect("a memory def has a memory output")
}

/// Whether `user` reads memory at `slot` and produces memory of its own.
fn consumes_memory_at(function: &Function, user: NodeId, slot: u32) -> bool {
    match function.node_kind(user) {
        NodeKind::MemPhi => slot > 0,
        NodeKind::Store(_) => slot == 0,
        NodeKind::Call { .. } | NodeKind::CallOther { .. } => slot == 1,
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

/// Lengauer-Tarjan immediate dominators, by preorder number.
fn dominators(preds: &[SmallVec<[u32; 2]>], dfs: &DepthFirst) -> Vec<u32> {
    let n = dfs.vertex.len();
    let mut semi: Vec<u32> = (0..n as u32).collect();
    let mut ancestor = vec![NONE; n];
    let mut label: Vec<u32> = (0..n as u32).collect();
    let mut idom = vec![0u32; n];
    let mut bucket: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut path: Vec<u32> = Vec::new();

    let eval =
        |v: u32, ancestor: &mut [u32], label: &mut [u32], semi: &[u32], path: &mut Vec<u32>| {
            if ancestor[v as usize] == NONE {
                return v;
            }
            let mut x = v;
            while ancestor[ancestor[x as usize] as usize] != NONE {
                path.push(x);
                x = ancestor[x as usize];
            }
            while let Some(y) = path.pop() {
                let a = ancestor[y as usize];
                if semi[label[a as usize] as usize] < semi[label[y as usize] as usize] {
                    label[y as usize] = label[a as usize];
                }
                ancestor[y as usize] = ancestor[a as usize];
            }
            label[v as usize]
        };

    for w in (1..n).rev() {
        for &p in &preds[dfs.vertex[w] as usize] {
            let v = dfs.num[p as usize];
            let u = eval(v, &mut ancestor, &mut label, &semi, &mut path);
            if semi[u as usize] < semi[w] {
                semi[w] = semi[u as usize];
            }
        }
        bucket[semi[w] as usize].push(w as u32);
        let parent = dfs.parent[w];
        ancestor[w] = parent;
        for v in std::mem::take(&mut bucket[parent as usize]) {
            let u = eval(v, &mut ancestor, &mut label, &semi, &mut path);
            idom[v as usize] = if semi[u as usize] < semi[v as usize] {
                u
            } else {
                parent
            };
        }
    }
    for w in 1..n {
        if idom[w] != semi[w] {
            idom[w] = idom[idom[w] as usize];
        }
    }
    idom[0] = 0;
    idom
}

/// Dominator-tree interval numbering, by preorder number.
struct DomNumbering {
    enter: Vec<u32>,
    exit: Vec<u32>,
}

impl DomNumbering {
    fn new(idom: &[u32]) -> Self {
        let n = idom.len();
        let mut children: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (v, &d) in idom.iter().enumerate().skip(1) {
            children[d as usize].push(v as u32);
        }
        let mut enter = vec![0u32; n];
        let mut exit = vec![0u32; n];
        let mut clock = 0u32;
        let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
        enter[0] = clock;
        clock += 1;
        while let Some(top) = stack.last_mut() {
            let (v, next) = *top;
            if let Some(&c) = children[v as usize].get(next) {
                top.1 += 1;
                enter[c as usize] = clock;
                clock += 1;
                stack.push((c, 0));
            } else {
                exit[v as usize] = clock;
                clock += 1;
                stack.pop();
            }
        }
        Self { enter, exit }
    }

    fn dominates(&self, a: u32, b: u32) -> bool {
        self.enter[a as usize] <= self.enter[b as usize]
            && self.exit[b as usize] <= self.exit[a as usize]
    }
}

/// Natural loops, found by collapsing each header's body into it innermost
/// first.
struct LoopForest {
    /// Innermost header whose loop holds the node, the node excluded.
    parent: Vec<u32>,
    is_header: Vec<bool>,
}

impl LoopForest {
    fn new(preds: &[SmallVec<[u32; 2]>], dfs: &DepthFirst) -> Self {
        let n = preds.len();
        let mut rep: Vec<u32> = (0..n as u32).collect();
        let mut parent = vec![NONE; n];
        let mut is_header = vec![false; n];
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
            while i < body.len() {
                let x = body[i];
                i += 1;
                for &y in &preds[x as usize] {
                    let r = find(&mut rep, y);
                    if r != w && mark[r as usize] != w && dfs.is_ancestor(dw, dfs.num[r as usize]) {
                        mark[r as usize] = w;
                        body.push(r);
                    }
                }
            }
            for &x in &body {
                parent[x as usize] = w;
                rep[x as usize] = w;
            }
        }
        Self { parent, is_header }
    }

    /// Nodes in position order, and each header's last position, or `None`
    /// when a level has no topological order from its entry.  Each level
    /// (the top, or one loop's body with inner loops collapsed into their
    /// headers) is ordered topologically from its entry, and an inner loop's
    /// block is spliced in where its header falls.
    fn nested_order(
        &self,
        preds: &[SmallVec<[u32; 2]>],
        dfs: &DepthFirst,
        initial: u32,
    ) -> Option<(Vec<u32>, Vec<u32>)> {
        let n = preds.len();
        let top = n as u32;
        let level = |x: u32| {
            let p = self.parent[x as usize];
            if p == NONE { top } else { p }
        };
        // Loop depth and skip pointers over headers, the top level at depth 0.
        let mut depth = vec![0u32; n + 1];
        let mut jump = vec![top; n + 1];
        for &v in &dfs.vertex {
            if self.is_header[v as usize] {
                let p = level(v);
                depth[v as usize] = depth[p as usize] + 1;
                let j = jump[p as usize];
                let jj = jump[j as usize];
                jump[v as usize] = if depth[p as usize] - depth[j as usize]
                    == depth[j as usize] - depth[jj as usize]
                {
                    jj
                } else {
                    p
                };
            }
        }
        let ancestor_at = |mut h: u32, d: u32| {
            while depth[h as usize] > d {
                h = if depth[jump[h as usize] as usize] >= d {
                    jump[h as usize]
                } else {
                    level(h)
                };
            }
            h
        };

        // Edges between the representatives of one level: a node at its own
        // level, or the header of the inner loop holding it.  A header opens
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
                indegree[v as usize] += 1;
                if u == lv {
                    source_succs[u as usize].push(v);
                } else {
                    let lu = level(u);
                    let r = if lu == lv {
                        u
                    } else {
                        ancestor_at(lu, depth[lv as usize] + 1)
                    };
                    member_succs[r as usize].push(v);
                }
            }
        }

        let mut order = Vec::with_capacity(n);
        let mut block_end = vec![NONE; n];
        // One frame per open level: its header (`top` at the root) and the
        // members still ready to place.
        let mut frames: Vec<(u32, Vec<u32>)> = vec![(top, vec![initial])];
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
        (order.len() == n).then_some((order, block_end))
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
