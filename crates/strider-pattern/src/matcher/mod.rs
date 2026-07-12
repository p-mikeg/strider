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

use itertools::{Either, Itertools};
use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::NodeId as PatNodeId;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{
    Function, Graph, IRViewer, IRWalker, control_dominators, control_reachable_from, dominates,
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
    /// # Errors
    /// Errors if any pattern is not a single-rooted, acyclic graph the
    /// matcher can handle (see [`Pattern::root`]), or if a capture-bearing
    /// pattern shares no capture with the patterns before it.
    pub fn find_joined(&self, pats: &[&Pattern]) -> anyhow::Result<Vec<Vec<Match>>> {
        self.find_joined_constrained(pats, &[])
    }

    /// Like [`find_joined`](Self::find_joined), but additionally filters the
    /// joined tuples by CFG [`JoinConstraint`]s (control dominance / forward
    /// control reachability) over captured entities. Each constraint is a
    /// **post-correlation** predicate: a tuple survives iff every constraint
    /// holds on the entities its captures bind. A constraint referencing a
    /// capture no tuple binds, or one whose captured node has no CFG position,
    /// simply fails (the tuple is dropped) — never an error.
    ///
    /// # Errors
    /// Same as [`find_joined`](Self::find_joined): a malformed pattern, or a
    /// capture-bearing pattern connected to the rest by neither a shared capture
    /// nor a constraint.
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
        // captures `c`, joined by `reaches(t, c)`). Union their owners so the
        // connectivity check accepts a constraint-correlated join.
        for con in constraints {
            let (x, y) = con.captures();
            if let (Some(&i), Some(&j)) = (cap_owner.get(&x), cap_owner.get(&y)) {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[ri] = rj;
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

    /// Returns a [`FunctionArgHandle`] for the first carrier node
    /// registered at side-table index `index`, or `None` if no such
    /// carrier exists.
    pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'f>> {
        let value = *self
            .function
            .side_tables()
            .arg_index_to_values(index)
            .first()?;
        let node = self.function.producer(value);
        Some(FunctionArgHandle {
            function: self.function,
            node,
        })
    }

    /// Iterate `(index, handle)` for every registered function-arg
    /// carrier in side-table-index order.
    pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'f>)> + '_ {
        let f = self.function;
        f.side_tables()
            .iter_arg_indices()
            .sorted_unstable()
            .filter_map(move |i| {
                f.side_tables()
                    .arg_index_to_values(i)
                    .first()
                    .copied()
                    .map(|value| {
                        (
                            i,
                            FunctionArgHandle {
                                function: f,
                                node: f.producer(value),
                            },
                        )
                    })
            })
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
    /// The node bound to `to` is forward-control-reachable from the branch edge
    /// bound to `from`. `from` must bind a control-output *value* (e.g. via
    /// [`IfPat::capture_true`](crate::IfPat::capture_true)); reachability starts
    /// at that value's consumer. A non-value `from` fails it.
    Reaches { from: crate::Capture, to: crate::Capture },
    /// The logical negation of [`Reaches`](Self::Reaches). An ill-typed `from`
    /// makes `Reaches` false, hence `NotReaches` vacuously true.
    NotReaches { from: crate::Capture, to: crate::Capture },
}

impl JoinConstraint {
    /// The two captures this constraint correlates (used to link their owner
    /// patterns for the `find_joined` connectivity check).
    fn captures(&self) -> (crate::Capture, crate::Capture) {
        match *self {
            JoinConstraint::Dominates { a, b } => (a, b),
            JoinConstraint::Reaches { from, to } | JoinConstraint::NotReaches { from, to } => {
                (from, to)
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
    reach: FxHashMap<ValueId, FxHashSet<NodeId>>,
}

impl<'f> ConstraintEval<'f> {
    fn new(function: &'f Function) -> Self {
        Self {
            function,
            doms: OnceCell::new(),
            reach: FxHashMap::default(),
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

    /// `to`'s node is forward-control-reachable from the branch edge `from`.
    fn reaches(&mut self, tuple: &[Match], from: crate::Capture, to: crate::Capture) -> bool {
        let (Some(edge), Some(target)) = (self.value_of(tuple, from), self.node_of(tuple, to))
        else {
            return false;
        };
        let function = self.function;
        let set = self.reach.entry(edge).or_insert_with(|| {
            // BFS from every consumer of the branch-edge value (exactly one in
            // well-formed IR; union anyway to stay robust to forks).
            let mut acc = FxHashSet::default();
            for (consumer, _) in function.graph().value_uses(edge) {
                acc.extend(control_reachable_from(function, consumer));
            }
            acc
        });
        set.contains(&target)
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
            JoinConstraint::Reaches { from, to } => self.reaches(tuple, from, to),
            JoinConstraint::NotReaches { from, to } => !self.reaches(tuple, from, to),
        }
    }
}

/// Returned by [`FunctionArgHandle::source`] when the caller needs to
/// distinguish register- vs stack-passed args.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgSource {
    /// Register-passed arg: carrier is an `InitialVar(vn)`.
    Register(rsleigh::Vn),
    /// Stack-passed arg: carrier is a `Load` node.
    Stack,
    /// Other kinds (defensive — should not occur in well-formed IR).
    Other,
}

/// Handle to a single function-arg carrier registered in
/// `Function::arg_index_to_values`. Returned by
/// [`Matcher::function_arg`] / [`Matcher::function_args`].
#[derive(Clone, Copy)]
pub struct FunctionArgHandle<'g> {
    function: &'g Function,
    node: NodeId,
}

impl FunctionArgHandle<'_> {
    /// Carrier [`NodeId`].
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// Classify the carrier's source (register vs stack vs other).
    pub fn source(&self) -> ArgSource {
        match self.function.node_kind(self.node) {
            NodeKind::InitialVar(id) => ArgSource::Register(self.function.initial_vn(*id)),
            NodeKind::Load(_) => ArgSource::Stack,
            _ => ArgSource::Other,
        }
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
