// `dead_code` allow: the `PatGraph` `Pattern` impl that exercises this
// trait + engine lands in a subsequent task.  Until then, `Matcher`
// constructors, `find_all`, `match_at`, and the `PatternExt` blanket
// have no call sites in this crate and clippy --release runs with
// `-D warnings`.  Module-level allow keeps the build green; the items
// themselves are `pub` for the upcoming consumers.
#![allow(dead_code)]

//! Pattern matcher.

mod cast_walk_through;
mod ctx;
mod try_match;

pub use ctx::{TemplateCtx, MatchCtx};
pub(crate) use cast_walk_through::skip_casts;
pub use strider_ir::walk::CastMask;

use std::mem::Discriminant;

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::bindings::Bindings;
use crate::match_result::Match;

/// LHS of a rewrite or query.  A `Pattern` can be matched against an
/// IR node-output to attempt to bind captures.
pub trait Pattern {
    /// Try the pattern against `root_out`.  On success returns `true`
    /// with any captures recorded in `bindings`; on failure returns
    /// `false` (caller is responsible for restoring bindings to a
    /// pre-attempt mark if needed).
    fn try_match(
        &self,
        ctx: &MatchCtx,
        root_out: NodeOutputId,
        bindings: &mut Bindings,
    ) -> bool;

    /// Discriminant of the root node's `NodeKind`, used by `find_all`
    /// to pre-filter IR nodes by kind.  Returns `None` for kind-`Any`
    /// roots (then `find_all` scans everything).
    fn root_kind_discriminant(&self) -> Option<Discriminant<NodeKind>>;

    /// Try the pattern against a node with **no** value outputs.  Used
    /// by zero-output kinds (`Return`, `If`, `IndirectBranch` — though
    /// `If` already has Control outputs that the matcher iterates).
    /// The default impl returns `false`; concrete `Pattern` impls
    /// (notably `PatGraph`) override this to dispatch into their
    /// recursive walker with `root_out = None`.
    fn try_match_node(
        &self,
        _ctx: &MatchCtx,
        _node: NodeId,
        _bindings: &mut Bindings,
    ) -> bool {
        false
    }
}

/// Default extension: match against a node directly (used for
/// zero-output kinds like `Return`).  Implemented for every `Pattern`
/// via a blanket impl.
pub trait PatternExt {
    fn try_match_node_id(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        bindings: &mut Bindings,
    ) -> bool;
}

impl<T: Pattern + ?Sized> PatternExt for T {
    fn try_match_node_id(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        bindings: &mut Bindings,
    ) -> bool {
        let outputs = ctx.function.node_outputs(node);
        if outputs.is_empty() {
            // Zero-output kinds (e.g. `Return`) — dispatch through the
            // `try_match_node` hook, which `Pattern` impls can override
            // to match without a `NodeOutputId`.
            return self.try_match_node(ctx, node, bindings);
        }
        for &out in outputs {
            let mark = bindings.mark();
            if self.try_match(ctx, out, bindings) {
                return true;
            }
            bindings.restore(mark);
        }
        false
    }
}

/// Builder-state options threaded into every match attempt.  Today
/// carries only the cast walk-through bitset; future modes (Region
/// walk-through, etc.) would extend this struct.
#[derive(Clone, Copy, Default)]
pub struct MatcherOptions {
    /// Bitset selecting which value-passthrough cast `NodeKind`s the
    /// matcher transparently traverses on a producer kind-mismatch.
    /// `CastMask::empty()` (the default) is strict — no walk-through.
    pub cast_mask: CastMask,
    /// Walk through `Region` (region-join) nodes when traversing
    /// control chains.  Lets `ret(call(...))` cross region joins
    /// between the Return and the Call.  Off by default.
    ///
    /// **Note:** the new matcher does not yet honour this flag at the
    /// walk site — it is accepted for API compatibility with the
    /// strider-analyze matcher during the migration but currently has
    /// no effect on Region traversal.  Callers needing semantic Region
    /// walk-through must reshape their patterns.
    pub ignore_regions: bool,
}

/// Top-level matcher.  Owns no per-match state; `try_new` validates
/// the function once up-front (matching the existing
/// `strider-analyze::pattern::Matcher` contract).
pub struct Matcher<'f> {
    pub(crate) function: &'f Function,
    pub(crate) options: MatcherOptions,
}

impl<'f> Matcher<'f> {
    /// Validate the post-build invariant (`function.entry()` is set) and
    /// return a matcher bound to the function.  Mirrors the looser
    /// `strider-analyze::pattern::Matcher::try_new` contract: only checks
    /// the entry-node post-build invariant up front, not whole-graph
    /// validation — that's left to callers (the orchestrator pipeline
    /// drives `validate::validate` separately and integration tests for
    /// in-place editors deliberately work with partially-built fixtures).
    ///
    /// # Errors
    /// Returns an error if `function` has no entry node.
    pub fn try_new(function: &'f Function) -> anyhow::Result<Self> {
        let _entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("Function has no entry"))?;
        Ok(Self {
            function,
            options: MatcherOptions::default(),
        })
    }

    /// Extend the cast walk-through bitset.  When `mask` is non-empty,
    /// a kind-mismatch on a sub-pattern producer triggers a transparent
    /// unwrap of any cast in `mask` (e.g. `CastMask::ZERO_EXTEND`),
    /// re-attempting the sub-pattern against the cast's value input.
    ///
    /// Calls are OR-cumulative: `.ignore_casts_mask(CastMask::TRUNCATE)`
    /// then `.ignore_casts_mask(CastMask::EXTEND)` is equivalent to one
    /// call with `CastMask::TRUNCATE | CastMask::EXTEND`.
    #[must_use]
    pub fn ignore_casts_mask(mut self, mask: CastMask) -> Self {
        self.options.cast_mask |= mask;
        self
    }

    /// Walk through every value-passthrough cast (equivalent to
    /// `.ignore_casts_mask(CastMask::all())`).  Convenience for the
    /// common "I don't care about cast chains" case.
    #[must_use]
    pub fn ignore_casts(self) -> Self {
        self.ignore_casts_mask(CastMask::all())
    }

    /// Walk through `Region` (region-join) nodes when traversing
    /// control chains.  Currently accepted as a no-op for API
    /// compatibility with strider-analyze's matcher during the
    /// migration — see [`MatcherOptions::ignore_regions`].
    #[must_use]
    pub fn ignore_regions(mut self) -> Self {
        self.options.ignore_regions = true;
        self
    }

    /// Function-entry [`NodeId`] of the wrapped function.  Panics-free
    /// because [`Self::try_new`] validates the post-build invariant up
    /// front.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn entry(&self) -> NodeId {
        self.function
            .entry()
            .expect("Matcher wraps a built function with an entry node (try_new invariant)")
    }

    /// Find every match for `pat` in the function.  Currently scans
    /// every reachable node and filters by the pattern's
    /// `root_kind_discriminant`; future revisions may add a kind index
    /// for speed.
    pub fn find_all<P: Pattern + ?Sized>(&self, pat: &P) -> Vec<Match> {
        let ctx = MatchCtx { matcher: self, function: self.function };
        let target_disc = pat.root_kind_discriminant();
        let mut out = Vec::new();
        for node in self.function.walk() {
            if let Some(d) = target_disc
                && std::mem::discriminant(self.function.node_kind(node)) != d
            {
                continue;
            }
            self.try_at_node(node, pat, &ctx, &mut out);
        }
        out
    }

    /// Find the first match of `pat` in the function, or `None` if
    /// `pat` doesn't match anywhere.  Streamed variant of
    /// [`Self::find_all`] that stops at the first hit.
    pub fn find_first<P: Pattern + ?Sized>(&self, pat: &P) -> Option<Match> {
        let ctx = MatchCtx { matcher: self, function: self.function };
        let target_disc = pat.root_kind_discriminant();
        for node in self.function.walk() {
            if let Some(d) = target_disc
                && std::mem::discriminant(self.function.node_kind(node)) != d
            {
                continue;
            }
            let outputs = self.function.node_outputs(node);
            if outputs.is_empty() {
                let mut bindings = Bindings::default();
                if pat.try_match_node_id(&ctx, node, &mut bindings) {
                    return Some(Match::from_root(node, bindings));
                }
                continue;
            }
            for &out_id in outputs {
                let mut bindings = Bindings::default();
                if pat.try_match(&ctx, out_id, &mut bindings) {
                    return Some(Match::from_root(node, bindings));
                }
            }
        }
        None
    }

    /// Run several patterns independently against the function and
    /// return the per-pattern matches.  The outer index corresponds to
    /// the input pattern index; the inner Vec is that pattern's match
    /// list (same shape as [`Self::find_all`]).
    ///
    /// Unlike [`Self::find_joined`], this does NOT filter on shared-
    /// capture agreement — each pattern's matches stand alone.  Useful
    /// when callers need every match list separately (e.g. for
    /// side-by-side reporting).
    pub fn find_all_multi(&self, pats: &[&dyn Pattern]) -> Vec<Vec<Match>> {
        pats.iter().map(|p| self.find_all(*p)).collect()
    }

    /// Try `pat` at a specific IR node; returns the first match if any
    /// (iterating outputs for value-producing nodes; node-rooted for
    /// zero-output kinds).
    pub fn match_at<P: Pattern + ?Sized>(&self, node: NodeId, pat: &P) -> Option<Match> {
        let ctx = MatchCtx { matcher: self, function: self.function };
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::default();
            if pat.try_match_node_id(&ctx, node, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
            return None;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::default();
            if pat.try_match(&ctx, out_id, &mut bindings) {
                return Some(Match::from_root(node, bindings));
            }
        }
        None
    }

    fn try_at_node<P: Pattern + ?Sized>(
        &self,
        node: NodeId,
        pat: &P,
        ctx: &MatchCtx,
        out: &mut Vec<Match>,
    ) {
        let outputs = self.function.node_outputs(node);
        if outputs.is_empty() {
            let mut bindings = Bindings::default();
            if pat.try_match_node_id(ctx, node, &mut bindings) {
                out.push(Match::from_root(node, bindings));
            }
            return;
        }
        for &out_id in outputs {
            let mut bindings = Bindings::default();
            if pat.try_match(ctx, out_id, &mut bindings) {
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
    /// Use case: a pattern set "A(K) ∧ B(K)" where A and B share a
    /// capture that must point at the *same* IR node — each pattern is
    /// matched independently via [`Self::find_all`], then a cross-
    /// product is filtered to those tuples whose shared captures agree.
    ///
    /// # Returns
    ///
    /// Outer index — one entry per joined-match tuple.  Inner index —
    /// one [`Match`] per input pattern, in input order.  Every inner
    /// `Match` in a given tuple agrees with the others on every shared
    /// capture's binding.
    ///
    /// # Edge cases
    ///
    /// * Empty `pats` slice → empty outer Vec.
    /// * Single pattern → equivalent to wrapping each [`Self::find_all`]
    ///   hit in a one-element inner Vec (no join work, no shared-capture
    ///   filter — every capture is local).
    /// * Any pattern with zero matches makes the joined result empty —
    ///   nothing to cross-product against.
    ///
    /// # Complexity
    ///
    /// O(N₁ × N₂ × … × N_M) worst case where N_i is the number of
    /// matches for pattern i.  Each cross-product term incurs a linear
    /// binding-overlap scan against the partial tuple.  Shared-capture
    /// filtering prunes the cross-product aggressively in practice.
    pub fn find_joined(&self, pats: &[&dyn Pattern]) -> Vec<Vec<Match>> {
        if pats.is_empty() {
            return Vec::new();
        }
        let per_pat: Vec<Vec<Match>> = pats.iter().map(|p| self.find_all(*p)).collect();
        if per_pat.iter().any(|hits| hits.is_empty()) {
            return Vec::new();
        }

        // Seed the accumulator with single-element tuples from the
        // first pattern's hits.
        let mut acc: Vec<Vec<Match>> = per_pat[0]
            .iter()
            .cloned()
            .map(|m| vec![m])
            .collect();

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
    /// carrier exists.  Mirrors the strider-analyze matcher's
    /// `function_arg` accessor for migration source-compat.
    #[must_use]
    pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'f>> {
        let node = *self.function.arg_index_to_nodes(index).first()?;
        Some(FunctionArgHandle { function: self.function, node })
    }

    /// Iterate `(index, handle)` for every registered function-arg
    /// carrier in side-table-index order.
    pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'f>)> + '_ {
        let f = self.function;
        // Collect + sort to give stable, index-ordered iteration; the
        // underlying side-table is a `FxHashMap<u32, Vec<NodeId>>`.
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
    /// registered carrier.  Equivalent to `max(registered idx) + 1`,
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
/// `Function::arg_index_to_nodes`.  Returned by
/// [`Matcher::function_arg`] / [`Matcher::function_args`].
#[derive(Clone, Copy)]
pub struct FunctionArgHandle<'g> {
    function: &'g Function,
    node: NodeId,
}

impl<'g> FunctionArgHandle<'g> {
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
/// previously-collected match in `prefix` binds to the same value.  A
/// shared [`Capture`](crate::Capture) must bind the same
/// [`Binding`](crate::bindings) across matches; captures local to `m`
/// (not seen in `prefix`) impose no constraint.
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
