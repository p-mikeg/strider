//! Pattern matcher over a lifted [`strider_ir::Function`].
//!
//! [`Matcher`] owns no per-match state beyond a lazy `KindIndex` bucketing
//! reachable IR nodes by `NodeKind` discriminant. A discriminant-rooted pattern
//! iterates just the matching bucket; a kind-`Any` root falls back to a full
//! reachable walk.
//!
//! The recursive match engine lives in `walk`. The cast mask is carried on the
//! [`Pattern`], not on the matcher.

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

/// Sentinel consumer slot marking an existential (`any_input`) input edge: the
/// sub-pattern is matched against some input slot of the consumer rather than a
/// fixed one, so `phi().any_input(p)` matches a `Phi` one of whose inputs
/// matches `p` without naming the predecessor. A typed sub only reaches value
/// edges; a wildcard can also reach control/memory/`PhiToken` slots.
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

/// Pre-filter discriminant for the `find_*` dispatch. `None` for a kind-`Any`
/// root, which makes the matcher scan every reachable node.
fn root_kind_discriminant(pat: &Pattern, root: PatNodeId) -> Option<Discriminant<NodeKind>> {
    pat.graph.node_weight(root).kind.discriminant()
}

pub struct Matcher<'f> {
    pub(crate) function: &'f Function,
    kind_index: OnceCell<KindIndex>,
}

/// Reachable `NodeKind` discriminant to node list, built on first query and
/// reused after.
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
    /// Performs no whole-graph validation: the orchestrator pipeline drives
    /// `validate::validate` separately, and in-place-editor integration tests
    /// deliberately work with partially-built fixtures.
    pub fn new(function: &'f Function) -> Self {
        Self {
            function,
            kind_index: OnceCell::new(),
        }
    }

    /// Single-threaded (`OnceCell`, not `OnceLock`).
    fn kind_index(&self) -> &KindIndex {
        self.kind_index
            .get_or_init(|| KindIndex::build(self.function))
    }

    /// The sole data-access point for match-time closures (`when_match`,
    /// `predicate`, `PostMatchFn`) that need to inspect IR side-tables.
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
    /// `Bindings::binding_signature`): `add(var(x), var(x))` swapped binds `x`
    /// identically and is ONE match, while `add(any().capture(k), any())` binds
    /// `k` to each operand in turn and is TWO. A pattern with no captures on
    /// commutative operands never duplicates. Ordering is deterministic:
    /// natural operand order before swapped.
    ///
    /// The root is resolved once per query, not per candidate.
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
    /// graph. `Either` keeps this static-dispatch, so neither arm allocates or
    /// pays a per-candidate virtual call.
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

    /// Attempt `pat` at `node`, iterating value outputs for value-producing
    /// nodes and falling back to a node-rooted attempt for zero-output kinds.
    ///
    /// `first_only` stops at the first match, which is what keeps
    /// [`Self::match_at`] cheap enough for the rewrite driver to call at every
    /// node. Otherwise every distinct match is enumerated, deduplicated by
    /// capture-to-binding map. The dedup set is per-node: two different roots
    /// are different matches whatever their bindings, while one node's outputs
    /// share the set so a pattern reachable through several outputs does not
    /// double-report an identical binding.
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
                walk::try_match_node(self, pat, root, node, &mut bindings, &mut collect);
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
        // Root-kind gate: reject a mismatched candidate before iterating
        // outputs, allocating a `Bindings` per output, and walking in only to
        // bail on the first kind check. This is what makes `match_at` cheap to
        // call at every node; `matches` gets the same prefilter from its
        // `KindIndex` bucket. A kind-`Any` root skips the gate.
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
    ///
    /// # Shared-capture requirement
    ///
    /// A capture-bearing pattern sharing no capture with the others is almost
    /// always a mis-wired correlation, and without a shared capture
    /// `prefix_agrees` approves every tuple, turning the join into an unbounded
    /// cartesian explosion. Such a pattern is rejected. A capture-free pattern
    /// (a pure filter like `call().at(0x1234).build()`) is exempt and degrades
    /// to a deliberate cross-product.
    ///
    /// # Deduplication
    ///
    /// Surviving tuples are deduplicated by their shared-capture binding
    /// signature resolved to nodes: two tuples agreeing on every shared capture
    /// but differing on an uncaptured or non-shared internal binding are
    /// indistinguishable to a correlated-site consumer, so only the first is
    /// kept.
    ///
    /// # Constraints
    ///
    /// `constraints` are post-correlation [`JoinConstraint`] filters over
    /// captured entities; a tuple survives iff it passes every one. Every
    /// capture a constraint mentions must be bound by some pattern in the join
    /// (range restriction); an unbound one is an error, not a silent
    /// drop-everything. A constraint whose captured node has no CFG position
    /// simply fails and drops its tuple, which is not an error. Pass `&[]` for
    /// an unconstrained join.
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
        // Constraints correlate too: patterns linked by a constraint whose
        // captures live one in each are connected without sharing a capture
        // (`guard` captures `t`, `call` captures `c`, joined by
        // `dominates(t, c)`).
        for con in constraints {
            // Range restriction, one rule rather than a negation carve-out.
            // An unbound capture makes a positive constraint fail for want of a
            // binding, silently dropping every tuple and returning an ambiguous
            // empty result; under `Not` the same failure flips to a vacuous
            // true and matches everything. Same authoring bug either way, so
            // reject it here. That also makes the vacuity impossible by
            // construction and lets the union below be total.
            let caps = con.captures();
            if let Some(c) = caps.iter().find(|c| !cap_owner.contains_key(c)) {
                anyhow::bail!(
                    "find_joined: constraint mentions capture {c:?}, which no pattern \
                     in the join binds — it could never be satisfied (and under \
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
                         with the others — a join correlates on shared captures (use a \
                         capture-free pattern for an intentional cross-product)"
                    );
                }
            }
        }

        // One `find_all` result per pattern: a list of INDEPENDENT matches, not
        // a joined row, so deliberately not a `JoinedMatch` despite the shape.
        let per_pat: Vec<Vec<Match>> = pats
            .iter()
            .map(|p| self.find_all(p))
            .collect::<anyhow::Result<_>>()?;
        if per_pat.iter().any(|hits| hits.is_empty()) {
            return Ok(Vec::new());
        }

        let mut acc: Vec<JoinedMatch> = per_pat[0].iter().cloned().map(|m| vec![m]).collect();

        // Cross-product with each subsequent pattern, filtering on
        // shared-capture agreement against the accumulated prefix.
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
            // A row survives iff every constraint returns `Some(true)`. Both
            // `Some(false)` and `None` (a capture unbound in this row, so the
            // relation is unanswerable) drop it.
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
/// [`Matcher::find_joined_constrained`] as a post-correlation filter.
///
/// Each captured entity resolves to a [`CtrlKey`] by WHAT IT BOUND (see
/// `ConstraintEval::ctrl_key_of`): a control-output capture (e.g. via
/// [`IfPat::capture_true`](crate::IfPat::capture_true)) becomes a
/// `CtrlKey::Edge`, anything else a `CtrlKey::Node` (a value's producer).
///
/// Every variant is a pure filter: it decides, it never binds.
#[derive(Debug, Clone)]
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
    /// Dispatch is dominator-first: node to node runs on the plain node
    /// dominator tree, any edge operand on the subsuming but slower-to-walk
    /// edge-split tree.
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
    /// input; see `ConstraintEval::phi_arms_from_edge` on why that is a
    /// zero-length path, not a special case) and an arm merged across
    /// intervening control such as a `Call` or a whole guarded loop.
    ///
    /// `edge` must bind a control-output value and `phi` a value. Bind `value`
    /// with [`PhiPat::any_input`](crate::PhiPat::any_input) on the same phi
    /// pattern where possible: that anchors it at the phi's own inputs
    /// (O(arity)) instead of letting it float as an independent whole-graph
    /// root.
    PhiInputFromEdge {
        phi: crate::Capture,
        edge: crate::Capture,
        value: crate::Capture,
    },
    /// A tuple survives iff `inner` does NOT hold on it.
    ///
    /// Two independent guards keep this sound:
    ///   * declared-ness (static): [`Matcher::find_joined_constrained`] rejects
    ///     a constraint mentioning a capture no pattern binds, which could
    ///     never be satisfied and under `Not` would match everything.
    ///   * bound-ness (per-tuple): evaluation is three-valued (see
    ///     `ConstraintEval::passes`). An unbound capture in THIS row makes
    ///     `inner` return `None`, and `Not(None) == None` drops the row rather
    ///     than producing the vacuous `true` of a two-valued `!false`.
    Not(Box<JoinConstraint>),
    /// Passes iff any listed constraint does. An empty list passes nothing.
    Or(Vec<JoinConstraint>),
    /// Passes iff every listed constraint does. An empty list passes
    /// everything. The top-level `constraints` slice is already an implicit
    /// `And`; this one nests inside an `Or`, where the flat slice cannot reach.
    And(Vec<JoinConstraint>),
}

impl JoinConstraint {
    /// Every capture this constraint correlates, used to link their owner
    /// patterns for the connectivity check.
    fn captures(&self) -> Vec<crate::Capture> {
        match self {
            JoinConstraint::Dominates {
                dominator,
                dominated,
            } => vec![*dominator, *dominated],
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => vec![*phi, *edge, *value],
            // A negation / connective correlates exactly the captures it
            // wraps; contributing them links the owner patterns and feeds the
            // range-restriction check.
            JoinConstraint::Not(inner) => inner.captures(),
            JoinConstraint::Or(cs) | JoinConstraint::And(cs) => {
                cs.iter().flat_map(JoinConstraint::captures).collect()
            }
        }
    }
}

/// One row of a join: one [`Match`] per pattern, in input order.
///
/// Named because [`Matcher::find_all`] returns a `Vec<Match>` meaning a list of
/// INDEPENDENT matches, whereas here a `Vec<Match>` is a single row. Same type,
/// opposite meanings.
pub type JoinedMatch = Vec<Match>;

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
///
/// # Why TWO trees, when one would do
///
/// The split tree subsumes the node tree: edge-splitting preserves paths 1:1,
/// so `dominates(split_doms, Node(a), Node(b))` equals `dominates(doms, a, b)`
/// exactly (`strider-ir`'s `split_dominance_subsumes_node_dominance` pins it).
/// `doms` is therefore deletable on correctness grounds and kept on measured
/// performance grounds alone.
///
/// Collapsing onto `split_doms` was benchmarked (`benches/matcher.rs`,
/// `join_dominates_only` / `join_dominates_and_branch` over a diamond chain):
///
/// ```text
///                        two trees   one tree
/// Dominates-only          4.18 ms     5.80 ms   +39%   (60 diamonds)
/// Dominates-only         33.6  µs    44.2  µs   +31%   ( 6 diamonds)
/// mixed (Dominates+edge)  8.06 ms     9.68 ms   +20%   (60 diamonds)
/// mixed (Dominates+edge) 52.4  µs    46.7  µs   -11%   ( 6 diamonds)
/// ```
///
/// What matters is the per-tuple chain walk, not the one-off build: `Edge`
/// vertices interleave, so a node-dominance chain in the split tree is ~2x
/// longer, and `Dominates` is re-evaluated per tuple (K guards by M calls).
/// Saving a build is a constant; doubling the walk scales with tuples * depth.
/// Hence the mixed shape, which saves a whole build, still wins at 6 diamonds
/// and loses at 60.
struct ConstraintEval<'f> {
    function: &'f Function,
    doms: OnceCell<petgraph::algo::dominators::Dominators<NodeId>>,
    /// Edge-split control subgraph. Lazy: a join with no edge constraint never
    /// builds it.
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

    /// Kept alongside [`Self::split_doms`] on measured grounds (see the type
    /// doc): node-to-node dominance walks it ~39% faster.
    fn doms(&self) -> &petgraph::algo::dominators::Dominators<NodeId> {
        self.doms.get_or_init(|| control_dominators(self.function))
    }

    fn split_doms(&self) -> &petgraph::algo::dominators::Dominators<CtrlKey> {
        self.split_doms
            .get_or_init(|| control_edge_dominators(self.function))
    }

    /// Resolve a capture to a [`CtrlKey`] by what it bound, which is the
    /// node-or-edge choice [`JoinConstraint::Dominates`] dispatches on:
    ///   1. a bound value of kind `Control` (an `If`'s
    ///      `capture_true`/`capture_false` edge) gives [`CtrlKey::Edge`];
    ///   2. any other bound value, [`CtrlKey::Node`] of its producer;
    ///   3. else a bound node, [`CtrlKey::Node`];
    ///   4. else `None`, feeding the three-valued [`Self::passes`].
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

    fn node_of(&self, tuple: &JoinedMatch, c: crate::Capture) -> Option<NodeId> {
        tuple.iter().find_map(|m| m.node(c, self.function.graph()))
    }

    fn value_of(&self, tuple: &JoinedMatch, c: crate::Capture) -> Option<ValueId> {
        tuple.iter().find_map(|m| m.value(c))
    }

    /// Three-valued verdict: `Some(b)` is real, `None` means a referenced
    /// capture was unbound in this row so the relation is unanswerable. The
    /// top-level fold keeps a row only on `Some(true)`, so an unbound capture
    /// never survives.
    ///
    /// This is what makes `Not` sound without the static range check carrying
    /// the whole burden: `Not(None) == None` drops the row instead of producing
    /// the vacuous `true` of a two-valued `!false`.
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
                // Short-circuits on the first qualifying arm.
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
                // operand routes to the subsuming `split_doms`. Both calls
                // typecheck because `dominates` is generic over
                // `Copy + Eq + Hash`.
                Some(match (key_a, key_b) {
                    (CtrlKey::Node(na), CtrlKey::Node(nb)) => dominates(self.doms(), na, nb),
                    (ka, kb) => dominates(self.split_doms(), ka, kb),
                })
            }
            // `Not(None) == None`: the negation of an unanswerable constraint
            // is itself unanswerable, so an unbound capture drops the row.
            JoinConstraint::Not(ref inner) => self.passes(inner, tuple).map(|b| !b),
            JoinConstraint::Or(ref cs) => kleene_or(cs.iter().map(|c| self.passes(c, tuple))),
            JoinConstraint::And(ref cs) => kleene_and(cs.iter().map(|c| self.passes(c, tuple))),
        }
    }

    /// Every value the `Phi`/`MemPhi` producing `phi_v` merges on a predecessor
    /// reached through branch edge `edge_v`. Backs
    /// [`JoinConstraint::PhiInputFromEdge`].
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
    /// edge-split graph this is plain dominance.
    ///
    /// The direct case is a zero-length path, not a special case: when
    /// `c_i == edge_v` it holds because `dominates(x, x)` is true, which is why
    /// there is no `||` with an `==` test. Do NOT rewrite this as edge against
    /// `producer(c_i)`: that producer is the `If`, which PRECEDES the edge, so
    /// the edge cannot dominate it and the direct case breaks. The direct-case
    /// tests are what catch that.
    ///
    /// The relation is exclusive, holding only where every path traverses
    /// `edge_v`, so an arm reachable from both sides of the branch belongs to
    /// neither. It needs no sole-entry gate: a guarded loop header has two
    /// predecessors (the guard's edge and its own latch), and edge dominance
    /// still reports correctly that every path into the loop traverses the
    /// guard's edge.
    ///
    /// Lazy and allocation-free; the caller `.any(..)`s and short-circuits.
    ///
    /// A branch whose block splits and reaches the join more than once yields
    /// one arm per qualifying predecessor, for the caller to enumerate. Under a
    /// direct-only `==` at most one could qualify; dominance can qualify
    /// several, and choosing among them would be a silent coin-flip.
    fn phi_arms_from_edge(
        &self,
        phi_v: ValueId,
        edge_v: ValueId,
    ) -> impl Iterator<Item = ValueId> + '_ {
        // Per-(phi, edge) work is resolved once here; the per-arm body below is
        // the single dominance clause.
        self.arm_scan(phi_v)
            .into_iter()
            .flat_map(move |(phi_inputs, region_inputs)| {
                region_inputs
                    .into_iter()
                    .enumerate()
                    .filter(move |(_, c)| {
                        // The split tree is memoised across the whole
                        // `find_joined_constrained` call.
                        dominates(self.split_doms(), CtrlKey::Edge(edge_v), CtrlKey::Edge(*c))
                    })
                    // Region control input `i` maps to phi data input `i + 1`.
                    .filter_map(move |(i, _)| phi_inputs.get(i + 1).copied())
            })
    }

    /// The phi's inputs and its region's control inputs. `Inputs` is a `Copy`
    /// borrow of the use-list, so carrying both views costs no allocation.
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

/// Deduplicate joined tuples by capture binding signature: every capture bound
/// anywhere in the tuple, resolved to its node and sorted. Equal signatures are
/// indistinguishable to a capture-consuming caller, so only the first is kept,
/// order-preserving.
///
/// An empty signature is always kept: that is the capture-free cross-product,
/// whose tuples are intentionally distinct despite binding nothing.
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
        // Pure cross-product tuple; never dedup.
        if sig.is_empty() {
            return true;
        }
        sig.sort_unstable_by_key(|(id, _)| *id);
        seen.insert(sig)
    });
}

/// Whether every capture shared between `m` and `prefix` agrees.
///
/// Agreement is at the resolved-NODE level, matching the join's contract, so
/// one IR node captured as `Value(v)` by one pattern and `Node(producer(v))` by
/// another still agrees; a raw `Binding`-variant compare would call them
/// different and silently drop a valid tuple. Two value captures are still
/// compared at value granularity, so distinct outputs of one multi-output node
/// do not falsely agree.
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
