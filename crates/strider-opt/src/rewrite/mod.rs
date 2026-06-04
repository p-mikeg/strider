//! Rewriter infrastructure: [`GraphRewriter`], [`BoxedRule`] / [`boxed_rule`],
//! [`apply_rules_in_order`], [`GraphEditFunctionExt`], and the
//! [`rewrite_rule`] / [`rewrite_rule_runtime`] constructors.
//!
//! The in-place editing context they build on — [`EditFunction`] and its
//! [`FunctionState`](strider_ir::FunctionState) bookkeeping — lives in
//! `strider-ir` and is re-exported through the crate root.
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

use strider_ir::node::NodeId;
use strider_ir::node::ValueId;
use strider_ir::EditFunction;
use strider_ir::Function;

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
/// The returned closure takes `&mut EditFunction<'g>` and a candidate root
/// [`NodeId`], attempts the match, and on success materialises the RHS
/// via [`instantiate`] and redirects the root's value output to the
/// built output via the self-cleaning
/// [`EditFunction::replace_value`](EditFunction::replace_value) — which absorbs
/// the old root's fingerprint, redirects every use, and enqueues the
/// now-orphaned old root for the cull (so its dead cone is reclaimed by the
/// next [`EditFunction::clean`]).
///
/// Returns `Ok(Some(new_out))` if the rule fired and at least one use
/// was redirected — `new_out` is the RHS-built output, which the
/// peephole driver re-examines for cascading folds. Returns `Ok(None)`
/// if the match failed, the RHS produced a
/// [`RewriteSkip`](strider_pattern::RewriteSkip), or `replace_value`
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
) -> impl for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>> + 'static {
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
) -> Result<impl for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>> + 'static> {
    check_capture_coverage(&lhs, &rhs)?;
    Ok(rewrite_rule_impl(lhs, rhs))
}

/// Shared body for [`rewrite_rule`] and [`rewrite_rule_runtime`].
///
/// On each candidate root: match the LHS, fetch the root's single value
/// output + type, snapshot the next `NodeId`, instantiate the RHS,
/// absorb the rewrite root's fingerprint into every freshly created
/// interior node, register those fresh nodes into the cached live/roots
/// state, then redirect the root's uses to the built output via the
/// self-cleaning [`EditFunction::replace_value`] (which also enqueues the
/// orphaned old root so its dead cone is reclaimed on the next `clean`).
fn rewrite_rule_impl(
    lhs: Pattern,
    rhs: Template,
) -> impl for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>> + 'static {
    move |ctx: &mut EditFunction<'_>, node: NodeId| -> Result<Option<ValueId>> {
        // 1. Match LHS. Keep the matcher borrow tight so we can mutate
        //    the wrapped function afterwards.
        let bindings = {
            let matcher = Matcher::try_new(ctx.function_ref())?;
            match matcher.match_at(node, &lhs)? {
                Some(m) => m.bindings_clone(),
                None => return Ok(None),
            }
        };

        // 2. Fetch root's single value output and its type.
        let [root_value] = ctx.function_ref().node_outputs_exact::<1>(node)?;
        let root_ty = ctx.function_ref().value_kind(root_value).as_value_or_err()?;

        // 3. Materialise RHS. A closure inside the tree may opt out via
        //    `Err(strider_pattern::skip())`; catch the sentinel here and
        //    convert it to "no change". Snapshot the next-NodeId BEFORE
        //    the build so we can identify which interior nodes are
        //    freshly allocated (vs returned as dedup-cache hits on
        //    pre-existing nodes).
        let pre_build_node_id = ctx.function_ref().graph().next_node_id();
        let new_value = match instantiate(&rhs, ctx.function_mut(), &bindings, node, root_ty) {
            Ok(value) => value,
            Err(e) if is_skip(&e) => return Ok(None),
            Err(e) => return Err(e),
        };

        // 4. Absorb the rewritten root's asm-fingerprint into EVERY
        //    freshly-created interior node of the RHS subtree (superset
        //    -only). The walk is bounded by `pre_build_node_id`: any
        //    NodeId allocated before the build is pre-existing and stays
        //    untouched.
        let new_node = ctx.function_ref().producer(new_value);
        ctx.function_mut().extend_asm_fingerprint_from(new_node, node);
        absorb_fingerprints_into_fresh_subtree(ctx.function_mut(), new_node, node, pre_build_node_id);

        // 4b. Register every freshly-instantiated RHS node into the cached
        //     live/roots state.  `instantiate` builds the RHS DIRECTLY on
        //     the graph (bypassing `create_node`), so without this the new
        //     nodes are absent from `state.live_nodes` and (if input-less)
        //     from `state.roots`, breaking cache-based iteration
        //     (`live_of_kind`, `postorder`).
        ctx.track_fresh_subtree(new_node, pre_build_node_id);

        // 5. Redirect every consumer of the old root's value output to the
        //    new output via the self-cleaning `replace_value`: it absorbs the
        //    old root's fingerprint into the new producer (superset-only, so
        //    it composes harmlessly with the fresh-subtree absorption above),
        //    redirects every use, AND enqueues the now-orphaned old root for
        //    the cull.  A subsequent `clean()` (run by the pipeline after the
        //    pass, and explicitly in tests) then culls the old root and
        //    cascades its dead operand cone — without this the dead
        //    rewritten-away cone would linger in `live_nodes` until `compact`.
        //
        //    `replace_value` returns `true` when at least one use was
        //    redirected; surface the RHS-built output so the peephole driver
        //    can re-examine the freshly-created node for cascading folds.
        let changed = ctx.replace_value(root_value, new_value)?;
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

// ── EditFunctionMatcherExt — `matcher()` on EditFunction ─────────────

/// Extension trait giving [`EditFunction`] a `matcher()` constructor.
///
/// [`Matcher`] is a `strider-pattern` type, and `strider-ir` (where
/// `EditFunction` lives) does not depend on `strider-pattern`, so this
/// convenience lives here in the optimizer crate rather than on the type
/// itself — keeping the crate dependency one-way.
pub trait EditFunctionMatcherExt {
    /// Build a [`Matcher`] anchored at this context's wrapped function.
    fn matcher(&self) -> Matcher<'_>;
}

impl EditFunctionMatcherExt for EditFunction<'_> {
    #[allow(clippy::expect_used)]
    fn matcher(&self) -> Matcher<'_> {
        Matcher::try_new(self.function_ref())
            .expect("EditFunction matcher: try_for_built invariant guarantees a built Function")
    }
}

// ── GraphEditFunctionExt — `with_rewrite_ctx` helper on Function ────────

/// Extension trait on [`strider_ir::Function`] providing a
/// `with_rewrite_ctx` callback that absorbs the
/// construct-then-pass pattern into one call.
pub trait GraphEditFunctionExt {
    /// Borrow `self` as a [`EditFunction`] and run `f` with mutable
    /// access. The closure's `Result<T>` and the un-built case are
    /// merged into one outer `Result<T>`.
    ///
    /// # Errors
    ///
    /// Returns an error if `self.entry()` is `None`, or if `f` errors.
    fn with_rewrite_ctx<F, T>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&mut EditFunction<'_>) -> Result<T>;
}

impl GraphEditFunctionExt for Function {
    fn with_rewrite_ctx<F, T>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&mut EditFunction<'_>) -> Result<T>,
    {
        let mut ctx = EditFunction::try_for_built(self)?;
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
    pub fn apply<R>(ctx: &mut EditFunction<'_>, rule: R) -> Result<bool>
    where
        R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
    {
        let nodes: Vec<NodeId> = ctx.walk().collect();
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
    pub fn apply_rules<R>(ctx: &mut EditFunction<'_>, rules: &[R]) -> Result<bool>
    where
        R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
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
        R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
    {
        let mut ctx = EditFunction::try_for_built(function)?;
        let candidates: Vec<NodeId> = ctx.walk().collect();
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
        R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
    {
        let mut ctx = EditFunction::try_for_built(function)?;
        let candidates: Vec<NodeId> = ctx.walk().collect();
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
) -> impl for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>> + '_
where
    R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
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
    Box<dyn for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>>;

/// Wraps a rewrite-rule closure in a [`BoxedRule`].
pub fn boxed_rule<R>(r: R) -> BoxedRule
where
    R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>> + 'static,
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
    //! ([`EditFunction::replace_value`] and
    //! [`EditFunction::remove_region_predecessors`]). Both build a *built*
    //! `Function` (entry set) so `EditFunction::try_for_built` succeeds.

    use strider_ir::{EditFunction, FunctionState};
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

        let mut ctx = EditFunction::try_for_built(&mut function).unwrap();
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

        let mut ctx = EditFunction::try_for_built(&mut function).unwrap();
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

        // Act: remove predecessor 0 via the EditFunction.
        let mut ctx = EditFunction::try_for_built(&mut function).unwrap();
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

    // ── kill_node + recursive clean ──────────────────────────────────

    use strider_ir::IntUnaryOp;

    /// Killing the sole consumer (`add`) and draining `clean()` recursively
    /// culls every operand cone node that thereby loses its last use:
    /// `neg`, `k`, and `k2`.
    #[test]
    fn clean_recursively_culls_orphaned_operands() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let k = b.build_int_const(5u64, ValueType::I64).unwrap();
        let k2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        let neg = b
            .build_int_unary_operation(k, IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        let add = b
            .build_int_binary_operation(neg, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // Return a *different* live value so the cone above is orphaned once
        // `add` is killed.
        let ret_val = b.build_int_const(99u64, ValueType::I64).unwrap();
        b.build_return(Some(ret_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let neg_node = function.producer(neg);
        let k_node = function.producer(k);
        let k2_node = function.producer(k2);
        let add_node = function.producer(add);

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        ctx.kill_node(add_node);
        ctx.clean();

        assert!(!ctx.is_live(add_node), "add was killed");
        assert!(!ctx.is_live(neg_node), "neg orphaned → culled");
        assert!(!ctx.is_live(k_node), "k orphaned → culled");
        assert!(!ctx.is_live(k2_node), "k2 orphaned → culled");
    }

    /// A node holding the SAME value in two input slots (`add(k, k)`):
    /// killing it must leave `k` with zero uses AND get `k` culled by the
    /// recursive `clean()`.  Each per-edge last-use check still sees ≥2 uses
    /// before the detach, so the deadness check has to run AFTER the detach
    /// (when all N edges are gone) for the repeated operand to be enqueued.
    #[test]
    fn kill_node_culls_repeated_operand() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        // Build `k` once and feed its value into BOTH operands of the add, so
        // the add genuinely holds the same `ValueId` twice.
        let k = b.build_int_const(5u64, ValueType::I64).unwrap();
        let add = b
            .build_int_binary_operation(k, k, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // Return the add's value so `k` (via `add`) starts entry-reachable and
        // is NOT culled by `EditFunction::new`'s initial cull — the test then
        // exercises the manual `kill_node` path.
        b.build_return(Some(add), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let k_node = function.producer(k);
        let add_node = function.producer(add);

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        assert!(ctx.is_live(k_node), "k starts live (reachable via add)");

        ctx.kill_node(add_node);
        ctx.clean();

        assert!(!ctx.is_live(add_node), "add was killed");
        assert!(!ctx.is_live(k_node), "repeated operand k must be culled");
    }

    /// A shared operand feeding two adds: dropping the use of ONE add must
    /// NOT cull the shared operand — its other consumer keeps it live.
    #[test]
    fn clean_keeps_shared_operand_with_another_live_use() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let k = b.build_int_const(7u64, ValueType::I64).unwrap();
        let other = b.build_int_const(8u64, ValueType::I64).unwrap();
        let add1 = b
            .build_int_binary_operation(k, other, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let add2 = b
            .build_int_binary_operation(k, other, IntBinaryOp::Mul, ValueType::I64)
            .unwrap();
        // Return carries add2 (keeps add2, k, other live).  add1 also shares
        // k/other but feeds nothing reachable.
        b.build_return(Some(add2), &[]).unwrap();
        // Touch add1's value so the binding isn't dropped by the builder.
        let _ = add1;
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let k_node = function.producer(k);
        let add1_node = function.producer(add1);
        let add2_node = function.producer(add2);

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        // `add1` was unreachable, so `new`'s initial cull already removed it,
        // detaching its operands.  `will_detach_value(k)` saw add2 still using
        // k, so k was NOT enqueued/culled.
        assert!(!ctx.is_live(add1_node), "unreachable add1 culled by initial cull");
        assert!(ctx.is_live(add2_node), "add2 stays live (returned)");
        assert!(ctx.is_live(k_node), "shared operand k kept live by add2");

        // A further explicit drain changes nothing — k still has add2's use.
        ctx.clean();
        assert!(ctx.is_live(k_node), "k still live after an extra clean");
    }


    // ── edit verbs maintain live/roots + enqueue maybe-dead ───────────

    use strider_ir::node::{ValueId, ValueKind};

    /// Creating an input-less node marks it live AND records it as a root;
    /// creating a node with inputs marks it live but NOT a root.
    #[test]
    fn create_node_marks_live_and_tracks_root() {
        let mut function = RegisterSet::new()
            .build_fn_single_region()
            .unwrap()
            .build()
            .unwrap();
        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        // Input-less const → live + root.
        let k = ctx.create_node(
            NodeKind::IntConst(5),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let kv = ctx.node_outputs(k)[0];
        assert!(ctx.is_live(k), "fresh const is live");
        assert!(
            ctx.is_root(k),
            "input-less const is a root"
        );

        // Another const + an Add over both → Add is live, NOT a root.
        let k2 = ctx.create_node(
            NodeKind::IntConst(6),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let k2v = ctx.node_outputs(k2)[0];
        let add = ctx.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [kv, k2v],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert!(ctx.is_live(add), "fresh Add is live");
        assert!(
            !ctx.is_root(add),
            "Add has inputs → not a root"
        );
    }

    /// `add_node_input` on a previously input-less node drops it from `roots`.
    #[test]
    fn add_node_input_drops_root_when_node_gains_input() {
        let mut function = RegisterSet::new()
            .build_fn_single_region()
            .unwrap()
            .build()
            .unwrap();
        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        // A fresh, input-less Region → root.
        let region = ctx.create_node(NodeKind::Region, [], [ValueKind::Control]);
        assert!(
            ctx.is_root(region),
            "input-less Region is a root"
        );

        // Feed it a control input → no longer a root.
        let entry = ctx.entry();
        let entry_ctrl = ctx.node_outputs(entry)[0];
        ctx.add_node_input(region, entry_ctrl).unwrap();
        assert!(
            !ctx.is_root(region),
            "Region with an input is no longer a root"
        );
    }

    /// `replace_value(old, new)` enqueues old's producer; a following `clean()`
    /// culls it once it has lost its last use.
    #[test]
    fn replace_value_enqueues_old_producer_and_clean_culls_it() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        // old: a non-side-effecting Neg whose value the Return consumes.
        let k = b.build_int_const(5u64, ValueType::I64).unwrap();
        let old = b
            .build_int_unary_operation(k, IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        // new: a distinct const to replace old with.
        let new = b.build_int_const(9u64, ValueType::I64).unwrap();
        b.build_return(Some(old), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let old_node = function.producer(old);
        let new_node = function.producer(new);
        let k_node = function.producer(k);

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        // Sanity: new starts dead (unreachable) — culled by the initial cull.
        assert!(!ctx.is_live(new_node), "new const was unreachable pre-replace");
        // Re-create new so it's live again (the initial cull removed the
        // dangling one); a fresh const dedups back to the same node and
        // re-enters the live set.
        let new_v: ValueId = ctx
            .make_int_const(9u64, ValueType::I64)
            .unwrap();
        let new_node = ctx.producer(new_v);
        assert!(ctx.is_live(new_node), "re-made new const is live");

        // Replace every use of old with new, then drain.
        let changed = ctx.replace_value(old, new_v).unwrap();
        assert!(changed, "the Return's use of old was redirected");
        ctx.clean();

        // old (and its now-orphaned operand k) are culled; new stays live.
        assert!(!ctx.is_live(old_node), "old producer enqueued + culled");
        assert!(!ctx.is_live(k_node), "old's orphaned operand culled too");
        assert!(ctx.is_live(new_node), "new producer stays live");
    }

    // ── cached walks (live_of_kind) ──────────────────────────────────

    /// `live_of_kind` filters the cached live set by node kind without
    /// re-walking.
    #[test]
    fn live_of_kind_filters_without_walking() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let k1 = b.build_int_const(11u64, ValueType::I64).unwrap();
        let k2 = b.build_int_const(22u64, ValueType::I64).unwrap();
        let add = b
            .build_int_binary_operation(k1, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(add), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let k1_node = function.producer(k1);
        let k2_node = function.producer(k2);

        let mut state = FunctionState::populate(&function);
        let ctx = EditFunction::new(&mut function, &mut state);

        use cranelift_entity::EntityRef;
        let mut consts: Vec<_> = ctx
            .live_of_kind(|k| matches!(k, NodeKind::IntConst(_)))
            .collect();
        consts.sort_unstable_by_key(|n| n.index());
        let mut expected = vec![k1_node, k2_node];
        expected.sort_unstable_by_key(|n| n.index());
        assert_eq!(consts, expected, "exactly the two IntConsts");
    }

    // ── rewrite_rule registers freshly-instantiated nodes ─────────────

    use super::rewrite_rule;
    use strider_pattern::{
        add, any_int_const, int_const_with, Capture, CaptureExt, MatchPat, Matcher,
    };

    /// A `rewrite_rule` whose RHS materialises a BRAND-NEW node (`3 + 4`
    /// folds to a fresh `IntConst(7)` built directly on the graph by
    /// `instantiate`, bypassing `EditFunction::create_node`) must register
    /// that fresh node into the cached live set — otherwise cache-based
    /// iteration (`live_of_kind` / `postorder`) would never see it.
    #[test]
    fn rewrite_rule_registers_fresh_node_in_live_set() {
        let c1 = Capture::new();
        let c2 = Capture::new();

        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let a = b.build_int_const(3u64, ValueType::I64).unwrap();
        let k = b.build_int_const(4u64, ValueType::I64).unwrap();
        let sum = b
            .build_int_binary_operation(a, k, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let add_root = {
            let m = Matcher::try_new(&function).unwrap();
            let pat =
                add(any_int_const().capture(c1), any_int_const().capture(c2)).into_pattern();
            let hits = m.find_all(&pat).unwrap();
            assert!(!hits.is_empty(), "3 + 4 add must match");
            hits[0].root()
        };

        let rule = rewrite_rule(
            add(any_int_const().capture(c1), any_int_const().capture(c2)),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        );

        // Use the primary (Borrowed) pipeline path so the cached
        // live/roots state is the pipeline-threaded `FunctionState`.
        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        let fired = rule(&mut ctx, add_root).unwrap();
        assert!(fired.is_some(), "3 + 4 fold must fire");
        let new_value = fired.unwrap();
        let new_node = ctx.producer(new_value);

        // The freshly-built IntConst(7) must be live and discoverable via
        // the cache-based `live_of_kind` iterator (no graph walk).
        assert!(
            matches!(ctx.node_kind(new_node), NodeKind::IntConst(7)),
            "RHS built IntConst(7)"
        );
        assert!(
            ctx.is_live(new_node),
            "freshly-instantiated RHS node must be registered live"
        );
        assert!(
            ctx.live_of_kind(|k| matches!(k, NodeKind::IntConst(7)))
                .any(|n| n == new_node),
            "live_of_kind must surface the fresh node"
        );
        // It is input-less, so it must also be a cached root.
        assert!(
            ctx.is_root(new_node),
            "input-less fresh const must be a cached root"
        );
    }

    // ── liveness-tracking invariant: cached == entry-reachable ────────
    //
    // The core soundness contract of the self-cleaning `EditFunction`:
    // after any sequence of edits + `clean()`, the cached `live_nodes`
    // must equal `GraphWalkInfo::compute_full(entry).live_nodes` AS A SET,
    // and the cached `roots` must equal that walk's `roots` AS A SET.
    // (Root ORDER differs — the cached form iterates ascending-NodeId,
    // `compute_full` records preorder-discovery order — so these tests
    // compare SETS only.)

    use std::collections::BTreeSet;
    use strider_pattern::template;
    use strider_pattern::var;

    /// Assert the cached live/roots state equals a fresh entry-reachable
    /// walk, as SETS.  This is the reusable liveness invariant: every
    /// node the live graph thinks is alive must be entry-reachable, and
    /// nothing entry-reachable may be missing from the cache.
    ///
    /// `NodeId` is not `Ord`, so sets key on `.index()`.
    fn assert_live_matches_reachable(ctx: &EditFunction) {
        use cranelift_entity::EntityRef;
        let entry = ctx.entry();
        let info =
            strider_ir::walk::GraphWalkInfo::compute_full(ctx.function_ref().graph(), entry);

        let fresh_live: BTreeSet<usize> = info.live_nodes.iter().map(|n| n.index()).collect();
        let cached_live: BTreeSet<usize> = ctx
            .live_snapshot()
            .iter()
            .map(|n| n.index())
            .collect();
        assert_eq!(
            cached_live, fresh_live,
            "cached live_nodes must equal the entry-reachable set"
        );

        let fresh_roots: BTreeSet<usize> = info.roots.into_iter().map(|n| n.index()).collect();
        let cached_roots: BTreeSet<usize> =
            ctx.roots_snapshot().iter().map(|n| n.index()).collect();
        assert_eq!(
            cached_roots, fresh_roots,
            "cached roots must equal the input-less reachable set"
        );
    }

    /// Find the single live root in the freshly built function (via the
    /// matcher).
    fn match_root<L: MatchPat + 'static>(
        function: &strider_ir::Function,
        lhs: L,
    ) -> super::NodeId {
        let m = Matcher::try_new(function).unwrap();
        let pat = lhs.into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert!(!hits.is_empty(), "LHS must match exactly once");
        hits[0].root()
    }

    /// Test 1 — a `rewrite_rule` that folds a chain so an intermediate
    /// `Add` dies.  `(var + 1) + 2 → var + 3`: the inner Add and its
    /// `IntConst(1)` operand are rewritten away.  This is the dominant
    /// Bug A: the raw `replace_all_uses` never enqueues the old outer-Add
    /// root, so its dead operand cone (the inner Add + the consts) lingers
    /// in `live_nodes` until `compact`.
    #[test]
    fn track_chain_fold_culls_dead_intermediate() {
        let vn = reg_vn(0x1000, 8);
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::I64).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::I64).unwrap();
        let outer = b
            .build_int_binary_operation(inner, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let inner_node = function.producer(inner);
        let k1_node = function.producer(k1);
        let outer_node = function.producer(outer);

        let lhs = add(
            add(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        );
        let root = match_root(&function, lhs);
        assert_eq!(root, outer_node, "matched root is the outer Add");

        let rule = rewrite_rule(
            add(
                add(var(x), any_int_const().capture(c1)),
                any_int_const().capture(c2),
            ),
            template::add(
                var(x),
                int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
            ),
        );

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        let fired = rule(&mut ctx, root).unwrap();
        assert!(fired.is_some(), "(var+1)+2 fold must fire");
        ctx.clean();

        // The folded-away nodes are gone; the fresh var+3 is live.
        assert!(!ctx.is_live(outer_node), "old outer Add culled");
        assert!(!ctx.is_live(inner_node), "dead inner Add culled");
        assert!(!ctx.is_live(k1_node), "dead IntConst(1) culled");
        let new_node = ctx.producer(fired.unwrap());
        assert!(ctx.is_live(new_node), "fresh var+3 Add is live");

        assert_live_matches_reachable(&ctx);
    }

    /// Test 2 — a `rewrite_rule` whose RHS materialises a brand-new
    /// multi-node subtree.  The AND-distribution rule
    /// `((a & C1) | (b & C2)) & C3 → (a & (C1&C3)) | (b & (C2&C3))` builds
    /// a fresh `Or`, two fresh `And`s, and two fresh folded consts.  Every
    /// fresh node must be tracked (Bug B: none missing) and the old factored
    /// subtree culled (Bug A).
    #[test]
    fn track_fresh_multi_node_subtree() {
        let a = reg_vn(0x1000, 8);
        let bb = reg_vn(0x1008, 8);
        let (ca, cb) = (Capture::new(), Capture::new());
        let (c1, c2, c3) = (Capture::new(), Capture::new(), Capture::new());

        let mut b = RegisterSet::new()
            .tracked(a)
            .tracked(bb)
            .arg(a)
            .arg(bb)
            .build_fn()
            .unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let av = b.read_variable(&a).unwrap();
        let bv = b.read_variable(&bb).unwrap();
        // C3 = 0x0F, C1 = 0xF0 (C1&C3 == 0 → a disjunct collapses → rule fires),
        // C2 = 0x0C.
        let k1 = b.build_int_const(0xF0u64, ValueType::I64).unwrap();
        let k2 = b.build_int_const(0x0Cu64, ValueType::I64).unwrap();
        let k3 = b.build_int_const(0x0Fu64, ValueType::I64).unwrap();
        let a_and = b
            .build_int_binary_operation(av, k1, IntBinaryOp::And, ValueType::I64)
            .unwrap();
        let b_and = b
            .build_int_binary_operation(bv, k2, IntBinaryOp::And, ValueType::I64)
            .unwrap();
        let or = b
            .build_int_binary_operation(a_and, b_and, IntBinaryOp::Or, ValueType::I64)
            .unwrap();
        let outer = b
            .build_int_binary_operation(or, k3, IntBinaryOp::And, ValueType::I64)
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let outer_node = function.producer(outer);
        let or_node = function.producer(or);
        let k3_node = function.producer(k3);

        use strider_pattern::{and, or as or_pat};
        let lhs = and(
            or_pat(
                and(var(ca), any_int_const().capture(c1)),
                and(var(cb), any_int_const().capture(c2)),
            ),
            any_int_const().capture(c3),
        );
        let root = match_root(&function, lhs);
        assert_eq!(root, outer_node, "matched root is the outer And");

        let rule = rewrite_rule(
            and(
                or_pat(
                    and(var(ca), any_int_const().capture(c1)),
                    and(var(cb), any_int_const().capture(c2)),
                ),
                any_int_const().capture(c3),
            ),
            template::or(
                template::and(var(ca), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
                template::and(var(cb), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
            ),
        );

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        let fired = rule(&mut ctx, root).unwrap();
        assert!(fired.is_some(), "AND-distribution must fire");
        ctx.clean();

        // Old factored shape culled.
        assert!(!ctx.is_live(outer_node), "old outer And culled");
        assert!(!ctx.is_live(or_node), "old Or culled");
        assert!(!ctx.is_live(k3_node), "old C3 const culled");
        // Fresh top Or is live.
        let new_node = ctx.producer(fired.unwrap());
        assert!(ctx.is_live(new_node), "fresh Or is live");
        assert!(matches!(ctx.node_kind(new_node), NodeKind::IntBinaryOp(IntBinaryOp::Or)));

        assert_live_matches_reachable(&ctx);
    }

    /// Test 3 — a `rewrite_rule` whose RHS is a bare captured var
    /// (`x + 0 → x`).  The old `Add` root dies; the captured `x` survives
    /// because the Return still uses it.
    #[test]
    fn track_identity_fold_keeps_survivor() {
        let vn = reg_vn(0x1000, 8);
        let x = Capture::new();

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let zero = b.build_int_const(0u64, ValueType::I64).unwrap();
        let add_node_val = b
            .build_int_binary_operation(xv, zero, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(add_node_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let add_node = function.producer(add_node_val);
        let zero_node = function.producer(zero);
        let x_node = function.producer(xv);

        use strider_pattern::int_const as int_const_match;
        let lhs = add(var(x), int_const_match(0u64));
        let root = match_root(&function, lhs);
        assert_eq!(root, add_node, "matched root is the x+0 Add");

        let rule = rewrite_rule(add(var(x), int_const_match(0u64)), var(x));

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        let fired = rule(&mut ctx, root).unwrap();
        assert!(fired.is_some(), "x+0 fold must fire");
        ctx.clean();

        assert!(!ctx.is_live(add_node), "old Add culled");
        assert!(!ctx.is_live(zero_node), "dead IntConst(0) culled");
        assert!(ctx.is_live(x_node), "captured survivor x stays live");

        assert_live_matches_reachable(&ctx);
    }

    /// Test 4 — a rule whose RHS const dedup-hits an existing identical
    /// const.  `(var + 1) + 2 → var + 3` where `IntConst(3)` already
    /// exists live in the graph (used elsewhere): the fold dedups to the
    /// existing node, which stays live (not double-counted), and the old
    /// root cone is culled.
    #[test]
    fn track_dedup_hit_stays_live() {
        let vn = reg_vn(0x1000, 8);
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::I64).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::I64).unwrap();
        let outer = b
            .build_int_binary_operation(inner, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // A pre-existing live IntConst(3) the fold will dedup onto, kept
        // live by a (side-effecting) Store so it survives independently of
        // the fold.
        let three = b.build_int_const(3u64, ValueType::I64).unwrap();
        let store_addr = b.build_int_const(0x4000u64, ValueType::I64).unwrap();
        b.build_store(store_addr, three, rsleigh::VnSpace::RAM).unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let inner_node = function.producer(inner);
        let outer_node = function.producer(outer);
        let three_node = function.producer(three);

        let root = match_root(
            &function,
            add(
                add(var(x), any_int_const().capture(c1)),
                any_int_const().capture(c2),
            ),
        );
        assert_eq!(root, outer_node);

        let rule = rewrite_rule(
            add(
                add(var(x), any_int_const().capture(c1)),
                any_int_const().capture(c2),
            ),
            template::add(
                var(x),
                int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
            ),
        );

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        let fired = rule(&mut ctx, root).unwrap();
        assert!(fired.is_some());
        let new_value = fired.unwrap();
        ctx.clean();

        // The fresh Add's const operand dedups to the pre-existing IntConst(3).
        let new_add = ctx.producer(new_value);
        let const_operand = ctx.producer(ctx.node_inputs(new_add).into_iter().nth(1).unwrap());
        assert_eq!(
            const_operand, three_node,
            "RHS const dedup-hit the pre-existing IntConst(3)"
        );
        assert!(ctx.is_live(three_node), "deduped const stays live");
        assert!(!ctx.is_live(outer_node), "old outer Add culled");
        assert!(!ctx.is_live(inner_node), "old inner Add culled");

        assert_live_matches_reachable(&ctx);
    }

    /// Test 5 — `GraphRewriter::apply` across a function with several
    /// folds.  After the pass + `clean()`, no accumulated dead cone may
    /// linger in `live_nodes`.
    #[test]
    fn track_graph_rewriter_apply_no_dead_cone() {
        let vn = reg_vn(0x1000, 8);
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

        // ((var + 1) + 2) wrapped again: ((var+1)+2) is itself an operand of
        // a further (… + 4) so apply folds twice.
        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::I64).unwrap();
        let a1 = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::I64).unwrap();
        let a2 = b
            .build_int_binary_operation(a1, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let k4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let a3 = b
            .build_int_binary_operation(a2, k4, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(a3), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let a1_node = function.producer(a1);
        let a2_node = function.producer(a2);

        let rule = rewrite_rule(
            add(
                add(var(x), any_int_const().capture(c1)),
                any_int_const().capture(c2),
            ),
            template::add(
                var(x),
                int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
            ),
        );

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        // Apply repeatedly to a fixed point (apply walks once per call).
        loop {
            let fired = super::GraphRewriter::apply(&mut ctx, &rule).unwrap();
            ctx.clean();
            if !fired {
                break;
            }
        }

        assert!(!ctx.is_live(a1_node), "intermediate Add a1 culled");
        assert!(!ctx.is_live(a2_node), "intermediate Add a2 culled");
        assert_live_matches_reachable(&ctx);
    }

    /// Test 6 — a multi-output template (the Store→Load shape).  The RHS
    /// root `Load` consumes a FRESH `Store`'s memory output; the Store is a
    /// non-root interior node reachable upward through the Load's memory
    /// input.  Both fresh interior nodes (incl. the non-root Store) must be
    /// tracked.
    #[test]
    fn track_multi_output_template_interior() {
        use super::rewrite_rule_runtime;
        use strider_ir::node::ValueType as VT;
        use strider_pattern::matcher::KindSpec;
        use strider_pattern::template::TemplateBuilder;
        use strider_pattern::load;

        // Build a function with a Load(addr) we will rewrite into
        // Load(addr, Store(addr, data, mem)) — forwarding nothing, just a
        // structural multi-output template exercise.
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x2000u64, VT::I64).unwrap();
        let loaded = b
            .build_load(addr, rsleigh::VnSpace::RAM, VT::I64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let load_node = function.producer(loaded);

        // LHS: a Load.  Capture its address so the RHS can re-use it.
        let addr_cap = Capture::new();
        let lhs = load()
            .addr(strider_pattern::any().capture(addr_cap))
            .build();

        // RHS (raw builder): Store(addr, data:IntConst(7), InitialMemory) →
        // Load(addr, store.mem).  Root is the Load (single value output).
        let rhs = {
            let mut tb = TemplateBuilder::new();
            let a = tb.capture(addr_cap);
            let data = tb.leaf(KindSpec::Exact(NodeKind::IntConst(7)));
            // The store needs an incoming memory token; use a fresh
            // InitialMemory leaf (input-less, becomes a root).
            let init_mem_node = tb.node(KindSpec::Exact(NodeKind::InitialMemory));
            let init_mem = tb.memory_output(init_mem_node, 0);
            // Store(RAM): inputs [addr, data, mem_in], output [mem_out].
            let store_node = tb.node(KindSpec::Exact(NodeKind::Store(rsleigh::VnSpace::RAM)));
            tb.input(store_node, 0, a);
            tb.input(store_node, 1, data);
            tb.input(store_node, 2, init_mem);
            let store_mem = tb.memory_output(store_node, 0);
            // Load(RAM): inputs [addr, mem_in], output [value].
            let load_n = tb.node(KindSpec::Exact(NodeKind::Load(rsleigh::VnSpace::RAM)));
            let a2 = tb.capture(addr_cap);
            tb.input(load_n, 0, a2);
            tb.input(load_n, 1, store_mem);
            let out = tb.value_output(load_n, 0);
            tb.set_value_ty(out, VT::I64);
            tb.finish()
        };

        let rule = rewrite_rule_runtime(lhs, rhs).unwrap();

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        let fired = rule(&mut ctx, load_node).unwrap();
        assert!(fired.is_some(), "Load → Load(Store) rewrite must fire");
        ctx.clean();

        let new_load = ctx.producer(fired.unwrap());
        assert!(matches!(ctx.node_kind(new_load), NodeKind::Load(_)));
        assert!(ctx.is_live(new_load), "fresh Load is live");

        // The fresh non-root Store feeding the Load's memory input must be
        // tracked live.
        let mem_in = ctx.node_inputs(new_load).into_iter().nth(1).unwrap();
        let store_node = ctx.producer(mem_in);
        assert!(
            matches!(ctx.node_kind(store_node), NodeKind::Store(_)),
            "Load's memory input is the fresh Store"
        );
        assert!(ctx.is_live(store_node), "fresh interior Store is tracked live");

        // Note: the fresh InitialMemory feeding the Store is input-less, so
        // it must be a cached root.
        let init_mem_in = ctx.node_inputs(store_node).into_iter().nth(2).unwrap();
        let init_mem_node = ctx.producer(init_mem_in);
        assert!(
            ctx.is_root(init_mem_node),
            "fresh input-less InitialMemory is a cached root"
        );

        assert_live_matches_reachable(&ctx);
    }

    /// Test 7 — a direct `ctx.replace_value` + `clean()` (the non-template
    /// path).  Sanity that the curated mutation façade already tracks
    /// correctly: after replacing old with a fresh const and draining, the
    /// cached state equals the entry-reachable walk.
    #[test]
    fn track_direct_replace_value() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let k = b.build_int_const(5u64, ValueType::I64).unwrap();
        let neg = b
            .build_int_unary_operation(k, IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        b.build_return(Some(neg), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let neg_node = function.producer(neg);
        let k_node = function.producer(k);

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        let new_v = ctx.make_int_const(9u64, ValueType::I64).unwrap();
        let new_node = ctx.producer(new_v);
        let changed = ctx.replace_value(neg, new_v).unwrap();
        assert!(changed);
        ctx.clean();

        assert!(!ctx.is_live(neg_node), "old Neg culled");
        assert!(!ctx.is_live(k_node), "Neg's orphaned operand culled");
        assert!(ctx.is_live(new_node), "fresh const live");

        assert_live_matches_reachable(&ctx);
    }

    /// Bug B — a `rewrite_rule` whose freshly-instantiated RHS const
    /// **dedup-revives** a node that was created in the builder but culled
    /// (as unreachable) by `EditFunction::new`'s initial cull.
    ///
    /// `instantiate` builds the RHS const directly on the graph; the dedup
    /// cache hands back the PRE-EXISTING (already-culled) `IntConst(3)`
    /// node, whose id is `< snapshot`.  The fresh-subtree tracking walk
    /// stops at any `< snapshot` node (assuming it is already tracked) — so
    /// the revived const becomes entry-reachable again yet is NEVER
    /// re-inserted into `live_nodes`.  That is the "entry-reachable const
    /// missing from `live_nodes`" symptom.
    #[test]
    fn track_rhs_dedup_revives_culled_const() {
        let vn = reg_vn(0x1000, 8);
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::I64).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::I64).unwrap();
        let outer = b
            .build_int_binary_operation(inner, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // A DANGLING IntConst(3): created in the builder but wired into
        // nothing, so it starts unreachable and is culled by the initial
        // cull.  The fold's RHS const (`1 + 2 == 3`) will dedup back onto it.
        let dangling_three = b.build_int_const(3u64, ValueType::I64).unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let three_node = function.producer(dangling_three);
        let outer_node = function.producer(outer);

        let root = match_root(
            &function,
            add(
                add(var(x), any_int_const().capture(c1)),
                any_int_const().capture(c2),
            ),
        );
        assert_eq!(root, outer_node);

        let rule = rewrite_rule(
            add(
                add(var(x), any_int_const().capture(c1)),
                any_int_const().capture(c2),
            ),
            template::add(
                var(x),
                int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
            ),
        );

        let mut state = FunctionState::populate(&function);
        let mut ctx = EditFunction::new(&mut function, &mut state);

        // The dangling const was culled by the initial cull.
        assert!(
            !ctx.is_live(three_node),
            "dangling IntConst(3) culled as unreachable"
        );

        let fired = rule(&mut ctx, root).unwrap();
        assert!(fired.is_some(), "(var+1)+2 fold must fire");
        ctx.clean();

        // The fresh var+3's const operand dedup-revives the culled IntConst(3).
        let new_add = ctx.producer(fired.unwrap());
        let const_operand =
            ctx.producer(ctx.node_inputs(new_add).into_iter().nth(1).unwrap());
        assert_eq!(
            const_operand, three_node,
            "RHS const dedup-hit the pre-existing (culled) IntConst(3)"
        );
        // Bug B: the revived, now-entry-reachable const must be live again.
        assert!(
            ctx.is_live(three_node),
            "dedup-revived const must be re-registered in live_nodes"
        );

        assert_live_matches_reachable(&ctx);
    }
}
