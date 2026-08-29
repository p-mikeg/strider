pub(crate) mod builder;
mod cast_walk_through;
pub(crate) mod graph;
pub(crate) mod match_pat;
pub(crate) mod vertex;
pub(crate) mod walk;

pub use builder::{MatcherBuilder, PatNodeRef, PatValueRef};
pub(crate) use cast_walk_through::cast_levels;
pub use graph::Pattern;
pub use strider_ir::walk::CastMask;
pub use vertex::{
    BindingWalkFn, KindSpec, NodePredicate, OutputKindSpec, PatNode, PatValue, PostMatchFn,
    WalkCaptures,
};

/// Sentinel consumer slot marking an existential (`any_input`) input edge: the
/// sub-pattern is matched against some input slot of the consumer rather than a
/// fixed one. A sub reaches the slots of its own output kind (a value sub value
/// edges, a memory sub memory edges); a wildcard reaches any input, control /
/// memory / `PhiToken` slots included.
pub(crate) const ANY_INPUT_SLOT: usize = usize::MAX;

use std::cell::OnceCell;
use std::mem::Discriminant;

use itertools::Either;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::NodeId as PatNodeId;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{
    CtrlKey, Function, Graph, IRViewer, IRWalker, control_dominators, control_edge_dominators,
    dominates,
};

use crate::bindings::{Binding, Bindings};
use crate::graph_ext::PatGraphRead;
use crate::match_result::Match;

/// `None` for a kind-`Any` root.
fn root_kind_discriminant(pat: &Pattern, root: PatNodeId) -> Option<Discriminant<NodeKind>> {
    pat.graph.node_weight(root).kind.discriminant()
}

pub struct Matcher<'f> {
    pub(crate) function: &'f Function,
    kind_index: OnceCell<KindIndex>,
    /// Configurations that have reached a ROOT continuation, i.e. produced a
    /// match. `first_of` cuts on it. Shared across nested walks: a branch
    /// pattern's own walk hands off to the enclosing continuation rather than
    /// to a root, so a per-walk counter would cut a nested `first_of` on the
    /// hand-off and drop the arms a later rejection needs.
    ///
    /// Shared across nested WALKS, not across QUERIES: caller-supplied logic
    /// (`when_match`, a `JoinPredicate`) holds this same `Matcher` and may run
    /// a query of its own, whose matches would otherwise count towards an
    /// enclosing arm's cut and discard the arm holding the real match.
    pub(crate) satisfied: std::cell::Cell<u64>,
}

/// Restores `satisfied` when dropped, so a query nested inside caller-supplied
/// logic cannot disturb the count an enclosing `first_of` is cutting on.
struct CountScope<'a> {
    cell: &'a std::cell::Cell<u64>,
    saved: u64,
}

impl Drop for CountScope<'_> {
    fn drop(&mut self) {
        self.cell.set(self.saved);
    }
}

/// Reachable nodes bucketed by `NodeKind` discriminant.
struct KindIndex {
    by_kind: FxHashMap<Discriminant<NodeKind>, Vec<NodeId>>,
}

impl KindIndex {
    fn build(function: &Function) -> Self {
        let mut by_kind: FxHashMap<Discriminant<NodeKind>, Vec<NodeId>> = FxHashMap::default();
        for node in function.walk() {
            let d = std::mem::discriminant(function.node_kind(node));
            by_kind.entry(d).or_default().push(node);
        }
        Self { by_kind }
    }

    fn nodes_of_kind(&self, d: Discriminant<NodeKind>) -> &[NodeId] {
        self.by_kind.get(&d).map_or(&[], Vec::as_slice)
    }
}

impl<'f> Matcher<'f> {
    /// Pattern validity is checked per query, against the pattern.
    pub fn new(function: &'f Function) -> Self {
        Self {
            function,
            kind_index: OnceCell::new(),
            satisfied: std::cell::Cell::new(0),
        }
    }

    fn kind_index(&self) -> &KindIndex {
        self.kind_index
            .get_or_init(|| KindIndex::build(self.function))
    }

    /// Held for the length of one candidate root's walk; see [`Self::satisfied`].
    fn scoped_count(&self) -> CountScope<'_> {
        CountScope {
            cell: &self.satisfied,
            saved: self.satisfied.get(),
        }
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
        let root = pat.root()?;
        Ok(self
            .candidates(pat, root)
            .flat_map(move |node| self.matches_at_node(node, pat, root, false)))
    }

    /// [`matches`](Self::matches)`.collect()`. Use `matches` directly to stop
    /// early or avoid the `Vec`.
    ///
    /// # Errors
    /// If `pat` is not a single-rooted, acyclic graph (see [`Pattern::root`]).
    pub fn find_all(&self, pat: &Pattern) -> anyhow::Result<Vec<Match>> {
        Ok(self.matches(pat)?.collect())
    }

    /// The IR nodes to attempt `pat` at: a discriminant-rooted pattern scans
    /// only its `KindIndex` bucket, a kind-`Any` root the whole reachable
    /// graph.
    fn candidates<'p>(
        &'p self,
        pat: &Pattern,
        root: PatNodeId,
    ) -> impl Iterator<Item = NodeId> + 'p {
        match root_kind_discriminant(pat, root) {
            Some(d) => Either::Left(self.kind_index().nodes_of_kind(d).iter().copied()),
            None => Either::Right(self.function.walk()),
        }
    }

    /// Attempt `pat` at `node`, once per output whatever its kind (a `Store`
    /// roots through its Memory output, an `If` through either Control one),
    /// falling back to a node-rooted attempt only when the node has no outputs
    /// at all.
    ///
    /// `first_only` stops at the first match. Otherwise every distinct match is
    /// enumerated, deduplicated by capture-to-binding map. The dedup set is
    /// per-node: two different roots are different matches whatever their
    /// bindings, while one node's outputs share the set, so a pattern reachable
    /// through several outputs does not double-report an identical binding.
    fn matches_at_node(
        &self,
        node: NodeId,
        pat: &Pattern,
        root: PatNodeId,
        first_only: bool,
    ) -> Vec<Match> {
        // One scope per candidate root: a query run by caller-supplied logic
        // during this walk restores the count on its way out, so an enclosing
        // `first_of` cuts on its OWN arms and nothing else.
        let _count = self.scoped_count();
        let mut hits: Vec<Match> = Vec::new();
        // Keyed on the bindings ALONE, deliberately: an existential
        // (`any_input`, a commutative pair) reaches the same binding through
        // different slots, and those are one match, not several. The cost is
        // that two `one_of` arms binding identically but covering different
        // nodes also collapse, and the first listed one's `matched_nodes`
        // wins; see `alternation::OneOf`.
        let mut seen: FxHashSet<Vec<(u32, Binding)>> = FxHashSet::default();
        // Records each fully guard-satisfying configuration the walk reaches.
        // Returning `false` (the `find_all` case) rejects it only as a STOPPING
        // POINT (the match is already banked in `hits`), which drives the
        // engine's backtracking on to the next operand ordering / existential
        // slot. `true` accepts and stops.
        {
            let mut collect = |b: &mut Bindings| -> bool {
                if seen.insert(b.binding_signature()) {
                    hits.push(Match::from_root(node, b.clone()));
                }
                first_only
            };

            let outputs = self.function.node_outputs(node);
            if outputs.is_empty() {
                let mut bindings = Bindings::default();
                walk::try_match_node(self, pat, root, node, &mut bindings, &mut collect, true);
            } else {
                for &out_id in outputs {
                    let mut bindings = Bindings::default();
                    // `try_match` returns `true` only when `collect` accepted,
                    // i.e. under `first_only`; otherwise every output is swept.
                    if walk::try_match(self, pat, root, out_id, &mut bindings, &mut collect) {
                        break;
                    }
                }
            }
        }
        hits
    }

    /// The first match of `pat` at `node`, if any.
    ///
    /// # Errors
    /// If `pat` is not a single-rooted, acyclic graph (see [`Pattern::root`]).
    pub fn match_at(&self, node: NodeId, pat: &Pattern) -> anyhow::Result<Option<Match>> {
        let root = pat.root()?;
        // Root-kind gate: reject a mismatched candidate before allocating a
        // `Bindings` per output and walking in. A kind-`Any` root skips it.
        if let Some(rk) = root_kind_discriminant(pat, root)
            && std::mem::discriminant(self.function.node_kind(node)) != rk
        {
            return Ok(None);
        }
        Ok(self
            .matches_at_node(node, pat, root, true)
            .into_iter()
            .next())
    }

    /// Matches `pat` at `node` against the LIVE `bindings`, extending them in
    /// place with what it binds. A capture already bound differently rejects
    /// the match; on rejection `bindings` is unchanged.
    ///
    /// Continuation-passing: `k` runs once per configuration `pat` reaches, so
    /// a `pat` able to bind several ways (commutative operands, existential
    /// slots) offers each in turn. `true` accepts and stops, leaving `bindings`
    /// in the accepted state; `false` drives the next. The return value is
    /// whether `k` accepted.
    ///
    /// A multi-output `node` is anchored on the first output whose match
    /// reaches `k` at all; the later outputs are a fallback for a mismatch, not
    /// a second anchor beside a working one.
    ///
    /// # Errors
    /// If `pat` is not a single-rooted, acyclic graph (see [`Pattern::root`]).
    pub(crate) fn match_at_into(
        &self,
        node: NodeId,
        pat: &Pattern,
        bindings: &mut Bindings,
        k: &mut dyn FnMut(&mut Bindings) -> bool,
    ) -> anyhow::Result<bool> {
        let root = pat.root()?;
        if let Some(rk) = root_kind_discriminant(pat, root)
            && std::mem::discriminant(self.function.node_kind(node)) != rk
        {
            return Ok(false);
        }
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            return Ok(walk::try_match_node(
                self, pat, root, node, bindings, k, false,
            ));
        }
        // Outputs are the mechanism for finding a root VALUE, not an axis of
        // the match: a branch pattern is rooted at the consumer node. The first
        // output the pattern reaches `k` through is that node's answer, so the
        // rest are not tried and a bare wildcard does not report the consumer's
        // control and memory edges as two.
        for &out_id in outputs {
            let mut reached = false;
            let mut counting = |b: &mut Bindings| {
                reached = true;
                k(b)
            };
            if walk::try_match_nested(self, pat, root, out_id, bindings, &mut counting) {
                return Ok(true);
            }
            if reached {
                return Ok(false);
            }
        }
        Ok(false)
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

        // Capture-bearing patterns must form ONE connected component under
        // "shares a capture", else the join is a cartesian product across an
        // unrelated group. Union-find, so the check is order-independent: a
        // pattern sharing a capture only with a LATER one is still connected.
        // Capture-free patterns are exempt.
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]]; // path halving
                x = parent[x];
            }
            x
        }
        let mut parent: Vec<usize> = (0..pats.len()).collect();
        let mut cap_owner: FxHashMap<crate::Capture, usize> = FxHashMap::default();
        // The highest-indexed pattern that can bind each capture: the row
        // length past which no further match can change how it resolves.
        let mut cap_settled: FxHashMap<crate::Capture, usize> = FxHashMap::default();
        let mut capture_bearing: Vec<usize> = Vec::new();
        for (i, p) in pats.iter().enumerate() {
            let mut has_cap = false;
            for c in p.bound_captures() {
                has_cap = true;
                cap_settled.insert(c, i);
                if let Some(&j) = cap_owner.get(&c) {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    parent[ri] = rj;
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
            // when positive, and matches everything under `Not`; reject it
            // here, which also lets the union below be total.
            let caps = con.captures();
            if let Some(c) = caps.iter().find(|c| !cap_owner.contains_key(c)) {
                anyhow::bail!(
                    "find_joined: constraint mentions capture {c:?}, which no pattern \
                     in the join binds, so it could never be satisfied (and under \
                     `negate` would hold vacuously, true because nothing was seen); \
                     bind it with a positive pattern"
                );
            }
            // Union the owners of all of a constraint's captures into one
            // component. Total, since the check above proved every capture has
            // an owner.
            let mut owners = caps.into_iter().map(|c| cap_owner[&c]);
            if let Some(first) = owners.next() {
                for j in owners {
                    let (ri, rj) = (find(&mut parent, first), find(&mut parent, j));
                    parent[ri] = rj;
                }
            }
        }
        if let Some((&first, rest)) = capture_bearing.split_first() {
            let root0 = find(&mut parent, first);
            for &i in rest {
                if find(&mut parent, i) != root0 {
                    anyhow::bail!(
                        "find_joined: pattern {i} shares no capture (even transitively) \
                         with the others; a join correlates on shared captures (use a \
                         capture-free pattern for an intentional cross-product)"
                    );
                }
            }
        }

        // One `find_all` result per pattern: a list of INDEPENDENT matches
        // despite the shape, not a joined row.
        let per_pat: Vec<Vec<Match>> = pats
            .iter()
            .map(|p| self.find_all(p))
            .collect::<anyhow::Result<_>>()?;
        if per_pat.iter().any(|hits| hits.is_empty()) {
            return Ok(Vec::new());
        }

        // A constraint's verdict is settled once every pattern that can bind
        // its captures is in the row, so it runs at that length and prunes the
        // remaining factors. `Where` reads the whole tuple and waits.
        let eval = ConstraintEval::new(self.function());
        let mut by_level: Vec<Vec<&JoinConstraint>> = vec![Vec::new(); pats.len()];
        let mut whole_tuple: Vec<&JoinConstraint> = Vec::new();
        for c in constraints {
            if c.reads_whole_tuple() {
                whole_tuple.push(c);
            } else {
                let level = c
                    .captures()
                    .iter()
                    .filter_map(|cap| cap_settled.get(cap).copied())
                    .max()
                    .unwrap_or(0);
                by_level[level].push(c);
            }
        }

        // Rows are index tuples into `per_pat` while the product builds, so a
        // pruned prefix costs no `Match` clones.
        let passes_all = |cs: &[&JoinConstraint], idx: &[usize]| {
            let row = Row::Indices {
                per_pat: &per_pat,
                idx,
            };
            cs.iter().all(|c| eval.passes(c, row) == Some(true))
        };

        let mut acc: Vec<Vec<usize>> = (0..per_pat[0].len())
            .map(|i| vec![i])
            .filter(|idx| passes_all(&by_level[0], idx))
            .collect();

        for (j, next) in per_pat.iter().enumerate().skip(1) {
            let mut new_acc: Vec<Vec<usize>> = Vec::new();
            let graph = self.function().graph();
            // Captures that EVERY match on both sides binds. Agreement implies
            // the two bindings resolve to the same node (equal values share a
            // producer, and the other arm compares nodes outright), so grouping
            // by those nodes is a necessary condition: only rows in the same
            // bucket can agree. The exact `row_agrees` check still runs inside
            // the bucket, so the result is the cartesian product's, without
            // visiting the pairs that cannot possibly agree.
            let key_caps = join_key_captures(&per_pat, &acc, next, j);
            if key_caps.is_empty() {
                // Nothing shared to index on: every pair is a candidate.
                for prefix in &acc {
                    for (i, m) in next.iter().enumerate() {
                        let row = Row::Indices {
                            per_pat: &per_pat,
                            idx: prefix,
                        };
                        if !row_agrees(row, m, graph) {
                            continue;
                        }
                        let mut cand = prefix.clone();
                        cand.push(i);
                        if passes_all(&by_level[j], &cand) {
                            new_acc.push(cand);
                        }
                    }
                }
            } else {
                let mut buckets: FxHashMap<Vec<u32>, Vec<usize>> = FxHashMap::default();
                for (i, m) in next.iter().enumerate() {
                    let Some(k) = join_key(&m.bindings, &key_caps, graph) else {
                        continue;
                    };
                    buckets.entry(k).or_default().push(i);
                }
                for prefix in &acc {
                    let row = Row::Indices {
                        per_pat: &per_pat,
                        idx: prefix,
                    };
                    let Some(k) = row
                        .iter()
                        .find_map(|m| join_key(&m.bindings, &key_caps, graph))
                    else {
                        continue;
                    };
                    for &i in buckets.get(&k).into_iter().flatten() {
                        if !row_agrees(row, &next[i], graph) {
                            continue;
                        }
                        let mut cand = prefix.clone();
                        cand.push(i);
                        if passes_all(&by_level[j], &cand) {
                            new_acc.push(cand);
                        }
                    }
                }
            }
            acc = new_acc;
            if acc.is_empty() {
                break;
            }
        }

        let mut out: Vec<JoinedMatch> = acc
            .iter()
            .map(|idx| {
                Row::Indices {
                    per_pat: &per_pat,
                    idx,
                }
                .iter()
                .cloned()
                .collect()
            })
            .collect();
        // A row survives iff every constraint returns `Some(true)`. Both
        // `Some(false)` and `None` (a capture unbound in this row, so the
        // relation is unanswerable) drop it.
        if !whole_tuple.is_empty() {
            out.retain(|tuple| {
                whole_tuple
                    .iter()
                    .all(|c| eval.passes(c, Row::Full(tuple)) == Some(true))
            });
        }

        dedup_on_shared_captures(&mut out, self.function().graph());
        Ok(out)
    }
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

impl std::fmt::Debug for JoinConstraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dominates {
                dominator,
                dominated,
            } => f
                .debug_struct("Dominates")
                .field("dominator", dominator)
                .field("dominated", dominated)
                .finish(),
            Self::PhiInputFromEdge { phi, edge, value } => f
                .debug_struct("PhiInputFromEdge")
                .field("phi", phi)
                .field("edge", edge)
                .field("value", value)
                .finish(),
            Self::Not(inner) => f.debug_tuple("Not").field(inner).finish(),
            Self::Or(cs) => f.debug_tuple("Or").field(cs).finish(),
            Self::And(cs) => f.debug_tuple("And").field(cs).finish(),
            Self::Where { captures, .. } => f
                .debug_struct("Where")
                .field("captures", captures)
                .finish_non_exhaustive(),
        }
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

    /// `Some` once the row is complete.
    fn as_tuple(&self) -> Option<&'a JoinedMatch> {
        match *self {
            Row::Full(tuple) => Some(tuple),
            Row::Indices { .. } => None,
        }
    }
}

impl JoinConstraint {
    /// Whether any part of it reads the tuple beyond the captures it names,
    /// which pins it to a complete row.
    fn reads_whole_tuple(&self) -> bool {
        match self {
            JoinConstraint::Where { .. } => true,
            JoinConstraint::Not(inner) => inner.reads_whole_tuple(),
            JoinConstraint::Or(cs) | JoinConstraint::And(cs) => {
                cs.iter().any(JoinConstraint::reads_whole_tuple)
            }
            JoinConstraint::Dominates { .. } | JoinConstraint::PhiInputFromEdge { .. } => false,
        }
    }

    /// Every capture this constraint correlates.
    fn captures(&self) -> Vec<crate::Capture> {
        match self {
            JoinConstraint::Dominates {
                dominator,
                dominated,
            } => vec![*dominator, *dominated],
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => vec![*phi, *edge, *value],
            JoinConstraint::Not(inner) => inner.captures(),
            JoinConstraint::Or(cs) | JoinConstraint::And(cs) => {
                cs.iter().flat_map(JoinConstraint::captures).collect()
            }
            JoinConstraint::Where { captures, .. } => captures.clone(),
        }
    }
}

/// One row of a join: one [`Match`] per pattern, in input order.
pub type JoinedMatch = Vec<Match>;

/// A user join predicate: decides a whole tuple, `None` when unanswerable.
pub type JoinPredicateFn =
    std::sync::Arc<dyn Fn(&Function, &JoinedMatch) -> Option<bool> + Send + Sync>;

/// Kleene OR: `Some(true)` if any input is (truth dominates and
/// short-circuits), else `None` if any is `None` (unknown poisons a would-be
/// `false`), else `Some(false)`. Empty yields `Some(false)`.
fn kleene_or(it: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut saw_unknown = false;
    for v in it {
        match v {
            Some(true) => return Some(true),
            None => saw_unknown = true,
            Some(false) => {}
        }
    }
    (!saw_unknown).then_some(false)
}

/// Kleene AND: `Some(false)` if any input is (falsity dominates and
/// short-circuits), else `None` if any is `None` (unknown poisons a would-be
/// `true`), else `Some(true)`. Empty yields `Some(true)`.
fn kleene_and(it: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut saw_unknown = false;
    for v in it {
        match v {
            Some(false) => return Some(false),
            None => saw_unknown = true,
            Some(true) => {}
        }
    }
    (!saw_unknown).then_some(true)
}

/// Evaluates [`JoinConstraint`]s against joined tuples, memoising both
/// dominator trees for the whole `find_joined_constrained` call.
struct ConstraintEval<'f> {
    function: &'f Function,
    doms: OnceCell<petgraph::algo::dominators::Dominators<NodeId>>,
    /// Edge-split control subgraph.
    split_doms: OnceCell<petgraph::algo::dominators::Dominators<CtrlKey>>,
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
    // kept on measured grounds alone: `Edge` vertices interleave, making a
    // node-dominance chain in the split tree longer to walk, and `Dominates` is
    // re-evaluated per tuple. `benches/matcher.rs`'s `join_dominates_only`
    // measures it.
    fn doms(&self) -> &petgraph::algo::dominators::Dominators<NodeId> {
        self.doms.get_or_init(|| control_dominators(self.function))
    }

    fn split_doms(&self) -> &petgraph::algo::dominators::Dominators<CtrlKey> {
        self.split_doms
            .get_or_init(|| control_edge_dominators(self.function))
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
    fn passes(&self, c: &JoinConstraint, tuple: Row<'_>) -> Option<bool> {
        match *c {
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => {
                let (Some(phi_v), Some(edge_v), Some(val_v)) = (
                    self.value_of(tuple, phi),
                    self.value_of(tuple, edge),
                    self.value_of(tuple, value),
                ) else {
                    return None;
                };
                Some(
                    self.phi_arms_from_edge(phi_v, edge_v)
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
                    (CtrlKey::Node(na), CtrlKey::Node(nb)) => {
                        strider_ir::dominance_verdict(self.doms(), na, nb)
                    }
                    (ka, kb) => strider_ir::dominance_verdict(self.split_doms(), ka, kb),
                }
            }
            // `Not(None) == None`: the negation of an unanswerable constraint
            // is itself unanswerable, so an unbound capture drops the row.
            JoinConstraint::Not(ref inner) => self.passes(inner, tuple).map(|b| !b),
            JoinConstraint::Or(ref cs) => kleene_or(cs.iter().map(|c| self.passes(c, tuple))),
            JoinConstraint::And(ref cs) => kleene_and(cs.iter().map(|c| self.passes(c, tuple))),
            JoinConstraint::Where {
                ref captures,
                ref pred,
            } => {
                let full = tuple.as_tuple()?;
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
    /// reached through branch edge `edge_v`.
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
    ) -> impl Iterator<Item = ValueId> + '_ {
        self.arm_scan(phi_v)
            .into_iter()
            .flat_map(move |(phi_inputs, region_inputs)| {
                region_inputs
                    .into_iter()
                    .enumerate()
                    .filter(move |(_, c)| {
                        // Edge against EDGE. Do NOT rewrite the right operand
                        // as `producer(*c)`: that is the `If`, which PRECEDES
                        // the edge, so the direct `c == edge_v` case breaks.
                        dominates(self.split_doms(), CtrlKey::Edge(edge_v), CtrlKey::Edge(*c))
                    })
                    // Region control input `i` maps to phi data input `i + 1`.
                    .filter_map(move |(i, _)| phi_inputs.get(i + 1).copied())
            })
    }

    /// The phi's inputs and its region's control inputs.
    fn arm_scan(&self, phi_v: ValueId) -> Option<(strider_ir::Inputs<'f>, strider_ir::Inputs<'f>)> {
        let f = self.function;
        let phi_node = f.producer(phi_v);
        if !matches!(f.node_kind(phi_node), NodeKind::Phi | NodeKind::MemPhi) {
            return None;
        }
        let phi_inputs = f.node_inputs(phi_node);
        // Slot 0 is the PhiToken; its producer is the owning Region.
        let region = f.producer(*phi_inputs.get(0)?);
        if !matches!(f.node_kind(region), NodeKind::Region) {
            return None;
        }
        Some((phi_inputs, f.node_inputs(region)))
    }
}

/// One capture's identity within a tuple, at the granularity [`row_agrees`]
/// compares: a value binding is its own value, so two rows binding one capture
/// to distinct outputs of one node are distinct rows rather than duplicates.
#[derive(PartialEq, Eq, Hash)]
enum CaptureKey {
    Value(ValueId),
    Node(NodeId),
}

/// Deduplicate joined tuples by capture binding signature: every capture bound
/// anywhere in the tuple, sorted. Of equal signatures only the first is kept,
/// order-preserving. An empty signature is always kept.
///
/// A pattern that binds NOTHING is identified by its root instead. Its matches
/// are otherwise indistinguishable, so every tuple differing only in which one
/// it holds would collapse to an arbitrary survivor, and adding a capture to
/// one side of a join would then SHRINK the reported cross product.
///
/// KNOWN LIMIT: a pattern whose captures are ALL owned by an earlier slot adds
/// no entry of its own, so tuples differing only in ITS root still collapse.
/// That follows the shared-capture contract above rather than the carve-out;
/// making it follow the carve-out instead would drop the contract, so the two
/// cannot both hold and the contract wins.
///
/// `row_agrees` already made each tuple internally consistent, so a capture
/// bound as a value anywhere in the tuple has the same value everywhere and the
/// first one seen identifies it.
fn dedup_on_shared_captures(acc: &mut Vec<JoinedMatch>, graph: &Graph) {
    let mut seen: FxHashSet<Vec<(u32, CaptureKey)>> = FxHashSet::default();
    acc.retain(|tuple| {
        let mut sig: Vec<(u32, CaptureKey)> = Vec::new();
        let mut sig_ids: FxHashSet<u32> = FxHashSet::default();
        for (slot, m) in tuple.iter().enumerate() {
            if m.bindings.iter().next().is_none() {
                // Keyed by tuple POSITION, so two capture-free patterns get
                // distinct ids and the sort below stays deterministic. Capture
                // ids count up from 0, so counting down from `u32::MAX` cannot
                // collide with one for any tuple that fits in memory.
                let slot_id = u32::MAX - u32::try_from(slot).unwrap_or(0);
                sig.push((slot_id, CaptureKey::Node(m.root())));
                continue;
            }
            for (cap, binding) in m.bindings.iter() {
                let key = match binding {
                    Binding::Value(v) => CaptureKey::Value(v),
                    Binding::Node(_) => match m.bindings.get_node(cap, graph) {
                        Some(node) => CaptureKey::Node(node),
                        None => continue,
                    },
                };
                if sig_ids.insert(cap.id()) {
                    sig.push((cap.id(), key));
                }
            }
        }
        // Nothing to key on; never dedup.
        if sig.is_empty() {
            return true;
        }
        sig.sort_unstable_by_key(|(id, _)| *id);
        seen.insert(sig)
    });
}

/// Captures that every accumulated prefix row AND every match of `next` binds.
///
/// Restricted to captures bound on BOTH sides so the key is always computable:
/// `row_agrees` treats a capture one side leaves unbound as agreeing with
/// anything, which no single hash bucket can express. Any shared capture that
/// fails this test is not indexed on; the exact check still catches it.
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
    let mut caps = bound_by_all(Box::new(
        acc.iter()
            .flat_map(|idx| (0..j).map(move |i| &per_pat[i][idx[i]].bindings)),
    ));
    caps.retain(|c| next_caps.contains(c));
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

#[cfg(test)]
mod kleene_tests {
    use super::{kleene_and, kleene_or};

    const T: Option<bool> = Some(true);
    const F: Option<bool> = Some(false);
    const U: Option<bool> = None;

    #[test]
    fn not_of_unknown_is_unknown() {
        // The load-bearing case: an unanswerable `inner` stays unanswerable
        // (dropping the row) rather than flipping to a vacuous `true`.
        assert_eq!(U.map(|b: bool| !b), U);
        assert_eq!(T.map(|b| !b), F);
        assert_eq!(F.map(|b| !b), T);
    }

    #[test]
    fn kleene_or_truth_dominates_then_unknown_poisons() {
        assert_eq!(kleene_or([U, T, F].into_iter()), T);
        // Unknown poisons a would-be `false`.
        assert_eq!(kleene_or([F, U, F].into_iter()), U);
        assert_eq!(kleene_or([F, F].into_iter()), F);
        // Identity of `Or`.
        assert_eq!(kleene_or(std::iter::empty()), F);
    }

    #[test]
    fn kleene_and_falsity_dominates_then_unknown_poisons() {
        assert_eq!(kleene_and([U, F, T].into_iter()), F);
        // Unknown poisons a would-be `true`.
        assert_eq!(kleene_and([T, U, T].into_iter()), U);
        assert_eq!(kleene_and([T, T].into_iter()), T);
        // Identity of `And`.
        assert_eq!(kleene_and(std::iter::empty()), T);
    }
}
