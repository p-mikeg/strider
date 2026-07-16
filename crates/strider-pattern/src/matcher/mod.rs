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
    Function, Graph, IRViewer, IRWalker, control_dominators, dominates,
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
/// Caches a lazy `KindIndex` (built on first [`find_all`](Self::find_all) /
/// [`find_first`](Self::find_first) query) that buckets reachable IR nodes by
/// `NodeKind` discriminant. Subsequent queries with a
/// discriminant-rooted pattern iterate just the matching bucket
/// instead of walking every reachable node.
///
/// [`find_all`]: Self::find_all
/// [`find_first`]: Self::find_first
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

    /// Find every match for `pat` in the function.
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
    pub fn find_all(&self, pat: &Pattern) -> anyhow::Result<Vec<Match>> {
        let root = pat.root()?;
        Ok(self
            .candidates(pat, root)
            .filter_map(|node| self.try_match_at_node(node, pat, root))
            .collect())
    }

    /// Find the first match of `pat` in the function, or `Ok(None)` if
    /// `pat` doesn't match anywhere. Streamed variant of
    /// [`Self::find_all`] that stops at the first hit.
    ///
    /// # Errors
    /// Errors if `pat` is not a single-rooted, acyclic graph the matcher
    /// can handle (see [`Pattern::root`]).
    pub fn find_first(&self, pat: &Pattern) -> anyhow::Result<Option<Match>> {
        let root = pat.root()?;
        Ok(self
            .candidates(pat, root)
            .find_map(|node| self.try_match_at_node(node, pat, root)))
    }

    /// The IR nodes to attempt `pat` (resolved match `root`) at, shared by
    /// [`Self::find_all`] / [`Self::find_first`]: a discriminant-rooted pattern
    /// scans only its matching `KindIndex` bucket (O(M) in nodes of that kind),
    /// a kind-`Any` root the whole reachable graph.  Static-dispatch `Either`,
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
    /// at `node`, returning the first successful match if any (iterates
    /// value outputs for value-producing nodes, falls back to a node-rooted
    /// attempt for zero-output kinds). Shared between [`Self::find_first`]
    /// and [`Self::match_at`].
    fn try_match_at_node(&self, node: NodeId, pat: &Pattern, root: PatNodeId) -> Option<Match> {
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::default();
            if walk::try_match_node(self, pat, root, node, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
            return None;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::default();
            if walk::try_match(self, pat, root, out_id, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
        }
        None
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
        // driver's usage); `find_all` gets the same prefilter from its
        // `KindIndex` bucket.  A kind-`Any` root has no discriminant and skips
        // the gate.
        if let Some(rk) = root_kind_discriminant(pat, root)
            && std::mem::discriminant(self.function.node_kind(node)) != rk
        {
            return Ok(None);
        }
        Ok(self.try_match_at_node(node, pat, root))
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
        constraints: &[JoinConstraint],
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
            let mut eval = ConstraintEval::new(self.function());
            acc.retain(|tuple| constraints.iter().all(|c| eval.holds(c, tuple)));
        }

        dedup_on_shared_captures(&mut acc, self.function().graph());
        Ok(acc)
    }

}

/// A CFG relation between two captured entities, applied by
/// [`Matcher::find_joined_constrained`] as a post-correlation filter over
/// joined tuples. Captured entities are resolved to control nodes; a value
/// capture resolves to its producer node (for `Dominates`) or is used directly
/// (the branch-edge value for `Reaches`).
#[derive(Clone, Copy, Debug)]
pub enum JoinConstraint {
    /// The node bound to `a` dominates the node bound to `b` in the control
    /// subgraph. A capture absent from the control subgraph fails it.
    Dominates { a: crate::Capture, b: crate::Capture },
    /// `node` is dominated by the consumer of the branch edge `branch` — i.e.
    /// `node` sits in the block that edge leads into, *exclusively*: true only
    /// where every path to `node` traverses the edge's target, so a single
    /// `dominated_by_branch(true_edge, c)` expresses "`c` is in the true block".
    /// `branch` must bind a control-output value (e.g. via
    /// [`IfPat::capture_true`](crate::IfPat::capture_true)); an ill-typed
    /// `branch` or an absent node fails it.
    DominatedByBranch { branch: crate::Capture, node: crate::Capture },
    /// The `Phi`/`MemPhi` bound to `phi` merges, on the predecessor whose owning
    /// `Region` control input equals the branch edge `edge`, the value bound to
    /// `value`.  Ties a phi's per-branch data input to the control edge that
    /// leads into that predecessor, so a pattern can say "the value merged from
    /// THIS branch is X" without a slot index.  Works for a value `Phi` (`value`
    /// binds the merged value) and a `MemPhi` (`value` binds the merged memory
    /// token).  Direct-edge: `edge` must be a *literal* control input of the
    /// phi's region (the converged/collapsed IR makes an `If`'s true/false
    /// output the join region's direct predecessor).  `edge` must bind a
    /// control-output value; `phi` / `value` bind values.
    PhiInputFromEdge {
        phi: crate::Capture,
        edge: crate::Capture,
        value: crate::Capture,
    },
}

impl JoinConstraint {
    /// Every capture this constraint correlates (used to link their owner
    /// patterns for the `find_joined` connectivity check).
    fn captures(&self) -> Vec<crate::Capture> {
        match *self {
            JoinConstraint::Dominates { a, b } => vec![a, b],
            JoinConstraint::DominatedByBranch { branch, node } => vec![branch, node],
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => {
                vec![phi, edge, value]
            }
        }
    }
}

/// Evaluates [`JoinConstraint`]s against joined tuples, memoising the control
/// dominators and per-branch-edge reachable sets across one
/// `find_joined_constrained` call.
struct ConstraintEval<'f> {
    function: &'f Function,
    doms: OnceCell<petgraph::algo::dominators::Dominators<NodeId>>,
}

impl<'f> ConstraintEval<'f> {
    fn new(function: &'f Function) -> Self {
        Self {
            function,
            doms: OnceCell::new(),
        }
    }

    /// Resolve a capture to a control node across the tuple's matches.
    fn node_of(&self, tuple: &[Match], c: crate::Capture) -> Option<NodeId> {
        tuple.iter().find_map(|m| m.node(c, self.function.graph()))
    }

    /// Resolve a capture to the value it binds across the tuple's matches.
    fn value_of(&self, tuple: &[Match], c: crate::Capture) -> Option<ValueId> {
        tuple.iter().find_map(|m| m.value(c))
    }

    fn holds(&mut self, c: &JoinConstraint, tuple: &[Match]) -> bool {
        match *c {
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
                // The branch edge's consumer (its target node); a control edge
                // has exactly one consumer in well-formed IR.
                let Some((consumer, _)) = self.function.graph().value_uses(edge).next() else {
                    return false;
                };
                let doms = self.doms.get_or_init(|| control_dominators(self.function));
                dominates(doms, consumer, target)
            }
            JoinConstraint::PhiInputFromEdge { phi, edge, value } => {
                let (Some(phi_v), Some(edge_v), Some(val_v)) = (
                    self.value_of(tuple, phi),
                    self.value_of(tuple, edge),
                    self.value_of(tuple, value),
                ) else {
                    return false;
                };
                self.phi_input_from_edge(phi_v, edge_v, val_v)
            }
        }
    }

    /// The `Phi`/`MemPhi` producing `phi_v` merges `val_v` on the predecessor
    /// whose owning `Region` control input is exactly `edge_v` (direct-edge).
    ///
    /// Slot alignment: a `Phi`/`MemPhi`'s inputs are `[PhiToken, v0, v1, …]` —
    /// data input `i+1` is predecessor `i`'s value (a `Memory` token for
    /// `MemPhi`) — and its owning `Region` (the `PhiToken`'s producer) has
    /// control input `i` for predecessor `i`.  So the region slot matching
    /// `edge_v` maps to the phi input one slot over.
    fn phi_input_from_edge(&self, phi_v: ValueId, edge_v: ValueId, val_v: ValueId) -> bool {
        let f = self.function;
        let phi_node = f.producer(phi_v);
        if !matches!(f.node_kind(phi_node), NodeKind::Phi | NodeKind::MemPhi) {
            return false;
        }
        let inputs: Vec<ValueId> = f.node_inputs(phi_node).into_iter().collect();
        // Slot 0 is the PhiToken; its producer is the owning Region.
        let Some(&token) = inputs.first() else {
            return false;
        };
        let region = f.producer(token);
        if !matches!(f.node_kind(region), NodeKind::Region) {
            return false;
        }
        let Some(slot) = f
            .node_inputs(region)
            .into_iter()
            .position(|c| c == edge_v)
        else {
            return false;
        };
        inputs.get(slot + 1).is_some_and(|&v| v == val_v)
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
