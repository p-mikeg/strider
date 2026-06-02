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
//! superset-only. The [`RewriteSkip`](crate::error::RewriteSkip)
//! sentinel is also preserved: a closure inside the RHS may return
//! `Err(crate::error::skip())`; the interpreter detects it via
//! [`crate::error::is_skip`] and returns `Ok(false)`.

use cranelift_entity::EntityRef;
use entity_utils::DenseEntitySet;
use strider_ir::node::{
    NodeId, UseId, NodeKind, ValueId, ValueKind, ValueType,
};
use strider_ir::{Function, Graph};

use crate::capture::Capture;
use crate::error::{Result, is_skip};
use crate::match_pat::MatchPat;
use crate::matcher::Matcher;
use crate::pattern::Pattern;
use crate::template::{Template, instantiate};
use crate::template_pat::TemplatePat;

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
/// [`RewriteSkip`](crate::error::RewriteSkip), or `replace_all_uses`
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
/// the raw [`TemplateBuilder`](crate::template::TemplateBuilder) `node` /
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
            match matcher.match_at(node, &lhs) {
                Some(m) => m.bindings_clone(),
                None => return Ok(None),
            }
        };

        // 2. Fetch root's single value output and its type.
        let [root_out] = ctx.function.node_outputs_exact::<1>(node)?;
        let root_ty = ctx.function.value_kind(root_out).as_value_or_err()?;

        // 3. Materialise RHS. A closure inside the tree may opt out via
        //    `Err(crate::error::skip())`; catch the sentinel here and
        //    convert it to "no change". Snapshot the next-NodeId BEFORE
        //    the build so we can identify which interior nodes are
        //    freshly allocated (vs returned as dedup-cache hits on
        //    pre-existing nodes).
        let pre_build_node_id = ctx.function.graph().next_node_id();
        let new_out = match instantiate(&rhs, ctx.function, &bindings, node, root_ty) {
            Ok(out) => out,
            Err(e) if is_skip(&e) => return Ok(None),
            Err(e) => return Err(e),
        };

        // 4. Absorb the rewritten root's asm-fingerprint into EVERY
        //    freshly-created interior node of the RHS subtree (superset
        //    -only). The walk is bounded by `pre_build_node_id`: any
        //    NodeId allocated before the build is pre-existing and stays
        //    untouched.
        let new_node = ctx.function.producer(new_out);
        ctx.function.extend_asm_fingerprint_from(new_node, node);
        absorb_fingerprints_into_fresh_subtree(ctx.function, new_node, node, pre_build_node_id);

        // 5. Redirect every consumer of the old root's value output
        //    to the new output.  `replace_all_uses` returns `true`
        //    when at least one use was redirected; surface the
        //    RHS-built output so the peephole driver can re-examine the
        //    freshly-created node for cascading folds.
        let changed = ctx.function.graph_mut().replace_all_uses(root_out, new_out)?;
        Ok(changed.then_some(new_out))
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
    let lhs_caps: rustc_hash::FxHashSet<Capture> =
        lhs.graph.node_weights().filter_map(|n| n.capture).collect();
    for n in rhs.graph.node_weights() {
        if let Some(cap) = n.capture
            && !lhs_caps.contains(&cap)
        {
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
    #[must_use]
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
    #[must_use]
    pub fn graph_ref(&self) -> &Graph {
        self.function.graph()
    }

    /// Read-only access to the wrapped [`Function`].
    #[must_use]
    pub fn function_ref(&self) -> &Function {
        self.function
    }

    /// Function-entry `NodeId` anchor.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn entry(&self) -> NodeId {
        self.function.entry().expect(
            "RewriteCtx wraps a built Function with an entry node (try_for_built invariant)",
        )
    }

    /// Lightweight read-only `&Function` view.
    #[must_use]
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
    #[must_use]
    pub fn node_kind(&self, node_id: NodeId) -> &strider_ir::node::NodeKind {
        self.function.node_kind(node_id)
    }

    /// Delegates to [`Function::node_inputs`].
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn value_kind(&self, output_id: ValueId) -> ValueKind {
        self.function.value_kind(output_id)
    }

    /// Delegates to [`Function::producer`].
    #[must_use]
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

    /// Absorb `from_out`'s producer asm-fingerprint into `into_out`'s producer
    /// (superset-only union) — delegates to
    /// [`Function::extend_asm_fingerprint_from`].
    ///
    /// This is a SAFE primitive: it can only *grow* a node's fingerprint, never
    /// shrink it, so it is consistent with the read-only-`Function` discipline
    /// even though raw `set_asm_fingerprint`/`extend_asm_fingerprint` are NOT
    /// exposed. Composite rewrites pair it with use-redirection to keep the
    /// superset-only fingerprint contract automatic.
    pub fn absorb_fingerprint(&mut self, into_out: ValueId, from_out: ValueId) {
        let into = self.function.producer(into_out);
        let from = self.function.producer(from_out);
        self.function.extend_asm_fingerprint_from(into, from);
    }

    /// Record a concrete stack slot `(base, offset)` for a Store/Load
    /// node — delegates to [`Function::set_stack_offset`].
    pub fn set_stack_offset(&mut self, id: NodeId, base: ValueId, offset: i64) {
        self.function.set_stack_offset(id, base, offset);
    }

    /// Register an argument-carrier node under a CC argument index —
    /// delegates to [`Function::register_arg_node`].
    pub fn register_arg_node(&mut self, index: u32, node: NodeId) {
        self.function.register_arg_node(index, node);
    }

    /// Drop every registered argument carrier — delegates to
    /// [`Function::clear_arg_nodes`].
    pub fn clear_arg_nodes(&mut self) {
        self.function.clear_arg_nodes();
    }

    /// Build a [`Matcher`] anchored at this context's wrapped function.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn matcher(&self) -> Matcher<'_> {
        Matcher::try_new(self.function)
            .expect("RewriteCtx::matcher: try_for_built invariant guarantees a built Function")
    }
}

impl<'g> RewriteCtxView<'g> {
    /// Pre-order graph walk starting at [`Self::entry`].
    #[must_use]
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
    #[must_use]
    pub fn graph_ref(&self) -> &Graph {
        self.function.graph()
    }

    /// Read-only access to the wrapped [`Function`].
    #[must_use]
    pub fn function_ref(&self) -> &Function {
        self.function
    }

    /// Function-entry `NodeId` anchor.
    #[must_use]
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
    #[must_use]
    pub fn node_kind(&self, node_id: NodeId) -> &strider_ir::node::NodeKind {
        self.function.node_kind(node_id)
    }

    /// Delegates to [`Function::node_inputs`].
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn value_kind(&self, output_id: ValueId) -> ValueKind {
        self.function.value_kind(output_id)
    }

    /// Delegates to [`Function::producer`].
    #[must_use]
    pub fn producer(&self, output_id: ValueId) -> NodeId {
        self.function.producer(output_id)
    }

    /// Build a [`Matcher`] anchored at this view's wrapped function.
    #[must_use]
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
/// policy from strider-analyze.
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
