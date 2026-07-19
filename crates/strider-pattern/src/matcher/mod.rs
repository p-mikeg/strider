//! Pattern matcher over a lifted [`strider_ir::Function`].
//!
//! [`Matcher`] owns no per-match state; [`new`](Matcher::new)
//! caches a lazy `KindIndex` (built on first query) bucketing
//! reachable IR nodes by `NodeKind` discriminant. A discriminant-rooted
//! pattern iterates just the matching bucket; a kind-`Any` root falls
//! back to a full reachable walk.
//!
//! The recursive match engine lives in `walk`; the cast walk-through
//! helper in `cast_walk_through`. The cast mask is carried on the
//! [`Pattern`] itself, not on the matcher.

pub(crate) mod builder;
mod cast_walk_through;
pub(crate) mod graph;
pub(crate) mod match_pat;
pub(crate) mod vertex;
pub(crate) mod walk;

pub use builder::{MatcherBuilder, PatNodeRef, PatValueRef};
pub(crate) use cast_walk_through::skip_casts;
pub use graph::Pattern;
pub use strider_ir::walk::CastMask;
pub use vertex::{KindSpec, NodePredicate, OutputKindSpec, PatNode, PatValue, PostMatchFn};

/// Sentinel consumer slot marking an **existential** (`any_input`) input edge:
/// its sub-pattern is not wired to a fixed IR input slot but matched against
/// some input slot of the consumer node (ANY input — the sub-pattern discriminates;
/// e.g. `phi().any_input(p)` matches a `Phi` one of whose inputs matches `p`,
/// without knowing which predecessor). A typed sub only matches value edges;
/// a wildcard can reach control/memory/`PhiToken` slots. Recognised by
/// [`walk::try_match_at`], which routes these edges through the existential
/// search instead of the fixed-slot `match_inputs`.
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

/// Discriminant of the pat node at `root`, used by the `find_*` dispatch
/// to pre-filter IR nodes by kind. Returns `None` for a kind-`Any` root
/// (then the matcher scans every reachable node). `root` is the
/// already-resolved match root (see [`Pattern::root`]).
fn root_kind_discriminant(pat: &Pattern, root: PatNodeId) -> Option<Discriminant<NodeKind>> {
    pat.graph.node_weight(root).kind.discriminant()
}

/// Top-level matcher. Owns no per-match state.
///
/// Caches a lazy `KindIndex` (built on first [`matches`](Self::matches) query)
/// that buckets reachable IR nodes by `NodeKind` discriminant. Subsequent
/// queries with a discriminant-rooted pattern iterate just the matching bucket
/// instead of walking every reachable node.
pub struct Matcher<'f> {
    pub(crate) function: &'f Function,
    kind_index: OnceCell<KindIndex>,
}

/// Lazy per-`Function` index mapping each reachable `NodeKind`
/// discriminant to its node list. Built on first query through
/// [`Matcher::kind_index`]; subsequent queries reuse the cache.
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
    /// Return a matcher bound to the function.
    ///
    /// Performs no whole-graph validation — that's left to callers (the
    /// orchestrator pipeline drives `validate::validate` separately and
    /// integration tests for in-place editors deliberately work with
    /// partially-built fixtures).
    pub fn new(function: &'f Function) -> Self {
        Self {
            function,
            kind_index: OnceCell::new(),
        }
    }

    /// Lazily build (or return the cached) `KindIndex` for the wrapped
    /// function. Single-threaded (`OnceCell`, not `OnceLock`).
    fn kind_index(&self) -> &KindIndex {
        self.kind_index
            .get_or_init(|| KindIndex::build(self.function))
    }

    /// Borrow of the [`Function`] this matcher operates over. The sole
    /// data-access point for closures (`when_match`, `predicate`,
    /// `PostMatchFn`) that need to inspect IR side-tables at match time.
    pub fn function(&self) -> &Function {
        self.function
    }

    /// Every match for `pat` in the function, lazily.
    ///
    /// The single-pattern query primitive: `.collect()` for all matches,
    /// `.next()` to stop at the first (the iterator is lazy per CANDIDATE, so
    /// `next` walks no further than the first matching node).
    /// [`find_joined_constrained`] is built on this too, one pass per pattern.
    ///
    /// # Several matches per root
    ///
    /// A root can match in more than one way — most often a commutative node
    /// whose two operands each satisfy a captured sub-pattern. Every DISTINCT
    /// way is yielded, deduplicated by the capture-to-binding map (see
    /// `Bindings::binding_signature`): `add(var(x), var(x))` matched swapped
    /// binds `x` identically and is ONE match, while `add(any().capture(k),
    /// any())` binds `k` to each operand in turn and is TWO. A pattern with no
    /// captures on commutative operands therefore never duplicates. Ordering is
    /// deterministic: natural operand order before swapped.
    ///
    /// The match root is resolved once up front via [`Pattern::root`]; a
    /// discriminant-rooted pattern then tries only the matching `KindIndex`
    /// bucket (O(M) in nodes of that kind) while a kind-`Any` root falls
    /// back to a full reachable walk. The resolved root is threaded into
    /// the walk, so it is computed once per query, not once per candidate.
    ///
    /// # Errors
    /// Errors if `pat` is not a single-rooted, acyclic graph the matcher
    /// can handle (see [`Pattern::root`]).
    ///
    /// [`find_joined_constrained`]: Self::find_joined_constrained
    pub fn matches<'p>(
        &'p self,
        pat: &'p Pattern,
    ) -> anyhow::Result<impl Iterator<Item = Match> + 'p> {
        let root = pat.root()?;
        Ok(self
            .candidates(pat, root)
            .flat_map(move |node| self.matches_at_node(node, pat, root, false)))
    }

    /// Every match for `pat`, collected. Sugar for
    /// [`matches`](Self::matches)`.collect()` — the eager form is what nearly
    /// every caller wants, so it is spelled once here rather than at each site.
    /// Reach for `matches` directly to stop early or to avoid the `Vec`.
    ///
    /// # Errors
    /// Errors if `pat` is not a single-rooted, acyclic graph the matcher
    /// can handle (see [`Pattern::root`]).
    pub fn find_all(&self, pat: &Pattern) -> anyhow::Result<Vec<Match>> {
        Ok(self.matches(pat)?.collect())
    }

    /// The IR nodes to attempt `pat` (resolved match `root`) at: a
    /// discriminant-rooted pattern scans only its matching `KindIndex` bucket
    /// (O(M) in nodes of that kind), a kind-`Any` root the whole reachable
    /// graph.  Static-dispatch `Either`,
    /// so neither arm allocates or pays a per-candidate virtual call.
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

    /// Internal helper: attempt `pat` (whose resolved match root is `root`)
    /// at `node`, returning its matches (iterates value outputs for
    /// value-producing nodes, falls back to a node-rooted attempt for
    /// zero-output kinds). Shared between [`Self::matches`] and
    /// [`Self::match_at`].
    ///
    /// `first_only` stops at the first match ([`Self::match_at`]'s contract,
    /// and what keeps it cheap enough to call at every node from the rewrite
    /// driver); otherwise every DISTINCT match at this root is enumerated,
    /// deduplicated by capture-to-binding map. The dedup set is per-node: two
    /// different roots are different matches regardless of their bindings, and
    /// the outputs of one node share the set so a pattern reachable through
    /// several outputs doesn't double-report an identical binding.
    fn matches_at_node(
        &self,
        node: NodeId,
        pat: &Pattern,
        root: PatNodeId,
        first_only: bool,
    ) -> Vec<Match> {
        let mut hits: Vec<Match> = Vec::new();
        let mut seen: FxHashSet<Vec<(u32, Binding)>> = FxHashSet::default();
        // Records each fully guard-satisfying configuration the walk reaches.
        // Returning `false` (the `find_all` case) rejects it *as a stopping
        // point* only — the match is already banked in `hits` — which drives
        // the engine's existing backtracking on to the next operand ordering /
        // existential slot. Returning `true` accepts and stops.
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
                walk::try_match_node(self, pat, root, node, &mut bindings, &mut collect);
            } else {
                for &out_id in outputs {
                    let mut bindings = Bindings::default();
                    // `try_match` returns `true` only when `collect` accepted,
                    // i.e. `first_only` — so this stops after the first hit
                    // there and sweeps every output otherwise.
                    if walk::try_match(self, pat, root, out_id, &mut bindings, &mut collect) {
                        break;
                    }
                }
            }
        }
        hits
    }

    /// Try `pat` at a specific IR node; returns the first match if any
    /// (iterating outputs for value-producing nodes; node-rooted for
    /// zero-output kinds).
    ///
    /// # Errors
    /// Errors if `pat` is not a single-rooted, acyclic graph the matcher
    /// can handle (see [`Pattern::root`]).
    pub fn match_at(&self, node: NodeId, pat: &Pattern) -> anyhow::Result<Option<Match>> {
        let root = pat.root()?;
        // Root-kind gate: a pattern rooted at a concrete op-kind can only match
        // a node of that same kind, so reject a mismatched candidate HERE —
        // before `try_match_at_node` iterates outputs, allocates a `Bindings`
        // per output, and walks in only to bail on the first kind check.  This
        // is what makes `match_at` cheap to call at every node (the rewrite
        // driver's usage); `matches` gets the same prefilter from its
        // `KindIndex` bucket.  A kind-`Any` root has no discriminant and skips
        // the gate.
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

    /// Run several patterns over the graph and return only the joined
    /// matches where every [`crate::Capture`] appearing in more than
    /// one pattern binds to the same node (and value output, when
    /// applicable) across every pattern in which it appears.
    ///
    /// # Returns
    ///
    /// Outer index — one entry per joined-match tuple. Inner index —
    /// one [`Match`] per input pattern, in input order. Every inner
    /// `Match` in a given tuple agrees with the others on every shared
    /// capture's binding.
    ///
    /// # Complexity
    ///
    /// O(N₁ × N₂ × … × N_M) worst case where N_i is the number of
    /// matches for pattern i.
    ///
    /// # Shared-capture requirement
    ///
    /// A join correlates patterns on their *shared* captures. A pattern
    /// that itself declares ≥1 capture but shares **none** of them with
    /// the preceding patterns is almost always a mis-wired correlation
    /// (the author meant the captures to line up but they don't), and
    /// without a shared capture `prefix_agrees` approves *every* tuple —
    /// turning the join into an unbounded cartesian explosion. Such a
    /// pattern is therefore rejected. A *capture-free* pattern (a pure
    /// filter, e.g. `call().at(0x1234).build()`) is exempt: it deliberately
    /// imposes no correlation and degrades to a documented cross-product.
    ///
    /// # Deduplication
    ///
    /// Surviving tuples are deduplicated by their shared-capture binding
    /// signature (resolved to nodes): two tuples that agree on every
    /// shared capture but differ only on an uncaptured / non-shared
    /// internal binding are equivalent for any correlated-site consumer,
    /// so only the first is kept.
    ///
    /// # Constraints
    ///
    /// `constraints` further filters the joined tuples by CFG
    /// [`JoinConstraint`]s (control dominance) over captured entities. Each is
    /// a **post-correlation** predicate: a tuple survives iff it passes every
    /// constraint on the entities its captures bind. Every capture a constraint
    /// mentions must be bound by some pattern in the join (RANGE RESTRICTION) —
    /// an unbound one is rejected with an error rather than silently failing
    /// every tuple. A constraint whose captured node has no CFG position simply
    /// fails (the tuple is dropped) — that is not an error. Pass `&[]` for an
    /// unconstrained join.
    ///
    /// # Errors
    /// Errors if any pattern is not a single-rooted, acyclic graph the
    /// matcher can handle (see [`Pattern::root`]), if a capture-bearing
    /// pattern is connected to the rest by neither a shared capture nor a
    /// constraint, or if a constraint mentions a capture no pattern binds.
    pub fn find_joined_constrained(
        &self,
        pats: &[&Pattern],
        constraints: &[JoinConstraint],
    ) -> anyhow::Result<Vec<JoinedMatch>> {
        if pats.is_empty() {
            return Ok(Vec::new());
        }

        // A join correlates patterns on shared captures, so the set of
        // capture-bearing patterns must form ONE connected component under the
        // "shares a capture" relation — otherwise the join is a cartesian
        // product across an unrelated group. Check connectivity over ALL
        // patterns via union-find, which is ORDER-INDEPENDENT: a pattern that
        // shares a capture only with a *later* pattern (e.g. `call` bridging to
        // `guard` through a `load` pattern listed after it) is still connected.
        // A capture-free pattern is exempt (an intentional cross-product).
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]]; // path halving
                x = parent[x];
            }
            x
        }
        let mut parent: Vec<usize> = (0..pats.len()).collect();
        let mut cap_owner: FxHashMap<crate::Capture, usize> = FxHashMap::default();
        let mut capture_bearing: Vec<usize> = Vec::new();
        for (i, p) in pats.iter().enumerate() {
            let mut has_cap = false;
            for c in p.bound_captures() {
                has_cap = true;
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
        // Constraints correlate patterns too: two patterns linked by a
        // constraint whose captures live one in each are connected even with no
        // shared capture (the common case — `guard` captures `t`, `call`
        // captures `c`, joined by `dominates(t, c)`). Union their
        // owners so the connectivity check accepts a constraint-correlated join.
        for con in constraints {
            // RANGE RESTRICTION. Every capture a constraint mentions must be
            // bound by some pattern in the join — `cap_owner` records exactly
            // the captures a pattern BINDS. This is one rule, not a negation
            // carve-out: an unbound capture makes a POSITIVE constraint fail for
            // want of a binding, silently dropping every tuple and returning the
            // ambiguous ∅ that reads as "no such shape"; under `Not` that same
            // failure flips to a vacuous TRUE and matches EVERYTHING. Both are
            // the same authoring bug, so both are rejected loudly here — which
            // also makes the vacuity impossible by construction rather than by a
            // special case, and lets the union below be total.
            let caps = con.captures();
            if let Some(c) = caps.iter().find(|c| !cap_owner.contains_key(c)) {
                anyhow::bail!(
                    "find_joined: constraint mentions capture {c:?}, which no pattern \
                     in the join binds — it could never be satisfied (and under \
                     `negate` would hold vacuously, true because nothing was seen); \
                     bind it with a positive pattern"
                );
            }
            // Union the owners of ALL of a constraint's captures into one
            // component (a 3-capture constraint links three patterns). Total: the
            // range-restriction check above proved every capture has an owner.
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
                         with the others — a join correlates on shared captures (use a \
                         capture-free pattern for an intentional cross-product)"
                    );
                }
            }
        }

        // One `find_all` result per pattern — a list of INDEPENDENT matches, not
        // a joined row, so deliberately not a `JoinedMatch` despite the shape.
        let per_pat: Vec<Vec<Match>> = pats
            .iter()
            .map(|p| self.find_all(p))
            .collect::<anyhow::Result<_>>()?;
        if per_pat.iter().any(|hits| hits.is_empty()) {
            return Ok(Vec::new());
        }

        // Seed the accumulator with single-element tuples from the
        // first pattern's hits.
        let mut acc: Vec<JoinedMatch> = per_pat[0].iter().cloned().map(|m| vec![m]).collect();

        // Incrementally cross-product with each subsequent pattern's
        // matches, filtering on shared-capture agreement against the
        // accumulated prefix.
        for next in per_pat.iter().skip(1) {
            let mut new_acc: Vec<JoinedMatch> = Vec::new();
            for prefix in &acc {
                for m in next {
                    if prefix_agrees(prefix, m, self.function().graph()) {
                        let mut joined: Vec<Match> = prefix.clone();
                        joined.push(m.clone());
                        new_acc.push(joined);
                    }
                }
            }
            acc = new_acc;
            if acc.is_empty() {
                break;
            }
        }

        if !constraints.is_empty() {
            let eval = ConstraintEval::new(self.function());
            // A row survives iff EVERY constraint returns a real `Some(true)`.
            // `Some(false)` (a genuine no) and `None` (a capture was unbound in
            // this row, so the relation is unanswerable) both drop it.
            acc.retain(|tuple| {
                constraints
                    .iter()
                    .all(|c| eval.passes(c, tuple) == Some(true))
            });
        }

        dedup_on_shared_captures(&mut acc, self.function().graph());
        Ok(acc)
    }
}

/// A CFG relation between captured entities, applied by
/// [`Matcher::find_joined_constrained`] as a post-correlation filter over
/// joined tuples. Each captured entity is resolved to a [`CtrlKey`] by WHAT IT
/// BOUND (see `ConstraintEval::ctrl_key_of`): a control-output capture (e.g.
/// via [`IfPat::capture_true`](crate::IfPat::capture_true)) resolves to a
/// `CtrlKey::Edge`, any other capture to a `CtrlKey::Node` (a value's producer).
/// `PhiInputFromEdge` uses the branch-edge value directly.
///
/// Every variant is a pure FILTER over a joined tuple: it decides, it never
/// binds. All it holds are [`Capture`](crate::Capture)s, so it is a plain
/// `Clone + Debug` value type.
#[derive(Debug, Clone)]
pub enum JoinConstraint {
    /// The entity bound to `dominator` dominates the entity bound to
    /// `dominated` in the control subgraph. Each operand is resolved to a
    /// node-or-edge by WHAT IT CAPTURED (see
    /// `ConstraintEval::ctrl_key_of`): a capture that bound a CONTROL value
    /// (e.g. via [`IfPat::capture_true`](crate::IfPat::capture_true)) is an
    /// EDGE, any other capture is a NODE (a value's producer). A capture with no
    /// control-flow position fails it.
    ///
    /// This ONE constraint subsumes three relations:
    ///   * NODE dominates NODE — the plain control-dominance query.
    ///   * EDGE dominates NODE — "the node sits in the block that edge leads
    ///     into, *exclusively*". A single `dominates(true_edge, c)` expresses
    ///     "`c` is in the true block". This is EDGE dominance (evaluated on the
    ///     edge-split control graph), not dominance by the edge's target: with
    ///     an empty arm (`if (c) {} else { X }`) the edge runs straight into the
    ///     join, and the join dominates everything past the merge — so
    ///     target-dominance would claim post-merge nodes sit inside the branch.
    ///   * EDGE dominates EDGE — "the outer branch edge dominates the inner
    ///     one", i.e. every path through the inner edge first traversed the
    ///     outer.
    ///
    /// Dispatch is dominator-first and keeps the fast path: a node→node query
    /// runs on the plain node dominator tree; any edge operand routes to the
    /// (subsuming but slower-to-walk) edge-split tree.
    Dominates {
        dominator: crate::Capture,
        dominated: crate::Capture,
    },
    /// The `Phi`/`MemPhi` bound to `phi` merges, on the predecessor whose owning
    /// `Region` control input equals the branch edge `edge`, the value bound to
    /// `value`.  Ties a phi's per-branch data input to the control edge that
    /// leads into that predecessor, so a pattern can say "the value merged from
    /// THIS branch is X" without a slot index.  Works for a value `Phi` (`value`
    /// binds the merged value) and a `MemPhi` (`value` binds the merged memory
    /// token).
    ///
    /// A predecessor qualifies when `edge` dominates its control input as an
    /// EDGE — every path traversing that predecessor first traversed `edge`.
    /// This covers both the direct case (`edge` IS the region's control input;
    /// see `ConstraintEval::phi_arms_from_edge` on why that is a zero-length
    /// path rather than a special case) and an arm merged across intervening
    /// control — a `Call`, or a whole guarded loop.
    ///
    /// `edge` must bind a control-output value; `phi` binds a value; `value`
    /// must be bound by some pattern in the join — most naturally by
    /// [`PhiPat::any_input`](crate::PhiPat::any_input) on the very phi pattern
    /// that binds `phi`, which anchors the value at the phi's own inputs
    /// (O(arity)) instead of letting it float as an independent whole-graph
    /// root.
    PhiInputFromEdge {
        phi: crate::Capture,
        edge: crate::Capture,
        value: crate::Capture,
    },
    /// The negation of `inner`: a tuple survives iff `inner` does NOT hold on
    /// it.
    ///
    /// Two independent guards keep this sound, on different axes:
    ///   * DECLARED-ness (static): [`Matcher::find_joined_constrained`] rejects
    ///     a constraint mentioning a capture NO pattern binds — that could never
    ///     be satisfied, and under `Not` would match everything.
    ///   * BOUND-ness (per-tuple): evaluation is three-valued (see
    ///     `ConstraintEval::passes`). An unbound capture in THIS row makes
    ///     `inner` return `None`, and `Not(None) == None` drops the row — never
    ///     the vacuous `true` a two-valued `!false` would produce.
    Not(Box<JoinConstraint>),
    /// Disjunction — a tuple passes iff it passes ANY listed constraint. An
    /// empty list passes nothing (the identity of `Or`). Every constraint is a
    /// pure `bool` filter, so this is a plain short-circuiting `any`.
    Or(Vec<JoinConstraint>),
    /// Conjunction — a tuple passes iff it passes EVERY listed constraint. An
    /// empty list passes everything (the identity of `And`). The top-level
    /// `constraints` slice is already an implicit `And`; this one nests inside an
    /// `Or`, where the flat slice cannot reach.
    And(Vec<JoinConstraint>),
}

impl JoinConstraint {
    /// Every capture this constraint correlates (used to link their owner
    /// patterns for the `find_joined` connectivity check).
    fn captures(&self) -> Vec<crate::Capture> {
        match self {
            JoinConstraint::Dominates {
                dominator,
                dominated,
            } => vec![*dominator, *dominated],
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => vec![*phi, *edge, *value],
            // A negation / connective correlates exactly the captures it wraps:
            // contributing these links the owner patterns AND is what the
            // range-restriction check in `find_joined_constrained` tests for
            // declared-ness.
            JoinConstraint::Not(inner) => inner.captures(),
            JoinConstraint::Or(cs) | JoinConstraint::And(cs) => {
                cs.iter().flat_map(JoinConstraint::captures).collect()
            }
        }
    }
}

/// One row of a join: exactly one [`Match`] per pattern in the query, in the
/// order the patterns were given.
///
/// Named because a bare `Vec<Match>` means something DIFFERENT 150 lines up —
/// [`Matcher::find_all`] returns a list of independent matches, whereas here a
/// `Vec<Match>` is a single row and the list of them is `Vec<JoinedMatch>`.
/// Same type, opposite meanings; the alias is what tells them apart at a glance.
pub type JoinedMatch = Vec<Match>;

/// Kleene OR over three-valued verdicts: `Some(true)` if ANY input is
/// `Some(true)` (truth dominates and short-circuits); else `None` if ANY input
/// is `None` (unknown poisons a would-be `false`); else `Some(false)`. The
/// empty iterator yields `Some(false)` — the identity of `Or`.
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

/// Kleene AND over three-valued verdicts: `Some(false)` if ANY input is
/// `Some(false)` (falsity dominates and short-circuits); else `None` if ANY
/// input is `None` (unknown poisons a would-be `true`); else `Some(true)`. The
/// empty iterator yields `Some(true)` — the identity of `And`.
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

/// Evaluates [`JoinConstraint`]s against joined tuples, memoising the two
/// dominator trees across one `find_joined_constrained` call — each is built at
/// most once, never per tuple and never per arm.
///
/// # Why TWO trees, when one would do
///
/// The split tree SUBSUMES the node tree: `dominates(split_doms, Node(a),
/// Node(b))` equals `dominates(doms, a, b)` exactly, because edge-splitting
/// preserves paths 1:1 (`strider-ir`'s `split_dominance_subsumes_node_dominance`
/// pins this over every ordered node pair of a diamond, a guarded loop, and an
/// empty arm).  So `doms` is deletable on correctness grounds — it is kept on
/// MEASURED performance grounds alone.
///
/// Collapsing onto `split_doms` was benchmarked (`benches/matcher.rs`,
/// `join_dominates_only` / `join_dominates_and_branch` over a 60-diamond chain):
///
/// ```text
///                        two trees   one tree
/// Dominates-only          4.18 ms     5.80 ms   +39%   (60 diamonds)
/// Dominates-only         33.6  µs    44.2  µs   +31%   ( 6 diamonds)
/// mixed (Dominates+edge)  8.06 ms     9.68 ms   +20%   (60 diamonds)
/// mixed (Dominates+edge) 52.4  µs    46.7  µs   -11%   ( 6 diamonds)
/// ```
///
/// The cost that matters is the PER-TUPLE chain walk, not the one-off build:
/// `Edge` vertices interleave, so a node-dominance chain in the split tree is
/// ~2x longer, and `Dominates` is re-evaluated for every joined tuple (a join of
/// K guards and M calls is K*M).  Saving a build is a constant; doubling the
/// walk scales with tuples * depth.  That is why the mixed shape — which saves
/// one whole build — still WINS at 6 diamonds and LOSES at 60: past a few
/// hundred tuples the walk dominates the build it saved.
///
/// So: node→node queries use `doms`; an edge operand of [`JoinConstraint::Dominates`]
/// and [`JoinConstraint::PhiInputFromEdge`] build and walk `split_doms`.  Both
/// stay lazy, so a join pays for exactly the relations it asks for.
struct ConstraintEval<'f> {
    function: &'f Function,
    /// Dominators of the plain control subgraph, keyed by `NodeId`.
    doms: OnceCell<petgraph::algo::dominators::Dominators<NodeId>>,
    /// Dominators of the EDGE-SPLIT control subgraph, keyed by `CtrlKey`. Lazy:
    /// a join with no edge constraint never builds it.
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

    /// The plain node dominator tree, built on first use. Kept alongside
    /// [`Self::split_doms`] on MEASURED grounds (see the type doc): node→node
    /// dominance walks it ~+39% faster than the subsuming split tree.
    fn doms(&self) -> &petgraph::algo::dominators::Dominators<NodeId> {
        self.doms.get_or_init(|| control_dominators(self.function))
    }

    /// The edge-split dominator tree, built on first use.
    fn split_doms(&self) -> &petgraph::algo::dominators::Dominators<CtrlKey> {
        self.split_doms
            .get_or_init(|| control_edge_dominators(self.function))
    }

    /// Resolve a capture to a [`CtrlKey`] across the tuple's matches, keyed on
    /// WHAT IT BOUND — the node-or-edge choice the general [`JoinConstraint::Dominates`]
    /// dispatches on:
    ///   1. a bound VALUE whose kind is `Control` (an `If`'s
    ///      `capture_true`/`capture_false` edge) → [`CtrlKey::Edge`];
    ///   2. any other bound value → [`CtrlKey::Node`] of its producer;
    ///   3. else a bound NODE → [`CtrlKey::Node`];
    ///   4. else `None` (unbound) — feeds the three-valued [`Self::passes`].
    fn ctrl_key_of(&self, tuple: &JoinedMatch, c: crate::Capture) -> Option<CtrlKey> {
        if let Some(v) = self.value_of(tuple, c) {
            return Some(if self.function.value_kind(v).is_control() {
                CtrlKey::Edge(v)
            } else {
                CtrlKey::Node(self.function.producer(v))
            });
        }
        self.node_of(tuple, c).map(CtrlKey::Node)
    }

    /// Resolve a capture to a control node across the tuple's matches.
    fn node_of(&self, tuple: &JoinedMatch, c: crate::Capture) -> Option<NodeId> {
        tuple.iter().find_map(|m| m.node(c, self.function.graph()))
    }

    /// Resolve a capture to the value it binds across the tuple's matches.
    fn value_of(&self, tuple: &JoinedMatch, c: crate::Capture) -> Option<ValueId> {
        tuple.iter().find_map(|m| m.value(c))
    }

    /// Three-valued (Kleene) verdict for `tuple` against `c`: `Some(b)` is a
    /// real verdict, `None` means "a referenced capture was UNBOUND in this row,
    /// so the relation cannot be answered". The top-level fold keeps a row iff
    /// every constraint returns `Some(true)` — `None` and `Some(false)` both
    /// drop it, so an unbound capture never survives.
    ///
    /// This is what makes `Not` sound WITHOUT the static range check having to
    /// carry the whole burden: `Not(None) == None` (drops), never the vacuous
    /// `true` a two-valued `!false` would produce for an unbound capture.
    fn passes(&self, c: &JoinConstraint, tuple: &JoinedMatch) -> Option<bool> {
        match *c {
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => {
                let (Some(phi_v), Some(edge_v), Some(val_v)) = (
                    self.value_of(tuple, phi),
                    self.value_of(tuple, edge),
                    self.value_of(tuple, value),
                ) else {
                    return None;
                };
                // Short-circuits on the first qualifying arm: a pure predicate
                // never needs to see the rest.
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
                // Dominator-first dispatch that PRESERVES the node-tree fast
                // path.  The split tree subsumes the node tree (it answers
                // node→node identically — `split_dominance_subsumes_node_dominance`
                // pins this), but its `Edge` vertices interleave, so a node
                // chain there is ~2x longer and the per-tuple walk is +39%
                // (`benches/matcher.rs`).  So node→node stays on `doms`; any edge
                // operand routes to `split_doms`.  Both `dominates` calls
                // typecheck because it is generic over `Copy + Eq + Hash`.
                Some(match (key_a, key_b) {
                    (CtrlKey::Node(na), CtrlKey::Node(nb)) => dominates(self.doms(), na, nb),
                    (ka, kb) => dominates(self.split_doms(), ka, kb),
                })
            }
            // `Not(None) == None`: the negation of an unanswerable constraint is
            // itself unanswerable, so an unbound capture drops the row instead of
            // vacuously keeping it. Reuses the same memoised `doms`, so a
            // negation costs no extra walk.
            JoinConstraint::Not(ref inner) => self.passes(inner, tuple).map(|b| !b),
            JoinConstraint::Or(ref cs) => kleene_or(cs.iter().map(|c| self.passes(c, tuple))),
            JoinConstraint::And(ref cs) => kleene_and(cs.iter().map(|c| self.passes(c, tuple))),
        }
    }

    /// Every value the `Phi`/`MemPhi` producing `phi_v` merges on a predecessor
    /// that comes from the branch edge `edge_v`.
    ///
    /// Internal to [`ConstraintEval`], backing
    /// [`JoinConstraint::PhiInputFromEdge`].
    ///
    /// Slot alignment: a `Phi`/`MemPhi`'s inputs are `[PhiToken, v0, v1, …]` —
    /// data input `i+1` is predecessor `i`'s value (a `Memory` token for
    /// `MemPhi`) — and its owning `Region` (the `PhiToken`'s producer) has
    /// control input `i` for predecessor `i`.  So a qualifying region slot maps
    /// to the phi input one slot over.
    ///
    /// # Which arms come from an edge
    ///
    /// Predecessor `i` (control input `c_i`) comes from `edge_v` when
    ///
    /// ```text
    /// dominates(split_doms, Edge(edge_v), Edge(c_i))
    /// ```
    ///
    /// One clause, EDGE against EDGE.  Both `edge_v` and `c_i` are control
    /// edges, so in the edge-split graph this is plain dominance: "every path
    /// that traverses `c_i` first traversed `edge_v`".
    ///
    /// **The direct case is a zero-length path, not a special case.** When the
    /// edge IS the predecessor (`c_i == edge_v`) this holds because
    /// `dominates(x, x)` is true — which is why there is no `||` union with an
    /// `==` test.  Do not "simplify" this into edge-against-`producer(c_i)`:
    /// `producer(c_i)` is the `If`, which PRECEDES the edge, so the edge cannot
    /// dominate it and the direct case would break.  (That trap has bitten
    /// twice; the direct-case tests are what catch it.)
    ///
    /// The relation is EXCLUSIVE: it holds only where every path traverses
    /// `edge_v`, so an arm reachable from both sides of the branch belongs to
    /// neither edge.  Unlike the old node-dominance proxy it needs no sole-entry
    /// gate: a guarded loop's header has two predecessors (the guard's edge and
    /// its own latch), and edge dominance still correctly reports that every
    /// path into the loop traverses the guard's edge.
    ///
    /// Yields arms LAZILY: the caller `.any(..)`s over it and short-circuits on
    /// the first qualifying arm.  Allocates nothing.
    ///
    /// A branch whose block splits and reaches the join more than once yields one
    /// arm per qualifying predecessor — the caller enumerates them rather than
    /// picking one.  (Under a direct-only `==` at most one arm could ever
    /// qualify, which is why a single `Option` used to be total; dominance can
    /// qualify several, and choosing among them would be a silent coin-flip.)
    fn phi_arms_from_edge(
        &self,
        phi_v: ValueId,
        edge_v: ValueId,
    ) -> impl Iterator<Item = ValueId> + '_ {
        // All per-(phi, edge) work — phi node, region — is resolved ONCE here;
        // the per-arm body below is the single dominance clause.
        self.arm_scan(phi_v)
            .into_iter()
            .flat_map(move |(phi_inputs, region_inputs)| {
                region_inputs
                    .into_iter()
                    .enumerate()
                    .filter(move |(_, c)| {
                        // Edge against EDGE.  The split dominator tree is
                        // memoised across the whole `find_joined_constrained`
                        // call — never rebuilt per arm or per tuple.
                        dominates(self.split_doms(), CtrlKey::Edge(edge_v), CtrlKey::Edge(*c))
                    })
                    // Region control input `i` ⇒ phi data input `i + 1`.
                    .filter_map(move |(i, _)| phi_inputs.get(i + 1).copied())
            })
    }

    /// Resolve the per-`phi` scan state for [`Self::phi_arms_from_edge`] once:
    /// the phi's inputs and its region's control inputs.
    ///
    /// `Inputs` is a `Copy` borrow of the graph's use-list, so carrying the two
    /// input views costs no allocation.
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

/// Deduplicate joined tuples by their capture binding signature.
///
/// The signature is every capture bound anywhere in the tuple, resolved
/// to its node and sorted. Two tuples with an identical signature bind
/// every capture to the same node, so they are indistinguishable to a
/// consumer that acts on captures (they differ only in uncaptured /
/// internal bindings or in which redundant correlated pairing produced
/// them) — only the first is kept, order-preserving.
///
/// A tuple whose signature is **empty** (no capture bound anywhere) is
/// always kept: that is the documented capture-free cross-product, whose
/// tuples are intentionally distinct even though they bind nothing.
fn dedup_on_shared_captures(acc: &mut Vec<JoinedMatch>, graph: &Graph) {
    let mut seen: FxHashSet<Vec<(u32, NodeId)>> = FxHashSet::default();
    acc.retain(|tuple| {
        let mut sig: Vec<(u32, NodeId)> = Vec::new();
        let mut sig_ids: FxHashSet<u32> = FxHashSet::default();
        for m in tuple {
            for (cap, _) in m.bindings.iter() {
                if let Some(node) = m.bindings.get_node(cap, graph)
                    && sig_ids.insert(cap.id())
                {
                    sig.push((cap.id(), node));
                }
            }
        }
        // Empty signature: a pure cross-product tuple — never dedup.
        if sig.is_empty() {
            return true;
        }
        sig.sort_unstable_by_key(|(id, _)| *id);
        seen.insert(sig)
    });
}

/// True when every capture in `m`'s bindings that also appears in any
/// previously-collected match in `prefix` agrees with it.
///
/// Agreement is at the resolved-NODE level (the join's documented contract is
/// "the same node"), so the same IR node captured as `Value(v)` by one pattern
/// and `Node(producer(v))` by another still agrees — a raw `Binding`-variant
/// compare would treat them as different and silently drop a valid tuple.  Two
/// *value* captures are still compared at value granularity so distinct outputs
/// of one multi-output node don't falsely agree.
fn prefix_agrees(prefix: &[Match], m: &Match, graph: &Graph) -> bool {
    for prev in prefix {
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

    // The three-valued truth values, spelled out for the table below.
    const T: Option<bool> = Some(true);
    const F: Option<bool> = Some(false);
    const U: Option<bool> = None;

    #[test]
    fn not_of_unknown_is_unknown() {
        // `Not` is `passes(inner).map(|b| !b)`; the load-bearing case is that an
        // unanswerable `inner` stays unanswerable (drops the row) rather than
        // flipping to a vacuous `true`.
        assert_eq!(U.map(|b: bool| !b), U);
        assert_eq!(T.map(|b| !b), F);
        assert_eq!(F.map(|b| !b), T);
    }

    #[test]
    fn kleene_or_truth_dominates_then_unknown_poisons() {
        // Any `Some(true)` wins outright, even alongside unknown / false.
        assert_eq!(kleene_or([U, T, F].into_iter()), T);
        // No truth, but an unknown present → unknown (a would-be `false`).
        assert_eq!(kleene_or([F, U, F].into_iter()), U);
        // All false → false.
        assert_eq!(kleene_or([F, F].into_iter()), F);
        // Empty Or is its identity: `Some(false)`.
        assert_eq!(kleene_or(std::iter::empty()), F);
    }

    #[test]
    fn kleene_and_falsity_dominates_then_unknown_poisons() {
        // Any `Some(false)` wins outright, even alongside unknown / true.
        assert_eq!(kleene_and([U, F, T].into_iter()), F);
        // No falsity, but an unknown present → unknown (a would-be `true`).
        assert_eq!(kleene_and([T, U, T].into_iter()), U);
        // All true → true.
        assert_eq!(kleene_and([T, T].into_iter()), T);
        // Empty And is its identity: `Some(true)`.
        assert_eq!(kleene_and(std::iter::empty()), T);
    }
}
