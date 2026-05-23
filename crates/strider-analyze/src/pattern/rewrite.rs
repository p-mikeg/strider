//! Rule composition: [`rewrite_rule`], [`apply_rules_in_order`], [`BoxedRule`],
//! [`boxed_rule`].

use cranelift_entity::EntityRef;
use entity_utils::DenseEntitySet;
use strider_ir::Graph;
use strider_ir::node::NodeId;

use crate::pattern::error::Result;
use crate::pattern::matcher::Matcher;
use crate::pattern::pat::Pat;
use crate::pattern::pat::traits::{BuildCtx, BuildOutcome};

/// Build a rewrite-rule closure from an LHS and RHS [`Pat`].
///
/// The returned closure takes `&mut RewriteCtx<'g>` and a candidate root
/// [`NodeId`], attempts the match, and on success materializes the RHS
/// template via `crate::pattern::pat::traits::Pattern::try_build` and redirects
/// the root's value output to the built output via
/// [`strider_ir::Graph::replace_all_uses`].
///
/// Returns `Ok(true)` if the rule fired and at least one use was redirected,
/// `Ok(false)` if the match failed, the RHS produced a skip, or
/// `replace_all_uses` found nothing to redirect.
///
/// Errors from the graph layer (`make_value_node`, `replace_all_uses`)
/// propagate as [`anyhow::Error`].  Errors from user closures inside
/// `*_const_with!` macros also propagate as [`anyhow::Error`] —
/// anyhow's blanket `From<E: Error + Send + Sync + 'static>` wraps any
/// custom error type the closure returns, and tests can downcast to
/// recover the original.  Use `crate::pattern::error::skip` inside a closure
/// to opt out of the rewrite without a hard error; the interpreter
/// detects the [`crate::pattern::error::RewriteSkip`] sentinel via
/// `crate::pattern::error::is_skip` and returns `Ok(false)`.
///
/// # Single-value-output constraint
///
/// The LHS root must have exactly one value output — the rule redirects that
/// output's uses to the RHS-built output.  Rooting a rule on a multi-output
/// node (any `Call`, `Load`, `Store`, control-flow node) returns an
/// `IrError` from `node_outputs_exact::<1>`.  If you need to rewrite a
/// multi-slot producer, operate on the value slot explicitly (e.g. match
/// the slot-consumer rather than the producer).
pub fn rewrite_rule(
    lhs: impl Into<Pat>,
    rhs: impl Into<Pat>,
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + Send + Sync + 'static {
    let lhs: Pat = lhs.into();
    let rhs: Pat = rhs.into();
    move |ctx: &mut RewriteCtx<'_>, node: NodeId| -> Result<bool> {
        // 1. Match LHS.  Keep the matcher borrow in a tight scope so we can
        //    mutate `ctx.graph` afterwards.
        let bindings = {
            let matcher = Matcher::for_graph(ctx.graph, ctx.entry);
            match matcher.match_at(node, &lhs) {
                Some(m) => m.bindings_clone(),
                None => return Ok(false),
            }
        };

        // 2. Fetch root's single value output and its type.
        let [root_out] = ctx.graph.node_outputs_exact::<1>(node)?;
        let root_ty = ctx.graph.output_kind(root_out).as_value_or_err()?;

        // 3. Materialize RHS.  A closure inside the tree may opt out of the
        //    rewrite by returning `Err(pattern::Error::skip())`; catch that
        //    sentinel here and convert it to "no change" instead of a hard
        //    error.  All other errors propagate.  Snapshot the next-NodeId
        //    BEFORE the build so we can identify which interior nodes are
        //    freshly allocated (vs returned as cache hits on pre-existing
        //    nodes) — see the asm-fingerprint walk after `BuildOutcome::Out`.
        let pre_build_node_id = ctx.graph.next_node_id();
        let outcome = {
            let mut bctx = BuildCtx {
                graph: ctx.graph,
                bindings: &bindings,
                root: node,
                root_ty,
            };
            match rhs.as_dyn().try_build(&mut bctx) {
                Ok(o) => o,
                Err(e) if crate::pattern::error::is_skip(&e) => return Ok(false),
                Err(e) => return Err(e),
            }
        };

        match outcome {
            BuildOutcome::Skip => Ok(false),
            BuildOutcome::Out(new_out) => {
                // Absorb the rewritten root's asm-fingerprint into EVERY
                // freshly-created interior node of the RHS subtree, not
                // just the outermost root.  Multi-node rules (e.g.
                // ConstantFold's `rule_and_dist`:
                // `((a&C1)|(b&C2))&C3 → (a&(C1&C3)) | (b&(C2&C3))`)
                // build fresh interior `And` / `IntConst` nodes that
                // would otherwise miss their contributor's asm
                // fingerprints and fail the always-on Layer-C check.
                //
                // The walk is bounded by `pre_build_node_id`: any
                // NodeId allocated before the build is pre-existing
                // (a captured LHS value, a pre-existing constant the
                // dedup cache returned, etc.) and stays untouched.
                // Fresh nodes (id ≥ snapshot) all inherit the
                // contributor's history via the union semantics of
                // `extend_asm_fingerprint_from`.
                let new_node = ctx.graph.get_node_from_output(new_out);
                // Always attribute the rewrite root: even when the dedup
                // cache returns a pre-existing node, it now ALSO carries
                // the rewritten root's history (union semantics).
                ctx.graph.extend_asm_fingerprint_from(new_node, node);
                // Walk freshly-allocated interior nodes (id >= snapshot)
                // and absorb the contributor's history into each one.
                // Pre-existing input nodes (id < snapshot) bound the
                // walk: they're outside the rewrite and stay untouched.
                let mut visited: DenseEntitySet<NodeId> = DenseEntitySet::new();
                visited.insert(new_node);
                let mut stack: Vec<NodeId> = ctx
                    .graph
                    .node_inputs(new_node)
                    .into_iter()
                    .map(|inp| ctx.graph.get_node_from_output(inp))
                    .collect();
                while let Some(cur) = stack.pop() {
                    if !visited.insert(cur) {
                        continue;
                    }
                    if cur.index() < pre_build_node_id.index() {
                        // Pre-existing node — outside the rewrite.
                        continue;
                    }
                    ctx.graph.extend_asm_fingerprint_from(cur, node);
                    let inputs: Vec<_> = ctx.graph.node_inputs(cur).into_iter().collect();
                    for inp in inputs {
                        stack.push(ctx.graph.get_node_from_output(inp));
                    }
                }

                let changed = ctx.graph.replace_all_uses(root_out, new_out)?;
                Ok(changed)
            }
        }
    }
}

/// Rewrite context: a borrowed `&mut Graph` together with the
/// function's `entry: NodeId`.  Used by `rewrite_rule` and the
/// destructive optimizer passes.
///
/// Replaces the prior "wrap into a dummy `BuiltFunctionGraph`" trick —
/// pure-rewrite paths (constant fold, known-bits, flag-cmp
/// canonicalisation, etc.) only ever consult graph + entry, never the
/// CC-bearing fields of `BuiltFunctionGraph`.
///
/// **Field visibility note.**  Both fields are `pub(crate)`; external
/// opt-pass code reaches `Graph` via the
/// [`Deref`](std::ops::Deref) / [`DerefMut`](std::ops::DerefMut) impls
/// (targeting `Graph`) for method calls, and uses [`Self::graph_ref`]
/// / [`Self::graph_mut`] when an explicit `&Graph` / `&mut Graph` is
/// needed for a free function or trait method.  This prevents
/// struct-literal rebinding (`ctx.graph = &mut other`) at distance —
/// the field could previously be aimed at a different graph than
/// `entry` belongs to, silently corrupting subsequent walks.
pub struct RewriteCtx<'g> {
    pub(crate) graph: &'g mut Graph,
    pub(crate) entry: NodeId,
}

/// Read-only `(&Graph, NodeId)` view used by opt's read-only public
/// API.  `Copy` and cheap to pass.  Constructible from `&RewriteCtx`
/// (via `as_view`) or `&BuiltFunctionGraph` (via
/// `From<&BuiltFunctionGraph>`).
#[derive(Clone, Copy)]
pub struct RewriteCtxView<'g> {
    pub(crate) graph: &'g Graph,
    pub(crate) entry: NodeId,
}

impl<'g> RewriteCtx<'g> {
    /// Constructs a `RewriteCtx` from a raw `(graph, entry)` pair —
    /// the rewrite-only path used by `opt::with_rewrite_ctx`,
    /// `strider::rewrite::GraphRewriter::apply_rule`, and similar.
    pub fn new(graph: &'g mut Graph, entry: NodeId) -> Self {
        Self { graph, entry }
    }

    /// Constructs a `RewriteCtx` borrowing from a `BuiltFunctionGraph`'s
    /// inner `graph` + `entry`.  Used by callers that already hold a
    /// fully-built form and want to drive the rewrite engine without
    /// surrendering the wrapper.
    ///
    /// # Panics
    ///
    /// Panics if the graph has not been built (i.e. `entry` is
    /// `None`).  Pre-condition: `bfg` must have been built via
    /// [`strider_ir::FunctionBuilder::build`].
    #[allow(clippy::expect_used)]
    pub fn for_built(bfg: &'g mut strider_ir::BuiltFunctionGraph) -> Self {
        let entry = bfg.entry().expect(
            "RewriteCtx::for_built: pre-condition violated — \
             graph has not been built (entry is None)",
        );
        Self {
            graph: bfg.graph_mut(),
            entry,
        }
    }

    /// pre-order graph walk starting at [`Self::entry`].  Mirrors
    /// `BuiltFunctionGraph::preorder` so optimizer pass bodies that
    /// call `ctx.preorder()` look the same as if they held a
    /// `BuiltFunctionGraph` directly.
    #[must_use]
    pub fn preorder(&self) -> strider_ir::walk::GraphWalk<'_> {
        self.graph.walk_from(self.entry)
    }

    /// kind-filtered pre-order walk.  Mirrors
    /// `BuiltFunctionGraph::preorder_kind`.
    pub fn preorder_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&strider_ir::node::NodeKind) -> bool + 'a,
    {
        let g: &Graph = self.graph;
        self.preorder().filter(move |&n| pred(g.node_kind(n)))
    }

    /// Read-only access to the wrapped `Graph`.
    #[must_use]
    pub fn graph_ref(&self) -> &Graph {
        self.graph
    }

    /// Function-entry `NodeId` anchor.
    #[must_use]
    pub fn entry(&self) -> NodeId {
        self.entry
    }

    /// Lightweight read-only `(graph, entry)` view.  Used by the
    /// public read-only opt API (`analyze_known_bits`,
    /// `classify_anchor`) so callers that hold either `&mut RewriteCtx`,
    /// `&BuiltFunctionGraph`, or a raw `(&Graph, NodeId)` pair can all
    /// pass the same `RewriteCtxView<'_>`.
    #[must_use]
    pub fn as_view(&self) -> RewriteCtxView<'_> {
        RewriteCtxView { graph: self.graph, entry: self.entry }
    }

    /// Mutable access to the wrapped `Graph`.
    pub fn graph_mut(&mut self) -> &mut Graph {
        self.graph
    }

    /// Build a [`Matcher`] anchored at this context's `(graph, entry)`.
    /// Single-source the `Matcher::for_graph(ctx.graph_ref(), ctx.entry())`
    /// pairing so call sites don't have to spell out both fields.
    #[must_use]
    pub fn matcher(&self) -> Matcher<'_> {
        Matcher::for_graph(self.graph, self.entry)
    }
}

impl<'g> RewriteCtxView<'g> {
    /// pre-order graph walk starting at [`Self::entry`].  Mirrors
    /// `BuiltFunctionGraph::preorder` so optimizer pass bodies that
    /// call `ctx.preorder()` look the same as if they held a
    /// `BuiltFunctionGraph` directly.
    #[must_use]
    pub fn preorder(&self) -> strider_ir::walk::GraphWalk<'_> {
        self.graph.walk_from(self.entry)
    }

    /// kind-filtered pre-order walk.  Mirrors
    /// `BuiltFunctionGraph::preorder_kind`.
    pub fn preorder_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&strider_ir::node::NodeKind) -> bool + 'a,
    {
        let g: &Graph = self.graph;
        self.preorder().filter(move |&n| pred(g.node_kind(n)))
    }

    /// Read-only access to the wrapped `Graph`.
    #[must_use]
    pub fn graph_ref(&self) -> &Graph {
        self.graph
    }

    /// Function-entry `NodeId` anchor.
    #[must_use]
    pub fn entry(&self) -> NodeId {
        self.entry
    }

    /// Build a [`Matcher`] anchored at this view's `(graph, entry)`.
    /// Single-source the `Matcher::for_graph(ctx.graph_ref(), ctx.entry())`
    /// pairing so call sites don't have to spell out both fields.
    #[must_use]
    pub fn matcher(&self) -> Matcher<'g> {
        Matcher::for_graph(self.graph, self.entry)
    }

    /// Borrows a built [`strider_ir::BuiltFunctionGraph`] as a shared
    /// rewrite-context view.  Fallible companion to the legacy
    /// `From<&BFG>` impl now that [`strider_ir::Graph::entry`] returns
    /// `Option`.  New code should prefer this method.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph has not been built (i.e. `entry`
    /// is `None`).
    pub fn from_built(bfg: &'g strider_ir::BuiltFunctionGraph) -> anyhow::Result<Self> {
        let entry = bfg.entry().ok_or_else(|| {
            anyhow::anyhow!("RewriteCtxView::from_built: graph has not been built (entry is None)")
        })?;
        Ok(Self { graph: bfg.graph(), entry })
    }
}

/// Legacy infallible conversion from a `BuiltFunctionGraph`.  Retained
/// for compatibility with the wide test surface (~200 call sites) that
/// uses `(&fg).into()`.  Now that [`strider_ir::Graph::entry`] returns
/// `Option`, this `From` impl can only honour the trait's infallible
/// shape by panicking when the pre-condition is violated — hence the
/// localised `expect_used` allow.  New code should prefer
/// [`RewriteCtxView::from_built`], which surfaces the `None` arm as a
/// typed error.
#[allow(clippy::expect_used)]
impl<'g> From<&'g strider_ir::BuiltFunctionGraph> for RewriteCtxView<'g> {
    fn from(bfg: &'g strider_ir::BuiltFunctionGraph) -> Self {
        let entry = bfg.entry().expect(
            "RewriteCtxView::<From<&BFG>>: pre-condition violated — \
             graph has not been built (entry is None); use \
             RewriteCtxView::from_built for the typed-error path",
        );
        Self { graph: bfg.graph(), entry }
    }
}

/// Extension trait on [`strider_ir::BuiltFunctionGraph`] (alias for
/// [`Graph`]) providing the `with_rewrite_ctx` callback that absorbs the
/// `let mut ctx = RewriteCtx::for_built(&mut bfg); apply_*(&mut ctx, …)`
/// construct-then-pass pattern into a single
/// `bfg.with_rewrite_ctx(|ctx| apply_*(ctx, …))` call.
///
/// `Graph` lives in `strider-ir`, which doesn't know about `RewriteCtx`,
/// so the helper has to ride on an extension trait defined here.
pub trait GraphRewriteCtxExt {
    /// Borrow `self` as a `RewriteCtx` and run `f` with mutable access.
    ///
    /// Mirrors `RewriteCtx::for_built(&mut self)` but folds the
    /// construction into a callback so call sites don't have to spell
    /// out the temporary.
    fn with_rewrite_ctx<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut RewriteCtx<'_>) -> R;
}

impl GraphRewriteCtxExt for strider_ir::BuiltFunctionGraph {
    fn with_rewrite_ctx<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut RewriteCtx<'_>) -> R,
    {
        let mut ctx = RewriteCtx::for_built(self);
        f(&mut ctx)
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
        self.graph
    }
}

// allow Graph methods to be
// called on `RewriteCtx` directly via Deref.  Mirrors
// `BuiltFunctionGraph::Deref<Target=Graph>` so optimizer pass bodies
// using `ctx.node_kind(_)` / `ctx.create_node(_)` look the same as
// if they held a `BuiltFunctionGraph` directly.
impl<'g> std::ops::Deref for RewriteCtx<'g> {
    type Target = Graph;
    fn deref(&self) -> &Graph {
        self.graph
    }
}

impl<'g> std::ops::DerefMut for RewriteCtx<'g> {
    fn deref_mut(&mut self) -> &mut Graph {
        self.graph
    }
}

/// Compose a list of rewrite-rule closures into a single closure.
///
/// The returned closure iterates every rule in `rules` on the same root node,
/// OR-ing the per-rule `bool` results.  Once the first rule fires the root's
/// uses are redirected — subsequent rules then see the new graph state and
/// may or may not still apply; this mirrors the "run every rule, once" policy
/// of the pre-rewrite `apply_identity_rules` fold in `constant_fold.rs`.
///
/// Borrows `rules` as a slice and returns a closure bound to that borrow's
/// lifetime, so callers can hoist the rule vec into a `LazyLock` (or any
/// other long-lived storage) and compose the per-call closure cheaply.
pub(crate) fn apply_rules_in_order<R>(
    rules: &[R],
) -> impl for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + Send + Sync + '_
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + Send + Sync,
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

/// Type-erased rewrite-rule closure.
///
/// Each call to [`rewrite_rule`] returns a distinct opaque `impl Fn` type, so a
/// `Vec<impl Fn>` can only hold rules with identical signatures — in practice,
/// only a single rule.  Consumers composing a list of heterogeneous rules need
/// to box each one to a common trait-object type; this alias plus
/// [`boxed_rule`] factor that boilerplate out of every call site.
pub type BoxedRule =
    Box<dyn for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + Send + Sync>;

/// Wraps a rewrite-rule closure in a [`BoxedRule`] for storage in a
/// `Vec<BoxedRule>` alongside rules built from other LHS/RHS shapes.
///
/// ```rust
/// use strider_analyze::pattern::{
///     BoxedRule, Capture, add, boxed_rule, int_const, rewrite_rule, sub, var,
/// };
///
/// let x = Capture::new();
/// let y = Capture::new();
/// let _rules: Vec<BoxedRule> = vec![
///     // add(x, 0) → x
///     boxed_rule(rewrite_rule(add(var(x), int_const(0)), var(x))),
///     // sub(y, y) → 0
///     boxed_rule(rewrite_rule(sub(var(y), var(y)), int_const(0))),
/// ];
/// ```
pub fn boxed_rule<R>(r: R) -> BoxedRule
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + Send + Sync + 'static,
{
    Box::new(r)
}
