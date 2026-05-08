//! Rule composition: [`rewrite_rule`], [`apply_rules_in_order`], [`BoxedRule`],
//! [`boxed_rule`].

use ir::Graph;
use ir::node::NodeId;

use crate::error::Result;
use crate::matcher::Matcher;
use crate::pat::Pat;
use crate::pat::traits::{BuildCtx, BuildOutcome};

/// Build a rewrite-rule closure from an LHS and RHS [`Pat`].
///
/// The returned closure takes `&mut BuiltFunctionGraph` and a candidate root
/// [`NodeId`], attempts the match, and on success materializes the RHS
/// template via [`crate::pat::traits::Pattern::try_build`] and redirects
/// the root's value output to the built output via
/// [`BuiltFunctionGraph::replace_all_uses`].
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
/// recover the original.  Use [`crate::error::skip`] inside a closure
/// to opt out of the rewrite without a hard error; the interpreter
/// detects the [`crate::error::RewriteSkip`] sentinel via
/// [`crate::error::is_skip`] and returns `Ok(false)`.
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
        //    error.  All other errors propagate.
        let outcome = {
            let mut bctx = BuildCtx {
                graph: ctx.graph,
                bindings: &bindings,
                root: node,
                root_ty,
            };
            match rhs.as_dyn().try_build(&mut bctx) {
                Ok(o) => o,
                Err(e) if crate::error::is_skip(&e) => return Ok(false),
                Err(e) => return Err(e),
            }
        };

        match outcome {
            BuildOutcome::Skip => Ok(false),
            BuildOutcome::Out(new_out) => {
                // Absorb the rewritten root's asm-fingerprint into the new
                // node BEFORE redirecting uses.  This is the single funnel
                // where every pattern-driven rewrite preserves the
                // contributing-asm-instruction history; whether the RHS
                // built a fresh node or hit the dedup cache, the union
                // semantics of `extend_asm_fingerprint_from` keep us
                // superset-correct.
                let new_node = ctx.graph.get_node_from_output(new_out);
                ctx.graph.extend_asm_fingerprint_from(new_node, node);
                let changed = ctx.graph.replace_all_uses(root_out, new_out)?;
                Ok(changed)
            }
        }
    }
}

/// Mutable rewrite context: a `&mut Graph` together with the function's
/// `entry: NodeId`.  Replaces the prior "wrap into a dummy
/// `BuiltFunctionGraph`" trick — pure-rewrite paths (constant fold,
/// known-bits, flag-cmp canonicalisation, etc.) only ever consult graph
/// + entry, never the CC-bearing fields of `BuiltFunctionGraph`.
///
/// Construct via `RewriteCtx::new(graph, entry)` or
/// `RewriteCtx::for_built(&mut bfg)`.  The matcher inside `rewrite_rule`
/// reads `ctx.graph` + `ctx.entry`; the build path mutates `ctx.graph`.
pub struct RewriteCtx<'g> {
    pub graph: &'g mut Graph,
    pub entry: NodeId,
}

impl<'g> RewriteCtx<'g> {
    /// Constructs a `RewriteCtx` from a raw `(graph, entry)` pair —
    /// the rewrite-only path used by `opt::with_built`,
    /// `strider::rewrite::GraphRewriter::apply_rule`, and similar.
    pub fn new(graph: &'g mut Graph, entry: NodeId) -> Self {
        Self { graph, entry }
    }

    /// Constructs a `RewriteCtx` borrowing from a `BuiltFunctionGraph`'s
    /// inner `graph` + `entry`.  Used by callers that already hold a
    /// fully-built form and want to drive the rewrite engine without
    /// surrendering the wrapper.
    pub fn for_built(bfg: &'g mut ir::BuiltFunctionGraph) -> Self {
        Self {
            graph: &mut bfg.graph,
            entry: bfg.entry,
        }
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
pub fn apply_rules_in_order<R>(
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
/// use pattern::{
///     BoxedRule, Capture, add, apply_rules_in_order, boxed_rule, int_const,
///     rewrite_rule, sub, var,
/// };
///
/// let x = Capture::new();
/// let y = Capture::new();
/// let rules: Vec<BoxedRule> = vec![
///     // add(x, 0) → x
///     boxed_rule(rewrite_rule(add(var(x), int_const(0)), var(x))),
///     // sub(y, y) → 0
///     boxed_rule(rewrite_rule(sub(var(y), var(y)), int_const(0))),
/// ];
/// let _apply = apply_rules_in_order(&rules);
/// ```
pub fn boxed_rule<R>(r: R) -> BoxedRule
where
    R: for<'g> Fn(&mut RewriteCtx<'g>, NodeId) -> Result<bool> + Send + Sync + 'static,
{
    Box::new(r)
}
