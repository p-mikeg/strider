use strider_ir::node::{NodeId, NodeOutputId};

use crate::pattern::pat::Pat;

mod function_arg_handle;

pub use function_arg_handle::{ArgSource, FunctionArgHandle};

pub(crate) mod bindings;
pub(crate) mod cast_mask;
mod match_result;
pub(crate) mod consumer;
pub(crate) mod walk_through;

pub use bindings::Bindings;
pub use cast_mask::CastMask;
pub use match_result::Match;

// ── Matcher ───────────────────────────────────────────────────────────────────

/// Optional behaviors that change how the matcher walks through "transparent"
/// producer / consumer nodes during input or control-chain matching.
///
/// Defaults are strict exact-walk semantics: `ignore_cast_mask` is empty and
/// `ignore_regions` is `false`.  Enable selective cast walk-through via
/// [`Matcher::ignore_casts_mask`] / [`Matcher::ignore_casts`] and
/// control-state walk-through via [`Matcher::ignore_regions`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MatcherOptions {
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
    /// Walk through `Region` (region-join) nodes when traversing
    /// control chains.  Lets `ret(call(...))`, `if_node().true_branch(p)`
    /// etc. cross region joins without intermediate awareness.
    pub ignore_regions: bool,
}

/// Executes pattern queries against a [`Graph`].
///
/// Construction is O(1).  `find_all` / `match_at` do a single preorder
/// walk of the graph each call and try the pattern against every
/// candidate node (kind-prefiltered when the pattern's
/// `KindSpec` is concrete).
///
/// The `FunctionArg` query API (`function_arg`, `function_args`,
/// `function_arg_index_upper_bound`, `function_arg_count`) reads the
/// `Function::arg_index_to_nodes` side-table directly — O(1) per call,
/// no scan.
pub struct Matcher<'g> {
    pub(super) function: &'g strider_ir::Function,
    pub(super) entry: NodeId,
    pub(crate) options: MatcherOptions,
    /// Lazily-cached preorder traversal of `graph`.  Built on
    /// first call to [`Self::walk_cached`]; stays valid for the
    /// `Matcher`'s lifetime because the matcher holds an immutable
    /// borrow of `graph` (any mutation would require a fresh
    /// `&mut Graph`, which forces this `Matcher` out of scope).
    ///
    /// Used by [`Self::find_all`], [`Self::find_all_multi`], and the
    /// kind-index bootstrap to avoid M independent preorder walks per
    /// session.
    walk: std::cell::OnceCell<Vec<NodeId>>,
    /// Lazily-cached `Discriminant<NodeKind> → Vec<NodeId>` index of
    /// the graph.  Populated on first call to [`Self::kind_index`];
    /// consulted by [`Self::find_all`] and [`Self::find_all_multi`]
    /// to skip every node whose `NodeKind` discriminant is
    /// incompatible with the pattern's root kind.
    ///
    /// Same staleness story as `preorder` — borrow-checker enforces
    /// no mutation during the `Matcher`'s lifetime.
    kind_index:
        std::cell::OnceCell<rustc_hash::FxHashMap<std::mem::Discriminant<strider_ir::node::NodeKind>, Vec<NodeId>>>,
}

impl<'g> Matcher<'g> {
    /// Creates a new `Matcher` over a built [`Function`].  Wrapper around
    /// [`Self::for_function`] for callers that hold the fully-built form
    /// (the common query path).  Rewrite-only callers that have just
    /// `&Function + NodeId` should use [`Self::for_function`] directly.
    ///
    /// # Errors
    ///
    /// Returns an error if the function has not been built (i.e. `entry`
    /// is `None`).
    pub fn try_new(function: &'g strider_ir::Function) -> anyhow::Result<Self> {
        let entry = function.entry().ok_or_else(|| {
            anyhow::anyhow!("Matcher::try_new: entry node is not set")
        })?;
        Ok(Self::for_function(function, entry))
    }

    /// Creates a new `Matcher` over a `(function, entry)` pair.
    #[must_use]
    pub fn for_function(function: &'g strider_ir::Function, entry: NodeId) -> Self {
        Self {
            function,
            entry,
            options: MatcherOptions::default(),
            walk: std::cell::OnceCell::new(),
            kind_index: std::cell::OnceCell::new(),
        }
    }

    /// Returns the cached preorder traversal of the graph, computing
    /// it on first call.
    fn walk_cached(&self) -> &[NodeId] {
        self.walk
            .get_or_init(|| self.function.walk_from(self.entry).collect())
            .as_slice()
    }

    /// Returns the cached node-kind index, computing it on first
    /// call.  Only NodeKinds that actually occur in the graph appear
    /// as keys.
    fn kind_index(
        &self,
    ) -> &rustc_hash::FxHashMap<std::mem::Discriminant<strider_ir::node::NodeKind>, Vec<NodeId>> {
        self.kind_index.get_or_init(|| {
            let mut index: rustc_hash::FxHashMap<
                std::mem::Discriminant<strider_ir::node::NodeKind>,
                Vec<NodeId>,
            > = rustc_hash::FxHashMap::default();
            for &node in self.walk_cached() {
                let d = std::mem::discriminant(self.function.node_kind(node));
                index.entry(d).or_default().push(node);
            }
            index
        })
    }

    /// Enables transparent walk-through of every value-passthrough cast
    /// kind — equivalent to `.ignore_casts_mask(CastMask::all())`, and
    /// to the previous boolean `ignore_casts()` behaviour.  See
    /// `MatcherOptions::ignore_cast_mask`.
    #[must_use]
    pub fn ignore_casts(mut self) -> Self {
        self.options.ignore_cast_mask = CastMask::all();
        self
    }

    /// Enables transparent walk-through of only the cast kinds present
    /// in `mask`.  Multiple calls union (OR-combine) — passing
    /// `CastMask::TRUNCATE` then `CastMask::EXTEND` is equivalent to
    /// passing `CastMask::TRUNCATE | CastMask::EXTEND` in one call.
    #[must_use]
    pub fn ignore_casts_mask(mut self, mask: CastMask) -> Self {
        self.options.ignore_cast_mask |= mask;
        self
    }

    /// Enables transparent walk-through of `Region` (region-join)
    /// nodes when traversing control chains.  See
    /// `MatcherOptions::ignore_regions`.
    #[must_use]
    pub fn ignore_regions(mut self) -> Self {
        self.options.ignore_regions = true;
        self
    }

    /// Finds all nodes in the graph where `pat` matches and returns a [`Match`]
    /// for each.
    ///
    /// Candidate selection is driven by the pattern's
    /// `Pattern::kind_spec`:
    /// * Concrete root kind (e.g. `add(...)`, `load()`, `call()`) — the
    ///   matcher consults its lazy `kind_index` and iterates only the
    ///   bucket of nodes whose discriminant matches.
    /// * Wildcard root (`KindSpec::Any`) — falls back to the lazy
    ///   `walk_cached` traversal.
    ///
    /// Both indices are computed once per `Matcher` and reused across
    /// every `find_all` / `find_all_multi` call on that matcher.
    pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
        let kind = pat.as_dyn().kind_spec();
        let mut bindings = Bindings::default();
        let mut hits: Vec<Match> = Vec::new();

        // Two scan strategies:
        //   - concrete root kind: iterate only the kind-index bucket
        //     for that discriminant (covers `add(...)`, `load()`,
        //     `call()`, `if_node()`, … — most production patterns).
        //   - wildcard root (`KindSpec::Any`): iterate the cached
        //     preorder (still preorder, just shared across
        //     `find_all` calls on this matcher).
        match kind.discriminant() {
            Some(d) => {
                let buckets = self.kind_index();
                if let Some(nodes) = buckets.get(&d) {
                    for &node in nodes {
                        let mark = bindings.mark();
                        if self.match_node_id(node, pat, &mut bindings) {
                            hits.push(Match {
                                root: node,
                                bindings: bindings.clone(),
                            });
                        }
                        bindings.restore(mark);
                    }
                }
            }
            None => {
                for &node in self.walk_cached() {
                    let mark = bindings.mark();
                    if self.match_node_id(node, pat, &mut bindings) {
                        hits.push(Match {
                            root: node,
                            bindings: bindings.clone(),
                        });
                    }
                    bindings.restore(mark);
                }
            }
        }
        hits
    }

    /// Run several patterns over the graph in a single pass.  Returns
    /// one `Vec<Match>` per input pattern, in input order.
    ///
    /// Equivalent to calling [`Self::find_all`] for each pattern
    /// sequentially, but cheaper:
    ///
    /// 1. The cached preorder + kind-index built on this matcher are
    ///    consulted directly — no per-pattern graph walk.
    /// 2. Patterns are bucketed by their root `NodeKind` discriminant
    ///    once; each non-wildcard pattern visits only its bucket of
    ///    candidate nodes.  Wildcard patterns (`KindSpec::Any`) visit
    ///    the cached preorder once and try every node.
    ///
    /// Match ordering within each output `Vec<Match>` matches what
    /// the corresponding `find_all(p)` would produce — preorder of
    /// root nodes — so callers comparing match sets across the two
    /// APIs see identical results.
    pub fn find_all_multi(&self, pats: &[&Pat]) -> Vec<Vec<Match>> {
        let mut results: Vec<Vec<Match>> = (0..pats.len()).map(|_| Vec::new()).collect();
        if pats.is_empty() {
            return results;
        }

        // Bucket the input patterns by their root discriminant (or
        // collect into a "wildcard" bucket if `KindSpec::Any`).
        let mut by_discriminant: rustc_hash::FxHashMap<
            std::mem::Discriminant<strider_ir::node::NodeKind>,
            Vec<usize>,
        > = rustc_hash::FxHashMap::default();
        let mut wildcards: Vec<usize> = Vec::new();
        for (i, pat) in pats.iter().enumerate() {
            match pat.as_dyn().kind_spec().discriminant() {
                Some(d) => by_discriminant.entry(d).or_default().push(i),
                None => wildcards.push(i),
            }
        }

        let mut bindings = Bindings::default();

        // Per-discriminant pass: each bucket of patterns iterates only
        // its kind's nodes.  Patterns within a bucket are tried in
        // input order at each node.
        let kind_buckets = self.kind_index();
        for (d, pat_indices) in &by_discriminant {
            let Some(nodes) = kind_buckets.get(d) else {
                continue;
            };
            for &node in nodes {
                for &i in pat_indices {
                    let mark = bindings.mark();
                    if self.match_node_id(node, pats[i], &mut bindings) {
                        results[i].push(Match {
                            root: node,
                            bindings: bindings.clone(),
                        });
                    }
                    bindings.restore(mark);
                }
            }
        }

        // Wildcard pass: visit every node in cached preorder, try
        // every wildcard pattern.  Skipped when `wildcards` is empty.
        if !wildcards.is_empty() {
            for &node in self.walk_cached() {
                for &i in &wildcards {
                    let mark = bindings.mark();
                    if self.match_node_id(node, pats[i], &mut bindings) {
                        results[i].push(Match {
                            root: node,
                            bindings: bindings.clone(),
                        });
                    }
                    bindings.restore(mark);
                }
            }
        }

        // No post-sort: `kind_index` was populated by iterating
        // `walk_cached()` once, so each bucket's `Vec<NodeId>`
        // is already in preorder.  Iterating one bucket per
        // pattern preserves per-pattern preorder of `find_all`.
        results
    }

    /// Run several patterns over the graph and return only the joined
    /// matches where every [`crate::pattern::Capture`] appearing in more than one
    /// pattern binds to the same node (and value output, when
    /// applicable) across every pattern in which it appears.
    ///
    /// Use case: a pattern set "A(K) ∧ B(K)" where A and B share a
    /// capture that must point at the *same* IR node — e.g. find a
    /// `K` such that both `store(<base>+K, 0)` and
    /// `call(at=F).arg(0, <base>)` match with the same `<base>`
    /// binding.  Each pattern is matched independently, then a
    /// cross-product is filtered to those tuples whose shared
    /// captures agree.
    ///
    /// # Returns
    ///
    /// Outer index — one entry per joined-match tuple.  Inner index —
    /// one [`Match`] per input pattern, in input order.  Every inner
    /// `Match` in a given tuple agrees with the others on every
    /// shared capture's `crate::pattern::matcher::bindings::Binding`.
    ///
    /// # Edge cases
    ///
    /// * Empty `pats` slice → empty outer Vec.
    /// * Single pattern → equivalent to wrapping each
    ///   [`Self::find_all`] hit in a one-element inner Vec (no join
    ///   work, no shared-capture filter — every capture is local).
    /// * Any pattern with zero matches makes the joined result
    ///   empty — nothing to cross-product against.
    ///
    /// # Complexity
    ///
    /// O(N₁ × N₂ × … × N_M) worst case where N_i is the number of
    /// matches for pattern i.  Each cross-product term incurs a
    /// linear binding-overlap scan against the partial tuple.  For
    /// typical patterns the shared-capture filter prunes the
    /// cross-product aggressively, so real-world performance is
    /// closer to O(N_max).
    ///
    /// # Worked example — recovering a struct-field offset
    ///
    /// To recover `nd.ni_vp`'s offset given a `vn_open(&nd, …)` call
    /// followed by a `script_vp = nd.ni_vp` field read, the natural
    /// thought is "capture `&nd` and find loads relative to it."  But
    /// constant-fold reassociates `Add(Add(rbp, K1), K_field)` into
    /// `Add(rbp, K1+K_field)` (see `opt::ConstantFold`), so `&nd` does
    /// not survive as a shared sub-expression of the load.  Anchor on
    /// the frame base (`InitialVar(rbp)` / `InitialVar(rsp)`) instead
    /// — both the call's arg and the load's address reference the
    /// same dedup'd `InitialVar` node, and the join is trivial:
    ///
    /// ```no_run
    /// # use strider_analyze::pattern::{Capture, Matcher, Pat};
    /// # use strider_analyze::pattern::{call, load, add, int_const, any_int_const, initial_var_for};
    /// # fn example(
    /// #     matcher: &Matcher<'_>,
    /// #     graph: &strider_ir::Graph,
    /// #     rbp: rsleigh::Vn,
    /// #     vn_open_addr: u64,
    /// # ) -> Option<()> {
    /// let k_call = Capture::new();         // K1 — &nd offset from frame base
    /// let k_load = Capture::new();         // K2 — nd.ni_vp offset from frame base
    /// let pats: [Pat; 2] = [
    ///     // call(at=vn_open).arg(0, lea rbp+K1)
    ///     call().target(int_const(vn_open_addr))
    ///         .arg(0, add(initial_var_for(rbp), any_int_const(k_call)).ordered())
    ///         .into(),
    ///     // load at rbp+K2
    ///     load().addr(add(initial_var_for(rbp), any_int_const(k_load)).ordered())
    ///         .into(),
    /// ];
    /// let pat_refs: Vec<&Pat> = pats.iter().collect();
    /// for tup in matcher.find_all_requirements(&pat_refs) {
    ///     let k1 = tup[0].get_uint(k_call, graph)?;
    ///     let k2 = tup[1].get_uint(k_load, graph)?;
    ///     let _ni_vp_offset = (k2 as i64) - (k1 as i64); // recovered field offset
    /// }
    /// # Some(())
    /// # }
    /// ```
    ///
    /// If the load on the same stack slot was earlier rewritten by
    /// `LoadForward`, the field read is folded away and this
    /// query returns nothing.  Add a `store(...)` arm to the pattern
    /// set when the field is being *written* rather than *read*.
    pub fn find_all_requirements(&self, pats: &[&Pat]) -> Vec<Vec<Match>> {
        if pats.is_empty() {
            return Vec::new();
        }
        let per_pat = self.find_all_multi(pats);
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

    /// Try to match `pat` against the subgraph rooted at `node`.  Returns the
    /// successful [`Match`] (with bindings) if the match succeeds, `None`
    /// otherwise.
    ///
    /// Unlike `find_all` which iterates every candidate root, this checks a
    /// single root.  Used by [`crate::pattern::rewrite_rule`] and other callers
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
    //
    // These methods read `Function::arg_index_to_nodes` directly; no lazy
    // scan of the graph is needed.

    /// Returns a [`FunctionArgHandle`] for the first carrier node registered
    /// for argument `index`, or `None` if no carriers are registered.
    ///
    /// For register args there is always exactly one carrier (`InitialVar`).
    /// For stack args there may be multiple carriers (different-width `Load`s
    /// at the same SP-relative offset); this method returns the first.  Use
    /// [`Self::function_args`] to iterate all carriers for an index.
    pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'g>> {
        let node_id = *self.function.arg_index_to_nodes(index).first()?;
        Some(FunctionArgHandle {
            node_id,
            function: self.function,
            index,
        })
    }

    /// Iterates over every registered argument index, yielding `(index,
    /// handle)` pairs for the first carrier of each index, sorted ascending
    /// by index.
    pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'g>)> + '_ {
        let mut indices: Vec<u32> = self.function.iter_arg_indices().collect();
        indices.sort_unstable();
        indices.into_iter().filter_map(move |idx| {
            self.function_arg(idx).map(|h| (idx, h))
        })
    }

    /// Returns the **highest observed** argument index plus one, or `0` if
    /// no arguments are registered.
    ///
    /// `FunctionArgDetect` does not enforce contiguous indices — a function
    /// that reads only `rdx` (the third x86_64 arg) will have index `2`
    /// registered with nothing at 0 or 1.  In that case this returns `3`
    /// while `function_arg(0)` and `function_arg(1)` return `None`.  Use
    /// [`Self::function_arg_count`] for the actual population count.
    pub fn function_arg_index_upper_bound(&self) -> usize {
        self.function
            .iter_arg_indices()
            .max()
            .map_or(0, |m| (m as usize) + 1)
    }

    /// Returns the number of distinct argument indices registered in the
    /// side-table.
    pub fn function_arg_count(&self) -> usize {
        self.function.iter_arg_indices().count()
    }

    // ── Dispatch entry points ────────────────────────────────────────────────
    //
    // `match_output` / `match_node_id` are the single entry points combinators
    // call (via `MatchCtx.matcher`) when recursing into an inner `Pat`.  They
    // forward directly to the pattern's `Pattern::try_match` impl.

    /// Build a [`MatchCtx`](crate::pattern::pat::traits::MatchCtx) that carries both
    /// the graph and a back-reference to this matcher.  Combinators clone it
    /// and pass it through their inner [`Self::match_output`] /
    /// [`Self::match_node_id`] dispatch.
    pub(crate) fn ctx(&self) -> crate::pattern::pat::traits::MatchCtx<'g, '_> {
        crate::pattern::pat::traits::MatchCtx {
            function: self.function,
            matcher: self,
        }
    }

    /// Match a `NodeOutputId` against a pattern — single-line delegation to
    /// the unified [`Pattern`](crate::pattern::pat::traits::Pattern) trait.
    pub(super) fn match_output(
        &self,
        output: NodeOutputId,
        pat: &Pat,
        bindings: &mut Bindings,
    ) -> bool {
        pat.as_dyn().try_match(&self.ctx(), output, bindings)
    }

    /// Match a `NodeId` against a pattern via
    /// [`Pattern::try_match_node`](crate::pattern::pat::traits::Pattern::try_match_node)
    /// — which iterates the node's outputs (default impl) or matches the
    /// node directly (zero-output nodes like `Return`, via `NodePat`'s
    /// override).
    pub(crate) fn match_node_id(&self, node: NodeId, pat: &Pat, bindings: &mut Bindings) -> bool {
        pat.as_dyn().try_match_node(&self.ctx(), node, bindings)
    }

    /// Top-level "match with options" entry point used by `NodePat::try_once`
    /// when walking sub-pattern inputs.  Tries direct match first; on
    /// failure, falls back to walk-through helpers gated by
    /// [`MatcherOptions`].
    ///
    /// **Cast walk-through is iterative** (a tight inner loop) so chained
    /// casts (e.g. `Extend(Truncate(Mul))`, x86 register-merge towers,
    /// or an adversarial input with thousands of nested casts) cannot
    /// blow the stack.  At each cast level we re-attempt the direct
    /// match before unwrapping further, exactly matching the
    /// semantics of the previous recursive helper.
    ///
    /// **Region walk-through stays recursive** through
    /// [`walk_through::try_walk_through_region`]: each branch
    /// of a region join is tried as a separate alternative, and the
    /// recursion depth equals the nested-join depth (bounded by CFG
    /// structure, not graph size).
    pub(crate) fn match_output_with_walk_through(
        &self,
        out: NodeOutputId,
        pat: &Pat,
        b: &mut Bindings,
    ) -> bool {
        // Recursion-depth guard for the mutual recursion with
        // `walk_through::try_walk_through_region`.  Region
        // walk-through fans out across region predecessors and each
        // alternative re-enters `match_output_with_walk_through`; nesting
        // depth equals the IR's CS-nesting depth in practice, which is
        // bounded by CFG structure.  An adversarial graph with thousands
        // of nested joins would blow the stack before the per-test
        // wallclock budget triggers — the cap surfaces a clean false
        // (no match) instead.
        struct DepthGuard;
        thread_local! {
            static MATCH_DEPTH: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
        }
        const MAX_WALK_THROUGH_DEPTH: usize = 512;
        if MATCH_DEPTH.with(|d| {
            let v = d.get();
            if v > MAX_WALK_THROUGH_DEPTH {
                true
            } else {
                d.set(v + 1);
                false
            }
        }) {
            return false;
        }
        impl Drop for DepthGuard {
            fn drop(&mut self) {
                MATCH_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            }
        }
        let _guard = DepthGuard;

        // Iterative cast-chain unwrapping.  Each iteration tries direct
        // match at the current `out`; on failure, if the producer is a
        // walk-through cast, advance `out` to its value input and
        // re-try.  Restores bindings between attempts so a successful
        // match never sees stale partial state from a failed sibling.
        let mut out = out;
        loop {
            let mark = b.mark();
            if self.match_output(out, pat, b) {
                return true;
            }
            b.restore(mark);

            // Cast walk-through: if `out`'s producer is a registered
            // cast, unwrap and loop.  Inlines what
            // `try_walk_through_cast` did via recursion.
            if !self.options.ignore_cast_mask.is_empty() {
                let producer = self.function.node_for_output(out);
                let bit = cast_mask::cast_mask_of(
                    self.function.node_kind(producer),
                );
                if !bit.is_empty()
                    && self.options.ignore_cast_mask.contains(bit)
                    && let Some(value_input) = self.function.node_inputs(producer).into_iter().next()
                {
                    out = value_input;
                    continue;
                }
            }

            break;
        }

        // Region walk-through fan-out — try each region join
        // input as an alternative.  Recursion here is bounded by the
        // CS-nesting depth of the IR, not by graph size, so the
        // helper stays recursive.
        if self.options.ignore_regions
            && walk_through::try_walk_through_region(&self.ctx(), out, pat, b)
        {
            return true;
        }

        false
    }
}

/// True when every capture in `m`'s bindings that also appears in any
/// previously-collected match in `prefix` binds to the same
/// [`crate::pattern::matcher::bindings::Binding`].  Helper for
/// [`Matcher::find_all_requirements`].
///
/// Captures local to `m` (not seen in `prefix`) impose no constraint —
/// they are unique to this pattern and join-trivial.
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    //! Unit tests for the [`MatcherOptions`] builder-state contract.
    //!
    //! The 9 tests below pin the chaining algebra of the matcher's
    //! ignore-flags: defaults, mask widening, mask union across
    //! repeated calls, and the independence of `ignore_casts` /
    //! `ignore_regions`.  Restored from the pre-rewrite
    //! `crates/pattern/tests/matching/matcher_api.rs` builder-state
    //! suite.

    use super::*;
    use strider_ir::node::NodeOutputType;

    fn trivial_graph() -> strider_ir::Function {
        strider_ir_test_utils::make_empty_fn(|b| {
            b.build_int_const(0u64, NodeOutputType::U64)
        })
        .expect("trivial graph")
    }

    #[test]
    fn matcher_default_options_are_both_off() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function).expect("matcher");
        assert!(
            m.options.ignore_cast_mask.is_empty(),
            "ignore_cast_mask must default to empty"
        );
        assert!(
            !m.options.ignore_regions,
            "ignore_regions must default to false"
        );
    }

    #[test]
    fn matcher_default_ignore_cast_mask_is_empty() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function).expect("matcher");
        assert!(
            m.options.ignore_cast_mask.is_empty(),
            "default ignore_cast_mask must be empty"
        );
    }

    #[test]
    fn ignore_casts_sets_mask_to_all() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function).expect("matcher").ignore_casts();
        assert_eq!(
            m.options.ignore_cast_mask,
            CastMask::all(),
            "ignore_casts() must set the mask to CastMask::all()"
        );
    }

    #[test]
    fn ignore_casts_mask_sets_just_truncate() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function)
            .expect("matcher")
            .ignore_casts_mask(CastMask::TRUNCATE);
        assert_eq!(
            m.options.ignore_cast_mask,
            CastMask::TRUNCATE,
            "ignore_casts_mask(TRUNCATE) must set only the TRUNCATE bit"
        );
    }

    #[test]
    fn ignore_casts_mask_unions_repeated_calls() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function)
            .expect("matcher")
            .ignore_casts_mask(CastMask::TRUNCATE)
            .ignore_casts_mask(CastMask::EXTEND);
        assert_eq!(
            m.options.ignore_cast_mask,
            CastMask::TRUNCATE | CastMask::EXTEND,
            "repeated ignore_casts_mask calls must union"
        );
    }

    #[test]
    fn ignore_casts_after_mask_widens_to_all() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function)
            .expect("matcher")
            .ignore_casts_mask(CastMask::TRUNCATE)
            .ignore_casts();
        assert_eq!(
            m.options.ignore_cast_mask,
            CastMask::all(),
            "ignore_casts() after a mask must widen to all()"
        );
    }

    #[test]
    fn ignore_casts_chains_and_flips_flag() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function).expect("matcher").ignore_casts();
        assert_eq!(
            m.options.ignore_cast_mask,
            CastMask::all(),
            "ignore_casts() must set the mask to CastMask::all()"
        );
        assert!(
            !m.options.ignore_regions,
            "ignore_casts() must not touch ignore_regions"
        );
    }

    #[test]
    fn ignore_regions_chains_and_flips_flag() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function)
            .expect("matcher")
            .ignore_regions();
        assert!(
            m.options.ignore_regions,
            "ignore_regions() must enable the flag"
        );
        assert!(
            m.options.ignore_cast_mask.is_empty(),
            "ignore_regions() must not touch ignore_cast_mask"
        );
    }

    #[test]
    fn both_flags_chain_independently() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function)
            .expect("matcher")
            .ignore_casts()
            .ignore_regions();
        assert_eq!(m.options.ignore_cast_mask, CastMask::all());
        assert!(m.options.ignore_regions);
    }

    #[test]
    fn ignore_flags_chain_independently() {
        let function = trivial_graph();
        let m = Matcher::try_new(&function)
            .expect("matcher")
            .ignore_casts()
            .ignore_regions();
        assert_eq!(m.options.ignore_cast_mask, CastMask::all());
        assert!(m.options.ignore_regions);
    }
}
