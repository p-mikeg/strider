//! Rewriter infrastructure: [`RewriteCtx`] / [`RewriteCtxView`],
//! [`GraphRewriter`], [`BoxedRule`] / [`boxed_rule`],
//! [`apply_rules_in_order`], [`GraphRewriteCtxExt`], and the
//! [`rewrite_rule`] / [`rewrite_rule_runtime`] constructors.
//!
//! Buildability of a rewrite RHS is a **compile-time** property:
//! [`rewrite_rule`] bounds its RHS on [`TemplatePat`], which is only
//! implemented by buildable typed structs — a wildcard RHS is a compile
//! error. [`rewrite_rule_runtime`] is the dynamic (FFI) counterpart that
//! takes two already-built [`Pattern`]s and checks buildability at
//! runtime via [`assert_buildable`].
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
use strider_ir::node::NodeId;
use strider_ir::{Function, Graph};

use crate::capture::Capture;
use crate::error::{Result, is_skip};
use crate::match_pat::MatchPat;
use crate::matcher::Matcher;
use crate::pattern::{PatVertex, Pattern};
use crate::template::instantiate;
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
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + 'static {
    let lhs_pat = lhs.into_pattern();
    let rhs_pat = rhs.into_template();
    check_capture_coverage(&lhs_pat, &rhs_pat).expect("rewrite_rule: RHS capture not bound by LHS");
    rewrite_rule_impl(lhs_pat, rhs_pat)
}

/// Build a rewrite-rule closure from two already-built [`Pattern`]s —
/// the dynamic (FFI / scripted) counterpart of [`rewrite_rule`].
///
/// Buildability cannot be enforced by the type system for a dynamically
/// constructed RHS, so it is checked at construction time via
/// [`assert_buildable`]: every reachable RHS pattern node must carry a
/// build spec or a capture.
///
/// # Errors
///
/// Returns an error if the RHS carries a node with neither a build spec
/// nor a capture (a match-only `Any` / predicate shape), or if the RHS
/// references a capture the LHS does not bind.
pub fn rewrite_rule_runtime(
    lhs: Pattern,
    rhs: Pattern,
) -> Result<impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + 'static> {
    assert_buildable(&rhs)?;
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
    rhs: Pattern,
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + 'static {
    move |ctx: &mut RewriteCtx<'_>, node: NodeId| -> Result<bool> {
        // 1. Match LHS. Keep the matcher borrow tight so we can mutate
        //    `ctx.function` afterwards.
        let bindings = {
            let matcher = Matcher::try_new(ctx.function)?;
            match matcher.match_at(node, &lhs) {
                Some(m) => m.bindings_clone(),
                None => return Ok(false),
            }
        };

        // 2. Fetch root's single value output and its type.
        let [root_out] = ctx.function.node_outputs_exact::<1>(node)?;
        let root_ty = ctx.function.output_kind(root_out).as_value_or_err()?;

        // 3. Materialise RHS. A closure inside the tree may opt out via
        //    `Err(crate::error::skip())`; catch the sentinel here and
        //    convert it to "no change". Snapshot the next-NodeId BEFORE
        //    the build so we can identify which interior nodes are
        //    freshly allocated (vs returned as dedup-cache hits on
        //    pre-existing nodes).
        let pre_build_node_id = ctx.function.next_node_id();
        let new_out = match instantiate(&rhs, ctx.function, &bindings, node, root_ty) {
            Ok(out) => out,
            Err(e) if is_skip(&e) => return Ok(false),
            Err(e) => return Err(e),
        };

        // 4. Absorb the rewritten root's asm-fingerprint into EVERY
        //    freshly-created interior node of the RHS subtree (superset
        //    -only). The walk is bounded by `pre_build_node_id`: any
        //    NodeId allocated before the build is pre-existing and stays
        //    untouched.
        let new_node = ctx.function.node_for_output(new_out);
        ctx.function.extend_asm_fingerprint_from(new_node, node);
        absorb_fingerprints_into_fresh_subtree(ctx.function, new_node, node, pre_build_node_id);

        // 5. Redirect every consumer of the old root's value output to
        //    the new output.
        let changed = ctx.function.replace_all_uses(root_out, new_out)?;
        Ok(changed)
    }
}

// ── construction-time checks ─────────────────────────────────────────

/// Walk every RHS pattern node; for each capture-bearing node assert
/// that the capture also appears somewhere in the LHS. An
/// unbound-in-LHS capture would surface as a missing binding at apply
/// time — catching it at construction turns a runtime bug into a
/// build-time error at the rule's authoring site.
fn check_capture_coverage(lhs: &Pattern, rhs: &Pattern) -> Result<()> {
    let lhs_caps: rustc_hash::FxHashSet<Capture> = lhs
        .inner
        .node_weights()
        .filter_map(|v| match v {
            PatVertex::Node(n) => n.capture,
            PatVertex::Output(_) => None,
        })
        .collect();
    for v in rhs.inner.node_weights() {
        if let PatVertex::Node(n) = v
            && let Some(cap) = n.capture
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

/// Assert that every reachable RHS pattern node is buildable — it
/// carries a build spec or a capture. The compile-time `TemplatePat`
/// bound on [`rewrite_rule`] makes this hold by construction; the
/// dynamic [`rewrite_rule_runtime`] path enforces it at runtime.
///
/// # Errors
///
/// Returns an error naming the first un-buildable node found.
pub fn assert_buildable(rhs: &Pattern) -> Result<()> {
    for v in rhs.inner.node_weights() {
        if let PatVertex::Node(n) = v
            && n.build.is_none()
            && n.capture.is_none()
        {
            return Err(anyhow::anyhow!(
                "rewrite RHS contains a node with neither a build spec nor a capture — \
                 a buildable RHS must consist of concrete builders (e.g. int_const(0), \
                 add(...)) and captures bound by the LHS"
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

    /// Mutable access to the wrapped structural [`Graph`].
    pub fn graph_mut(&mut self) -> &mut Graph {
        self.function.graph_mut()
    }

    /// Mutable access to the wrapped [`Function`] (graph + overlay).
    pub fn function_mut(&mut self) -> &mut Function {
        self.function
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

impl<'g> std::ops::Deref for RewriteCtxView<'g> {
    type Target = Graph;
    fn deref(&self) -> &Graph {
        self.function
    }
}

// Allow `Function` overlay methods to be called on `RewriteCtx`
// directly via Deref. `Function` derefs to `Graph`, so structural graph
// methods are reachable through the two-step chain.
impl<'g> std::ops::Deref for RewriteCtx<'g> {
    type Target = Function;
    fn deref(&self) -> &Function {
        self.function
    }
}

impl<'g> std::ops::DerefMut for RewriteCtx<'g> {
    fn deref_mut(&mut self) -> &mut Function {
        self.function
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
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool>,
    {
        let nodes: Vec<NodeId> = ctx.function.walk().collect();
        let mut any = false;
        for n in nodes {
            if rule(ctx, n)? {
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
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool>,
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
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool>,
    {
        let mut ctx = RewriteCtx::try_for_built(function)?;
        let candidates: Vec<NodeId> = ctx.function.walk().collect();
        let mut applied: usize = 0;
        for node in candidates {
            if rule(&mut ctx, node)? {
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
        R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool>,
    {
        let mut ctx = RewriteCtx::try_for_built(function)?;
        let candidates: Vec<NodeId> = ctx.function.walk().collect();
        let mut applied: usize = 0;
        for node in candidates {
            for r in rules {
                if r(&mut ctx, node)? {
                    applied += 1;
                }
            }
        }
        Ok(applied)
    }
}

// ── apply_rules_in_order / BoxedRule / boxed_rule ────────────────────

/// Compose a list of rewrite-rule closures into a single closure that
/// iterates every rule on the same root, OR-ing the per-rule results.
pub fn apply_rules_in_order<R>(
    rules: &[R],
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + '_
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool>,
{
    move |ctx, node| {
        let mut any = false;
        for r in rules {
            if r(ctx, node)? {
                any = true;
            }
        }
        Ok(any)
    }
}

/// Type-erased rewrite-rule closure. Each [`rewrite_rule`] call returns
/// a distinct opaque `impl Fn` type, so a heterogeneous rule list must
/// box each one to this common trait-object type.
pub type BoxedRule = Box<dyn for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool>>;

/// Wraps a rewrite-rule closure in a [`BoxedRule`].
pub fn boxed_rule<R>(r: R) -> BoxedRule
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + 'static,
{
    Box::new(r)
}
