//! Rule composition: [`rewrite_rule`], [`apply_rules_in_order`], [`BoxedRule`],
//! [`boxed_rule`].

use ir::BuiltFunctionGraph;
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
/// Errors from the graph layer (`make_value_node`, `replace_all_uses`) are
/// wrapped in [`crate::error::ErrorKind::IrError`]; errors from user closures
/// inside `*_const_with!` macros are wrapped in
/// [`crate::error::ErrorKind::RewriteClosure`] (via
/// [`crate::error::Error::rewrite_closure`]).
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
) -> impl Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + 'static {
    let lhs: Pat = lhs.into();
    let rhs: Pat = rhs.into();
    move |fg: &mut BuiltFunctionGraph, node: NodeId| -> Result<bool> {
        // 1. Match LHS.  Keep the matcher borrow in a tight scope so we can
        //    mutate `fg` afterwards.
        let bindings = {
            let matcher = Matcher::new(fg);
            match matcher.match_at(node, &lhs) {
                Some(m) => m.bindings_clone(),
                None => return Ok(false),
            }
        };

        // 2. Fetch root's single value output and its type.
        let [root_out] = fg.graph.node_outputs_exact::<1>(node)?;
        let root_ty = fg.graph.output_kind(root_out).as_value_or_err()?;

        // 3. Materialize RHS.  A closure inside the tree may opt out of the
        //    rewrite by returning `Err(pattern::Error::skip())`; catch that
        //    sentinel here and convert it to "no change" instead of a hard
        //    error.  All other errors propagate.
        let outcome = {
            let mut ctx = BuildCtx {
                graph: fg,
                bindings: &bindings,
                root: node,
                root_ty,
            };
            match rhs.as_dyn().try_build(&mut ctx) {
                Ok(o) => o,
                Err(e) if e.is_skip() => return Ok(false),
                Err(e) => return Err(e),
            }
        };

        match outcome {
            BuildOutcome::Skip => Ok(false),
            BuildOutcome::Out(new_out) => {
                let changed = fg.replace_all_uses(root_out, new_out)?;
                Ok(changed)
            }
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
) -> impl Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + '_
where
    R: Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync,
{
    move |fg, node| {
        let mut any = false;
        for r in rules {
            if r(fg, node)? {
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
    Box<dyn Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync>;

/// Wraps a rewrite-rule closure in a [`BoxedRule`] for storage in a
/// `Vec<BoxedRule>` alongside rules built from other LHS/RHS shapes.
///
/// Typical use:
///
/// ```rust,ignore
/// let rules: Vec<BoxedRule> = vec![
///     boxed_rule(rewrite_rule(add(var(x), int_const(0)), build::cap(x))),
///     boxed_rule(rewrite_rule(sub(var(x), var(x)),       build::int_const_lit(0))),
///     // …
/// ];
/// let apply = apply_rules_in_order(rules);
/// ```
pub fn boxed_rule<R>(r: R) -> BoxedRule
where
    R: Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + 'static,
{
    Box::new(r)
}
