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
/// *some* value input of the consumer node (e.g. `phi().any_input(p)` matches
/// a `Phi` one of whose data inputs matches `p`, without knowing which
/// predecessor). Recognised by [`walk::try_match_at`], which routes these edges
/// through the existential search instead of the fixed-slot `match_inputs`.
pub(crate) const ANY_INPUT_SLOT: usize = usize::MAX;

use std::cell::OnceCell;
use std::mem::Discriminant;

use itertools::Either;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::NodeId as PatNodeId;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{
    CtrlKey, Function, Graph, IRViewer, IRWalker, control_dominators, control_edge_dominators,
    dominates, edge_dominates,
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
    /// [`Bindings::binding_signature`]): `add(var(x), var(x))` matched swapped
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
    /// a **post-correlation** predicate: a tuple survives iff every constraint
    /// holds on the entities its captures bind. A constraint referencing a
    /// capture no tuple binds, or one whose captured node has no CFG position,
    /// simply fails (the tuple is dropped) — never an error. Pass `&[]` for an
    /// unconstrained join.
    ///
    /// # Errors
    /// Errors if any pattern is not a single-rooted, acyclic graph the
    /// matcher can handle (see [`Pattern::root`]), or if a capture-bearing
    /// pattern is connected to the rest by neither a shared capture nor a
    /// constraint.
    pub fn find_joined_constrained(
        &self,
        pats: &[&Pattern],
        constraints: &[&JoinConstraint],
    ) -> anyhow::Result<Vec<Vec<Match>>> {
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
        // captures `c`, joined by `dominated_by_branch(t, c)`). Union their
        // owners so the connectivity check accepts a constraint-correlated join.
        for con in constraints {
            // RANGE RESTRICTION for negation. `cap_owner` only records captures
            // some pattern BINDS, and the union below silently skips a capture
            // it has no owner for — fine for a positive constraint (an unbound
            // capture just fails it, dropping the tuple) but catastrophic under
            // negation, where that failure would flip to a vacuous TRUE and
            // match everything. So a `Not` must have every capture bound by a
            // positive pattern; reject loudly otherwise.
            if let JoinConstraint::Not(inner) = con {
                if inner.is_binding() {
                    anyhow::bail!(
                        "find_joined: cannot negate a binding constraint \
                         (phi_input_from_edge with an inline value pattern binds \
                         captures rather than deciding a predicate, so there is \
                         nothing to bind on the false branch) — spell the negated \
                         fact with a capture value instead"
                    );
                }
                if let Some(c) = inner.captures().iter().find(|c| !cap_owner.contains_key(c)) {
                    anyhow::bail!(
                        "find_joined: cannot negate a constraint mentioning capture \
                         {c:?}, which no pattern in the join binds — negating an unbound \
                         capture would hold vacuously (true because nothing was seen) \
                         rather than meaningfully; bind it with a positive pattern"
                    );
                }
            }
            // Union the owners of ALL of a constraint's captures into one
            // component (a 3-capture constraint links three patterns).
            let mut owners = con
                .captures()
                .into_iter()
                .filter_map(|c| cap_owner.get(&c).copied());
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

        let per_pat: Vec<Vec<Match>> = pats
            .iter()
            .map(|p| self.find_all(p))
            .collect::<anyhow::Result<_>>()?;
        if per_pat.iter().any(|hits| hits.is_empty()) {
            return Ok(Vec::new());
        }

        // Seed the accumulator with single-element tuples from the
        // first pattern's hits.
        let mut acc: Vec<Vec<Match>> = per_pat[0].iter().cloned().map(|m| vec![m]).collect();

        // Incrementally cross-product with each subsequent pattern's
        // matches, filtering on shared-capture agreement against the
        // accumulated prefix.
        for next in per_pat.iter().skip(1) {
            let mut new_acc: Vec<Vec<Match>> = Vec::new();
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
            for c in constraints.iter().copied() {
                // Per-CONSTRAINT prep, hoisted OUT of the per-tuple loop: an
                // inline `value` pattern's root resolution and capture list are
                // fixed for the whole pass, so the per-tuple cost stays at one
                // match attempt at one known value — strictly less work than the
                // whole-graph root search the capture spelling needs.
                let inline = match c {
                    JoinConstraint::PhiInputFromEdge {
                        value: ValueSpec::Pattern(p),
                        ..
                    } => Some((p, p.root()?, p.bound_captures().collect::<Vec<_>>())),
                    _ => None,
                };
                let mut next: Vec<Vec<Match>> = Vec::with_capacity(acc.len());
                for tuple in acc {
                    match (c, &inline) {
                        (
                            JoinConstraint::PhiInputFromEdge { phi, edge, .. },
                            Some((pat, root, caps)),
                        ) => self.expand_phi_inline(
                            &eval, tuple, *phi, *edge, pat, *root, caps, &mut next,
                        ),
                        _ => {
                            if eval.holds(c, &tuple) {
                                next.push(tuple);
                            }
                        }
                    }
                }
                acc = next;
                if acc.is_empty() {
                    break;
                }
            }
        }

        dedup_on_shared_captures(&mut acc, self.function().graph());
        Ok(acc)
    }

    /// Evaluate a `PhiInputFromEdge` whose `value` is an INLINE pattern against
    /// one joined `tuple`, pushing a tuple per surviving binding onto `out`.
    ///
    /// Unlike a pure predicate this can also BIND, so it emits 0..n tuples
    /// rather than a `bool`. The sub-pattern is anchored at the arm VALUE
    /// (`inputs[slot+1]`) via the ordinary [`walk::try_match`] entry point — not
    /// at its producer node, which would bind the wrong output on a multi-output
    /// producer.
    ///
    /// # Unification
    ///
    /// The engine's `Bindings` is SEEDED with whatever the tuple already bound
    /// for the captures the inline pattern mentions. `Bindings::bind_capture`'s
    /// existing rebind-conflict detection then does the unification for free: an
    /// inline capture that disagrees with the tuple rejects that configuration
    /// instead of overwriting it.
    #[allow(clippy::too_many_arguments)]
    fn expand_phi_inline(
        &self,
        eval: &ConstraintEval,
        tuple: Vec<Match>,
        phi: crate::Capture,
        edge: crate::Capture,
        pat: &Pattern,
        root: PatNodeId,
        caps: &[crate::Capture],
        out: &mut Vec<Vec<Match>>,
    ) {
        let (Some(phi_v), Some(edge_v)) = (eval.value_of(&tuple, phi), eval.value_of(&tuple, edge))
        else {
            return;
        };
        // Pull the first qualifying arm to bail out before the per-tuple seed
        // build below, then chain it back on — still lazy, nothing collected.
        let mut arms = eval.phi_arms_from_edge(phi_v, edge_v);
        let Some(first_arm) = arms.next() else {
            return;
        };
        let arms = std::iter::once(first_arm).chain(arms);
        // The inline bindings are merged into the match that bound `phi` — the
        // constraint's anchor — so a caller reads them off the tuple exactly as
        // it would a real root's.
        let Some(anchor) = tuple.iter().position(|m| m.is_bound(phi)) else {
            return;
        };

        let mut seed = Bindings::default();
        for m in &tuple {
            for (cap, b) in m.bindings.iter() {
                // First binding wins, matching `value_of`'s `find_map`; a tuple
                // agrees on shared captures already, but may spell one as a
                // `Node` and another as a `Value` of the same node.
                if caps.contains(&cap) && !seed.is_bound(cap) {
                    seed.bind_capture(cap, b);
                }
            }
        }

        let mut seen: FxHashSet<Vec<(u32, Binding)>> = FxHashSet::default();
        let mut hits: Vec<Bindings> = Vec::new();
        // One attempt per QUALIFYING arm (a split branch can reach the join more
        // than once), each from a fresh copy of the seed so one arm's bindings
        // never leak into the next.  `seen` spans the arms, so two arms that
        // yield an identical binding collapse to one tuple.
        for arm in arms {
            let mut arm_seed = seed.clone();
            // `false` = record and keep enumerating, so every DISTINCT inline
            // binding yields its own tuple (the `find_all` enumeration contract).
            let mut collect = |b: &mut Bindings| -> bool {
                if seen.insert(b.binding_signature()) {
                    hits.push(b.clone());
                }
                false
            };
            walk::try_match(self, pat, root, arm, &mut arm_seed, &mut collect);
        }

        for b in hits {
            let mut t = tuple.clone();
            for (cap, bind) in b.iter() {
                // Already in the tuple (seeded, hence already unified) — skip.
                if t.iter().any(|m| m.is_bound(cap)) {
                    continue;
                }
                t[anchor].bindings.bind_capture(cap, bind);
            }
            out.push(t);
        }
    }
}

/// A CFG relation between two captured entities, applied by
/// [`Matcher::find_joined_constrained`] as a post-correlation filter over
/// joined tuples. Captured entities are resolved to control nodes; a value
/// capture resolves to its producer node (for `Dominates`) or is used directly
/// as the branch-edge value (for the edge constraints, `DominatedByBranch` and
/// `PhiInputFromEdge`).
///
/// Not `Clone`: [`ValueSpec::Pattern`] owns a [`Pattern`], which holds match-time
/// closures and so cannot be cloned. Constraints are passed by reference
/// (`&[&JoinConstraint]`), symmetric with the patterns themselves.
#[derive(Debug)]
pub enum JoinConstraint {
    /// The node bound to `a` dominates the node bound to `b` in the control
    /// subgraph. A capture absent from the control subgraph fails it.
    Dominates {
        a: crate::Capture,
        b: crate::Capture,
    },
    /// `node` is dominated by the branch EDGE `branch` — i.e. every path from
    /// the entry to `node` traverses that edge, so `node` sits in the block that
    /// edge leads into, *exclusively*.  A single
    /// `dominated_by_branch(true_edge, c)` therefore expresses "`c` is in the
    /// true block".
    ///
    /// This is EDGE dominance (evaluated on the edge-split control graph), not
    /// dominance by the edge's target.  The distinction is not academic: with an
    /// empty arm (`if (c) {} else { X }`) the edge runs straight into the join,
    /// and the join dominates everything past the merge — so target-dominance
    /// would claim post-merge nodes sit inside the branch.
    ///
    /// `branch` must bind a control-output value (e.g. via
    /// [`IfPat::capture_true`](crate::IfPat::capture_true)); an ill-typed
    /// `branch` or an absent node fails it.  `node` must resolve to a node of
    /// the CONTROL subgraph (a `Call`, `Region`, `Return`, …): a data node is
    /// absent from that graph and so is never dominated by anything.
    DominatedByBranch {
        branch: crate::Capture,
        node: crate::Capture,
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
    /// see [`ConstraintEval::phi_arms_from_edge`] on why that is a zero-length
    /// path rather than a special case) and an arm merged across intervening
    /// control — a `Call`, or a whole guarded loop.
    ///
    /// `edge` must bind a control-output value; `phi` binds a value; `value` is
    /// a [`ValueSpec`] — either a capture bound elsewhere in the join, or a
    /// pattern matched INLINE at the arm value.
    PhiInputFromEdge {
        phi: crate::Capture,
        edge: crate::Capture,
        value: ValueSpec,
    },
    /// The negation of `inner`: a tuple survives iff `inner` does NOT hold on
    /// it.
    ///
    /// Negation-as-failure is only sound under RANGE RESTRICTION: every capture
    /// `inner` mentions must be bound by a *positive* pattern in the same join.
    /// Otherwise `inner` would fail merely for want of a binding and the
    /// negation would hold VACUOUSLY — "true because it could not see
    /// anything". [`Matcher::find_joined_constrained`] enforces this and
    /// rejects a range-unrestricted `Not` with an error rather than matching
    /// everything.
    ///
    /// Negating a BINDING constraint (a [`PhiInputFromEdge`] whose `value` is
    /// an inline [`ValueSpec::Pattern`]) is likewise rejected: such a
    /// constraint enumerates bindings rather than deciding a predicate, and
    /// there is nothing to bind on the false branch.
    ///
    /// [`PhiInputFromEdge`]: JoinConstraint::PhiInputFromEdge
    Not(Box<JoinConstraint>),
}

/// How [`JoinConstraint::PhiInputFromEdge`] names the merged arm value.
///
/// [`Capture`](ValueSpec::Capture) is the pure-predicate form: the value must
/// already be bound by some pattern in the join, and the constraint only
/// compares. [`Pattern`](ValueSpec::Pattern) states the fact LOCALLY — the
/// sub-pattern is matched at the arm value itself, so it needs no independent
/// root floating over the whole function, and it BINDS: captures inside it are
/// merged into the joined tuple (unifying with, never overwriting, whatever the
/// tuple already bound).
pub enum ValueSpec {
    /// A capture bound by another pattern in the join; compared by identity.
    Capture(crate::Capture),
    /// A pattern matched inline at the phi's arm value.
    ///
    /// Boxed: a `Pattern` is a whole match graph with closures in it, so an
    /// unboxed variant would make every `JoinConstraint` — including the
    /// capture-only ones — as large as a `Pattern`.
    Pattern(Box<Pattern>),
}

// `Pattern` is a graph with closures in it and so is not `Debug`; print the
// variant and let the capture form show its capture (all `JoinConstraint`'s
// other fields are captures, and it has always been `Debug`).
impl std::fmt::Debug for ValueSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValueSpec::Capture(c) => f.debug_tuple("Capture").field(c).finish(),
            ValueSpec::Pattern(_) => f.write_str("Pattern(..)"),
        }
    }
}

impl From<crate::Capture> for ValueSpec {
    fn from(c: crate::Capture) -> Self {
        ValueSpec::Capture(c)
    }
}

impl From<Pattern> for ValueSpec {
    fn from(p: Pattern) -> Self {
        ValueSpec::Pattern(Box::new(p))
    }
}

impl JoinConstraint {
    /// Every capture this constraint correlates (used to link their owner
    /// patterns for the `find_joined` connectivity check).
    ///
    /// An inline `value` pattern contributes the captures INSIDE it rather than
    /// a `value` capture of its own: those are exactly the captures it can
    /// correlate with (and unify against) the rest of the join.
    fn captures(&self) -> Vec<crate::Capture> {
        match self {
            JoinConstraint::Dominates { a, b } => vec![*a, *b],
            JoinConstraint::DominatedByBranch { branch, node } => vec![*branch, *node],
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => {
                let mut caps = vec![*phi, *edge];
                match value {
                    ValueSpec::Capture(v) => caps.push(*v),
                    ValueSpec::Pattern(p) => caps.extend(p.bound_captures()),
                }
                caps
            }
            // A negation correlates exactly what it negates: contributing these
            // links the owner patterns AND is what the range-restriction check
            // in `find_joined_constrained` tests for boundness.
            JoinConstraint::Not(inner) => inner.captures(),
        }
    }

    /// Whether this constraint BINDS captures rather than deciding a predicate
    /// — true only for an inline-pattern [`JoinConstraint::PhiInputFromEdge`],
    /// which [`Matcher::expand_phi_inline`] expands into 0..n tuples.
    fn is_binding(&self) -> bool {
        matches!(
            self,
            JoinConstraint::PhiInputFromEdge {
                value: ValueSpec::Pattern(_),
                ..
            }
        )
    }
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
/// So: node queries use `doms`; only the edge queries
/// ([`JoinConstraint::DominatedByBranch`] and [`JoinConstraint::PhiInputFromEdge`])
/// build and walk `split_doms`.  Both stay lazy, so a join pays for exactly the
/// relations it asks for.
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

    /// The edge-split dominator tree, built on first use.
    fn split_doms(&self) -> &petgraph::algo::dominators::Dominators<CtrlKey> {
        self.split_doms
            .get_or_init(|| control_edge_dominators(self.function))
    }

    /// Resolve a capture to a control node across the tuple's matches.
    fn node_of(&self, tuple: &[Match], c: crate::Capture) -> Option<NodeId> {
        tuple.iter().find_map(|m| m.node(c, self.function.graph()))
    }

    /// Resolve a capture to the value it binds across the tuple's matches.
    fn value_of(&self, tuple: &[Match], c: crate::Capture) -> Option<ValueId> {
        tuple.iter().find_map(|m| m.value(c))
    }

    /// Pure-predicate constraints. The inline-pattern `PhiInputFromEdge` is NOT
    /// evaluated here — it can bind, so it goes through
    /// [`Matcher::expand_phi_inline`] instead.
    fn holds(&self, c: &JoinConstraint, tuple: &[Match]) -> bool {
        match *c {
            JoinConstraint::PhiInputFromEdge {
                phi,
                edge,
                value: ValueSpec::Capture(value),
            } => {
                let (Some(phi_v), Some(edge_v), Some(val_v)) = (
                    self.value_of(tuple, phi),
                    self.value_of(tuple, edge),
                    self.value_of(tuple, value),
                ) else {
                    return false;
                };
                // Short-circuits on the first qualifying arm: a pure predicate
                // never needs to see the rest.
                self.phi_arms_from_edge(phi_v, edge_v)
                    .any(|arm| arm == val_v)
            }
            // Handled by `Matcher::expand_phi_inline` (it binds, so it cannot be
            // a predicate); unreachable via this entry point.
            JoinConstraint::PhiInputFromEdge {
                value: ValueSpec::Pattern(_),
                ..
            } => false,
            JoinConstraint::Dominates { a, b } => {
                let (Some(na), Some(nb)) = (self.node_of(tuple, a), self.node_of(tuple, b)) else {
                    return false;
                };
                let doms = self.doms.get_or_init(|| control_dominators(self.function));
                dominates(doms, na, nb)
            }
            JoinConstraint::DominatedByBranch { branch, node } => {
                let (Some(edge), Some(target)) =
                    (self.value_of(tuple, branch), self.node_of(tuple, node))
                else {
                    return false;
                };
                // EDGE dominance — the real relation.  Asking instead whether
                // the edge's TARGET dominates `target` (as this once did) is a
                // strictly weaker proxy: it coincides only when the edge is its
                // target's sole entry.  With an empty arm the edge runs straight
                // into the join, and the join dominates everything past the
                // merge — so the proxy silently claimed post-merge nodes were
                // inside the branch.
                edge_dominates(self.split_doms(), edge, target)
            }
            // Sound because `find_joined_constrained` has already range-checked
            // `inner`'s captures against the positive patterns and rejected a
            // binding `inner`, so a `false` here means "`inner` is false", never
            // "`inner` could not see its captures". Reuses the same memoised
            // `doms`, so a negation costs no extra walk.
            JoinConstraint::Not(ref inner) => !self.holds(inner, tuple),
        }
    }

    /// Every value the `Phi`/`MemPhi` producing `phi_v` merges on a predecessor
    /// that comes from the branch edge `edge_v`.
    ///
    /// Internal to [`ConstraintEval`] — shared by the two `value` spellings of
    /// [`JoinConstraint::PhiInputFromEdge`], which stays the only public surface.
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
    /// Yields arms LAZILY so both `value` spellings share one scan without
    /// materialising it: the capture form `.any(..)`s over it and short-circuits
    /// on the first qualifying arm, the inline-pattern form matches at each arm
    /// as it comes.  Allocates nothing.
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
fn dedup_on_shared_captures(acc: &mut Vec<Vec<Match>>, graph: &Graph) {
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
