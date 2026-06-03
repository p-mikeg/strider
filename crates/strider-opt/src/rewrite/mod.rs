//! Rewriter infrastructure: [`RewriteCtx`] / [`RewriteCtxView`],
//! [`GraphRewriter`], [`BoxedRule`] / [`boxed_rule`],
//! [`apply_rules_in_order`], [`GraphRewriteCtxExt`], and the
//! [`rewrite_rule`] / [`rewrite_rule_runtime`] constructors.
//!
//! Buildability of a rewrite RHS is a **compile-time** property:
//! [`rewrite_rule`] bounds its RHS on [`TemplatePat`], which is only
//! implemented by buildable typed structs — a wildcard RHS is a compile
//! error. [`rewrite_rule_runtime`] is the dynamic (FFI) counterpart that
//! takes an already-built match [`Pattern`] LHS and an already-built
//! [`Template`] RHS. A [`Template`] is buildable by construction (every
//! node has a build kind or is capture-only), so the only construction
//! check is that the RHS's captures are LHS-bound
//! (`check_capture_coverage`).
//!
//! The asm-fingerprint absorption contract is preserved verbatim: every
//! freshly-created interior node of the RHS subtree absorbs the rewrite
//! root's fingerprint via
//! [`Function::extend_asm_fingerprint_from`](strider_ir::Function::extend_asm_fingerprint_from),
//! superset-only. The [`RewriteSkip`](strider_pattern::RewriteSkip)
//! sentinel is also preserved: a closure inside the RHS may return
//! `Err(strider_pattern::skip())`; the interpreter detects it via
//! [`strider_pattern::is_skip`] and returns `Ok(false)`.

use cranelift_entity::EntityRef;
use entity_utils::DenseEntitySet;
use strider_ir::node::{
    NodeId, UseId, NodeKind, ValueId, ValueKind, ValueType,
};
use strider_ir::{Function, Graph};

use strider_pattern::Capture;
use strider_pattern::{Result, is_skip};
use strider_pattern::MatchPat;
use strider_pattern::Matcher;
use strider_pattern::Pattern;
use strider_pattern::{Template, instantiate};
use strider_pattern::TemplatePat;

// ── rule constructors ────────────────────────────────────────────────

/// Build a rewrite-rule closure from a typed LHS and a **buildable**
/// typed RHS.
///
/// Buildability is enforced at compile time via the `T: TemplatePat`
/// bound — a wildcard RHS (e.g. `add(any(), int_const(1))`) does not
/// implement [`TemplatePat`] and is therefore a compile error.
///
/// The returned closure takes `&mut RewriteCtx<'g>` and a candidate root
/// [`NodeId`], attempts the match, and on success materialises the RHS
/// via [`instantiate`] and redirects the root's value output to the
/// built output via
/// [`Graph::replace_all_uses`](strider_ir::Graph::replace_all_uses).
///
/// Returns `Ok(Some(new_out))` if the rule fired and at least one use
/// was redirected — `new_out` is the RHS-built output, which the
/// peephole driver re-examines for cascading folds. Returns `Ok(None)`
/// if the match failed, the RHS produced a
/// [`RewriteSkip`](strider_pattern::RewriteSkip), or `replace_all_uses`
/// found nothing to redirect.
///
/// # Single-value-output constraint
///
/// The LHS root must have exactly one value output — the rule redirects
/// that output's uses to the RHS-built output. Rooting a rule on a
/// multi-output node returns an error from `node_outputs_exact::<1>`.
///
/// # Panics
///
/// Panics if the RHS references a [`Capture`] the LHS does not bind
/// (`check_capture_coverage`) — a programming error at the rule's
/// authoring site, surfaced eagerly at construction.
#[allow(clippy::expect_used)]
pub fn rewrite_rule<L: MatchPat + 'static, T: TemplatePat + 'static>(
    lhs: L,
    rhs: T,
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>> + 'static {
    let lhs_pat = lhs.into_pattern();
    let rhs_tpl = rhs.into_template();
    check_capture_coverage(&lhs_pat, &rhs_tpl).expect("rewrite_rule: RHS capture not bound by LHS");
    rewrite_rule_impl(lhs_pat, rhs_tpl)
}

/// Build a rewrite-rule closure from an already-built match [`Pattern`]
/// LHS and an already-built [`Template`] RHS — the dynamic (FFI /
/// scripted) counterpart of [`rewrite_rule`].
///
/// A [`Template`] is buildable by construction (every node carries a
/// build kind or is capture-only), so the only construction check is
/// that every capture the RHS references is bound by the LHS
/// (`check_capture_coverage`).
///
/// # Output-signature validity is author-owned
///
/// The rewrite path materialises the RHS via [`instantiate`], which calls
/// `Graph::create_node` with the [`Template`]'s **declared** output
/// signature and **never runs** [`strider_ir::validate`]. The author of
/// the RHS therefore owns two invariants that nothing downstream checks:
///
/// * Every template node's declared output signature must match its
///   `NodeKind`'s real `expected_signature` (kind + slot count + types).
/// * No two producers may be wired into the same input slot (`instantiate`
///   collects inputs into a `BTreeMap` keyed by slot, so a duplicate slot
///   silently drops the earlier edge).
///
/// The typed `template::` builders (`template::int_binary`, …) guarantee
/// both by construction and are always safe. A [`Template`] hand-built via
/// the raw [`TemplateBuilder`](strider_pattern::template::TemplateBuilder) `node` /
/// `input` / `*_output` verbs does **not** — it can declare a
/// structurally-invalid IR node, and because the rewrite path skips
/// `validate`, the invalidity is not caught here. Authors using the raw
/// verbs must uphold these invariants themselves.
///
/// # Errors
///
/// Returns an error if the RHS references a capture the LHS does not
/// bind.
pub fn rewrite_rule_runtime(
    lhs: Pattern,
    rhs: Template,
) -> Result<impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>> + 'static> {
    check_capture_coverage(&lhs, &rhs)?;
    Ok(rewrite_rule_impl(lhs, rhs))
}

/// Shared body for [`rewrite_rule`] and [`rewrite_rule_runtime`].
///
/// On each candidate root: match the LHS, fetch the root's single value
/// output + type, snapshot the next `NodeId`, instantiate the RHS,
/// absorb the rewrite root's fingerprint into every freshly created
/// interior node, then redirect the root's uses to the built output.
fn rewrite_rule_impl(
    lhs: Pattern,
    rhs: Template,
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>> + 'static {
    move |ctx: &mut RewriteCtx<'_>, node: NodeId| -> Result<Option<ValueId>> {
        // 1. Match LHS. Keep the matcher borrow tight so we can mutate
        //    `ctx.function` afterwards.
        let bindings = {
            let matcher = Matcher::try_new(ctx.function)?;
            match matcher.match_at(node, &lhs)? {
                Some(m) => m.bindings_clone(),
                None => return Ok(None),
            }
        };

        // 2. Fetch root's single value output and its type.
        let [root_value] = ctx.function.node_outputs_exact::<1>(node)?;
        let root_ty = ctx.function.value_kind(root_value).as_value_or_err()?;

        // 3. Materialise RHS. A closure inside the tree may opt out via
        //    `Err(strider_pattern::skip())`; catch the sentinel here and
        //    convert it to "no change". Snapshot the next-NodeId BEFORE
        //    the build so we can identify which interior nodes are
        //    freshly allocated (vs returned as dedup-cache hits on
        //    pre-existing nodes).
        let pre_build_node_id = ctx.function.graph().next_node_id();
        let new_value = match instantiate(&rhs, ctx.function, &bindings, node, root_ty) {
            Ok(value) => value,
            Err(e) if is_skip(&e) => return Ok(None),
            Err(e) => return Err(e),
        };

        // 4. Absorb the rewritten root's asm-fingerprint into EVERY
        //    freshly-created interior node of the RHS subtree (superset
        //    -only). The walk is bounded by `pre_build_node_id`: any
        //    NodeId allocated before the build is pre-existing and stays
        //    untouched.
        let new_node = ctx.function.producer(new_value);
        ctx.function.extend_asm_fingerprint_from(new_node, node);
        absorb_fingerprints_into_fresh_subtree(ctx.function, new_node, node, pre_build_node_id);

        // 5. Redirect every consumer of the old root's value output
        //    to the new output.  `replace_all_uses` returns `true`
        //    when at least one use was redirected; surface the
        //    RHS-built output so the peephole driver can re-examine the
        //    freshly-created node for cascading folds.
        let changed = ctx.function.graph_mut().replace_all_uses(root_value, new_value)?;
        Ok(changed.then_some(new_value))
    }
}

// ── construction-time checks ─────────────────────────────────────────

/// Walk every RHS template node; for each capture-bearing node assert
/// that the capture also appears somewhere in the LHS. An
/// unbound-in-LHS capture would surface as a missing binding at apply
/// time — catching it at construction turns a runtime bug into a
/// build-time error at the rule's authoring site.
///
/// This is the **only** RHS construction check: a [`Template`] is
/// structurally buildable by construction (every node carries a build
/// kind or a capture), so there is no separate buildability assertion.
fn check_capture_coverage(lhs: &Pattern, rhs: &Template) -> Result<()> {
    // LHS captures live on the value side (the producing output vertex)
    // for value captures, and on the node for value-less roots — both are
    // collected by `Pattern::bound_captures`.
    let lhs_caps: rustc_hash::FxHashSet<Capture> = lhs.bound_captures().collect();
    // RHS captures live on the value side (a `ValueCapture` output).
    for cap in rhs.referenced_captures() {
        if !lhs_caps.contains(&cap) {
            return Err(anyhow::anyhow!(
                "RHS references Capture id={} that the LHS does not bind",
                cap.id()
            ));
        }
    }
    Ok(())
}

/// Walk freshly-allocated interior nodes (id ≥ `snapshot`) reachable
/// upward from `new_node` and absorb `contributor`'s asm-fingerprint
/// into each. Pre-existing input nodes (id < snapshot) bound the walk:
/// they're outside the rewrite and stay untouched.
fn absorb_fingerprints_into_fresh_subtree(
    function: &mut Function,
    new_node: NodeId,
    contributor: NodeId,
    snapshot: NodeId,
) {
    let mut visited: DenseEntitySet<NodeId> = DenseEntitySet::new();
    visited.insert(new_node);
    let mut stack: Vec<NodeId> = function
        .node_inputs(new_node)
        .into_iter()
        .map(|inp| function.producer(inp))
        .collect();
    while let Some(cur) = stack.pop() {
        if !visited.insert(cur) {
            continue;
        }
        if cur.index() < snapshot.index() {
            // Pre-existing node — outside the rewrite.
            continue;
        }
        function.extend_asm_fingerprint_from(cur, contributor);
        let inputs: Vec<_> = function.node_inputs(cur).into_iter().collect();
        for inp in inputs {
            stack.push(function.producer(inp));
        }
    }
}

// ── RewriteCtx / RewriteCtxView ──────────────────────────────────────

/// Rewrite context: a borrowed `&mut Function`. Used by
/// [`rewrite_rule`] and destructive optimizer passes.
///
/// The function's entry [`NodeId`] is derived on demand via
/// [`Self::entry`]; the wrapped function is required to be in its built
/// form (`function.entry()` is `Some(_)`), checked at construction time
/// by [`Self::try_for_built`].
pub struct RewriteCtx<'g> {
    pub(crate) function: &'g mut Function,
}

/// Read-only `&Function` view used by opt's read-only public API.
/// `Copy` and cheap to pass.
#[derive(Clone, Copy)]
pub struct RewriteCtxView<'g> {
    pub(crate) function: &'g Function,
}

impl<'g> RewriteCtx<'g> {
    /// Constructs a `RewriteCtx` borrowing from a [`Function`]'s built
    /// form (i.e. `entry` is populated).
    ///
    /// # Errors
    ///
    /// Returns an error if the function has not been built.
    pub fn try_for_built(function: &'g mut Function) -> Result<Self> {
        let _entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("RewriteCtx::try_for_built: entry node is not set"))?;
        Ok(Self { function })
    }

    /// Pre-order graph walk starting at [`Self::entry`].
    pub fn walk(&self) -> strider_ir::walk::GraphWalk<'_> {
        self.function.graph().walk_from(self.entry())
    }

    /// Kind-filtered pre-order walk.
    pub fn walk_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&strider_ir::node::NodeKind) -> bool + 'a,
    {
        let g: &Graph = self.function.graph();
        self.walk().filter(move |&n| pred(g.node_kind(n)))
    }

    /// Entry-reachable nodes in **global reverse-post-order** (entry-first),
    /// filtered by a predicate over each node's kind.  The reachable SET
    /// matches [`Self::walk_kind`]; only the ORDER is canonicalised to RPO
    /// (every producer precedes its consumers), so worklist-seeding and
    /// node scans settle in fewer iterations.
    pub fn rpo_filter<'a>(
        &'a self,
        pred: impl Fn(&strider_ir::node::NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.function.rpo_filter(pred)
    }

    /// Read-only access to the wrapped structural [`Graph`].
    pub fn graph_ref(&self) -> &Graph {
        self.function.graph()
    }

    /// Read-only access to the wrapped [`Function`].
    pub fn function_ref(&self) -> &Function {
        self.function
    }

    /// Function-entry `NodeId` anchor.
    #[allow(clippy::expect_used)]
    pub fn entry(&self) -> NodeId {
        self.function.entry().expect(
            "RewriteCtx wraps a built Function with an entry node (try_for_built invariant)",
        )
    }

    /// Lightweight read-only `&Function` view.
    pub fn as_view(&self) -> RewriteCtxView<'_> {
        RewriteCtxView {
            function: self.function,
        }
    }

    // ── forwarded read methods ───────────────────────────────────────
    //
    // Shared-read delegators onto the wrapped `&mut Function` (auto-
    // reborrowed as `&`).  These let opt passes and helpers keep calling
    // `ctx.<m>(..)` for structural reads without naming `function_ref()`.

    /// Delegates to [`Function::node_kind`].
    pub fn node_kind(&self, node_id: NodeId) -> &strider_ir::node::NodeKind {
        self.function.node_kind(node_id)
    }

    /// Delegates to [`Function::node_inputs`].
    pub fn node_inputs(&self, node_id: NodeId) -> strider_ir::Inputs<'_> {
        self.function.node_inputs(node_id)
    }

    /// Delegates to [`Graph::node_inputs_exact`].
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` inputs.
    pub fn node_inputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> strider_ir::error::Result<[ValueId; N]> {
        self.function.graph().node_inputs_exact(node_id)
    }

    /// Delegates to [`Function::node_outputs`].
    pub fn node_outputs(&self, node_id: NodeId) -> &[ValueId] {
        self.function.node_outputs(node_id)
    }

    /// Delegates to [`Function::node_outputs_exact`].
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` outputs.
    pub fn node_outputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> strider_ir::error::Result<[ValueId; N]> {
        self.function.node_outputs_exact(node_id)
    }

    /// Delegates to [`Function::value_kind`].
    pub fn value_kind(&self, output_id: ValueId) -> ValueKind {
        self.function.value_kind(output_id)
    }

    /// Delegates to [`Function::producer`].
    pub fn producer(&self, output_id: ValueId) -> NodeId {
        self.function.producer(output_id)
    }

    // ── mutation façade ──────────────────────────────────────────────
    //
    // `RewriteCtx` is read-only over `Function` (it exposes `Deref` but
    // NOT `DerefMut`, and has no `function_mut`/`graph_mut` escape).
    // Every mutation a pass performs routes through one of the curated
    // methods below, which delegate to the private `&mut Function`.
    //
    // Asm-fingerprint propagation stays automatic: there is no raw
    // `set_asm_fingerprint`/`extend_asm_fingerprint` here.  Passes that
    // need to stamp a fresh node's history use [`Self::create_node_attributed`]
    // (contributor-attributed creation) or [`Self::absorb_fingerprint`] (the
    // superset-only union primitive that opt-domain composite rewrites pair
    // with use-redirection).

    /// Create a node — delegates to [`Graph::create_node`].
    pub fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        self.function.graph_mut().create_node(kind, inputs, output_kinds)
    }

    /// Create a node and union every contributor's asm-fingerprint into
    /// it — delegates to [`Function::create_node_attributed`].  This is
    /// the fingerprint-aware creation path; passes use it instead of
    /// hand-stamping a fresh node.
    pub fn create_node_attributed(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
        contributors: &[NodeId],
    ) -> NodeId {
        self.function
            .create_node_attributed(kind, inputs, output_kinds, contributors)
    }

    /// Create (or dedup to) an `IntConst` of the given type — delegates
    /// to [`Graph::make_int_const`].
    ///
    /// # Errors
    /// Propagates [`Graph::make_int_const`]'s error arm (non-integer or
    /// wide `ty`, or a malformed output count).
    pub fn make_int_const(
        &mut self,
        val: impl Into<u128>,
        ty: ValueType,
    ) -> strider_ir::error::Result<ValueId> {
        self.function.graph_mut().make_int_const(val, ty)
    }

    /// Detach every input of `node` from its producers' use-lists —
    /// delegates to [`Graph::detach_node_inputs`].
    pub fn detach_node_inputs(&mut self, node: NodeId) {
        self.function.graph_mut().detach_node_inputs(node);
    }

    /// Redirect an input slot to a new producer output — delegates to
    /// [`Graph::update_input`].
    pub fn update_input(&mut self, input_id: UseId, output_id: ValueId) {
        self.function.graph_mut().update_input(input_id, output_id);
    }

    /// Append an input to a (non-cacheable) node — delegates to
    /// [`Graph::add_node_input`].
    ///
    /// # Errors
    /// Propagates [`Graph::add_node_input`]'s error arm.
    pub fn add_node_input(
        &mut self,
        node: NodeId,
        output_id: ValueId,
    ) -> strider_ir::error::Result<()> {
        self.function.graph_mut().add_node_input(node, output_id)
    }

    /// Remove the input at `index` from a (non-cacheable) node —
    /// delegates to [`Graph::remove_node_input`].
    ///
    /// # Errors
    /// Propagates [`Graph::remove_node_input`]'s error arm.
    pub fn remove_node_input(
        &mut self,
        node: NodeId,
        index: u32,
    ) -> strider_ir::error::Result<()> {
        self.function.graph_mut().remove_node_input(node, index)
    }

    /// Redirect every use of `old` to `new` — delegates to
    /// [`Graph::replace_all_uses`].
    ///
    /// A generic use-redirection primitive (no fingerprint work). Higher-level
    /// composites that pair this with fingerprint absorption layer on top of it
    /// (see [`Self::absorb_fingerprint`]).
    ///
    /// Returns `true` iff at least one use was redirected.
    ///
    /// # Errors
    /// Propagates [`Graph::replace_all_uses`]'s error arm unchanged.
    pub fn replace_all_uses(
        &mut self,
        old: ValueId,
        new: ValueId,
    ) -> strider_ir::error::Result<bool> {
        self.function.graph_mut().replace_all_uses(old, new)
    }

    /// Absorb `from_value`'s producer asm-fingerprint into `into_value`'s producer
    /// (superset-only union) — delegates to
    /// [`Function::extend_asm_fingerprint_from`].
    ///
    /// This is a SAFE primitive: it can only *grow* a node's fingerprint, never
    /// shrink it, so it is consistent with the read-only-`Function` discipline
    /// even though raw `set_asm_fingerprint`/`extend_asm_fingerprint` are NOT
    /// exposed. Composite rewrites pair it with use-redirection to keep the
    /// superset-only fingerprint contract automatic.
    pub fn absorb_fingerprint(&mut self, into_value: ValueId, from_value: ValueId) {
        let into = self.function.producer(into_value);
        let from = self.function.producer(from_value);
        self.function.extend_asm_fingerprint_from(into, from);
    }

    /// Record a concrete stack slot `(base, offset)` for a Store/Load
    /// node — delegates to [`Function::set_stack_offset`].
    pub fn set_stack_offset(&mut self, id: NodeId, base: ValueId, offset: i64) {
        self.function.set_stack_offset(id, base, offset);
    }

    /// Register an argument-carrier value under a CC argument index —
    /// delegates to [`Function::register_arg_value`].
    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.function.register_arg_value(index, value);
    }

    /// Drop every registered argument carrier — delegates to
    /// [`Function::clear_arg_values`].
    pub fn clear_arg_values(&mut self) {
        self.function.clear_arg_values();
    }

    /// Build a [`Matcher`] anchored at this context's wrapped function.
    #[allow(clippy::expect_used)]
    pub fn matcher(&self) -> Matcher<'_> {
        Matcher::try_new(self.function)
            .expect("RewriteCtx::matcher: try_for_built invariant guarantees a built Function")
    }

    // ── opt-domain composite rewrites ────────────────────────────────
    //
    // These compose the generic primitives above into the higher-level
    // operations the optimizer needs (value replacement with fingerprint
    // absorption, single-input redirection, region-predecessor removal).

    /// The single value-replacement primitive: redirect every use of `old`
    /// to `new`, after **absorbing** `old`'s producer asm-fingerprint into
    /// `new`'s producer (superset-only union).
    ///
    /// This is the one place that pairs fingerprint absorption with
    /// use-replacement — optimization passes call this instead of hand-writing
    /// the absorb + redirect pair, so the superset-only fingerprint contract has
    /// one implementation for value rewrites.
    ///
    /// Returns `true` iff at least one use was redirected.
    ///
    /// # Errors
    /// Propagates [`Self::replace_all_uses`]'s error arm unchanged.
    pub fn replace_value(&mut self, old: ValueId, new: ValueId) -> Result<bool> {
        self.absorb_fingerprint(new, old);
        self.replace_all_uses(old, new)
    }

    /// Redirect a single input slot from its current producer to `new`,
    /// absorbing the displaced producer's asm-fingerprint into `new`'s
    /// producer **iff** the redirect leaves the displaced producer with
    /// no remaining uses.
    ///
    /// The companion to [`Self::replace_value`] for the single-slot case:
    /// where `replace_value` redirects *every* use of a value,
    /// `redirect_input` rewires exactly one input edge. When the displaced
    /// producer becomes dead as a result, its contributing-asm history would
    /// otherwise be lost, so it is folded into the surviving consumer's new
    /// producer (superset-only union). When the displaced producer keeps other
    /// live uses, no absorption happens — those uses still explain its value via
    /// its own fingerprint, and contaminating `new`'s producer would violate the
    /// "fingerprint names the asm insns that contribute to this value" contract.
    pub fn redirect_input(&mut self, input_id: UseId, new: ValueId) {
        let old_value = self.graph_ref().value_of_use(input_id);
        let displaced_uses_before = self.graph_ref().value_uses(old_value).count();
        self.update_input(input_id, new);
        if displaced_uses_before == 1 {
            // `old_value` is the displaced producer's output; absorb its
            // fingerprint into `new`'s producer (superset-only union).
            self.absorb_fingerprint(new, old_value);
        }
    }

    /// Removes a batch of predecessor slots from a `Region` and the matching
    /// value slots from every `Phi`/`MemPhi` that consumes the Region's
    /// phi-token output — the single structural primitive for dropping dead
    /// control edges into a join.
    ///
    /// A `Region` produces `[control, phi_token]`; a `Phi`/`MemPhi` over it has
    /// inputs `[phi_token, val_pred0, val_pred1, …]`, so the value for Region
    /// predecessor `i` lives at phi input `i + 1`. Region/Phi nodes are exempt
    /// from the asm-fingerprint non-empty check, so no fingerprint work is needed.
    ///
    /// The caller passes ALL dead predecessor indices for the region at once;
    /// this method removes them highest-index-first internally so earlier
    /// removals never invalidate a later (lower) index — the caller does not
    /// need to pre-sort or remove one-by-one. Duplicate indices are deduped,
    /// and out-of-range indices are skipped per-node via bounds checks.
    ///
    /// # Errors
    /// Propagates [`Self::remove_node_input`]'s error arm.
    pub fn remove_region_predecessors(
        &mut self,
        region: NodeId,
        pred_indices: &[u32],
    ) -> Result<()> {
        debug_assert!(
            matches!(self.node_kind(region), NodeKind::Region),
            "remove_region_predecessors: node is not a Region",
        );
        if pred_indices.is_empty() {
            return Ok(());
        }
        // Highest-index-first, deduped: removing a higher slot never shifts a
        // lower one, so every remaining index stays valid across the batch.
        let mut indices: Vec<u32> = pred_indices.to_vec();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();

        // Collect the phi-token consumers once (the set of Phi/MemPhi nodes
        // doesn't change as we remove their value inputs).
        let phi_nodes: Vec<NodeId> = {
            let outputs = self.node_outputs(region);
            if outputs.len() >= 2 {
                let phi_value = outputs[1]; // ValueId: Copy
                self.graph_ref()
                    .value_uses(phi_value)
                    .map(|(n, _)| n)
                    .collect()
            } else {
                Vec::new()
            }
        };

        for pred_index in indices {
            let phi_input_idx = pred_index + 1;
            for &phi in &phi_nodes {
                if phi_input_idx < self.node_inputs(phi).len() as u32 {
                    self.remove_node_input(phi, phi_input_idx)?;
                }
            }
            if pred_index < self.node_inputs(region).len() as u32 {
                self.remove_node_input(region, pred_index)?;
            }
        }
        Ok(())
    }
}

impl<'g> RewriteCtxView<'g> {
    /// Pre-order graph walk starting at [`Self::entry`].
    pub fn walk(&self) -> strider_ir::walk::GraphWalk<'_> {
        self.function.graph().walk_from(self.entry())
    }

    /// Kind-filtered pre-order walk.
    pub fn walk_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&strider_ir::node::NodeKind) -> bool + 'a,
    {
        let g: &Graph = self.function.graph();
        self.walk().filter(move |&n| pred(g.node_kind(n)))
    }

    /// Entry-reachable nodes in **global reverse-post-order** (entry-first),
    /// filtered by a predicate over each node's kind.  See
    /// [`RewriteCtx::rpo_filter`].
    pub fn rpo_filter<'a>(
        &'a self,
        pred: impl Fn(&strider_ir::node::NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.function.rpo_filter(pred)
    }

    /// Read-only access to the wrapped `Graph`.
    pub fn graph_ref(&self) -> &Graph {
        self.function.graph()
    }

    /// Read-only access to the wrapped [`Function`].
    pub fn function_ref(&self) -> &Function {
        self.function
    }

    /// Function-entry `NodeId` anchor.
    #[allow(clippy::expect_used)]
    pub fn entry(&self) -> NodeId {
        self.function.entry().expect(
            "RewriteCtxView wraps a built Function with an entry node (from_built invariant)",
        )
    }

    // ── forwarded read methods ───────────────────────────────────────
    //
    // Shared-read delegators onto the wrapped `&Function`, mirroring the
    // set on [`RewriteCtx`] so read-only call sites keep working.

    /// Delegates to [`Function::node_kind`].
    pub fn node_kind(&self, node_id: NodeId) -> &strider_ir::node::NodeKind {
        self.function.node_kind(node_id)
    }

    /// Delegates to [`Function::node_inputs`].
    pub fn node_inputs(&self, node_id: NodeId) -> strider_ir::Inputs<'_> {
        self.function.node_inputs(node_id)
    }

    /// Delegates to [`Graph::node_inputs_exact`].
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` inputs.
    pub fn node_inputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> strider_ir::error::Result<[ValueId; N]> {
        self.function.graph().node_inputs_exact(node_id)
    }

    /// Delegates to [`Function::node_outputs`].
    pub fn node_outputs(&self, node_id: NodeId) -> &[ValueId] {
        self.function.node_outputs(node_id)
    }

    /// Delegates to [`Function::node_outputs_exact`].
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` outputs.
    pub fn node_outputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> strider_ir::error::Result<[ValueId; N]> {
        self.function.node_outputs_exact(node_id)
    }

    /// Delegates to [`Function::value_kind`].
    pub fn value_kind(&self, output_id: ValueId) -> ValueKind {
        self.function.value_kind(output_id)
    }

    /// Delegates to [`Function::producer`].
    pub fn producer(&self, output_id: ValueId) -> NodeId {
        self.function.producer(output_id)
    }

    /// Build a [`Matcher`] anchored at this view's wrapped function.
    #[allow(clippy::expect_used)]
    pub fn matcher(&self) -> Matcher<'g> {
        Matcher::try_new(self.function)
            .expect("RewriteCtxView::matcher: from_built invariant guarantees a built Function")
    }

    /// Borrows a built [`Function`] as a shared rewrite-context view.
    ///
    /// # Errors
    ///
    /// Returns an error if the function has not been built.
    pub fn from_built(function: &'g Function) -> Result<Self> {
        let _entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("RewriteCtxView::from_built: entry node is not set"))?;
        Ok(Self { function })
    }
}

impl<'a, 'g> From<&'a RewriteCtx<'g>> for RewriteCtxView<'a> {
    fn from(ctx: &'a RewriteCtx<'g>) -> Self {
        ctx.as_view()
    }
}

// ── GraphRewriteCtxExt — `with_rewrite_ctx` helper on Function ────────

/// Extension trait on [`strider_ir::Function`] providing a
/// `with_rewrite_ctx` callback that absorbs the
/// construct-then-pass pattern into one call.
pub trait GraphRewriteCtxExt {
    /// Borrow `self` as a [`RewriteCtx`] and run `f` with mutable
    /// access. The closure's `Result<T>` and the un-built case are
    /// merged into one outer `Result<T>`.
    ///
    /// # Errors
    ///
    /// Returns an error if `self.entry()` is `None`, or if `f` errors.
    fn with_rewrite_ctx<F, T>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&mut RewriteCtx<'_>) -> Result<T>;
}

impl GraphRewriteCtxExt for Function {
    fn with_rewrite_ctx<F, T>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&mut RewriteCtx<'_>) -> Result<T>,
    {
        let mut ctx = RewriteCtx::try_for_built(self)?;
        f(&mut ctx)
    }
}

// ── GraphRewriter — pattern-rewrite facade ──────────────────────────

/// Pattern-rewrite facade over [`rewrite_rule`]. Walks every reachable
/// node of the function under inspection and applies a rule at each
/// candidate, OR-composing per-node results into a single `bool`.
pub struct GraphRewriter;

impl GraphRewriter {
    /// Apply a single rule closure across every reachable node of the
    /// function wrapped by `ctx`. Returns `true` if it fired at least
    /// once.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by `rule`.
    pub fn apply<R>(ctx: &mut RewriteCtx<'_>, rule: R) -> Result<bool>
    where
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>>,
    {
        let nodes: Vec<NodeId> = ctx.function.walk().collect();
        let mut any = false;
        for n in nodes {
            if rule(ctx, n)?.is_some() {
                any = true;
            }
        }
        Ok(any)
    }

    /// Apply a slice of rules across every reachable node of the
    /// function wrapped by `ctx`.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by any rule.
    pub fn apply_rules<R>(ctx: &mut RewriteCtx<'_>, rules: &[R]) -> Result<bool>
    where
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>>,
    {
        Self::apply(ctx, apply_rules_in_order(rules))
    }

    /// Apply a single rule closure across every reachable node of
    /// `function`, returning the total per-node fire count.
    ///
    /// # Errors
    ///
    /// Returns an error if `function.entry()` is `None`, or if the rule
    /// closure returns a non-skip error.
    pub fn apply_count<R>(function: &mut Function, rule: R) -> Result<usize>
    where
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>>,
    {
        let mut ctx = RewriteCtx::try_for_built(function)?;
        let candidates: Vec<NodeId> = ctx.function.walk().collect();
        let mut applied: usize = 0;
        for node in candidates {
            if rule(&mut ctx, node)?.is_some() {
                applied += 1;
            }
        }
        Ok(applied)
    }

    /// Apply a slice of rules across every reachable node of `function`
    /// round-robin, returning the total per-rule per-node fire count.
    ///
    /// # Errors
    ///
    /// Propagates the first error from any rule, or surfaces the
    /// un-built case.
    pub fn apply_rules_count<R>(function: &mut Function, rules: &[R]) -> Result<usize>
    where
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>>,
    {
        let mut ctx = RewriteCtx::try_for_built(function)?;
        let candidates: Vec<NodeId> = ctx.function.walk().collect();
        let mut applied: usize = 0;
        for node in candidates {
            for r in rules {
                if r(&mut ctx, node)?.is_some() {
                    applied += 1;
                }
            }
        }
        Ok(applied)
    }
}

// ── apply_rules_in_order / BoxedRule / boxed_rule ────────────────────

/// Compose a list of rewrite-rule closures into a single closure.
///
/// The returned closure iterates every rule in `rules` on the same
/// root node.  Once the first rule fires the root's uses are
/// redirected — subsequent rules then see the new graph state and may
/// or may not still apply; this mirrors the "run every rule, once"
/// policy from strider-orchestrator.
///
/// Returns `Ok(Some(new_out))` if at least one rule fired — `new_out`
/// is the output produced by the **last** rule to fire (the one whose
/// redirect won, so it names the surviving freshly-built node for the
/// peephole driver to re-examine).  Returns `Ok(None)` if no rule
/// fired.
///
/// Borrows `rules` as a slice and returns a closure bound to that
/// borrow's lifetime, so callers can hoist the rule vec into a
/// `LazyLock` (or any other long-lived storage) and compose the
/// per-call closure cheaply.
pub fn apply_rules_in_order<R>(
    rules: &[R],
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>> + '_
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>>,
{
    move |ctx, node| {
        let mut last: Option<ValueId> = None;
        for r in rules {
            if let Some(out) = r(ctx, node)? {
                last = Some(out);
            }
        }
        Ok(last)
    }
}

/// Type-erased rewrite-rule closure.
///
/// Each call to [`rewrite_rule`] returns a distinct opaque `impl Fn`
/// type, so a `Vec<impl Fn>` can only hold rules with identical
/// signatures — in practice, only a single rule.  Consumers
/// composing a list of heterogeneous rules need to box each one to a
/// common trait-object type; this alias plus [`boxed_rule`] factor
/// that boilerplate out of every call site.
pub type BoxedRule =
    Box<dyn for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>>>;

/// Wraps a rewrite-rule closure in a [`BoxedRule`].
pub fn boxed_rule<R>(r: R) -> BoxedRule
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<ValueId>> + 'static,
{
    Box::new(r)
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]
mod tests {
    //! Verification for the opt-domain composite mutations
    //! ([`RewriteCtx::replace_value`] and
    //! [`RewriteCtx::remove_region_predecessors`]). Both build a *built*
    //! `Function` (entry set) so `RewriteCtx::try_for_built` succeeds.

    use super::RewriteCtx;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{FunctionBuilder, IntBinaryOp};
    use strider_ir_test_utils::{RegisterSet, reg_vn};

    // ── replace_value ────────────────────────────────────────────────

    /// `replace_value` absorbs the old producer's asm-fingerprint into the
    /// new producer (superset union) and redirects every use of `old` to
    /// `new`.
    #[test]
    fn replace_value_absorbs_fingerprint_and_redirects_uses() {
        let mut b: FunctionBuilder = RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region");

        // old: IntConst(10) stamped with fingerprint 0xAA.
        b.set_lift_addr(Some(0xAA));
        let old_value = b.build_int_const(10u64, ValueType::I64).unwrap();
        // new: IntConst(20) stamped with fingerprint 0xBB.
        b.set_lift_addr(Some(0xBB));
        let new_value = b.build_int_const(20u64, ValueType::I64).unwrap();
        // sink: Add(old, old) — two uses of old_value.
        let sink = b
            .build_int_binary_operation(old_value, old_value, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(sink), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let new_node = function.producer(new_value);
        let sink_node = function.producer(sink);

        let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
        let changed = ctx.replace_value(old_value, new_value).unwrap();
        assert!(changed, "a live use existed → changed");

        // new_node absorbs old_node's fingerprint (superset) while keeping
        // its own.
        let fp = function.asm_fingerprint(new_node);
        assert!(
            fp.contains(&0xAA),
            "absorbed old's fingerprint 0xAA: {fp:?}"
        );
        assert!(
            fp.contains(&0xBB),
            "kept new's own fingerprint 0xBB: {fp:?}"
        );

        // sink now refers to new_value for all inputs.
        let sink_inputs: Vec<_> = function.node_inputs(sink_node).into_iter().collect();
        assert_eq!(
            sink_inputs,
            vec![new_value, new_value],
            "sink inputs must now point at new_value"
        );

        // old_value has no remaining uses.
        assert_eq!(
            function.graph().value_uses(old_value).count(),
            0,
            "old_value must have no remaining uses"
        );
    }

    /// With no uses to redirect, `replace_value` returns `false` but STILL
    /// absorbs the old producer's fingerprint into the new one.
    #[test]
    fn replace_value_no_uses_returns_false() {
        let mut b: FunctionBuilder = RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region");

        // old has fingerprint 0xAA but is wired to nothing.
        b.set_lift_addr(Some(0xAA));
        let old_value = b.build_int_const(1u64, ValueType::I64).unwrap();
        // new (the Return value) has fingerprint 0xBB.
        b.set_lift_addr(Some(0xBB));
        let new_value = b.build_int_const(2u64, ValueType::I64).unwrap();
        // Only `new_value` is used (by the Return); `old_value` is unused.
        b.build_return(Some(new_value), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let new_node = function.producer(new_value);

        let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
        let changed = ctx.replace_value(old_value, new_value).unwrap();
        assert!(!changed, "no uses of old → changed must be false");

        // Fingerprint is still absorbed even with no uses redirected.
        let fp = function.asm_fingerprint(new_node);
        assert!(
            fp.contains(&0xAA),
            "fingerprint absorbed even when no uses redirected: {fp:?}"
        );
        assert!(
            fp.contains(&0xBB),
            "kept new's own fingerprint 0xBB: {fp:?}"
        );
    }

    // ── remove_region_predecessors ────────────────────────────────────

    /// A 2-predecessor `Region` with a value `Phi` over it: removing
    /// predecessor 0 strips the first control slot from the Region AND the
    /// matching value slot (phi index 1) from the Phi, leaving 1 control
    /// input on the Region and `[token, surviving_value]` on the Phi.
    #[test]
    fn remove_region_predecessors_strips_ctrl_and_phi_slot() {
        // Build `if (true) { var = 1 } else { var = 2 }; return var;` so the
        // `join` Region has two control predecessors and a 2-value VarPhi.
        let var = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(var).arg(var).build_fn().unwrap();
        let entry = b.create_region().unwrap();
        let true_r = b.create_region().unwrap();
        let false_r = b.create_region().unwrap();
        let join = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();

        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, true_r, false_r).unwrap();

        b.set_region(true_r);
        let v_t = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.write_variable(&var, v_t).unwrap();
        b.build_branch(join).unwrap();

        b.set_region(false_r);
        let v_f = b.build_int_const(2u64, ValueType::I64).unwrap();
        b.write_variable(&var, v_f).unwrap();
        b.build_branch(join).unwrap();

        b.set_region(join);
        let merged = b.read_variable(&var).unwrap();
        b.build_return(Some(merged), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        // Locate the 2-value VarPhi at the join (inputs `[token, val0, val1]`)
        // and the Region it joins.  Filtering on input count = 3 skips any
        // single-predecessor VarPhi the builder may have produced for an
        // intermediate region.
        let phi = function
            .graph()
            .all_node_ids()
            .find(|&n| {
                matches!(function.node_kind(n), NodeKind::Phi)
                    && function.phi_var_tag(n) == Some(var)
                    && function.node_inputs(n).len() == 3
            })
            .expect("2-value VarPhi at the join must exist");
        let phi_token = function.node_inputs(phi)[0];
        let region = function.producer(phi_token);
        assert!(
            matches!(function.node_kind(region), NodeKind::Region),
            "phi token producer must be the join Region"
        );

        // Sanity: two control predecessors, phi inputs [token, val0, val1].
        assert_eq!(
            function.node_inputs(region).len(),
            2,
            "join region starts with 2 control predecessors"
        );
        let pre_phi_inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
        assert_eq!(pre_phi_inputs.len(), 3, "phi: [token, val0, val1]");
        // Capture pred-1's value (phi index 2) before removal.
        let pred1_val = pre_phi_inputs[2];

        // Act: remove predecessor 0 via the RewriteCtx.
        let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
        ctx.remove_region_predecessors(region, &[0])
            .expect("remove_region_predecessors must succeed");

        // Region drops to 1 control input.
        assert_eq!(
            function.node_inputs(region).len(),
            1,
            "region drops to 1 ctrl input"
        );

        // Phi must have exactly 2 inputs: [token, surviving value].
        let phi_inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
        assert_eq!(phi_inputs.len(), 2, "phi: [token, surviving value]");
        assert_eq!(phi_inputs[1], pred1_val, "surviving slot is pred 1's value");
    }
}
