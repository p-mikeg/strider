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
pub(crate) mod builder;
pub(crate) mod graph;
pub(crate) mod match_pat;
pub(crate) mod vertex;
pub(crate) mod walk;

pub(crate) use cast_walk_through::skip_casts;
pub use builder::{MatcherBuilder, PatNodeRef, PatValueRef};
pub use graph::Pattern;
pub use strider_ir::walk::CastMask;
pub use vertex::{
    KindSpec, NodePredicate, OutputKindSpec, PatNode, PatValue, PostMatchFn, ValuePredicate,
};

use std::cell::OnceCell;
use std::mem::Discriminant;

use rustc_hash::FxHashMap;
use strider_graph::NodeId as PatNodeId;
use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind};

use crate::bindings::Bindings;
use crate::graph_ext::PatGraphRead;
use crate::match_result::Match;

/// Discriminant of the pat node at `root`, used by the `find_*` dispatch
/// to pre-filter IR nodes by kind. Returns `None` for a kind-`Any` root
/// (then the matcher scans every reachable node). `root` is the
/// already-resolved match root (see [`Pattern::root`]).
fn root_kind_discriminant(pat: &Pattern, root: PatNodeId) -> Option<Discriminant<NodeKind>> {
    pat.graph.node_weight(root).kind.discriminant()
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
    #[allow(clippy::expect_used)]
    pub fn entry(&self) -> NodeId {
        self.function
            .entry()
            .expect("Matcher wraps a built function with an entry node (try_new invariant)")
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
        let mut out = Vec::new();
        match root_kind_discriminant(pat, root) {
            Some(d) => {
                for &node in self.kind_index().nodes_of_kind(d) {
                    self.try_at_node(node, pat, root, &mut out);
                }
            }
            None => {
                for node in self.function.walk() {
                    self.try_at_node(node, pat, root, &mut out);
                }
            }
        }
        Ok(out)
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
        match root_kind_discriminant(pat, root) {
            Some(d) => {
                for &node in self.kind_index().nodes_of_kind(d) {
                    if let Some(m) = self.try_match_at_node(node, pat, root) {
                        return Ok(Some(m));
                    }
                }
            }
            None => {
                for node in self.function.walk() {
                    if let Some(m) = self.try_match_at_node(node, pat, root) {
                        return Ok(Some(m));
                    }
                }
            }
        }
        Ok(None)
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

    /// Run several patterns independently against the function and
    /// return the per-pattern matches. The outer index corresponds to
    /// the input pattern index; the inner Vec is that pattern's match
    /// list (same shape as [`Self::find_all`]).
    ///
    /// Unlike [`Self::find_joined`], this does NOT filter on shared-
    /// capture agreement — each pattern's matches stand alone.
    ///
    /// Reserved for a future batched multi-pattern query path; not yet
    /// wired into a public caller, so kept crate-internal.
    ///
    /// # Errors
    /// Errors if any pattern is not a single-rooted, acyclic graph.
    #[allow(dead_code)]
    pub(crate) fn find_all_multi(&self, pats: &[&Pattern]) -> anyhow::Result<Vec<Vec<Match>>> {
        pats.iter().map(|p| self.find_all(p)).collect()
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
        Ok(self.try_match_at_node(node, pat, root))
    }

    fn try_at_node(&self, node: NodeId, pat: &Pattern, root: PatNodeId, out: &mut Vec<Match>) {
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::default();
            if walk::try_match_node(self, pat, root, node, &mut bindings) {
                out.push(Match::from_root(node, bindings));
            }
            return;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::default();
            if walk::try_match(self, pat, root, out_id, &mut bindings) {
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
    ///
    /// # Errors
    /// Errors if any pattern is not a single-rooted, acyclic graph the
    /// matcher can handle (see [`Pattern::root`]).
    pub fn find_joined(&self, pats: &[&Pattern]) -> anyhow::Result<Vec<Vec<Match>>> {
        if pats.is_empty() {
            return Ok(Vec::new());
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
        Ok(acc)
    }

    /// Returns a [`FunctionArgHandle`] for the first carrier node
    /// registered at side-table index `index`, or `None` if no such
    /// carrier exists.
    pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'f>> {
        let value = *self.function.arg_index_to_values(index).first()?;
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
        let mut indices: Vec<u32> = f.iter_arg_indices().collect();
        indices.sort_unstable();
        indices.into_iter().filter_map(move |i| {
            f.arg_index_to_values(i)
                .first()
                .copied()
                .map(|value| (i, FunctionArgHandle { function: f, node: f.producer(value) }))
        })
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

#[cfg(test)]
mod find_all_multi_tests {
    use super::Matcher;
    use crate::{Capture, CaptureExt, Match, MatchPat, add, any, any_int_const, load};
    use strider_ir::IRBuilderExt;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::RegisterSet;

    fn fresh_fb() -> strider_ir::FunctionBuilder {
        RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region")
    }

    /// `ret(add(a, b))` over two `I64` constants.
    fn add_consts(a: u64, b: u64) -> strider_ir::Function {
        let mut fb = fresh_fb();
        let la = fb.build_int_const(a, ValueType::I64).unwrap();
        let lb = fb.build_int_const(b, ValueType::I64).unwrap();
        let s = fb
            .build_int_binary_operation(la, lb, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        fb.build_return(Some(s), &[]).unwrap();
        fb.build().unwrap()
    }

    /// `ret(add(add(a, b), c))` — two nested adds over three `I64` consts.
    fn add_nested_3(a: u64, b: u64, c: u64) -> strider_ir::Function {
        let mut fb = fresh_fb();
        let la = fb.build_int_const(a, ValueType::I64).unwrap();
        let lb = fb.build_int_const(b, ValueType::I64).unwrap();
        let lc = fb.build_int_const(c, ValueType::I64).unwrap();
        let s = fb
            .build_int_binary_operation(la, lb, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let s = fb
            .build_int_binary_operation(s, lc, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        fb.build_return(Some(s), &[]).unwrap();
        fb.build().unwrap()
    }

    #[test]
    fn find_all_multi_matches_sequential_find_all() {
        let function = add_nested_3(5, 7, 11);
        let m = Matcher::try_new(&function).unwrap();

        let p_add = add(any(), any()).into_pattern();
        let p_const = any_int_const().capture(Capture::new()).into_pattern();
        let p_load = load().build();

        let multi = m.find_all_multi(&[&p_add, &p_const, &p_load]).unwrap();

        let seq_add = m.find_all(&p_add).unwrap();
        let seq_const = m.find_all(&p_const).unwrap();
        let seq_load = m.find_all(&p_load).unwrap();

        let roots = |hits: &[Match]| hits.iter().map(|h| h.root()).collect::<Vec<_>>();
        assert_eq!(roots(&multi[0]), roots(&seq_add));
        assert_eq!(roots(&multi[1]), roots(&seq_const));
        assert_eq!(roots(&multi[2]), roots(&seq_load));
    }

    #[test]
    fn find_all_multi_empty_input() {
        let function = add_consts(2, 3);
        let m = Matcher::try_new(&function).unwrap();
        let results = m.find_all_multi(&[]).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn find_all_multi_all_wildcards() {
        let function = add_consts(1, 2);
        let m = Matcher::try_new(&function).unwrap();
        let p1 = any().into_pattern();
        let p2 = any().into_pattern();
        let multi = m.find_all_multi(&[&p1, &p2]).unwrap();
        assert_eq!(multi[0].len(), m.find_all(&p1).unwrap().len());
        assert_eq!(multi[1].len(), m.find_all(&p2).unwrap().len());
    }

    #[test]
    fn find_all_multi_mixed_concrete_and_wildcard() {
        let function = add_nested_3(2, 3, 5);
        let m = Matcher::try_new(&function).unwrap();
        let p_add = add(any(), any()).into_pattern();
        let p_wild = any().into_pattern();
        let multi = m.find_all_multi(&[&p_add, &p_wild]).unwrap();
        let roots = |hits: &[Match]| hits.iter().map(|h| h.root()).collect::<Vec<_>>();
        assert_eq!(roots(&multi[0]), roots(&m.find_all(&p_add).unwrap()));
        assert_eq!(roots(&multi[1]), roots(&m.find_all(&p_wild).unwrap()));
    }
}
