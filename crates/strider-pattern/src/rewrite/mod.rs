//! Rewriter infrastructure: [`Rewrite`], [`GraphRewriter`],
//! [`RewriteCtx`] / [`RewriteCtxView`], [`BoxedRule`] / [`boxed_rule`],
//! [`apply_rules_in_order`], and the [`rewrite_rule`] interpreter.
//!
//! Ported from `strider-analyze::pattern::rewrite` with two semantic
//! additions:
//!
//! 1. The typed [`Rewrite<L, T>`] entry runs construction-time soundness
//!    checks (capture-coverage + root-output-type agreement) so RHS-side
//!    bugs surface at rule-build time rather than at apply time.
//! 2. The interpreter preserves the asm-fingerprint absorption contract
//!    from the strider-analyze port — every freshly-created interior
//!    node of the RHS subtree absorbs the rewrite root's fingerprint
//!    via [`Function::extend_asm_fingerprint_from`].
//!
//! The [`RewriteSkip`](crate::error::RewriteSkip) sentinel semantics are
//! also preserved: a closure inside the RHS may return
//! `Err(crate::error::skip())`; the interpreter detects it via
//! [`crate::error::is_skip`] and returns `Ok(false)` instead of bubbling.

use std::marker::PhantomData;

use cranelift_entity::EntityRef;
use entity_utils::DenseEntitySet;
use strider_ir::node::{
    NodeId, NodeInputId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType,
};
use strider_ir::{Function, Graph};

use crate::builders::Pat;
use crate::capture::Capture;
use crate::error::{Result, is_skip};
use crate::matcher::{Matcher, Pattern};
use crate::pat_graph::{TemplateTy, Concrete, PatGraph, Role};
use crate::template::Template;

// ── Rewrite<L, T> — typed entry with construction-time soundness ─────

/// A typed rewrite rule.  `L: Pattern` matches; `T: Template` builds.
///
/// Construct one with [`Rewrite::new`] — its constructor enforces two
/// soundness invariants up front:
///
/// 1. **Capture coverage:** every [`Capture`] referenced anywhere in
///    the RHS must appear in the LHS, otherwise the RHS would
///    necessarily reference an unbound capture at apply time.
/// 2. **Root output-type agreement:** when both sides statically know
///    their root output type, they must agree.  Either side declaring
///    [`TemplateTy::InheritRoot`] (or `output_ty: None`) defers to apply
///    time and skips this check.
///
/// The runtime apply path is exposed via the [`rewrite_rule`] free
/// function — it accepts the same `(lhs, rhs)` pair as `Rewrite::new`
/// and returns the closure that drives one match-and-replace attempt.
pub struct Rewrite<L: Pattern, T: Template> {
    /// LHS — matched at every candidate root.
    pub lhs: L,
    /// RHS — materialised via [`Template::instantiate`] on a hit.
    pub rhs: T,
    // PhantomData so future fields stay struct-private.
    _marker: PhantomData<()>,
}

impl<RLhs> Rewrite<Pat<RLhs>, Pat<Concrete>>
where
    RLhs: Role,
    Pat<RLhs>: Pattern,
{
    /// Construct a `Rewrite` from an `RLhs`-roled LHS and a `Concrete`
    /// RHS.  Runs construction-time soundness checks (see the
    /// type-level docs).
    ///
    /// # Errors
    ///
    /// Returns an error if any [`Capture`] referenced by the RHS is not
    /// bound by the LHS, or if the two roots' statically-known output
    /// types disagree.
    pub fn new(lhs: Pat<RLhs>, rhs: Pat<Concrete>) -> Result<Self> {
        check_capture_coverage(&lhs.0, &rhs.0)?;
        check_root_ty_agreement(&lhs.0, &rhs.0)?;
        Ok(Self {
            lhs,
            rhs,
            _marker: PhantomData,
        })
    }
}

/// Walk every node of `rhs`; for each capture-bearing node assert that
/// the capture also appears somewhere in `lhs`.  An unbound-in-LHS
/// capture would surface as `MissingBinding` at apply time — catching
/// it at construction time turns a "fires once during apply" runtime
/// bug into a build-time error at the call site that authored the
/// rule.
fn check_capture_coverage<RL: Role, RR: Role>(
    lhs: &PatGraph<RL>,
    rhs: &PatGraph<RR>,
) -> Result<()> {
    let lhs_caps: rustc_hash::FxHashSet<Capture> = lhs
        .inner
        .node_weights()
        .filter_map(|nd| nd.capture)
        .collect();
    for nd in rhs.inner.node_weights() {
        if let Some(cap) = nd.capture
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

/// Compare the statically-known root output types when *both* sides
/// have one.  If either side defers to apply time (`InheritRoot` on
/// the RHS or `output_ty: None` on the LHS), this check is skipped.
fn check_root_ty_agreement<RL: Role, RR: Role>(
    lhs: &PatGraph<RL>,
    rhs: &PatGraph<RR>,
) -> Result<()> {
    let (Some(lhs_root), Some(rhs_root)) = (lhs.root, rhs.root) else {
        return Ok(());
    };
    let lhs_ty = lhs.inner.node_weight(lhs_root).and_then(|n| n.output_ty);
    let rhs_ty = match rhs.inner.node_weight(rhs_root) {
        Some(nd) => match &nd.template_spec {
            Some(bs) => match bs.ty {
                TemplateTy::InheritRoot => None,
                TemplateTy::Fixed(t) => Some(t),
            },
            // No build spec — capture-only RHS node — defers to apply time.
            None => nd.output_ty,
        },
        None => None,
    };
    if let (Some(l), Some(r)) = (lhs_ty, rhs_ty)
        && l != r
    {
        return Err(anyhow::anyhow!(
            "Rewrite root output types disagree: LHS={l:?} RHS={r:?}"
        ));
    }
    Ok(())
}

// ── rewrite_rule — match → build → redirect uses ─────────────────────

/// Build a rewrite-rule closure from an LHS [`Pat`] and a Concrete RHS
/// [`Pat`].
///
/// The returned closure takes `&mut RewriteCtx<'g>` and a candidate
/// root [`NodeId`], attempts the match, and on success materialises
/// the RHS template via [`Template::instantiate`] and redirects the
/// root's value output to the built output via
/// [`Graph::replace_all_uses`].
///
/// Returns `Ok(Some(new_out))` if the rule fired and at least one use
/// was redirected — `new_out` is the RHS-built output the root's uses
/// were redirected to.  Returns `Ok(None)` if the match failed, the
/// RHS produced a [`RewriteSkip`](crate::error::RewriteSkip), or
/// `replace_all_uses` found nothing to redirect.
///
/// Errors from the graph layer propagate as [`anyhow::Error`].
/// `crate::error::skip()` inside an RHS closure opts out of the
/// rewrite without surfacing a hard error; the interpreter detects
/// the sentinel via [`crate::error::is_skip`] and returns `Ok(None)`.
///
/// # Single-value-output constraint
///
/// The LHS root must have exactly one value output — the rule
/// redirects that output's uses to the RHS-built output.  Rooting a
/// rule on a multi-output node returns an error from
/// `node_outputs_exact::<1>`.
pub fn rewrite_rule<RLhs>(
    lhs: Pat<RLhs>,
    rhs: Pat<Concrete>,
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>> + 'static
where
    RLhs: Role + 'static,
    Pat<RLhs>: Pattern + 'static,
{
    rewrite_rule_impl(lhs, rhs)
}

/// Like [`rewrite_rule`] but accepts a `Pat<Wildcard>` RHS, validated
/// at construction time via
/// [`PatGraph::assert_concrete_at_runtime`](crate::pat_graph::PatGraph::assert_concrete_at_runtime).
///
/// The compile-time `Pat<Concrete>` bound on [`rewrite_rule`] is the
/// preferred path for Rust callers — buildability is enforced
/// statically.  Dynamic callers (Python bindings, scripted rewrites
/// built from a configuration file, etc.) can't always express the
/// `Concrete` role on the wire, so this variant accepts a `Wildcard`
/// RHS and converts the would-be type error into a runtime error at
/// rule-construction time (the caller's first opportunity to react).
///
/// Returns the same closure shape as [`rewrite_rule`].  The
/// per-`Template::instantiate` runtime check (also installed on
/// `Pat<Wildcard>: Template`) is the final guard; this function's
/// up-front check is purely an early-error convenience.
///
/// # Errors
///
/// Returns an error if the RHS would fail
/// [`PatGraph::assert_concrete_at_runtime`] — i.e. carries a kind-`Any`
/// node, a custom predicate, or any other match-only shape without a
/// build path.  Capture-coverage and root output-type agreement are
/// also checked, mirroring [`Rewrite::new`].
pub fn rewrite_rule_dynamic<RLhs>(
    lhs: Pat<RLhs>,
    rhs: Pat<crate::pat_graph::Wildcard>,
) -> Result<impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>> + 'static>
where
    RLhs: Role + 'static,
    Pat<RLhs>: Pattern + 'static,
{
    // Up-front buildability check — surfaces the failure at rule
    // construction time rather than first-match time.  `Template`'s
    // own runtime check inside `instantiate_pat_graph` is the final
    // safety net.
    rhs.0.assert_concrete_at_runtime()?;
    // Same construction-time soundness checks as `Rewrite::new`,
    // adapted for the Wildcard RHS.
    check_capture_coverage(&lhs.0, &rhs.0)?;
    check_root_ty_agreement(&lhs.0, &rhs.0)?;
    Ok(rewrite_rule_impl(lhs, rhs))
}

/// Shared implementation body for [`rewrite_rule`] and
/// [`rewrite_rule_dynamic`].  Generic over both the LHS and RHS roles
/// so the two entry points dispatch into one code path.  The RHS role
/// is constrained by `Pat<R>: Template`, which is implemented for both
/// `Concrete` (compile-time-checked) and `Wildcard` (runtime-checked).
fn rewrite_rule_impl<RLhs, RRhs>(
    lhs: Pat<RLhs>,
    rhs: Pat<RRhs>,
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>> + 'static
where
    RLhs: Role + 'static,
    Pat<RLhs>: Pattern + 'static,
    RRhs: Role + 'static,
    Pat<RRhs>: Template + 'static,
{
    move |ctx: &mut RewriteCtx<'_>, node: NodeId| -> Result<Option<NodeOutputId>> {
        // 1. Match LHS.  Keep the matcher borrow in a tight scope so
        //    we can mutate `ctx.function` afterwards.
        let bindings = {
            // `ctx` wraps a built function (the `RewriteCtx::try_for_built`
            // invariant), so `Matcher::try_new` cannot fail here; `?`
            // surfaces a defensive error rather than panicking if the
            // invariant is ever violated.
            let matcher = Matcher::try_new(ctx.function)?;
            match matcher.match_at(node, &lhs) {
                Some(m) => m.bindings_clone(),
                None => return Ok(None),
            }
        };

        // 2. Fetch root's single value output and its type.
        let [root_out] = ctx.function.node_outputs_exact::<1>(node)?;
        let root_ty = ctx.function.output_kind(root_out).as_value_or_err()?;

        // 3. Materialise RHS.  A closure inside the tree may opt out
        //    of the rewrite by returning `Err(crate::error::skip())`;
        //    catch that sentinel here and convert it to "no change"
        //    instead of a hard error.  Snapshot the next-NodeId
        //    BEFORE the build so we can identify which interior nodes
        //    are freshly allocated (vs returned as cache hits on
        //    pre-existing nodes).
        let pre_build_node_id = ctx.function.next_node_id();
        let new_out = match rhs.instantiate(ctx.function, &bindings, node, root_ty) {
            Ok(out) => out,
            Err(e) if is_skip(&e) => return Ok(None),
            Err(e) => return Err(e),
        };

        // 4. Absorb the rewritten root's asm-fingerprint into EVERY
        //    freshly-created interior node of the RHS subtree, not
        //    just the outermost root.  Multi-node rules build fresh
        //    interior nodes that would otherwise miss their
        //    contributor's asm fingerprints and fail the always-on
        //    Layer-C check.
        //
        //    The walk is bounded by `pre_build_node_id`: any NodeId
        //    allocated before the build is pre-existing (a captured
        //    LHS value, a pre-existing constant the dedup cache
        //    returned, etc.) and stays untouched.  Fresh nodes
        //    (id ≥ snapshot) all inherit the contributor's history
        //    via the union semantics of `extend_asm_fingerprint_from`.
        let new_node = ctx.function.node_for_output(new_out);
        // Always attribute the rewrite root: even when the dedup
        // cache returns a pre-existing node, it now ALSO carries the
        // rewritten root's history (union semantics).
        ctx.function.extend_asm_fingerprint_from(new_node, node);
        absorb_fingerprints_into_fresh_subtree(
            ctx.function,
            new_node,
            node,
            pre_build_node_id,
        );

        // 5. Redirect every consumer of the old root's value output
        //    to the new output.  `replace_all_uses` returns `true`
        //    when at least one use was redirected; surface the
        //    RHS-built output so the peephole driver can re-examine the
        //    freshly-created node for cascading folds.
        let changed = ctx.function.replace_all_uses(root_out, new_out)?;
        Ok(changed.then_some(new_out))
    }
}

/// Walk freshly-allocated interior nodes (id ≥ `snapshot`) reachable
/// upward from `new_node` and absorb `contributor`'s asm-fingerprint
/// into each.  Pre-existing input nodes (id < snapshot) bound the
/// walk: they're outside the rewrite and stay untouched.
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
        .map(|inp| function.node_for_output(inp))
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
            stack.push(function.node_for_output(inp));
        }
    }
}

// ── RewriteCtx / RewriteCtxView ──────────────────────────────────────

/// Rewrite context: a borrowed `&mut Function`.  Used by
/// [`rewrite_rule`] and destructive optimizer passes.
///
/// The function's entry [`NodeId`] is derived on demand via
/// [`Self::entry`] from `Function::entry()`; the wrapped function is
/// required to be in its built form (i.e. `function.entry()` is
/// `Some(_)`), which is checked at construction time by
/// [`Self::try_for_built`].
pub struct RewriteCtx<'g> {
    pub(crate) function: &'g mut Function,
}

/// Read-only `&Function` view used by opt's read-only public API.
/// `Copy` and cheap to pass.  Constructible from `&RewriteCtx` (via
/// [`RewriteCtx::as_view`]) or `&Function` (via [`Self::from_built`]).
/// The entry [`NodeId`] is derived on demand via [`Self::entry`].
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
    /// Returns an error if the function has not been built (i.e.
    /// `entry` is `None`).
    pub fn try_for_built(function: &'g mut Function) -> Result<Self> {
        let _entry = function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("RewriteCtx::try_for_built: entry node is not set"))?;
        Ok(Self { function })
    }

    /// Pre-order graph walk starting at [`Self::entry`].  Mirrors
    /// `Graph::preorder` so optimizer-pass bodies that call
    /// `ctx.walk()` look the same as if they held a `Graph` directly.
    #[must_use]
    pub fn walk(&self) -> strider_ir::walk::GraphWalk<'_> {
        self.function.walk_from(self.entry())
    }

    /// Kind-filtered pre-order walk.
    pub fn walk_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&strider_ir::node::NodeKind) -> bool + 'a,
    {
        let g: &Graph = self.function;
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
        self.function
    }

    /// Read-only access to the wrapped [`Function`].
    #[must_use]
    pub fn function_ref(&self) -> &Function {
        self.function
    }

    /// Function-entry `NodeId` anchor.  Derived from
    /// `Function::entry()`; [`Self::try_for_built`] enforces the
    /// post-build invariant.
    ///
    /// The `expect()` cannot panic in practice: `try_for_built`
    /// validates the post-build invariant at construction time, and
    /// `Function::entry` is monotonic — once set it never reverts.
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
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        self.function.create_node(kind, inputs, output_kinds)
    }

    /// Create a node and union every contributor's asm-fingerprint into
    /// it — delegates to [`Function::create_node_attributed`].  This is
    /// the fingerprint-aware creation path; passes use it instead of
    /// hand-stamping a fresh node.
    pub fn create_node_attributed(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
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
        ty: NodeOutputType,
    ) -> strider_ir::error::Result<NodeOutputId> {
        self.function.make_int_const(val, ty)
    }

    /// Detach every input of `node` from its producers' use-lists —
    /// delegates to [`Graph::detach_node_inputs`].
    pub fn detach_node_inputs(&mut self, node: NodeId) {
        self.function.detach_node_inputs(node);
    }

    /// Redirect an input slot to a new producer output — delegates to
    /// [`Graph::update_input`].
    pub fn update_input(&mut self, input_id: NodeInputId, output_id: NodeOutputId) {
        self.function.update_input(input_id, output_id);
    }

    /// Append an input to a (non-cacheable) node — delegates to
    /// [`Graph::add_node_input`].
    ///
    /// # Errors
    /// Propagates [`Graph::add_node_input`]'s error arm.
    pub fn add_node_input(
        &mut self,
        node: NodeId,
        output_id: NodeOutputId,
    ) -> strider_ir::error::Result<()> {
        self.function.add_node_input(node, output_id)
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
        self.function.remove_node_input(node, index)
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
        old: NodeOutputId,
        new: NodeOutputId,
    ) -> strider_ir::error::Result<bool> {
        self.function.replace_all_uses(old, new)
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
    pub fn absorb_fingerprint(&mut self, into_out: NodeOutputId, from_out: NodeOutputId) {
        let into = self.function.node_for_output(into_out);
        let from = self.function.node_for_output(from_out);
        self.function.extend_asm_fingerprint_from(into, from);
    }

    /// Record a concrete stack slot `(base, offset)` for a Store/Load
    /// node — delegates to [`Function::set_stack_offset`].
    pub fn set_stack_offset(&mut self, id: NodeId, base: NodeOutputId, offset: i64) {
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

    /// Build a [`Matcher`] anchored at this context's wrapped
    /// function.
    ///
    /// `try_for_built` already validated the post-build invariant,
    /// so `Matcher::try_new` cannot fail here; the `expect()`
    /// surfaces a clear panic message if the invariant is ever
    /// violated.
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
        self.function.walk_from(self.entry())
    }

    /// Kind-filtered pre-order walk.
    pub fn walk_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&strider_ir::node::NodeKind) -> bool + 'a,
    {
        let g: &Graph = self.function;
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
        self.function
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
    /// Returns an error if the function has not been built (i.e.
    /// `entry` is `None`).
    pub fn from_built(function: &'g Function) -> Result<Self> {
        let _entry = function.entry().ok_or_else(|| {
            anyhow::anyhow!("RewriteCtxView::from_built: entry node is not set")
        })?;
        Ok(Self { function })
    }
}

impl<'a, 'g> From<&'a RewriteCtx<'g>> for RewriteCtxView<'a> {
    fn from(ctx: &'a RewriteCtx<'g>) -> Self {
        ctx.as_view()
    }
}

impl<'g> std::ops::Deref for RewriteCtxView<'g> {
    type Target = Graph;
    fn deref(&self) -> &Graph {
        self.function
    }
}

// Allow `Function` overlay READ methods (asm fingerprints, phi var
// tags, etc.) to be called on `RewriteCtx` directly via `Deref`.
// `Function` itself derefs to `Graph`, so structural graph reads like
// `node_kind` are also reachable through the two-step deref chain:
// `RewriteCtx → Function → Graph`.
//
// There is deliberately NO `DerefMut`: `RewriteCtx` is read-only over
// `Function`.  Every mutation routes through one of the curated
// mutation-façade methods above, enforcing "all rewrites go through
// RewriteCtx".
impl<'g> std::ops::Deref for RewriteCtx<'g> {
    type Target = Function;
    fn deref(&self) -> &Function {
        self.function
    }
}

// ── GraphRewriteCtxExt — `with_rewrite_ctx` helper on Function ────────

/// Extension trait on [`strider_ir::Function`] providing a
/// `with_rewrite_ctx` callback that absorbs the
/// `let mut ctx = RewriteCtx::try_for_built(&mut f)?; apply_*(&mut ctx, …)`
/// construct-then-pass pattern into a single
/// `f.with_rewrite_ctx(|ctx| apply_*(ctx, …))?` call.
///
/// The callback's `Result<T>` output is flattened into the method's
/// return type — the un-built case and the closure's failure path
/// share one `?` at the call site.
///
/// `Function` lives in `strider-ir`, which doesn't know about
/// `RewriteCtx`, so the helper has to ride on an extension trait
/// defined here.
pub trait GraphRewriteCtxExt {
    /// Borrow `self` as a [`RewriteCtx`] and run `f` with mutable
    /// access.  The closure's `Result<T>` and the un-built case are
    /// merged into one outer `Result<T>` — call sites need a single
    /// `?` to surface either failure mode.
    ///
    /// # Errors
    ///
    /// Returns an error if `self.entry()` is `None` (function not
    /// built), or if the closure returns an `Err`.
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

/// Pattern-rewrite facade over [`rewrite_rule`].  Walks every reachable
/// node of the function under inspection and applies a rule at each
/// candidate.  OR-composes per-node results into a single `bool`
/// describing whether the rule fired anywhere.
///
/// This is the moral equivalent of the strider-analyze
/// `GraphRewriter` — kept name-and-shape-compatible so downstream
/// optimizer-pass code can swap the import path without further edits.
pub struct GraphRewriter;

impl GraphRewriter {
    /// Apply a single rule closure across every reachable node of the
    /// function wrapped by `ctx`.  Returns `true` if the rule fired at
    /// least once.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by `rule`.
    pub fn apply<R>(ctx: &mut RewriteCtx<'_>, rule: R) -> Result<bool>
    where
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>>,
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
    /// function wrapped by `ctx`.  Equivalent to
    /// `Self::apply(ctx, apply_rules_in_order(rules))`.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by any rule.
    pub fn apply_rules<R>(ctx: &mut RewriteCtx<'_>, rules: &[R]) -> Result<bool>
    where
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>>,
    {
        Self::apply(ctx, apply_rules_in_order(rules))
    }

    /// Apply a single rule closure across every reachable node of
    /// `function`, returning the total per-node fire count (not just a
    /// boolean "did anything fire").  Pre-collects the candidate node
    /// ids before invoking the rule because the rule may mutate the
    /// graph (e.g. detach an Add by rewiring its uses), and walking
    /// `preorder` while the graph mutates underneath is undefined.
    /// Nodes detached by an earlier invocation simply return `false`
    /// from the rule (their matcher's structural check fails on a
    /// rewired node) and don't contribute to the count.
    ///
    /// `cranelift_entity::PrimaryMap` doesn't reuse keys, so every id
    /// from the pre-collected preorder is still a valid arena slot —
    /// even if the node was detached by an earlier rule firing on this
    /// same walk.
    ///
    /// # Errors
    ///
    /// Returns an error if `function.entry()` is `None`, or if the
    /// rule closure returns a non-skip error.
    pub fn apply_count<R>(function: &mut Function, rule: R) -> Result<usize>
    where
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>>,
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

    /// Apply a slice of rules across every reachable node of
    /// `function` round-robin (every rule once per root), returning the
    /// total per-rule per-node fire count.
    ///
    /// # Errors
    ///
    /// Propagates the first error from any rule, or surfaces the
    /// un-built case if `function.entry()` is `None`.
    pub fn apply_rules_count<R>(function: &mut Function, rules: &[R]) -> Result<usize>
    where
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>>,
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
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>> + '_
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>>,
{
    move |ctx, node| {
        let mut last: Option<NodeOutputId> = None;
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
    Box<dyn for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>>>;

/// Wraps a rewrite-rule closure in a [`BoxedRule`] for storage in a
/// `Vec<BoxedRule>` alongside rules built from other LHS/RHS shapes.
pub fn boxed_rule<R>(r: R) -> BoxedRule
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<Option<NodeOutputId>> + 'static,
{
    Box::new(r)
}
