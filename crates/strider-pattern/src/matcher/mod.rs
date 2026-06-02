//! Pattern matcher over a lifted [`strider_ir::Function`].
//!
//! [`Matcher`] owns no per-match state; [`try_new`](Matcher::try_new)
//! validates the function's post-build invariant once up front and
//! caches a lazy `KindIndex` (built on first query) bucketing
//! reachable IR nodes by `NodeKind` discriminant. A discriminant-rooted
//! pattern iterates just the matching bucket; a kind-`Any` root falls
//! back to a full reachable walk.
//!
//! The recursive match engine lives in `walk`; the cast walk-through
//! helper in `cast_walk_through`. The cast mask is carried on the
//! [`Pattern`] itself, not on the matcher.

mod cast_walk_through;
pub(crate) mod walk;

pub(crate) use cast_walk_through::skip_casts;
pub use strider_ir::walk::CastMask;

use std::cell::OnceCell;
use std::mem::Discriminant;

use rustc_hash::FxHashMap;
use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind};

use crate::bindings::Bindings;
use crate::match_result::Match;
use crate::pattern::Pattern;

/// Discriminant of `pat`'s root node kind, used by the `find_*`
/// dispatch to pre-filter IR nodes by kind. Returns `None` for a
/// kind-`Any` root (then the matcher scans every reachable node).
#[must_use]
pub fn root_kind_discriminant(pat: &Pattern) -> Option<Discriminant<NodeKind>> {
    let root = pat.root()?;
    pat.graph.node_weight(root)?.kind.discriminant()
}

/// Top-level matcher. Owns no per-match state; [`try_new`](Self::try_new)
/// validates the function once up-front.
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
    /// Validate the post-build invariant (`function.entry()` is set) and
    /// return a matcher bound to the function.
    ///
    /// Only checks the entry-node post-build invariant up front, not
    /// whole-graph validation — that's left to callers (the orchestrator
    /// pipeline drives `validate::validate` separately and integration
    /// tests for in-place editors deliberately work with partially-built
    /// fixtures).
    ///
    /// # Errors
    /// Returns an error if `function` has no entry node.
    pub fn try_new(function: &'f Function) -> anyhow::Result<Self> {
        let _entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("Function has no entry"))?;
        Ok(Self {
            function,
            kind_index: OnceCell::new(),
        })
    }

    /// Lazily build (or return the cached) `KindIndex` for the wrapped
    /// function. Single-threaded (`OnceCell`, not `OnceLock`).
    fn kind_index(&self) -> &KindIndex {
        self.kind_index
            .get_or_init(|| KindIndex::build(self.function))
    }

    /// Function-entry [`NodeId`] of the wrapped function.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn entry(&self) -> NodeId {
        self.function
            .entry()
            .expect("Matcher wraps a built function with an entry node (try_new invariant)")
    }

    /// Borrow of the [`Function`] this matcher operates over. The sole
    /// data-access point for closures (`when_match`, `predicate`,
    /// `PostMatchFn`) that need to inspect IR side-tables at match time.
    #[must_use]
    pub fn function(&self) -> &Function {
        self.function
    }

    /// Find every match for `pat` in the function.
    ///
    /// When `root_kind_discriminant` returns `Some(d)`, the lazy
    /// `KindIndex` is built on demand (cached for subsequent queries)
    /// and only the nodes in the matching bucket are tried — O(M) where
    /// M is the count of nodes with that kind, instead of O(N) over the
    /// full reachable walk. Kind-`Any` roots fall back to a full walk.
    pub fn find_all(&self, pat: &Pattern) -> Vec<Match> {
        let mut out = Vec::new();
        match root_kind_discriminant(pat) {
            Some(d) => {
                for &node in self.kind_index().nodes_of_kind(d) {
                    self.try_at_node(node, pat, &mut out);
                }
            }
            None => {
                for node in self.function.walk() {
                    self.try_at_node(node, pat, &mut out);
                }
            }
        }
        out
    }

    /// Find the first match of `pat` in the function, or `None` if
    /// `pat` doesn't match anywhere. Streamed variant of
    /// [`Self::find_all`] that stops at the first hit. Consults the
    /// same lazy `KindIndex` for discriminant-rooted patterns.
    pub fn find_first(&self, pat: &Pattern) -> Option<Match> {
        match root_kind_discriminant(pat) {
            Some(d) => {
                for &node in self.kind_index().nodes_of_kind(d) {
                    if let Some(m) = self.try_match_at_node(node, pat) {
                        return Some(m);
                    }
                }
                None
            }
            None => {
                for node in self.function.walk() {
                    if let Some(m) = self.try_match_at_node(node, pat) {
                        return Some(m);
                    }
                }
                None
            }
        }
    }

    /// Internal helper: attempt `pat` at `node`, returning the first
    /// successful match if any (iterates value outputs for
    /// value-producing nodes, falls back to a node-rooted attempt for
    /// zero-output kinds). Shared between [`Self::find_first`] and
    /// [`Self::match_at`].
    fn try_match_at_node(&self, node: NodeId, pat: &Pattern) -> Option<Match> {
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::default();
            if walk::try_match_node(self, pat, node, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
            return None;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::default();
            if walk::try_match(self, pat, out_id, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
        }
        None
    }

    /// Run several patterns independently against the function and
    /// return the per-pattern matches. The outer index corresponds to
    /// the input pattern index; the inner Vec is that pattern's match
    /// list (same shape as [`Self::find_all`]).
    ///
    /// Unlike [`Self::find_joined`], this does NOT filter on shared-
    /// capture agreement — each pattern's matches stand alone.
    pub fn find_all_multi(&self, pats: &[&Pattern]) -> Vec<Vec<Match>> {
        pats.iter().map(|p| self.find_all(p)).collect()
    }

    /// Try `pat` at a specific IR node; returns the first match if any
    /// (iterating outputs for value-producing nodes; node-rooted for
    /// zero-output kinds).
    pub fn match_at(&self, node: NodeId, pat: &Pattern) -> Option<Match> {
        self.try_match_at_node(node, pat)
    }

    fn try_at_node(&self, node: NodeId, pat: &Pattern, out: &mut Vec<Match>) {
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::default();
            if walk::try_match_node(self, pat, node, &mut bindings) {
                out.push(Match::from_root(node, bindings));
            }
            return;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::default();
            if walk::try_match(self, pat, out_id, &mut bindings) {
                out.push(Match::from_root(node, bindings));
                break;
            }
        }
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
    pub fn find_joined(&self, pats: &[&Pattern]) -> Vec<Vec<Match>> {
        if pats.is_empty() {
            return Vec::new();
        }
        let per_pat: Vec<Vec<Match>> = pats.iter().map(|p| self.find_all(p)).collect();
        if per_pat.iter().any(|hits| hits.is_empty()) {
            return Vec::new();
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
                    if prefix_agrees(prefix, m) {
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
        acc
    }

    /// Returns a [`FunctionArgHandle`] for the first carrier node
    /// registered at side-table index `index`, or `None` if no such
    /// carrier exists.
    #[must_use]
    pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'f>> {
        let node = *self.function.arg_index_to_nodes(index).first()?;
        Some(FunctionArgHandle {
            function: self.function,
            node,
        })
    }

    /// Iterate `(index, handle)` for every registered function-arg
    /// carrier in side-table-index order.
    pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'f>)> + '_ {
        let f = self.function;
        let mut indices: Vec<u32> = f.iter_arg_indices().collect();
        indices.sort_unstable();
        indices.into_iter().filter_map(move |i| {
            f.arg_index_to_nodes(i)
                .first()
                .copied()
                .map(|node| (i, FunctionArgHandle { function: f, node }))
        })
    }

    /// Smallest `idx + 1` such that no `idx' >= idx + 1` has a
    /// registered carrier. Equivalent to `max(registered idx) + 1`,
    /// or `0` if no carriers are registered.
    #[must_use]
    pub fn function_arg_index_upper_bound(&self) -> usize {
        self.function
            .iter_arg_indices()
            .max()
            .map_or(0, |m| (m as usize) + 1)
    }

    /// Count of registered function-arg carriers.
    #[must_use]
    pub fn function_arg_count(&self) -> usize {
        self.function.iter_arg_indices().count()
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
/// `Function::arg_index_to_nodes`. Returned by
/// [`Matcher::function_arg`] / [`Matcher::function_args`].
#[derive(Clone, Copy)]
pub struct FunctionArgHandle<'g> {
    function: &'g Function,
    node: NodeId,
}

impl FunctionArgHandle<'_> {
    /// Carrier [`NodeId`].
    #[must_use]
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// Classify the carrier's source (register vs stack vs other).
    #[must_use]
    pub fn source(&self) -> ArgSource {
        match self.function.node_kind(self.node) {
            NodeKind::InitialVar(vn) => ArgSource::Register(*vn),
            NodeKind::Load(_) => ArgSource::Stack,
            _ => ArgSource::Other,
        }
    }
}

/// True when every capture in `m`'s bindings that also appears in any
/// previously-collected match in `prefix` binds to the same value.
fn prefix_agrees(prefix: &[Match], m: &Match) -> bool {
    for prev in prefix {
        for (cap, prev_binding) in prev.bindings.iter() {
            if let Some(m_binding) = m.bindings.get_binding(cap)
                && prev_binding != m_binding
            {
                return false;
            }
        }
    }
    true
}
