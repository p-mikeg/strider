pub(crate) mod builder;
pub(crate) mod graph;
pub use graph::MAX_PATTERN_NODES;
pub(crate) mod match_pat;
pub(crate) mod vertex;
pub(crate) mod walk;

pub use builder::{MatcherBuilder, PatNodeRef, PatValueRef};
pub use graph::Pattern;
pub use strider_ir::node::CastMask;
pub(crate) use vertex::{InputSlot, OutputKindSpec, PatNode, PatValue, Role, SlotMode};
pub use vertex::{KindSpec, NodePredicate, PostMatchFn};

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::mem::Discriminant;

use either::Either;
use petgraph::unionfind::UnionFind;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::NodeId as PatNodeId;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{
    CtrlKey, DominatorTree, Function, Graph, IRViewer, IRWalker, control_dominator_tree,
    control_edge_dominator_tree,
};

use crate::bindings::{Binding, Bindings};
use crate::match_result::Match;

/// `None` for a kind-`Any` root.
fn root_kind_discriminant(pat: &Pattern, root: PatNodeId) -> Option<Discriminant<NodeKind>> {
    pat.graph.node_kind(root).kind.discriminant()
}

/// The deduplicated kinds an alternation root's arms accept. `None` for a non-alternation root or
/// one with a kind-`Any` arm.
fn alternation_kind_discriminants(
    pat: &Pattern,
    root: PatNodeId,
) -> Option<Vec<Discriminant<NodeKind>>> {
    if !pat.graph.node_kind(root).is_alternation() {
        return None;
    }
    let mut kinds: Vec<Discriminant<NodeKind>> = Vec::new();
    let mut seen = vec![root];
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for (_, vertex) in crate::graph_ext::consumed_inputs(&pat.graph, node) {
            let arm = pat.graph.producer(vertex);
            match root_kind_discriminant(pat, arm) {
                Some(d) => {
                    if !kinds.contains(&d) {
                        kinds.push(d);
                    }
                }
                None if pat.graph.node_kind(arm).is_alternation() => {
                    if !seen.contains(&arm) {
                        seen.push(arm);
                        stack.push(arm);
                    }
                }
                None => return None,
            }
        }
    }
    Some(kinds)
}

pub struct Matcher<'f> {
    pub(crate) function: &'f Function,
}

impl<'f> Matcher<'f> {
    /// Pattern validity is checked per query.
    pub fn new(function: &'f Function) -> Self {
        Self { function }
    }

    pub fn function(&self) -> &Function {
        self.function
    }

    /// Every match for `pat`, lazily per candidate node.
    ///
    /// Each distinct capture-to-binding map is yielded once, natural operand order before
    /// swapped. `n` commutative nodes can yield, and cost, up to 2^n even when nothing matches;
    /// `.ordered()` pins them.
    ///
    /// # Errors
    /// As [`Pattern::root`].
    pub fn matches<'p>(
        &'p self,
        pat: &'p Pattern,
    ) -> anyhow::Result<impl Iterator<Item = Match> + 'p> {
        self.matches_over(pat, self.function.walk())
    }

    /// [`matches`](Self::matches) over the reachable nodes `walk` yields.
    fn matches_over<'p>(
        &'p self,
        pat: &'p Pattern,
        walk: impl Iterator<Item = NodeId> + 'p,
    ) -> anyhow::Result<impl Iterator<Item = Match> + 'p> {
        let root = pat.root()?;
        // With no capture every configuration has one signature: the first is the whole answer.
        let first_only = pat.bound_captures().next().is_none();
        let mut scratch = Bindings::default();
        Ok(self
            .candidates(pat, root, walk)
            .filter(move |&n| walk::may_match_at(self, pat, root, n))
            .flat_map(move |node| self.matches_at_node(node, pat, root, first_only, &mut scratch)))
    }

    /// [`matches`](Self::matches)`.collect()`.
    ///
    /// # Errors
    /// As [`Pattern::root`].
    pub fn find_all(&self, pat: &Pattern) -> anyhow::Result<Vec<Match>> {
        Ok(self.matches(pat)?.collect())
    }

    /// The IR nodes to attempt `pat` at, in walk order, filtered by the root's kind (or each
    /// alternation arm's kind in turn).
    fn candidates<'p>(
        &'p self,
        pat: &Pattern,
        root: PatNodeId,
        walk: impl Iterator<Item = NodeId> + 'p,
    ) -> impl Iterator<Item = NodeId> + 'p {
        let f = self.function;
        let is = move |n: NodeId, d| std::mem::discriminant(f.node_kind(n)) == d;
        let kinds = root_kind_discriminant(pat, root)
            .map(|d| vec![d])
            .or_else(|| alternation_kind_discriminants(pat, root));
        match kinds.as_deref() {
            Some(&[d]) => Either::Left(Either::Left(walk.filter(move |&n| is(n, d)))),
            Some(ds) => {
                let order: Vec<NodeId> = walk.collect();
                let by_kind: Vec<NodeId> = ds
                    .iter()
                    .flat_map(|&d| order.iter().copied().filter(move |&n| is(n, d)))
                    .collect();
                Either::Left(Either::Right(by_kind.into_iter()))
            }
            None => Either::Right(walk),
        }
    }

    /// Every match of `pat` rooted at `node`, deduplicated by capture-to-binding map; see
    /// [`walk::anchor`]. `first_only` stops at the first, moving `scratch` into it.
    fn matches_at_node(
        &self,
        node: NodeId,
        pat: &Pattern,
        root: PatNodeId,
        first_only: bool,
        scratch: &mut Bindings,
    ) -> Vec<Match> {
        if pat.holds_first_of || !pat.cast_mask.is_empty() {
            return self.lowest_arm_matches(node, pat, root, first_only, scratch);
        }
        let mut hits: Vec<Match> = Vec::new();
        // Keyed on bindings alone: an existential reaching one binding through different slots is
        // one match. Two `one_of` arms binding identically collapse too, the first one's
        // `matched_nodes` winning. Filled lazily, since most roots have one configuration.
        let mut seen: FxHashSet<Vec<(u32, Binding)>> = FxHashSet::default();
        // The callback's `true` stops the walk; `false` (match banked) moves it to the next
        // configuration.
        let generation = self.function.graph().generation();
        walk::anchor(self, pat, root, node, scratch, None, &mut |b| {
            if first_only {
                hits.push(Match::new(node, std::mem::take(b), generation));
            } else if hits.is_empty() || {
                if seen.is_empty() {
                    seen.insert(hits[0].bindings.binding_signature());
                }
                seen.insert(b.binding_signature())
            } {
                hits.push(Match::new(node, b.clone(), generation));
            }
            first_only
        });
        hits
    }

    /// [`Self::matches_at_node`] for a pattern holding a `first_of` or a cast mask.
    ///
    /// Keeps, at each choice point (a `first_of` or a cast-mask read), only the matches that took
    /// the lowest arm there, settled in a fixed order so the walk order does not matter.
    #[inline(never)]
    fn lowest_arm_matches(
        &self,
        node: NodeId,
        pat: &Pattern,
        root: PatNodeId,
        first_only: bool,
        scratch: &mut Bindings,
    ) -> Vec<Match> {
        let generation = self.function.graph().generation();
        let mut found: Vec<(Match, Vec<walk::ArmTaken>)> = Vec::new();
        let trail = walk::ArmTrail::default();
        // An earlier match through a higher arm at another choice point can
        // survive the settle, so only a capture-free pattern stops early.
        let stop_early = first_only && pat.bound_captures().next().is_none();
        walk::anchor(self, pat, root, node, scratch, Some(&trail), &mut |b| {
            let arms = trail.arms.borrow();
            if stop_early && arms.iter().all(|e| e.1 == 0) {
                found = vec![(Match::new(node, std::mem::take(b), generation), Vec::new())];
                return true;
            }
            found.push((Match::new(node, b.clone(), generation), arms.clone()));
            false
        });
        // (key, match, arm), sorted into settle order.
        let mut taken: Vec<(walk::ArmKey, usize, usize)> = found
            .iter()
            .enumerate()
            .flat_map(|(i, (_, arms))| arms.iter().map(move |&(key, arm)| (key, i, arm)))
            .collect();
        taken.sort_unstable_by_key(|&((c, (p, n, o), v, read), _, _)| {
            let id = |v: Option<ValueId>| v.map(ValueId::as_u32);
            (c, p.as_u32(), read, n.as_u32(), id(o), id(v))
        });
        let mut kept = vec![true; found.len()];
        for group in taken.chunk_by(|a, b| a.0 == b.0) {
            let Some(low) = group.iter().filter(|e| kept[e.1]).map(|e| e.2).min() else {
                continue;
            };
            for e in group {
                kept[e.1] &= e.2 == low;
            }
        }
        let mut seen = FxHashSet::default();
        let distinct = found
            .into_iter()
            .zip(kept)
            .filter_map(|((m, _), keep)| keep.then_some(m))
            .filter(|m| seen.insert(m.bindings.binding_signature()));
        distinct
            .take(if first_only { 1 } else { usize::MAX })
            .collect()
    }

    /// The first match of `pat` at `node`, if any. `node` need not be reachable, unlike every
    /// [`find_all`](Self::find_all) root.
    ///
    /// # Errors
    /// As [`Pattern::root`].
    pub fn match_at(&self, node: NodeId, pat: &Pattern) -> anyhow::Result<Option<Match>> {
        let root = pat.root()?;
        if !walk::may_match_at(self, pat, root, node) {
            return Ok(None);
        }
        Ok(self
            .matches_at_node(node, pat, root, true, &mut Bindings::default())
            .into_iter()
            .next())
    }

    /// The tuples (one [`Match`] per pattern) whose shared captures agree and where every
    /// [`JoinConstraint`] holds; an unanswerable constraint drops the tuple, negated or not.
    /// Deduplicated by capture bindings. Worst case O(N1 * ... * NM).
    ///
    /// # Errors
    /// If a pattern has no root (see [`Pattern::root`]), a capture-bearing pattern shares
    /// neither a capture nor a constraint with the rest, or a constraint names an unbound capture.
    pub fn find_joined_constrained(
        &self,
        pats: &[&Pattern],
        constraints: &[JoinConstraint],
    ) -> anyhow::Result<Vec<JoinedMatch>> {
        if pats.is_empty() {
            return Ok(Vec::new());
        }
        let cap_settled = correlate(pats, constraints)?;

        // Each pattern's independent matches, not a joined row.
        let order: Vec<NodeId> = self.function.walk().collect();
        let per_pat: Vec<Vec<Match>> = pats
            .iter()
            .map(|p| Ok(self.matches_over(p, order.iter().copied())?.collect()))
            .collect::<anyhow::Result<_>>()?;
        if per_pat.iter().any(|hits| hits.is_empty()) {
            return Ok(Vec::new());
        }

        let eval = ConstraintEval::new(self.function());
        let by_level = level_constraints(constraints, &cap_settled, pats.len());
        let whole_tuple: Vec<_> = constraints
            .iter()
            .filter(|c| c.reads_whole_tuple())
            .collect();
        // Index rows, so a pruned prefix clones no `Match`.
        let passes_all = |cs: &[&JoinConstraint], idx: &[usize]| {
            let row = Row::Indices {
                per_pat: &per_pat,
                idx,
            };
            cs.iter().all(|c| eval.passes(c, row) == Some(true))
        };

        let mut acc: Vec<Vec<usize>> = vec![Vec::new()];
        for (j, level) in by_level.iter().enumerate() {
            acc = join_level(&per_pat, &acc, j, self.function().graph(), |cand| {
                passes_all(level, cand)
            });
            if acc.is_empty() {
                break;
            }
        }

        let mut out: Vec<JoinedMatch> = acc
            .iter()
            .map(|idx| {
                idx.iter()
                    .enumerate()
                    .map(|(i, &j)| per_pat[i][j].clone())
                    .collect()
            })
            .collect();
        out.retain(|tuple| {
            whole_tuple
                .iter()
                .all(|c| eval.passes(c, Row::Full(tuple)) == Some(true))
        });

        dedup_on_shared_captures(&mut out);
        Ok(out)
    }
}

/// Checks that capture-bearing patterns form one component under "shares a capture or a
/// constraint" and that every constraint capture is bound. Returns each capture's
/// highest-indexed binding pattern.
fn correlate(
    pats: &[&Pattern],
    constraints: &[JoinConstraint],
) -> anyhow::Result<FxHashMap<crate::Capture, usize>> {
    let mut components = UnionFind::<usize>::new(pats.len());
    let mut cap_owner: FxHashMap<crate::Capture, usize> = FxHashMap::default();
    let mut cap_settled: FxHashMap<crate::Capture, usize> = FxHashMap::default();
    let mut capture_bearing: Vec<usize> = Vec::new();
    for (i, p) in pats.iter().enumerate() {
        let mut has_cap = false;
        for c in p.bound_captures() {
            has_cap = true;
            cap_settled.insert(c, i);
            if let Some(&j) = cap_owner.get(&c) {
                components.union(i, j);
            } else {
                cap_owner.insert(c, i);
            }
        }
        if has_cap {
            capture_bearing.push(i);
        }
    }
    for con in constraints {
        let caps = con.captures();
        if let Some(c) = caps.iter().find(|c| !cap_owner.contains_key(c)) {
            anyhow::bail!(
                "find_joined_constrained: constraint mentions capture {c:?}, which no pattern \
                 in the join binds, so it could never be satisfied (and under \
                 `negate` would hold vacuously, true because nothing was seen); \
                 bind it with a positive pattern"
            );
        }
        let mut owners = caps.into_iter().map(|c| cap_owner[&c]);
        if let Some(first) = owners.next() {
            for j in owners {
                components.union(first, j);
            }
        }
    }
    if let Some((&first, rest)) = capture_bearing.split_first() {
        for &i in rest {
            if !components.equiv(first, i) {
                anyhow::bail!(
                    "find_joined_constrained: pattern {i} shares no capture (even transitively) \
                     with the others; a join correlates on shared captures (use a \
                     capture-free pattern for an intentional cross-product)"
                );
            }
        }
    }
    Ok(cap_settled)
}

/// Each constraint not reading the whole tuple, at the earliest row length that settles it.
fn level_constraints<'c>(
    constraints: &'c [JoinConstraint],
    cap_settled: &FxHashMap<crate::Capture, usize>,
    levels: usize,
) -> Vec<Vec<&'c JoinConstraint>> {
    let mut by_level = vec![Vec::new(); levels];
    for c in constraints.iter().filter(|c| !c.reads_whole_tuple()) {
        let caps = c.captures();
        let level = caps.iter().filter_map(|cap| cap_settled.get(cap)).max();
        by_level[level.copied().unwrap_or(0)].push(c);
    }
    by_level
}

/// Every row of `acc` extended by each match of pattern `j` it agrees with and `keep` accepts.
/// Agreeing bindings resolve to the same node, so only pairs sharing a [`join_key`] are checked.
fn join_level(
    per_pat: &[Vec<Match>],
    acc: &[Vec<usize>],
    j: usize,
    graph: &Graph,
    keep: impl Fn(&[usize]) -> bool,
) -> Vec<Vec<usize>> {
    let next = &per_pat[j];
    let key_caps = join_key_captures(per_pat, acc, next, j);
    let mut buckets: FxHashMap<Vec<u32>, Vec<usize>> = FxHashMap::default();
    for (i, m) in next.iter().enumerate() {
        if let Some(k) = join_key(&m.bindings, &key_caps, graph) {
            buckets.entry(k).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for prefix in acc {
        let row = Row::Indices {
            per_pat,
            idx: prefix,
        };
        let Some(k) = row_join_key(row, &key_caps, graph) else {
            continue;
        };
        for &i in buckets.get(&k).into_iter().flatten() {
            if !row_agrees(row, &next[i], graph) {
                continue;
            }
            let mut cand = prefix.clone();
            cand.push(i);
            if keep(&cand) {
                out.push(cand);
            }
        }
    }
    out
}

/// A filter over captured entities for [`Matcher::find_joined_constrained`].
///
/// A control-output capture (e.g. [`IfPat::capture_true`](crate::IfPat::capture_true)) resolves
/// to a `CtrlKey::Edge`, anything else to a `CtrlKey::Node` (a value's producer).
#[derive(Clone)]
pub enum JoinConstraint {
    /// `dominator` dominates `dominated` in the control subgraph, an edge on the edge-split graph
    /// (so `dominates(true_edge, c)` puts `c` in the true block). A capture outside the dominator
    /// tree is unanswerable.
    Dominates {
        dominator: crate::Capture,
        dominated: crate::Capture,
    },
    /// The `Phi`/`MemPhi` bound to `phi` merges `value` on a predecessor whose control input
    /// `edge` dominates as an edge. `edge` must bind a control-output value and `phi` a value.
    PhiInputFromEdge {
        phi: crate::Capture,
        edge: crate::Capture,
        value: crate::Capture,
    },
    /// Passes iff `inner` fails; an unanswerable `inner` still drops the row.
    Not(Box<JoinConstraint>),
    /// Passes iff any listed constraint does. An empty list passes nothing.
    Or(Vec<JoinConstraint>),
    /// Passes iff every listed constraint does. An empty list passes
    /// everything.
    And(Vec<JoinConstraint>),
    /// A user predicate over the whole tuple; `None` is unanswerable. `captures` join the
    /// correlation checks, and a row leaving any unbound drops before `pred` runs.
    Where {
        captures: Vec<crate::Capture>,
        pred: JoinPredicateFn,
    },
}

/// Iterative, since the tree's depth is the caller's.
impl Drop for JoinConstraint {
    fn drop(&mut self) {
        match self {
            Self::Not(inner) => crate::node_builders::defer_drop(std::mem::replace(
                &mut **inner,
                Self::And(Vec::new()),
            )),
            Self::Or(list) | Self::And(list) => {
                crate::node_builders::defer_drop(std::mem::take(list));
            }
            _ => {}
        }
    }
}

impl std::fmt::Debug for JoinConstraint {
    /// Iterative, since the tree's depth is the caller's.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        enum DebugItem<'a> {
            Node(&'a JoinConstraint),
            Text(&'static str),
        }

        let mut stack = vec![DebugItem::Node(self)];
        while let Some(item) = stack.pop() {
            let c = match item {
                DebugItem::Text(t) => {
                    f.write_str(t)?;
                    continue;
                }
                DebugItem::Node(c) => c,
            };
            match c {
                Self::Dominates {
                    dominator,
                    dominated,
                } => f
                    .debug_struct("Dominates")
                    .field("dominator", dominator)
                    .field("dominated", dominated)
                    .finish()?,
                Self::PhiInputFromEdge { phi, edge, value } => f
                    .debug_struct("PhiInputFromEdge")
                    .field("phi", phi)
                    .field("edge", edge)
                    .field("value", value)
                    .finish()?,
                Self::Where { captures, .. } => f
                    .debug_struct("Where")
                    .field("captures", captures)
                    .finish_non_exhaustive()?,
                Self::Not(_) | Self::Or(_) | Self::And(_) => {
                    let (open, close) = match c {
                        Self::Not(_) => ("Not(", ")"),
                        Self::Or(_) => ("Or([", "])"),
                        _ => ("And([", "])"),
                    };
                    f.write_str(open)?;
                    stack.push(DebugItem::Text(close));
                    for (i, arm) in c.children().iter().enumerate().rev() {
                        stack.push(DebugItem::Node(arm));
                        if i > 0 {
                            stack.push(DebugItem::Text(", "));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// A joined row: indices into the per-pattern matches while building, then a full tuple.
#[derive(Clone, Copy)]
enum Row<'a> {
    Indices {
        per_pat: &'a [Vec<Match>],
        idx: &'a [usize],
    },
    Full(&'a JoinedMatch),
}

impl<'a> Row<'a> {
    fn iter(&self) -> impl Iterator<Item = &'a Match> + 'a {
        match *self {
            Row::Indices { per_pat, idx } => {
                Either::Left(idx.iter().enumerate().map(move |(i, &j)| &per_pat[i][j]))
            }
            Row::Full(tuple) => Either::Right(tuple.iter()),
        }
    }
}

impl JoinConstraint {
    /// A connective's arms; none for a leaf.
    fn children(&self) -> &[JoinConstraint] {
        match self {
            Self::Not(inner) => std::slice::from_ref(&**inner),
            Self::Or(cs) | Self::And(cs) => cs,
            _ => &[],
        }
    }

    /// The tree in preorder, on an explicit stack: its depth is the caller's.
    fn nodes(&self) -> impl Iterator<Item = &JoinConstraint> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let c = stack.pop()?;
            stack.extend(c.children().iter().rev());
            Some(c)
        })
    }

    /// Whether it needs a complete row.
    fn reads_whole_tuple(&self) -> bool {
        self.nodes().any(|c| matches!(c, Self::Where { .. }))
    }

    /// Every capture this constraint correlates, in the order it names them.
    fn captures(&self) -> Vec<crate::Capture> {
        let mut out = Vec::new();
        for c in self.nodes() {
            match c {
                Self::Dominates {
                    dominator,
                    dominated,
                } => out.extend([*dominator, *dominated]),
                Self::PhiInputFromEdge { phi, edge, value } => {
                    out.extend([*phi, *edge, *value]);
                }
                Self::Where { captures, .. } => out.extend(captures.iter().copied()),
                Self::Not(_) | Self::Or(_) | Self::And(_) => {}
            }
        }
        out
    }
}

/// One row of a join: one [`Match`] per pattern, in input order.
pub type JoinedMatch = Vec<Match>;

/// A user join predicate: decides a whole tuple, `None` when unanswerable.
pub type JoinPredicateFn =
    std::sync::Arc<dyn Fn(&Function, &JoinedMatch) -> Option<bool> + Send + Sync>;

/// Kleene fold over a connective's arms. `dominant` (`true` for OR, `false` for AND) settles it
/// on sight; otherwise an unknown arm makes it unknown, and no arms yield `!dominant`.
#[derive(Clone, Copy)]
struct Kleene {
    dominant: bool,
    saw_unknown: bool,
}

impl Kleene {
    /// `true` for OR, `false` for AND.
    fn new(dominant: bool) -> Self {
        Self {
            dominant,
            saw_unknown: false,
        }
    }

    /// `Some` once the connective is settled.
    fn arm(&mut self, verdict: Option<bool>) -> Option<Option<bool>> {
        match verdict {
            Some(b) if b == self.dominant => Some(Some(self.dominant)),
            None => {
                self.saw_unknown = true;
                None
            }
            Some(_) => None,
        }
    }

    /// The verdict with every arm folded in.
    fn finish(self) -> Option<bool> {
        (!self.saw_unknown).then_some(!self.dominant)
    }
}

/// Evaluates [`JoinConstraint`]s, memoising both dominator trees.
struct ConstraintEval<'f> {
    function: &'f Function,
    doms: OnceCell<DominatorTree<NodeId>>,
    /// Edge-split control subgraph.
    split_doms: OnceCell<DominatorTree<CtrlKey>>,
}

impl<'f> ConstraintEval<'f> {
    fn new(function: &'f Function) -> Self {
        Self {
            function,
            doms: OnceCell::new(),
            split_doms: OnceCell::new(),
        }
    }

    // Subsumed by `split_doms`, but cheaper for a node-only join.
    fn doms(&self) -> &DominatorTree<NodeId> {
        self.doms
            .get_or_init(|| control_dominator_tree(self.function))
    }

    fn split_doms(&self) -> &DominatorTree<CtrlKey> {
        self.split_doms
            .get_or_init(|| control_edge_dominator_tree(self.function))
    }

    /// A control value is a [`CtrlKey::Edge`], another value its producer's [`CtrlKey::Node`], a
    /// node its own; `None` if unbound.
    fn ctrl_key_of(&self, tuple: Row<'_>, c: crate::Capture) -> Option<CtrlKey> {
        if let Some(v) = self.value_of(tuple, c) {
            return Some(if self.function.value_kind(v).is_control() {
                CtrlKey::Edge(v)
            } else {
                CtrlKey::Node(self.function.producer(v))
            });
        }
        self.node_of(tuple, c).map(CtrlKey::Node)
    }

    fn node_of(&self, tuple: Row<'_>, c: crate::Capture) -> Option<NodeId> {
        tuple.iter().find_map(|m| m.node(c, self.function.graph()))
    }

    fn value_of(&self, tuple: Row<'_>, c: crate::Capture) -> Option<ValueId> {
        tuple.iter().find_map(|m| m.value(c))
    }

    /// Three-valued verdict, `None` when unanswerable. Iterative, since the tree's depth is the
    /// caller's.
    fn passes(&self, c: &JoinConstraint, tuple: Row<'_>) -> Option<bool> {
        /// A connective waiting on the arm being evaluated.
        enum Frame<'a> {
            Not,
            Fold {
                acc: Kleene,
                rest: std::slice::Iter<'a, JoinConstraint>,
            },
        }

        let mut stack: Vec<Frame<'_>> = Vec::new();
        let mut next = c;
        loop {
            let mut verdict = loop {
                let (acc, cs) = match next {
                    JoinConstraint::Not(inner) => {
                        stack.push(Frame::Not);
                        next = &**inner;
                        continue;
                    }
                    JoinConstraint::Or(cs) => (Kleene::new(true), cs),
                    JoinConstraint::And(cs) => (Kleene::new(false), cs),
                    leaf => break self.leaf_passes(leaf, tuple),
                };
                let mut rest = cs.iter();
                match rest.next() {
                    Some(first) => {
                        stack.push(Frame::Fold { acc, rest });
                        next = first;
                    }
                    None => break acc.finish(),
                }
            };
            loop {
                let Some(frame) = stack.last_mut() else {
                    return verdict;
                };
                match frame {
                    Frame::Not => {
                        verdict = verdict.map(|b| !b);
                        stack.pop();
                    }
                    Frame::Fold { acc, rest } => match acc.arm(verdict) {
                        Some(settled) => {
                            verdict = settled;
                            stack.pop();
                        }
                        None => match rest.next() {
                            Some(arm) => {
                                next = arm;
                                break;
                            }
                            None => {
                                verdict = acc.finish();
                                stack.pop();
                            }
                        },
                    },
                }
            }
        }
    }

    /// One constraint that names its own captures, with no arms below it.
    fn leaf_passes(&self, c: &JoinConstraint, tuple: Row<'_>) -> Option<bool> {
        match *c {
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => {
                let (Some(phi_v), Some(edge_v), Some(val_v)) = (
                    self.value_of(tuple, phi),
                    self.value_of(tuple, edge),
                    self.value_of(tuple, value),
                ) else {
                    return None;
                };
                // A non-phi `phi` or non-control `edge` is unanswerable, not `false`, so `Not`
                // still drops the row.
                if !self.function.value_kind(edge_v).is_control() {
                    return None;
                }
                Some(
                    self.phi_arms_from_edge(phi_v, edge_v)?
                        .any(|arm| arm == val_v),
                )
            }
            JoinConstraint::Dominates {
                dominator,
                dominated,
            } => {
                let (Some(key_a), Some(key_b)) = (
                    self.ctrl_key_of(tuple, dominator),
                    self.ctrl_key_of(tuple, dominated),
                ) else {
                    return None;
                };
                match (key_a, key_b) {
                    (CtrlKey::Node(na), CtrlKey::Node(nb)) => self.doms().dominance_verdict(na, nb),
                    (ka, kb) => self.split_doms().dominance_verdict(ka, kb),
                }
            }
            JoinConstraint::Not(_) | JoinConstraint::Or(_) | JoinConstraint::And(_) => {
                unreachable!("a connective reached the leaf walk")
            }
            JoinConstraint::Where {
                ref captures,
                ref pred,
            } => {
                let Row::Full(full) = tuple else {
                    return None;
                };
                if captures.iter().any(|&c| self.node_of(tuple, c).is_none()) {
                    return None;
                }
                pred(self.function, full)
            }
        }
    }

    /// Every value the phi producing `phi_v` merges on a predecessor whose control input `c` has
    /// `Edge(edge_v)` dominating `Edge(c)`; `None` if `phi_v` is not a phi's.
    fn phi_arms_from_edge(
        &self,
        phi_v: ValueId,
        edge_v: ValueId,
    ) -> Option<impl Iterator<Item = ValueId> + '_> {
        let f = self.function;
        let phi_node = f.producer(phi_v);
        if !matches!(f.node_kind(phi_node), NodeKind::Phi | NodeKind::MemPhi) {
            return None;
        }
        Some(
            f.phi_arms(phi_node)
                .filter(move |&(c, _)| {
                    // Not `producer(c)`: that `If` precedes the edge, breaking `c == edge_v`.
                    self.split_doms()
                        .dominates(CtrlKey::Edge(edge_v), CtrlKey::Edge(c))
                })
                .map(|(_, arm)| arm),
        )
    }
}

/// Keep the first tuple of each signature: each capture's first binding in the tuple, plus the
/// root of each capture-free match. A pattern whose captures all appear earlier in the tuple adds
/// nothing, so tuples differing only in its root collapse.
fn dedup_on_shared_captures(acc: &mut Vec<JoinedMatch>) {
    let mut seen: FxHashSet<BTreeMap<u32, Binding>> = FxHashSet::default();
    acc.retain(|tuple| {
        let mut sig: BTreeMap<u32, Binding> = BTreeMap::new();
        for (slot, m) in tuple.iter().enumerate() {
            if m.bindings.iter().next().is_none() {
                // Capture ids stay below 2^31, so ids counted down from the top cannot collide.
                let slot_id =
                    u32::MAX - u32::try_from(slot).expect("a join has fewer than 2^32 patterns");
                sig.insert(slot_id, Binding::Node(m.root()));
                continue;
            }
            for (cap, binding) in m.bindings.iter() {
                sig.entry(cap.id()).or_insert(binding);
            }
        }
        sig.is_empty() || seen.insert(sig)
    });
}

/// Captures every match of `next` binds and some fixed prefix position binds in every row. An
/// unbound capture agrees with anything, which no bucket expresses.
fn join_key_captures(
    per_pat: &[Vec<Match>],
    acc: &[Vec<usize>],
    next: &[Match],
    j: usize,
) -> Vec<crate::Capture> {
    let bound_by_all = |mut it: Box<dyn Iterator<Item = &Bindings> + '_>| {
        let Some(first) = it.next() else {
            return Vec::new();
        };
        let mut caps: Vec<crate::Capture> = first.iter().map(|(c, _)| c).collect();
        for b in it {
            caps.retain(|c| b.is_bound(*c));
        }
        caps
    };
    let next_caps = bound_by_all(Box::new(next.iter().map(|m| &m.bindings)));
    if next_caps.is_empty() {
        return Vec::new();
    }
    let mut caps: Vec<crate::Capture> = Vec::new();
    for i in 0..j {
        let mut at_i = bound_by_all(Box::new(
            acc.iter().map(move |idx| &per_pat[i][idx[i]].bindings),
        ));
        at_i.retain(|c| next_caps.contains(c));
        caps.append(&mut at_i);
    }
    caps.sort_unstable_by_key(|c| c.id());
    caps.dedup_by_key(|c| c.id());
    caps
}

/// The node each key capture resolves to, or `None` if any is unbound.
fn join_key(b: &Bindings, key_caps: &[crate::Capture], graph: &Graph) -> Option<Vec<u32>> {
    key_caps
        .iter()
        .map(|c| b.get_node(*c, graph).map(|n| n.as_u32()))
        .collect()
}

/// A prefix row's key, each capture read from whichever position binds it (they agree).
fn row_join_key(row: Row<'_>, key_caps: &[crate::Capture], graph: &Graph) -> Option<Vec<u32>> {
    key_caps
        .iter()
        .map(|c| {
            row.iter()
                .find_map(|m| m.bindings.get_node(*c, graph))
                .map(|n| n.as_u32())
        })
        .collect()
}

/// Whether every capture shared between `m` and the row agrees: by value when both bind values,
/// else by resolved node.
fn row_agrees(prefix: Row<'_>, m: &Match, graph: &Graph) -> bool {
    for prev in prefix.iter() {
        for (cap, prev_binding) in prev.bindings.iter() {
            let Some(m_binding) = m.bindings.get_binding(cap) else {
                continue;
            };
            let agree = match (prev_binding, m_binding) {
                (Binding::Value(a), Binding::Value(b)) => a == b,
                _ => prev.bindings.get_node(cap, graph) == m.bindings.get_node(cap, graph),
            };
            if !agree {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod join_key_tests {
    use super::{Binding, Bindings, Match, join_key_captures};
    use crate::Capture;
    use strider_ir::node::NodeId;

    /// One match binding each listed capture to a node, root irrelevant.
    fn hit(bound: &[(Capture, u32)]) -> Match {
        let mut b = Bindings::default();
        for &(c, node) in bound {
            assert!(b.bind_capture(c, Binding::Node(NodeId::from_u32(node))));
        }
        Match::new(NodeId::from_u32(0), b, 0)
    }

    /// A chain join `P0(a) - P1(a, b) - P2(b)`: level 2's key is `b`, which the
    /// prefix binds at position 1 only. Keyed on the positions flattened
    /// together it would be empty and the level a cartesian product.
    #[test]
    fn a_chain_joins_key_comes_from_the_one_prefix_position_that_binds_it() {
        let (a, b) = (Capture::new(), Capture::new());
        let per_pat = vec![
            vec![hit(&[(a, 1)]), hit(&[(a, 2)])],
            vec![hit(&[(a, 1), (b, 10)]), hit(&[(a, 2), (b, 20)])],
            vec![hit(&[(b, 10)]), hit(&[(b, 20)])],
        ];
        let level1 = vec![vec![0], vec![1]];
        assert_eq!(
            join_key_captures(&per_pat, &level1, &per_pat[1], 1),
            vec![a]
        );
        let level2 = vec![vec![0, 0], vec![1, 1]];
        assert_eq!(
            join_key_captures(&per_pat, &level2, &per_pat[2], 2),
            vec![b]
        );
    }

    /// A capture only SOME matches of a position bind is not indexable: the
    /// unbound ones agree with anything, which no bucket expresses.
    #[test]
    fn a_capture_a_position_does_not_always_bind_is_not_a_key() {
        let (a, b) = (Capture::new(), Capture::new());
        let per_pat = vec![
            vec![hit(&[(a, 1), (b, 10)]), hit(&[(a, 2)])],
            vec![hit(&[(b, 10)]), hit(&[(b, 20)])],
        ];
        let acc = vec![vec![0], vec![1]];
        assert!(join_key_captures(&per_pat, &acc, &per_pat[1], 1).is_empty());
    }
}

#[cfg(test)]
mod kleene_tests {
    use super::Kleene;

    const T: Option<bool> = Some(true);
    const F: Option<bool> = Some(false);
    const U: Option<bool> = None;

    /// Every arm folded in, short-circuiting as [`Kleene::arm`] says.
    fn fold(mut acc: Kleene, arms: impl IntoIterator<Item = Option<bool>>) -> Option<bool> {
        for a in arms {
            if let Some(settled) = acc.arm(a) {
                return settled;
            }
        }
        acc.finish()
    }

    #[test]
    fn or_truth_dominates_then_unknown_poisons() {
        assert_eq!(fold(Kleene::new(true), [U, T, F]), T);
        // Unknown poisons a would-be `false`.
        assert_eq!(fold(Kleene::new(true), [F, U, F]), U);
        assert_eq!(fold(Kleene::new(true), [F, F]), F);
        // Identity of `Or`.
        assert_eq!(fold(Kleene::new(true), []), F);
    }

    #[test]
    fn and_falsity_dominates_then_unknown_poisons() {
        assert_eq!(fold(Kleene::new(false), [U, F, T]), F);
        // Unknown poisons a would-be `true`.
        assert_eq!(fold(Kleene::new(false), [T, U, T]), U);
        assert_eq!(fold(Kleene::new(false), [T, T]), T);
        // Identity of `And`.
        assert_eq!(fold(Kleene::new(false), []), T);
    }
}
