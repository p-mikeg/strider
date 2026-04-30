use std::collections::HashMap;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::pat::Pat;

mod function_arg_handle;

pub use function_arg_handle::FunctionArgHandle;

pub(crate) mod bindings;
pub(crate) mod cast_mask;
pub(crate) mod commutativity;
mod match_result;
pub(crate) mod walk;
pub(crate) mod walk_through;

pub use bindings::Bindings;
pub use cast_mask::CastMask;
pub use match_result::Match;

// ── Matcher ───────────────────────────────────────────────────────────────────

/// Lazy index used by the `FunctionArg` query API ([`Matcher::function_arg`],
/// [`Matcher::function_args`], [`Matcher::function_arg_count`]).
///
/// Built on first access; [`Matcher::match_at`] and [`Matcher::find_all`]
/// never need it.  Layer C of the IR validator enforces at most one
/// `FunctionArg` per index, so at most one entry exists per key.
struct FunctionArgIndex(HashMap<u32, NodeId>);

/// Optional behaviors that change how the matcher walks through "transparent"
/// producer / consumer nodes during input or control-chain matching.
///
/// Defaults are strict exact-walk semantics: `ignore_cast_mask` is empty
/// and `ignore_control_states` is `false`.  Enable selective cast
/// walk-through via [`Matcher::ignore_casts_mask`] /
/// [`Matcher::ignore_casts`], and control-state walk-through via
/// [`Matcher::ignore_control_states`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatcherOptions {
    /// Mask of value-passthrough cast `NodeKind`s the matcher walks
    /// through transparently when matching value inputs.  Default:
    /// [`CastMask::empty()`] (no walk-through — strict semantics).
    /// [`Matcher::ignore_casts`] sets it to [`CastMask::all()`] for
    /// source-compatibility with the previous boolean toggle.
    ///
    /// Use case: x86 / x64 register-merge chains and width casts cause
    /// patterns like `add(mul(_,_), _)` to find `Add(Extend(Mul), arg)`
    /// without re-shaping the source.  See the
    /// `add_mul_pattern_does_not_match_through_extend_by_default`
    /// regression test for the canonical case.
    pub ignore_cast_mask: CastMask,
    /// Walk through `ControlState` (region-join) nodes when traversing
    /// control chains.  Lets `ret(call(...))`, `if_node().true_branch(p)`
    /// etc. cross region joins without intermediate awareness.
    pub ignore_control_states: bool,
}

/// Executes pattern queries against a [`BuiltFunctionGraph`].
///
/// Construction is O(1).  `find_all` / `match_at` do a single preorder
/// walk of the graph each call and try the pattern against every
/// candidate node (kind-prefiltered when the pattern's
/// [`KindSpec`](crate::pat::node_pat::KindSpec) is concrete).  These
/// paths never touch the `FunctionArg` index.
///
/// The `FunctionArg` query API (`function_arg`, `function_args`,
/// `function_arg_count`, `function_arg_len`) builds an index lazily on
/// first use via `OnceCell`: the first call pays a one-time preorder
/// walk to populate `index → NodeId`; subsequent calls are O(1).
pub struct Matcher<'g> {
    pub(super) fn_graph: &'g BuiltFunctionGraph,
    pub(crate) options: MatcherOptions,
    function_arg_index: std::cell::OnceCell<FunctionArgIndex>,
}

impl<'g> Matcher<'g> {
    /// Creates a new `Matcher` with default options (both walk-through
    /// flags off — strict exact-walk semantics).
    #[must_use]
    pub fn new(fn_graph: &'g BuiltFunctionGraph) -> Self {
        Self {
            fn_graph,
            options: MatcherOptions::default(),
            function_arg_index: std::cell::OnceCell::new(),
        }
    }

    /// Enables transparent walk-through of every value-passthrough cast
    /// kind — equivalent to `.ignore_casts_mask(CastMask::all())`, and
    /// to the previous boolean `ignore_casts()` behaviour.  See
    /// [`MatcherOptions::ignore_cast_mask`].
    #[must_use]
    pub fn ignore_casts(mut self) -> Self {
        self.options.ignore_cast_mask = CastMask::all();
        self
    }

    /// Enables transparent walk-through of only the cast kinds present
    /// in `mask`.  Multiple calls union (OR-combine):
    ///
    /// ```rust
    /// # use ir::FunctionBuilder;
    /// # use pattern::{CastMask, Matcher};
    /// # let mut fb = FunctionBuilder::empty().unwrap();
    /// # let r = fb.create_region().unwrap();
    /// # fb.set_entry_region(r).unwrap();
    /// # fb.set_region(r);
    /// # fb.build_return(None, &[]).unwrap();
    /// # let g = fb.build().unwrap();
    /// let m = Matcher::new(&g)
    ///     .ignore_casts_mask(CastMask::TRUNCATE)
    ///     .ignore_casts_mask(CastMask::EXTEND);
    /// assert_eq!(
    ///     m.options_for_test().ignore_cast_mask,
    ///     CastMask::TRUNCATE | CastMask::EXTEND
    /// );
    /// ```
    #[must_use]
    pub fn ignore_casts_mask(mut self, mask: CastMask) -> Self {
        self.options.ignore_cast_mask |= mask;
        self
    }

    /// Enables transparent walk-through of `ControlState` (region-join)
    /// nodes when traversing control chains.  See
    /// [`MatcherOptions::ignore_control_states`].
    #[must_use]
    pub fn ignore_control_states(mut self) -> Self {
        self.options.ignore_control_states = true;
        self
    }

    /// Returns the active matcher options.  Used by walk-through helpers
    /// that gate their behavior on the flags, and by tests that pin the
    /// builder-API contracts.
    #[must_use]
    pub fn options_for_test(&self) -> MatcherOptions {
        self.options
    }

    /// Returns the lazily-built `FunctionArg` index.
    fn function_arg_index(&self) -> &FunctionArgIndex {
        self.function_arg_index.get_or_init(|| {
            let mut map: HashMap<u32, NodeId> = HashMap::new();
            for node in self.fn_graph.preorder() {
                if let NodeKind::FunctionArg { index, .. } =
                    self.fn_graph.graph.node_kind(node)
                {
                    map.insert(*index, node);
                }
            }
            FunctionArgIndex(map)
        })
    }

    /// Finds all nodes in the graph where `pat` matches and returns a [`Match`]
    /// for each.  Does a preorder walk of the graph and tries every node as a
    /// potential root.
    ///
    /// Candidate nodes are pre-filtered by the pattern's
    /// [`Pattern::kind_spec`](crate::pat::traits::Pattern::kind_spec)
    /// (discriminant-only check via
    /// [`KindSpec::accepts_discriminant`](crate::pat::node_pat::KindSpec::accepts_discriminant)):
    /// for a pattern with a concrete root kind (e.g. `add(...)`) this skips
    /// every node whose `NodeKind` discriminant is different, turning a
    /// graph-wide scan into an effectively kind-indexed scan.  Patterns that
    /// match any kind (wildcards, `KindSpec::Any`) fall through to the
    /// unfiltered loop.
    pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
        let kind = pat.as_dyn().kind_spec();
        let mut bindings = Bindings::default();
        let mut hits: Vec<Match> = Vec::new();
        for node in self
            .fn_graph
            .preorder()
            .filter(|&node| kind.accepts_discriminant(self.fn_graph.graph.node_kind(node)))
        {
            let mark = bindings.mark();
            if self.match_node_id(node, pat, &mut bindings) {
                hits.push(Match {
                    root: node,
                    bindings: bindings.clone(),
                });
            }
            // Roll back to the pre-attempt state regardless of outcome —
            // successful matches kept their bindings via clone, failed
            // matches discard the speculative entries.  Net: one
            // allocation per find_all + one per successful match,
            // versus one per candidate previously.
            bindings.restore(mark);
        }
        hits
    }

    /// Try to match `pat` against the subgraph rooted at `node`.  Returns the
    /// successful [`Match`] (with bindings) if the match succeeds, `None`
    /// otherwise.
    ///
    /// Unlike [`find_all`] which iterates every candidate root, this checks a
    /// single root.  Used by [`crate::rewrite_rule`] and other callers
    /// that already know the candidate.
    pub fn match_at(&self, node: NodeId, pat: &Pat) -> Option<Match> {
        let mut bindings = Bindings::default();
        if self.match_node_id(node, pat, &mut bindings) {
            Some(Match { root: node, bindings })
        } else {
            None
        }
    }

    // ── FunctionArg query API ─────────────────────────────────────────────────

    /// Returns a [`FunctionArgHandle`] for the `FunctionArg` node at argument
    /// position `index`, if the `FunctionArgDetect` pass emitted one.
    pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'g>> {
        let node_id = *self.function_arg_index().0.get(&index)?;
        self.make_function_arg_handle(node_id)
    }

    /// Returns the **highest observed** argument index plus one, or `0` if
    /// the graph has no `FunctionArg` nodes.
    ///
    /// `FunctionArgDetect` does not enforce contiguous indices — a function
    /// that reads only `rdx` (the third x86_64 arg) will yield a
    /// `FunctionArg { index: 2, .. }` with no entries at indices 0 or 1.
    /// In that case this method returns `3` but `function_arg(0)` and
    /// `function_arg(1)` both return `None`.  Use [`Self::function_arg_len`]
    /// for the actual population count.
    pub fn function_arg_count(&self) -> usize {
        self.function_arg_index()
            .0
            .keys()
            .max()
            .map_or(0, |&m| (m as usize) + 1)
    }

    /// Returns the number of distinct `FunctionArg` nodes in the graph.
    /// Unlike [`Self::function_arg_count`] this is insensitive to gaps in
    /// the index space.
    pub fn function_arg_len(&self) -> usize {
        self.function_arg_index().0.len()
    }

    /// Iterates over every `FunctionArg` node, yielding `(index, handle)`
    /// pairs sorted ascending by index.
    pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'g>)> + '_ {
        let mut pairs: Vec<(u32, NodeId)> = self
            .function_arg_index()
            .0
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        pairs.sort_by_key(|(k, _)| *k);
        pairs.into_iter().filter_map(move |(k, node_id)| {
            self.make_function_arg_handle(node_id).map(|h| (k, h))
        })
    }

    /// Builds a [`FunctionArgHandle`] from `node_id`, pulling `source` and
    /// `index` out of the node's `NodeKind`.  Returns `None` if the node is
    /// not actually a `FunctionArg` — the index-map only contains such nodes
    /// by construction, so this never fires in practice, but preserves the
    /// "no-panic" discipline.
    fn make_function_arg_handle(&self, node_id: NodeId) -> Option<FunctionArgHandle<'g>> {
        let NodeKind::FunctionArg { source, index } = *self.fn_graph.graph.node_kind(node_id)
        else {
            return None;
        };
        Some(FunctionArgHandle {
            fn_graph: self.fn_graph,
            node_id,
            source,
            index,
        })
    }

    // ── Dispatch entry points ────────────────────────────────────────────────
    //
    // `match_output` / `match_node_id` are the single entry points combinators
    // call (via `MatchCtx.matcher`) when recursing into an inner `Pat`.  They
    // forward directly to the pattern's `Pattern::try_match` impl.

    /// Build a [`MatchCtx`](crate::pat::traits::MatchCtx) that carries both
    /// the graph and a back-reference to this matcher.  Combinators clone it
    /// and pass it through their inner [`Self::match_output`] /
    /// [`Self::match_node_id`] dispatch.
    pub(crate) fn ctx(&self) -> crate::pat::traits::MatchCtx<'g, '_> {
        crate::pat::traits::MatchCtx {
            graph: self.fn_graph,
            matcher: self,
        }
    }

    /// Match a `NodeOutputId` against a pattern — single-line delegation to
    /// the unified [`Pattern`](crate::pat::traits::Pattern) trait.
    pub(super) fn match_output(
        &self,
        output: NodeOutputId,
        pat: &Pat,
        bindings: &mut Bindings,
    ) -> bool {
        pat.as_dyn().try_match(&self.ctx(), output, bindings)
    }

    /// Match a `NodeId` against a pattern via
    /// [`Pattern::try_match_node`](crate::pat::traits::Pattern::try_match_node)
    /// — which iterates the node's outputs (default impl) or matches the
    /// node directly (zero-output nodes like `Return`, via `NodePat`'s
    /// override).
    pub(crate) fn match_node_id(&self, node: NodeId, pat: &Pat, bindings: &mut Bindings) -> bool {
        pat.as_dyn().try_match_node(&self.ctx(), node, bindings)
    }

    /// Top-level "match with options" entry point used by `NodePat::try_once`
    /// when walking sub-pattern inputs.  Tries direct match first; on
    /// failure, falls back to the walk-through helpers gated by
    /// [`MatcherOptions`].  The walk-through helpers call this method
    /// recursively so chained casts (e.g. `Extend(Truncate(Mul))`) also
    /// resolve.
    pub(crate) fn match_output_with_walk_through(
        &self,
        out: NodeOutputId,
        pat: &Pat,
        b: &mut Bindings,
    ) -> bool {
        let mark = b.mark();
        if self.match_output(out, pat, b) {
            return true;
        }
        b.restore(mark);

        if !self.options.ignore_cast_mask.is_empty()
            && walk_through::try_walk_through_cast(&self.ctx(), out, pat, b)
        {
            return true;
        }
        b.restore(mark);

        if self.options.ignore_control_states
            && walk_through::try_walk_through_control_state(&self.ctx(), out, pat, b)
        {
            return true;
        }
        b.restore(mark);

        false
    }
}
