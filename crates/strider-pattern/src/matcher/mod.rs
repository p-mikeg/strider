pub(crate) mod builder;
pub(crate) mod graph;
pub use graph::MAX_PATTERN_NODES;
pub(crate) mod match_pat;
pub(crate) mod vertex;
pub(crate) mod walk;

pub use builder::{MatcherBuilder, PatNodeRef, PatValueRef};
pub use graph::Pattern;
pub use strider_ir::walk::CastMask;
/// The vertex payloads behind a [`Pattern`]'s graph. Not the seam: a pattern is
/// described through the builder, never by handing it a vertex.
pub(crate) use vertex::{InputSlot, OutputKindSpec, PatNode, PatValue, Role, SlotMode};
/// The builder seam: the verbs of [`MatcherBuilder`] and the types its
/// signatures name. A downstream `MatchPat` is lowered through them, so they
/// stay public even where only this crate calls them today.
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

/// The kinds an alternation root accepts: its arms', deduplicated, since the
/// arms are tried against the SAME IR node. `None` for a non-alternation root
/// and for one with a kind-`Any` arm, which admits every node.
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
    /// Pattern validity is checked per query, against the pattern.
    pub fn new(function: &'f Function) -> Self {
        Self { function }
    }

    pub fn function(&self) -> &Function {
        self.function
    }

    /// Every match for `pat`, lazily per candidate, so `.next()` walks no
    /// further than the first matching node.
    ///
    /// # Several matches per root
    ///
    /// A root can match more than one way, most often a commutative node whose
    /// two operands each satisfy a captured sub-pattern. Every distinct way is
    /// yielded, deduplicated by the capture-to-binding map (see
    /// `Bindings::binding_signature`): `int_add(var(x), var(x))` swapped binds
    /// `x` identically and is ONE match, while
    /// `int_add(anything().capture(k), anything())` binds
    /// `k` to each operand in turn and is TWO. A pattern with no captures on
    /// commutative operands never duplicates. Ordering is deterministic:
    /// natural operand order before swapped.
    ///
    /// # Errors
    /// If `pat` is not a single-rooted, acyclic graph (see [`Pattern::root`]).
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
        // Binding no capture, every configuration shares one signature, so the
        // first to reach the root is the whole answer for that root.
        let first_only = pat.bound_captures().next().is_none();
        let mut scratch = Bindings::default();
        Ok(self
            .candidates(pat, root, walk)
            .filter(move |&n| walk::may_match_at(self, pat, root, n))
            .flat_map(move |node| self.matches_at_node(node, pat, root, first_only, &mut scratch)))
    }

    /// [`matches`](Self::matches)`.collect()`. Use `matches` directly to stop
    /// early or avoid the `Vec`.
    ///
    /// # Errors
    /// If `pat` is not a single-rooted, acyclic graph (see [`Pattern::root`]).
    pub fn find_all(&self, pat: &Pattern) -> anyhow::Result<Vec<Match>> {
        Ok(self.matches(pat)?.collect())
    }

    /// The IR nodes to attempt `pat` at, in walk order: a discriminant-rooted
    /// pattern's kind, an alternation root's arm kinds one after another,
    /// every reachable node for a kind-`Any` root.
    fn candidates<'p>(
        &'p self,
        pat: &Pattern,
        root: PatNodeId,
        walk: impl Iterator<Item = NodeId> + 'p,
    ) -> impl Iterator<Item = NodeId> + 'p {
        let f = self.function;
        let is = move |n: NodeId, d| std::mem::discriminant(f.node_kind(n)) == d;
        // Deduplicated, so no node is offered twice and no match is reported
        // twice.
        let kinds = root_kind_discriminant(pat, root)
            .map(|d| vec![d])
            .or_else(|| alternation_kind_discriminants(pat, root));
        match kinds.as_deref() {
            Some(&[d]) => Either::Left(Either::Left(walk.filter(move |&n| is(n, d)))),
            // One walk, filtered once per kind.
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

    /// Every match of `pat` rooted at `node`; see [`walk::anchor`].
    ///
    /// `first_only` stops at the first match, moving `scratch` into it.
    /// Otherwise every distinct match is enumerated, deduplicated by
    /// capture-to-binding map, and `scratch` comes back empty, the walk
    /// restoring it on every rejection. The dedup set is
    /// per-node: two different roots are different matches whatever their
    /// bindings, while one node's outputs share the set, so a pattern reachable
    /// through several outputs does not double-report an identical binding.
    fn matches_at_node(
        &self,
        node: NodeId,
        pat: &Pattern,
        root: PatNodeId,
        first_only: bool,
        scratch: &mut Bindings,
    ) -> Vec<Match> {
        let mut hits: Vec<Match> = Vec::new();
        // Keyed on the bindings ALONE, deliberately: an existential
        // (`any_input`, a commutative pair) reaches the same binding through
        // different slots, and those are one match, not several. The cost is
        // that two `one_of` arms binding identically but covering different
        // nodes also collapse, and the first listed one's `matched_nodes`
        // wins; see `alternation::OneOf`. Filled from the second
        // configuration on, since most roots have one.
        let mut seen: FxHashSet<Vec<(u32, Binding)>> = FxHashSet::default();
        // Returning `false` (the `find_all` case) rejects a configuration only
        // as a STOPPING POINT (the match is already banked in `hits`), which
        // drives the engine on to the next operand ordering / existential
        // slot. `true` accepts and stops.
        let generation = self.function.graph().generation();
        walk::anchor(self, pat, root, node, scratch, &mut |b| {
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

    /// The first match of `pat` at `node`, if any.
    ///
    /// `node` is taken as given, so a node unreachable from the entry answers
    /// here while [`find_all`](Self::find_all), which seeds from
    /// [`IRWalker::walk`], never offers it.
    ///
    /// # Errors
    /// If `pat` is not a single-rooted, acyclic graph (see [`Pattern::root`]).
    pub fn match_at(&self, node: NodeId, pat: &Pattern) -> anyhow::Result<Option<Match>> {
        let root = pat.root()?;
        // Reject a candidate its root kind or an operand rules out before
        // allocating a `Bindings` per output and walking in.
        if !walk::may_match_at(self, pat, root, node) {
            return Ok(None);
        }
        Ok(self
            .matches_at_node(node, pat, root, true, &mut Bindings::default())
            .into_iter()
            .next())
    }

    /// Run several patterns and keep only the joined tuples where every
    /// [`crate::Capture`] appearing in more than one pattern binds the same
    /// node (and value output, where applicable) in all of them.
    ///
    /// # Returns
    ///
    /// One entry per joined-match tuple; within a tuple, one [`Match`] per
    /// input pattern in input order.
    ///
    /// # Complexity
    ///
    /// O(N1 * N2 * ... * NM) worst case, Ni being pattern i's match count.
    /// Constraints run as soon as every pattern that can bind their captures is
    /// in the row, so a selective one prunes the remaining factors.
    ///
    /// # Shared-capture requirement
    ///
    /// A capture-bearing pattern sharing no capture with the others is
    /// rejected. A capture-free pattern (a pure filter like
    /// `call().at(0x1234).build()`) is exempt and degrades to a deliberate
    /// cross-product.
    ///
    /// # Deduplication
    ///
    /// Surviving tuples are deduplicated by the bindings of EVERY capture the
    /// tuple binds, at value granularity: two tuples differing only in an
    /// uncaptured internal binding collapse to one, but differing in a capture
    /// no other pattern shares does not.
    ///
    /// # Constraints
    ///
    /// `constraints` are post-correlation [`JoinConstraint`] filters over
    /// captured entities; a tuple survives iff it passes every one. Every
    /// capture a constraint mentions must be bound by some pattern in the join
    /// (range restriction); an unbound one is an error. A constraint the graph
    /// cannot answer drops its tuple, which is not an error, and negating it
    /// drops the tuple too rather than admitting it. Pass `&[]` for an
    /// unconstrained join.
    ///
    /// A [`JoinConstraint::Where`] reads the whole tuple, so it runs last; the
    /// built-ins read only the captures they name and run at the earliest row
    /// length that settles them.
    ///
    /// # Errors
    /// If any pattern is not a single-rooted, acyclic graph (see
    /// [`Pattern::root`]), if a capture-bearing pattern is connected to the
    /// rest by neither a shared capture nor a constraint, or if a constraint
    /// mentions a capture no pattern binds.
    pub fn find_joined_constrained(
        &self,
        pats: &[&Pattern],
        constraints: &[JoinConstraint],
    ) -> anyhow::Result<Vec<JoinedMatch>> {
        if pats.is_empty() {
            return Ok(Vec::new());
        }
        let cap_settled = correlate(pats, constraints)?;

        // One `find_all` result per pattern: a list of INDEPENDENT matches
        // despite the shape, not a joined row.
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
        // `Where` reads the whole tuple, so it waits for a complete row.
        let whole_tuple: Vec<_> = constraints
            .iter()
            .filter(|c| c.reads_whole_tuple())
            .collect();
        // Rows are index tuples into `per_pat` while the product builds, so a
        // pruned prefix costs no `Match` clones.
        let passes_all = |cs: &[&JoinConstraint], idx: &[usize]| {
            let row = Row::Indices {
                per_pat: &per_pat,
                idx,
            };
            cs.iter().all(|c| eval.passes(c, row) == Some(true))
        };

        // One empty prefix, which every match of the first pattern extends.
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
        // A row survives iff every constraint returns `Some(true)`. Both
        // `Some(false)` and `None` (a capture unbound in this row, so the
        // relation is unanswerable) drop it.
        out.retain(|tuple| {
            whole_tuple
                .iter()
                .all(|c| eval.passes(c, Row::Full(tuple)) == Some(true))
        });

        dedup_on_shared_captures(&mut out);
        Ok(out)
    }
}

/// The join's correlation checks, returning for each capture the
/// highest-indexed pattern that can bind it: the row length past which no
/// further match can change how it resolves.
///
/// Capture-bearing patterns must form ONE connected component under "shares a
/// capture", else the join is a cartesian product across an unrelated group.
/// Union-find, so the check is order-independent: a pattern sharing a capture
/// only with a LATER one is still connected. Capture-free patterns are exempt.
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
    // Constraints correlate too: patterns linked by a constraint whose
    // captures live one in each are connected without sharing a capture
    // (`guard` captures `t`, `call` captures `c`, joined by
    // `dominates(t, c)`).
    for con in constraints {
        // Range restriction. An unbound capture silently drops every tuple
        // when positive, and matches everything under `Not`; reject it here,
        // which also lets the union below be total.
        let caps = con.captures();
        if let Some(c) = caps.iter().find(|c| !cap_owner.contains_key(c)) {
            anyhow::bail!(
                "find_joined_constrained: constraint mentions capture {c:?}, which no pattern \
                 in the join binds, so it could never be satisfied (and under \
                 `negate` would hold vacuously, true because nothing was seen); \
                 bind it with a positive pattern"
            );
        }
        // Union the owners of all of a constraint's captures into one
        // component. Total, since the check above proved every capture has an
        // owner.
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

/// Each constraint not reading the whole tuple, at the row length that
/// settles it: once every pattern that can bind its captures is in the row, so
/// it prunes the remaining factors.
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

/// Every row of `acc` extended by each match of pattern `j` it agrees with and
/// `keep` accepts.
///
/// Matches are bucketed by the nodes their key captures (see
/// [`join_key_captures`]) resolve to. Agreement implies the two bindings
/// resolve to the same node (equal values share a producer, and the other arm
/// compares nodes outright), so only rows in the same bucket can agree. The
/// exact [`row_agrees`] check still runs inside the bucket, so the result is
/// the cartesian product's, without visiting the pairs that cannot possibly
/// agree. With no key capture every match shares the one empty-key bucket.
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

/// A CFG relation between captured entities, applied by
/// [`Matcher::find_joined_constrained`] as a post-correlation filter.
///
/// Each captured entity resolves to a [`CtrlKey`] by WHAT IT BOUND: a
/// control-output capture (e.g. via
/// [`IfPat::capture_true`](crate::IfPat::capture_true)) becomes a
/// `CtrlKey::Edge`, anything else a `CtrlKey::Node` (a value's producer).
///
/// Every variant is a pure filter: it decides, it never binds.
#[derive(Clone)]
pub enum JoinConstraint {
    /// `dominator` dominates `dominated` in the control subgraph. A capture
    /// with no control-flow position fails it.
    ///
    /// One constraint subsuming three relations:
    ///   * NODE dominates NODE: the plain control-dominance query.
    ///   * EDGE dominates NODE: the node sits *exclusively* in the block that
    ///     edge leads into, so `dominates(true_edge, c)` says "`c` is in the
    ///     true block". This is EDGE dominance on the edge-split graph, NOT
    ///     dominance by the edge's target: with an empty arm
    ///     (`if (c) {} else { X }`) the edge runs straight into the join, which
    ///     dominates everything past the merge, so target-dominance would claim
    ///     post-merge nodes sit inside the branch.
    ///   * EDGE dominates EDGE: every path through the inner edge first
    ///     traversed the outer one.
    ///
    /// Answerable only for captures that carry a control edge. A `Load`,
    /// `Store` or arithmetic node has no vertex in the dominator tree, so both
    /// the constraint and its negation drop the tuple.
    Dominates {
        dominator: crate::Capture,
        dominated: crate::Capture,
    },
    /// The `Phi`/`MemPhi` bound to `phi` merges `value` on the predecessor
    /// reached through branch edge `edge`. Lets a pattern say "the value merged
    /// from THIS branch is X" without a slot index. Works for a value `Phi` and
    /// for a `MemPhi` (where `value` binds the merged memory token).
    ///
    /// A predecessor qualifies when `edge` dominates its control input as an
    /// EDGE, i.e. every path traversing that predecessor first traversed
    /// `edge`. That covers the direct case (`edge` IS the region's control
    /// input) and an arm merged across intervening control such as a `Call` or
    /// a whole guarded loop.
    ///
    /// `edge` must bind a control-output value and `phi` a value.
    PhiInputFromEdge {
        phi: crate::Capture,
        edge: crate::Capture,
        value: crate::Capture,
    },
    /// A tuple survives iff `inner` does NOT hold on it. A capture left unbound
    /// by the row makes `inner` unanswerable, which drops the row rather than
    /// vacuously passing it.
    Not(Box<JoinConstraint>),
    /// Passes iff any listed constraint does. An empty list passes nothing.
    Or(Vec<JoinConstraint>),
    /// Passes iff every listed constraint does. An empty list passes
    /// everything.
    And(Vec<JoinConstraint>),
    /// A user-supplied predicate over the whole joined tuple. `captures` are
    /// the captures it correlates: fed to the join's connectivity and range
    /// checks like a built-in constraint's, and, before `pred` runs, resolved
    /// against the row so that any left unbound drops it as unanswerable (`pred`
    /// never sees a partial tuple). Otherwise `pred` decides, and its `None`
    /// likewise drops the row and stays sound under `Not`.
    Where {
        captures: Vec<crate::Capture>,
        pred: JoinPredicateFn,
    },
}

/// The tree is caller-shaped, so the derived glue would recurse one frame per
/// level and abort on the free, after every walk over it has already returned.
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
    /// The tree's depth is the caller's, so the walk carries its own stack
    /// rather than the machine's.
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

/// A joined row: an index tuple into the per-pattern match lists while the
/// product is building, a materialised tuple once it is complete.
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

    /// Whether any part of it reads the tuple beyond the captures it names,
    /// which pins it to a complete row.
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

/// Kleene fold over one connective's arms, one arm at a time. `dominant` is
/// the verdict that settles it on sight and short-circuits the rest: `true`
/// for OR, `false` for AND. An unknown arm poisons what would otherwise be
/// `!dominant`; no arm at all yields `!dominant`, the connective's identity.
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

    /// `Some` once the arms folded in settle the connective, whatever the
    /// remaining ones say.
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

/// Evaluates [`JoinConstraint`]s against joined tuples, memoising both
/// dominator trees for the whole `find_joined_constrained` call.
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

    // `split_doms` subsumes this tree, so it is redundant for correctness and
    // kept on measured grounds alone: a node-only join skips building the split
    // tree, which has a vertex per control edge as well. `benches/matcher.rs`'s
    // `join_dominates_only` measures it.
    fn doms(&self) -> &DominatorTree<NodeId> {
        self.doms
            .get_or_init(|| control_dominator_tree(self.function))
    }

    fn split_doms(&self) -> &DominatorTree<CtrlKey> {
        self.split_doms
            .get_or_init(|| control_edge_dominator_tree(self.function))
    }

    /// Resolve a capture to a [`CtrlKey`] by what it bound:
    ///   1. a bound value of kind `Control` (an `If`'s
    ///      `capture_true`/`capture_false` edge) gives [`CtrlKey::Edge`];
    ///   2. any other bound value, [`CtrlKey::Node`] of its producer;
    ///   3. else a bound node, [`CtrlKey::Node`];
    ///   4. else `None`, feeding the three-valued [`Self::passes`].
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

    /// Three-valued verdict: `Some(b)` is real, `None` means a referenced
    /// capture was unbound in this row so the relation is unanswerable.
    ///
    /// The connectives are unwound on an explicit stack: the tree's depth is
    /// the caller's, and recursing it deep enough overflows.
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
            // Down to the next leaf, stacking the connectives above it.
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
            // Back up through the connectives the leaf settled.
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
                // Three-valued like `Dominates`: a `phi` capture bound to a
                // non-phi, or an `edge` one carrying no control edge, leaves
                // the relation unanswerable. Answering `false` would hand
                // `Not` the rows it was asked to exclude.
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
                // Node to node stays on `doms` for the fast path; any edge
                // operand routes to the subsuming `split_doms`.  Three-valued:
                // a capture bound to a kind with no control edge has no vertex
                // in either tree, and saying "does not dominate" there would
                // hand `Not` the rows it was asked to exclude.
                match (key_a, key_b) {
                    (CtrlKey::Node(na), CtrlKey::Node(nb)) => self.doms().dominance_verdict(na, nb),
                    (ka, kb) => self.split_doms().dominance_verdict(ka, kb),
                }
            }
            // `passes` unwinds these, so a leaf walk never sees one.
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
                // Unbound declared capture => unanswerable: drop the row before
                // `pred` runs (sound under `Not`; matches the built-ins).
                if captures.iter().any(|&c| self.node_of(tuple, c).is_none()) {
                    return None;
                }
                pred(self.function, full)
            }
        }
    }

    /// Every value the `Phi`/`MemPhi` producing `phi_v` merges on a predecessor
    /// reached through branch edge `edge_v`. `None` when `phi_v` does not come
    /// from a phi at all, which no arm set can express.
    ///
    /// Slot alignment: a phi's inputs are `[PhiToken, v0, v1, ...]`, so data
    /// input `i+1` is predecessor `i`'s value, while its owning `Region` (the
    /// `PhiToken`'s producer) has control input `i` for predecessor `i`. A
    /// qualifying region slot maps to the phi input one slot over.
    ///
    /// # Which arms come from an edge
    ///
    /// Predecessor `i` with control input `c_i` comes from `edge_v` when
    ///
    /// ```text
    /// dominates(split_doms, Edge(edge_v), Edge(c_i))
    /// ```
    ///
    /// One clause, edge against edge: both are control edges, so in the
    /// edge-split graph this is plain dominance. The direct case
    /// (`c_i == edge_v`) is subsumed, since `dominates(x, x)` is true.
    ///
    /// The relation is exclusive, holding only where every path traverses
    /// `edge_v`, so an arm reachable from both sides of the branch belongs to
    /// neither. A branch whose block splits and reaches the join more than once
    /// yields one arm per qualifying predecessor.
    fn phi_arms_from_edge(
        &self,
        phi_v: ValueId,
        edge_v: ValueId,
    ) -> Option<impl Iterator<Item = ValueId> + '_> {
        let (phi_inputs, region_inputs) = self.arm_scan(phi_v)?;
        Some(
            region_inputs
                .into_iter()
                .enumerate()
                .filter(move |(_, c)| {
                    // Edge against EDGE. Do NOT rewrite the right operand
                    // as `producer(*c)`: that is the `If`, which PRECEDES
                    // the edge, so the direct `c == edge_v` case breaks.
                    self.split_doms()
                        .dominates(CtrlKey::Edge(edge_v), CtrlKey::Edge(*c))
                })
                // Region control input `i` maps to phi data input `i + 1`.
                .filter_map(move |(i, _)| phi_inputs.get(i + 1).copied()),
        )
    }

    /// The phi's inputs and its region's control inputs.
    fn arm_scan(&self, phi_v: ValueId) -> Option<(strider_ir::Inputs<'f>, strider_ir::Inputs<'f>)> {
        let f = self.function;
        let phi_node = f.producer(phi_v);
        if !matches!(f.node_kind(phi_node), NodeKind::Phi | NodeKind::MemPhi) {
            return None;
        }
        let phi_inputs = f.node_inputs(phi_node);
        // The PhiToken's producer is the owning Region.
        let region = f.producer(*phi_inputs.get(NodeKind::PHI_TOKEN_SLOT)?);
        if !matches!(f.node_kind(region), NodeKind::Region) {
            return None;
        }
        Some((phi_inputs, f.node_inputs(region)))
    }
}

/// Deduplicate joined tuples by capture binding signature: every capture bound
/// anywhere in the tuple, sorted. Keyed on the `Binding` itself, the
/// granularity [`row_agrees`] compares: two rows whose first binder of a
/// capture binds distinct outputs of one node are distinct rows. Of equal
/// signatures only the first is kept, order-preserving. An empty signature is
/// always kept.
///
/// A pattern that binds NOTHING is identified by its root instead. Its matches
/// are otherwise indistinguishable, so every tuple differing only in which one
/// it holds would collapse to an arbitrary survivor, and adding a capture to
/// one side of a join would then SHRINK the reported cross product.
///
/// KNOWN LIMIT: a pattern whose captures are ALL owned by an earlier slot adds
/// no entry of its own, so tuples differing only in ITS root still collapse.
/// That follows the shared-capture contract above rather than the carve-out.
///
/// `row_agrees` already made each tuple internally consistent, so a capture
/// bound as a value anywhere in the tuple has the same value everywhere and the
/// first one seen identifies it. Where that first binder binds the node, rows
/// differing only in which of its outputs a later one bound collapse.
fn dedup_on_shared_captures(acc: &mut Vec<JoinedMatch>) {
    let mut seen: FxHashSet<BTreeMap<u32, Binding>> = FxHashSet::default();
    acc.retain(|tuple| {
        // Keyed and so sorted by id, the first binder of each kept.
        let mut sig: BTreeMap<u32, Binding> = BTreeMap::new();
        for (slot, m) in tuple.iter().enumerate() {
            if m.bindings.iter().next().is_none() {
                // Keyed by tuple POSITION, so two capture-free patterns get
                // distinct ids and the sort below stays deterministic. Capture
                // ids count up from 0, so counting down from `u32::MAX` cannot
                // collide with one for any tuple that fits in memory.
                let slot_id = u32::MAX - u32::try_from(slot).unwrap_or(0);
                sig.insert(slot_id, Binding::Node(m.root()));
                continue;
            }
            for (cap, binding) in m.bindings.iter() {
                sig.entry(cap.id()).or_insert(binding);
            }
        }
        // Nothing to key on; never dedup.
        sig.is_empty() || seen.insert(sig)
    });
}

/// Captures every match of `next` binds, and that some FIXED prefix position
/// binds in every accumulated row.
///
/// Both sides must always bind a key capture, or the key is not computable:
/// `row_agrees` treats a capture one side leaves unbound as agreeing with
/// anything, which no single hash bucket can express. The prefix test is per
/// position, not over the positions flattened together, which is what a chain
/// join needs: in `P0(a) - P1(a, b) - P2(b)` no capture is bound at every
/// position, yet `b` is bound at position 1 of every row.
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

/// A prefix row's key, each capture read from whichever position binds it.
///
/// Any binder is a valid representative: earlier levels' `row_agrees` already
/// forced every position of the row that binds a capture onto the same node.
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

/// Whether every capture shared between `m` and the row so far agrees.
///
/// Agreement is at the resolved-NODE level, so one IR node captured as
/// `Value(v)` by one pattern and `Node(producer(v))` by another still agrees.
/// Two value captures are compared at value granularity, so distinct outputs of
/// one multi-output node do not falsely agree.
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

// `Binding` and `bind_capture` are `pub(crate)`, out of reach of an
// integration test.
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
    fn not_of_unknown_is_unknown() {
        // The load-bearing case: an unanswerable `inner` stays unanswerable
        // (dropping the row) rather than flipping to a vacuous `true`.
        assert_eq!(U.map(|b: bool| !b), U);
        assert_eq!(T.map(|b| !b), F);
        assert_eq!(F.map(|b| !b), T);
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
