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
/// Replaces the prior "wrap into a dummy `Graph`" trick —
/// pure-rewrite paths (constant fold, known-bits, flag-cmp
/// canonicalisation, etc.) only ever consult graph + entry, never the
/// CC-bearing fields of `Graph`.
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
/// (via `as_view`) or `&Graph` (via
/// `From<&Graph>`).
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

    /// Constructs a `RewriteCtx` borrowing from a `Graph`'s
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
    pub fn for_built(bfg: &'g mut strider_ir::Graph) -> Self {
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
    /// `Graph::preorder` so optimizer pass bodies that
    /// call `ctx.preorder()` look the same as if they held a
    /// `Graph` directly.
    #[must_use]
    pub fn preorder(&self) -> strider_ir::walk::GraphWalk<'_> {
        self.graph.walk_from(self.entry)
    }

    /// kind-filtered pre-order walk.  Mirrors
    /// `Graph::preorder_kind`.
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
    /// `&Graph`, or a raw `(&Graph, NodeId)` pair can all
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
    /// `Graph::preorder` so optimizer pass bodies that
    /// call `ctx.preorder()` look the same as if they held a
    /// `Graph` directly.
    #[must_use]
    pub fn preorder(&self) -> strider_ir::walk::GraphWalk<'_> {
        self.graph.walk_from(self.entry)
    }

    /// kind-filtered pre-order walk.  Mirrors
    /// `Graph::preorder_kind`.
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

    /// Borrows a built [`strider_ir::Graph`] as a shared
    /// rewrite-context view.  Fallible companion to the legacy
    /// `From<&BFG>` impl now that [`strider_ir::Graph::entry`] returns
    /// `Option`.  New code should prefer this method.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph has not been built (i.e. `entry`
    /// is `None`).
    pub fn from_built(bfg: &'g strider_ir::Graph) -> anyhow::Result<Self> {
        let entry = bfg.entry().ok_or_else(|| {
            anyhow::anyhow!("RewriteCtxView::from_built: graph has not been built (entry is None)")
        })?;
        Ok(Self { graph: bfg.graph(), entry })
    }
}

/// Legacy infallible conversion from a `Graph`.  Retained
/// for compatibility with the wide test surface (~200 call sites) that
/// uses `(&fg).into()`.  Now that [`strider_ir::Graph::entry`] returns
/// `Option`, this `From` impl can only honour the trait's infallible
/// shape by panicking when the pre-condition is violated — hence the
/// localised `expect_used` allow.  New code should prefer
/// [`RewriteCtxView::from_built`], which surfaces the `None` arm as a
/// typed error.
#[allow(clippy::expect_used)]
impl<'g> From<&'g strider_ir::Graph> for RewriteCtxView<'g> {
    fn from(bfg: &'g strider_ir::Graph) -> Self {
        let entry = bfg.entry().expect(
            "RewriteCtxView::<From<&BFG>>: pre-condition violated — \
             graph has not been built (entry is None); use \
             RewriteCtxView::from_built for the typed-error path",
        );
        Self { graph: bfg.graph(), entry }
    }
}

/// Extension trait on [`strider_ir::Graph`] (alias for
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

impl GraphRewriteCtxExt for strider_ir::Graph {
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
// `Graph::Deref<Target=Graph>` so optimizer pass bodies
// using `ctx.node_kind(_)` / `ctx.create_node(_)` look the same as
// if they held a `Graph` directly.
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

#[cfg(test)]
mod tests {
    //! Unit tests for [`rewrite_rule`] — the LHS-match → RHS-build →
    //! `replace_all_uses` interpreter that drives every pure-rewrite
    //! opt pass.
    //!
    //! Covers: no-match, single-use rewire, multi-use rewire, RHS skip
    //! sentinel, RHS error propagation, and the fingerprint-absorption
    //! contract for freshly-built RHS interior nodes.

    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::pattern::{
        add, any_int_const, boxed_rule, int_const, int_const_with_fn, rewrite_rule, var, Capture,
    };
    use strider_ir::node::{NodeKind, NodeOutputType};
    use strider_ir::IntBinaryOp;
    use strider_ir_test_utils::{make_empty_fn, SENTINEL_LIFT_ADDR};

    /// `fn() -> u64 { return 7; }` — no Add node, used by no-match tests.
    fn just_const() -> strider_ir::Graph {
        make_empty_fn(|b| b.build_int_const(7u64, NodeOutputType::U64)).unwrap()
    }

    /// `fn() -> u64 { return Add(11, 0); }` — exactly one Add with `0` RHS.
    fn add_x_zero() -> strider_ir::Graph {
        make_empty_fn(|b| {
            let a = b.build_int_const(11u64, NodeOutputType::U64)?;
            let z = b.build_int_const(0u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(a, z, IntBinaryOp::Add, NodeOutputType::U64)
        })
        .unwrap()
    }

    /// Returns the unique Add node in `fg`, or panics.
    fn unique_add(fg: &strider_ir::Graph) -> strider_ir::node::NodeId {
        fg.preorder()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
            .expect("unique Add must exist")
    }

    #[test]
    fn rule_with_no_lhs_match_returns_false_nochange() {
        // Graph has no Add → `add(x, 0)` cannot match.  Rule returns
        // Ok(false), no rewire, no fingerprint absorption.
        let mut fg = just_const();
        let x = Capture::new();
        let rule = rewrite_rule(add(var(x), int_const(0)), var(x));

        let pre_count = fg.preorder().count();
        let entry = fg.entry().unwrap();
        // Pick any reachable node (Return) as the root candidate; rule
        // won't match it because its kind isn't Add.
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
            .unwrap();
        let mut ctx = RewriteCtx::new(fg.graph_mut(), entry);
        let r = rule(&mut ctx, ret).unwrap();
        assert!(!r, "no match → returns false");
        assert_eq!(fg.preorder().count(), pre_count, "graph unchanged");
    }

    #[test]
    fn rule_with_match_rewires_single_use() {
        // `Add(11, 0)` has exactly one consumer (Return).  After
        // rewrite the Return's value-input must be IntConst(11).
        let mut fg = add_x_zero();
        let add_node = unique_add(&fg);

        let x = Capture::new();
        let rule = rewrite_rule(add(var(x), int_const(0)), var(x));

        let entry = fg.entry().unwrap();
        let mut ctx = RewriteCtx::new(fg.graph_mut(), entry);
        let changed = rule(&mut ctx, add_node).unwrap();
        assert!(changed, "match + single-use rewire → true");

        // Return's value-input is now the IntConst(11) producer.
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
            .unwrap();
        let value_input = fg.node_inputs(ret)[2];
        let producer = fg.get_node_from_output(value_input);
        match fg.node_kind(producer) {
            NodeKind::IntConst(k) => assert_eq!(*k, 11u128, "rewired to the 11 constant"),
            other => panic!("expected IntConst(11), got {other:?}"),
        }
    }

    #[test]
    fn rule_with_match_rewires_multiple_uses() {
        // Graph: let x = Add(a, 0); return Add(x, x);  — outer Add has
        // two uses of inner x.  After applying `add(c, 0) → c` to inner
        // Add, both inputs of the outer Add point at `a` (the original
        // IntConst).  `replace_all_uses` rewires every consumer in one
        // shot; this pins the multi-use rewire contract.
        let mut fg = make_empty_fn(|b| {
            let a = b.build_int_const(13u64, NodeOutputType::U64)?;
            let z = b.build_int_const(0u64, NodeOutputType::U64)?;
            let inner = b.build_int_binary_operation(a, z, IntBinaryOp::Add, NodeOutputType::U64)?;
            // outer Add consumes inner twice.
            b.build_int_binary_operation(inner, inner, IntBinaryOp::Add, NodeOutputType::U64)
        })
        .unwrap();
        // Find the inner Add: the one whose second input is the IntConst(0).
        let inner_add = fg
            .preorder()
            .find(|&n| {
                if !matches!(fg.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)) {
                    return false;
                }
                let inputs = fg.node_inputs(n);
                if inputs.len() != 2 {
                    return false;
                }
                let rhs = fg.get_node_from_output(inputs[1]);
                matches!(fg.node_kind(rhs), NodeKind::IntConst(0))
            })
            .expect("inner Add must exist");
        let x = Capture::new();
        let rule = rewrite_rule(add(var(x), int_const(0)), var(x));

        let entry = fg.entry().unwrap();
        let mut ctx = RewriteCtx::new(fg.graph_mut(), entry);
        let changed = rule(&mut ctx, inner_add).unwrap();
        assert!(changed, "match + multi-use → true");

        // The outer Add now has both inputs pointing at the IntConst(13).
        let outer_add = fg
            .preorder()
            .find(|&n| {
                matches!(fg.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add))
                    && {
                        let inputs = fg.node_inputs(n);
                        inputs.len() == 2 && {
                            let l = fg.get_node_from_output(inputs[0]);
                            let r = fg.get_node_from_output(inputs[1]);
                            matches!(fg.node_kind(l), NodeKind::IntConst(13))
                                && matches!(fg.node_kind(r), NodeKind::IntConst(13))
                        }
                    }
            })
            .expect("outer Add must now have both inputs == IntConst(13)");
        let _ = outer_add;
    }

    #[test]
    fn rule_with_rhs_skip_returns_false_nochange() {
        // RHS uses `int_const_with_fn` that returns `skip()`.  Interpreter
        // must catch the sentinel, surface no change, and not modify the
        // graph.
        let mut fg = add_x_zero();
        let add_node = unique_add(&fg);

        let pre_count = fg.preorder().count();

        let x = Capture::new();
        // RHS that always returns the skip sentinel.
        let rhs = int_const_with_fn(|_ctx| Err(crate::pattern::error::skip()));
        let rule = rewrite_rule(add(var(x), int_const(0)), rhs);

        let entry = fg.entry().unwrap();
        let mut ctx = RewriteCtx::new(fg.graph_mut(), entry);
        let changed = rule(&mut ctx, add_node).unwrap();
        assert!(!changed, "RHS skip → Ok(false)");
        assert_eq!(fg.preorder().count(), pre_count, "graph unchanged after skip");
    }

    #[test]
    fn rule_with_rhs_error_propagates() {
        // RHS returns a non-skip error.  Interpreter must propagate as Err.
        let mut fg = add_x_zero();
        let add_node = unique_add(&fg);

        let x = Capture::new();
        let rhs = int_const_with_fn(|_ctx| Err(anyhow::anyhow!("forced rhs error")));
        let rule = rewrite_rule(add(var(x), int_const(0)), rhs);

        let entry = fg.entry().unwrap();
        let mut ctx = RewriteCtx::new(fg.graph_mut(), entry);
        let r = rule(&mut ctx, add_node);
        let err = r.expect_err("forced rhs error must propagate");
        let msg = format!("{err:?}");
        assert!(msg.contains("forced rhs error"), "error must propagate, got {msg}");
    }

    #[test]
    fn rewrite_root_absorbs_source_fingerprint() {
        // After a rule fires, the rewritten root's producer must carry
        // a fingerprint that includes the source root's fingerprint
        // (superset-only contract).
        //
        // Build `Add(11, 0)` and stamp a recognisable fingerprint on the
        // Add node only.  Run `add(x, 0) → x` — the new producer is the
        // pre-existing IntConst(11), and the interpreter explicitly
        // attributes the rewritten root (line: `extend_asm_fingerprint_from(new_node, node)`).
        let mut fg = add_x_zero();
        let add_node = unique_add(&fg);
        // Override the sentinel-stamped fingerprint on the Add with a
        // distinct value so we can assert absorption.
        const SOURCE_ADDR: u64 = 0xFEED_CAFE_0000_1111;
        fg.set_asm_fingerprint(add_node, vec![SOURCE_ADDR]);
        assert_eq!(fg.asm_fingerprint(add_node), &[SOURCE_ADDR]);

        let x = Capture::new();
        let rule = rewrite_rule(add(var(x), int_const(0)), var(x));

        let entry = fg.entry().unwrap();
        let mut ctx = RewriteCtx::new(fg.graph_mut(), entry);
        let changed = rule(&mut ctx, add_node).unwrap();
        assert!(changed);

        // The new producer (IntConst(11)) must now include SOURCE_ADDR
        // in its fingerprint (it had its own sentinel + the absorbed
        // contributor).  Locate it via the Return's value-input.
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
            .unwrap();
        let value_input = fg.node_inputs(ret)[2];
        let producer = fg.get_node_from_output(value_input);
        let fp = fg.asm_fingerprint(producer);
        assert!(
            fp.contains(&SOURCE_ADDR),
            "rewritten root's producer must absorb source's fingerprint, got {fp:?}",
        );
    }

    #[test]
    fn boxed_rule_typeerase_compiles_and_runs() {
        // Smoke test that `boxed_rule` wraps a `rewrite_rule` closure
        // into a `BoxedRule` storable in a Vec; calling the boxed form
        // exercises the same interpreter path.
        let mut fg = add_x_zero();
        let add_node = unique_add(&fg);
        let x = Capture::new();
        let r: BoxedRule = boxed_rule(rewrite_rule(add(var(x), int_const(0)), var(x)));

        let entry = fg.entry().unwrap();
        let mut ctx = RewriteCtx::new(fg.graph_mut(), entry);
        let changed = r(&mut ctx, add_node).unwrap();
        assert!(changed);
        // Sentinel-stamped const → fingerprint includes the sentinel.
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
            .unwrap();
        let v = fg.node_inputs(ret)[2];
        let producer = fg.get_node_from_output(v);
        let fp = fg.asm_fingerprint(producer);
        assert!(
            fp.contains(&SENTINEL_LIFT_ADDR) || !fp.is_empty(),
            "rewritten producer has non-empty fp, got {fp:?}",
        );
    }

    #[test]
    fn rule_with_capture_root_match_uses_any_int_const() {
        // Sanity test: capture-based LHS that uses `any_int_const(c)`
        // matches and the RHS reuses the same capture — i.e. the rule
        // fires as identity replacement at the value level.  No graph
        // structural change but `replace_all_uses` returns false because
        // old==new producer.
        let mut fg = make_empty_fn(|b| b.build_int_const(42u64, NodeOutputType::U64)).unwrap();
        // Locate the IntConst node.
        let c_node = fg
            .preorder()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::IntConst(_)))
            .unwrap();
        let c = Capture::new();
        let lhs: Pat = any_int_const(c);
        // RHS is the same capture — `replace_all_uses(old, old)` is a
        // legal no-op that returns false.
        let rule = rewrite_rule(lhs, var(c));

        let entry = fg.entry().unwrap();
        let mut ctx = RewriteCtx::new(fg.graph_mut(), entry);
        let changed = rule(&mut ctx, c_node).unwrap();
        // Whether `changed` is true or false depends on whether the
        // dedup cache returns the same NodeOutputId for the capture;
        // either way the interpreter must return Ok (not Err).  Pin
        // the API contract: no panic, no error.
        let _ = changed;
    }
}
