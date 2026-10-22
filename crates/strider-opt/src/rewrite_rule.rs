//! RHS buildability is a COMPILE-TIME property: [`rewrite_rule`] bounds its
//! RHS on [`TemplatePat`], implemented only by buildable typed structs, so a
//! wildcard RHS fails to compile.  [`rewrite_rule_runtime`] is the dynamic
//! (FFI) counterpart taking an already-built [`Pattern`] and [`Template`].
//!
//! Asm-fingerprint absorption holds by construction: every fresh RHS node
//! absorbs the matched footprint's fingerprints at creation (superset-only).

use strider_ir::node::{NodeId, ValueId};
use strider_ir::{EditFunction, IRViewer, IRWalker};

use strider_pattern::{
    Capture, MatchPat, Matcher, Pattern, Result, Template, TemplatePat, instantiate_guarded,
    is_skip,
};

/// Builds a rewrite-rule closure from a typed LHS and a buildable typed
/// RHS. A wildcard RHS does not implement [`TemplatePat`], so it is a
/// compile error.
///
/// The returned closure attempts the match at a candidate root and on
/// success materialises the RHS and redirects the root's value output to
/// it. Returns `Ok(Some(new_out))` when at least one use was redirected,
/// `Ok(None)` for a failed match, a skipped RHS, or nothing to redirect.
/// It errors, before building anything, when the RHS would build nodes and
/// no matched node carries an asm-fingerprint (an `InitialVar` or `Phi`
/// root), even where a built constant would dedup onto a fingerprinted node.
///
/// # Single-value-output constraint
///
/// The LHS root must have exactly one value output, since the rule
/// redirects that output's uses. Rooting on a multi-output node errors.
///
/// # Panics
///
/// Panics if the RHS references a [`Capture`] the LHS does not bind.
pub fn rewrite_rule<L: MatchPat + 'static, T: TemplatePat + 'static>(lhs: L, rhs: T) -> BoxedRule {
    let lhs_pat = lhs.into_pattern();
    let rhs_tpl = rhs.into_template();
    check_capture_coverage(&lhs_pat, &rhs_tpl).expect("rewrite_rule: RHS capture not bound by LHS");
    Box::new(rewrite_rule_impl(lhs_pat, rhs_tpl))
}

/// The dynamic (FFI / scripted) counterpart of [`rewrite_rule`], taking an
/// already-built [`Pattern`] and [`Template`].
///
/// # What the node signature leaves open is author-owned
///
/// [`instantiate`](strider_pattern::instantiate) checks every template node
/// against its `NodeKind`'s `expected_signature`: the declared outputs, the
/// input count, each input slot's value kind and the operand widths. It also
/// errors on a gapped or duplicate input slot. What the signature does not pin
/// is left to the RHS author, such as which address a `Load` reads.
///
/// # Errors
///
/// Errors if the RHS references a capture the LHS does not bind. The returned
/// closure errors as [`rewrite_rule`]'s does, including when the match holds
/// no node with an asm-fingerprint.
pub fn rewrite_rule_runtime(lhs: Pattern, rhs: Template) -> Result<BoxedRule> {
    check_capture_coverage(&lhs, &rhs)?;
    Ok(Box::new(rewrite_rule_impl(lhs, rhs)))
}

/// Shared body for [`rewrite_rule`] and [`rewrite_rule_runtime`].
fn rewrite_rule_impl(
    lhs: Pattern,
    rhs: Template,
) -> impl for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>> + 'static {
    move |edit: &mut EditFunction<'_>, node: NodeId| -> Result<Option<ValueId>> {
        // The root check before anything is allocated. A fold table tries
        // every rule at every candidate node, and `Matcher::new` plus
        // `match_at` allocate per attempt, so a rule whose root cannot match
        // used to pay for the attempt and be rejected inside. ~40 of the 60
        // `ConstantFold` rules are rooted on one `IntBinaryOp` variant, which
        // the peephole's own `matches_kind` gate cannot separate: it is a
        // discriminant, identical for all of them.
        if !lhs.root_admits_kind(edit.function().node_kind(node)) {
            return Ok(None);
        }
        // Keep the matcher borrow tight so the function can be mutated
        // afterwards. The snapshotted footprint (root, interior, captured
        // leaves) is the rewrite's proof: the interior nodes get culled, so
        // their asm-fingerprints must be carried onto the RHS.
        let (bindings, matched_nodes) = {
            let matcher = Matcher::new(edit.function());
            match matcher.match_at(node, &lhs)? {
                Some(m) => (m.bindings().clone(), m.matched_nodes().to_vec()),
                None => return Ok(None),
            }
        };

        let (root_value, root_ty) = edit.function().single_value_output(node)?;

        // A fresh node's fingerprint is the footprint's, so a footprint of
        // exempt nodes only (an `InitialVar` or `Phi` root) would leave every
        // built node with none, which `validate` rejects.
        let attributable = |f: &strider_ir::Function| {
            let side = f.side_tables();
            if matched_nodes
                .iter()
                .all(|&n| side.asm_fingerprint_is_empty(n))
            {
                anyhow::bail!(
                    "rewrite matched no node with an instruction address, so the \
                     nodes its replacement builds would carry none; match a node \
                     an instruction produced, or replace with a capture"
                );
            }
            Ok(())
        };

        // Threading the matched footprint as the proof-node set makes EVERY
        // fresh node, not just the root output, absorb the matched subgraph's
        // fingerprints at creation. A closure in the tree may opt out via
        // `skip()`, which becomes "no change" here.
        let new_value = match instantiate_guarded(
            &rhs,
            edit,
            &bindings,
            node,
            &matched_nodes,
            root_ty,
            attributable,
        ) {
            Ok(value) => value,
            Err(e) if is_skip(&e) => return Ok(None),
            Err(e) => return Err(e),
        };

        // A replacement of a different width is not a narrower spelling of the
        // same value, it IS a different value: every use of the root silently
        // starts reading the wrong number of bits. Reachable whenever the RHS
        // hands back an LHS-bound value that the match reached THROUGH a cast
        // (`ignore_casts`), or a template that pins its own root width.
        let new_ty = edit.function().value_type(new_value)?;
        if new_ty != root_ty {
            anyhow::bail!(
                "rewrite would replace a {root_ty:?} value with a {new_ty:?} one; \
                 a capture bound through a cast carries the pre-cast width, so the \
                 replacement has to restate the conversion"
            );
        }

        // `replace_value` redirects EVERY use of the root, the RHS's own fresh
        // edges included, so an RHS reading the root closes a data cycle: the
        // node becomes its own input and nothing downstream rejects it.
        if new_value != root_value && reads_value(edit, new_value, root_value, &matched_nodes) {
            anyhow::bail!(
                "rewrite would make the replacement read the value it replaces, \
                 closing a data cycle; an RHS may not consume its own match root, \
                 and rebuilding the LHS shape reaches the same node through the \
                 dedup cache"
            );
        }

        // Redundant for a fresh-node RHS, but load-bearing for a
        // BARE-CAPTURE RHS such as `add(x, 0) -> x`: that returns the
        // LHS-bound value verbatim, so nothing else carries the culled
        // interior nodes' addresses onto the survivor.
        let new_producer = edit.producer(new_value);
        for &matched in &matched_nodes {
            edit.absorb_node_fingerprint(new_producer, matched);
        }

        // `replace_value` absorbs the old root's fingerprint, redirects every
        // use, and enqueues the orphaned old root for the cull.
        let changed = edit.replace_value(root_value, new_value);
        Ok(changed.then_some(new_value))
    }
}

/// Whether `from`'s cone reads `target`.
///
/// Prunes at the matched footprint: every LHS-bound value other than the root
/// sits strictly below it, so in an acyclic graph none of them reaches it. What
/// is left to walk is the freshly built RHS, bounded by the template.
fn reads_value(f: &impl IRViewer, from: ValueId, target: ValueId, matched: &[NodeId]) -> bool {
    let mut seen = rustc_hash::FxHashSet::default();
    let mut stack = vec![from];
    while let Some(v) = stack.pop() {
        if v == target {
            return true;
        }
        if !seen.insert(v) {
            continue;
        }
        let producer = f.producer(v);
        if matched.contains(&producer) {
            continue;
        }
        stack.extend(f.node_inputs(producer));
    }
    false
}

/// Asserts every capture the RHS references also appears in the LHS.
fn check_capture_coverage(lhs: &Pattern, rhs: &Template) -> Result<()> {
    // GUARANTEED, not merely present: under a `one_of` only one arm fires, so a
    // capture bound in some arms but not all leaves the RHS with nothing to
    // instantiate. Accepting it here would defer the failure to the first node
    // where a non-binding arm happens to match, and `instantiate`'s unbound
    // -capture error is not a skip, so it aborts the whole pipeline for that
    // function. Reject at construction instead: deterministic, and independent
    // of which binary the rule is later run against.
    let lhs_caps = lhs.guaranteed_captures()?;
    let present: rustc_hash::FxHashSet<Capture> = lhs.bound_captures().collect();
    for cap in rhs.referenced_captures() {
        if !lhs_caps.contains(&cap) {
            return if present.contains(&cap) {
                Err(anyhow::anyhow!(
                    "RHS references Capture id={} that the LHS binds only on SOME \
                     alternation arms; every arm must bind it, since only one arm \
                     fires per match",
                    cap.id()
                ))
            } else {
                Err(anyhow::anyhow!(
                    "RHS references Capture id={} that the LHS does not bind",
                    cap.id()
                ))
            };
        }
    }
    Ok(())
}

/// Applies the FIRST matching rule at every reachable node, returning the fire
/// count.
///
/// Rules are tried in order and the first fire ends the node: it redirects the
/// matched root's uses, leaving the root itself detached, and a later rule
/// would still instantiate a full RHS and union the discarded match's
/// fingerprints into what can be a live node.  Same rule as the in-tree
/// peephole driver's `first_matching_rule`.
///
/// One call walks the graph once, so a rule whose output its own LHS matches
/// needs the caller to loop to a fixed point.
///
/// Ends by re-deriving the memory classes `stack_only()` / `heap_only()`
/// read, under the claims the function holds.
///
/// # Errors
///
/// Propagates the first error returned by any rule.
pub fn apply_rules_count<R>(edit: &mut EditFunction<'_>, rules: &[R]) -> Result<usize>
where
    R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
{
    let candidates: Vec<NodeId> = edit.walk().collect();
    let mut applied: usize = 0;
    for node in candidates {
        for r in rules {
            if r(edit, node)?.is_some() {
                applied += 1;
                break;
            }
        }
    }
    // A rule can rewire an address value's inputs, so a surviving memory
    // class names the wrong slot and a fresh `Load` has none; `RegionFilter`
    // reads that table directly, so a stale entry changes what `stack_only()`
    // / `heap_only()` match.
    crate::refresh_memory_classes(edit);
    Ok(applied)
}

/// The common trait-object type both rule constructors box into.
pub type BoxedRule = Box<dyn for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>>;

#[cfg(test)]
mod tests {
    //! Every fixture builds a BUILT `Function` (entry set).

    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{EditFunction, IRBuilderExt, IRViewer, IntBinaryOp};
    use strider_ir_test_utils::{RegisterSet, reg_vn};

    use super::rewrite_rule;
    use strider_pattern::{
        Capture, CaptureExt, MatchPat, Matcher, int_add, int_const, int_const_with,
    };

    /// A sub-pattern the match reached THROUGH a cast matches at the pre-cast
    /// width, so an RHS rebuilt from its captures is narrower than the root it
    /// replaces. Installing it would redirect every I64 use at an I32 value.
    #[test]
    fn a_rewrite_may_not_replace_a_value_with_a_narrower_one() {
        use strider_ir::ExtendOp;
        use strider_pattern::{int_const as int_const_match, int_mul, var};

        let vn = reg_vn(0x1000, 8);
        let a = Capture::new();
        let bcap = Capture::new();

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        // Register-derived, so the cast survives instead of const-folding away.
        let wide_in = b.read_variable(&vn).unwrap();
        let n = b.truncate_if_needed(wide_in, ValueType::int(32)).unwrap();
        let prod = b
            .build_int_binary_operation(n, n, IntBinaryOp::Mul, ValueType::int(32))
            .unwrap();
        let widened = b
            .extend_if_needed(prod, ValueType::int(64), ExtendOp::ZeroExtend)
            .unwrap();
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(widened, zero, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        // `add(mul(a,b), 0) -> a`, written to see through casts. The `mul`
        // exists only BELOW the zero-extend, so the ladder engages and binds
        // `a` at the pre-cast width.
        let lhs = int_add(int_mul(var(a), var(bcap)), int_const_match(0u64))
            .into_pattern()
            .ignore_casts();
        let root = {
            let m = Matcher::new(&function);
            let hits = m.find_all(&lhs).unwrap();
            assert!(
                !hits.is_empty(),
                "the add must match through the zero-extend"
            );
            hits[0].root()
        };
        let rule =
            super::rewrite_rule_runtime(lhs, strider_pattern::TemplatePat::into_template(var(a)))
                .unwrap();

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let err = rule(&mut edit, root).expect_err("a width-changing replacement must be refused");
        let msg = format!("{err}");
        assert!(
            msg.contains("I64") && msg.contains("I32"),
            "the error must name both widths, got: {msg}"
        );
    }

    /// A match made only of fingerprint-exempt nodes has no instruction
    /// address to stamp on a fresh node, so building one is refused before
    /// anything is created; an RHS that skips or is a bare capture builds
    /// nothing and is no change.
    #[test]
    fn a_rewrite_of_an_exempt_only_match_builds_nothing() {
        use strider_pattern::initial_var_for;

        let vn = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(vn).build_fn().unwrap();
        // Entry state carries no instruction address, as in a lift.
        b.set_lift_addr(None);
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        let xv = b.read_variable(&vn).unwrap();
        b.set_lift_addr(Some(0x10));
        b.build_return(Some(xv), &[]).unwrap();
        let mut function = b.build().unwrap();
        let root = match_root(&function, initial_var_for(vn));
        assert!(matches!(function.node_kind(root), NodeKind::InitialVar(_)));
        assert!(function.side_tables().asm_fingerprint_is_empty(root));
        let nodes_before = function.graph().all_node_ids().count();

        let rule = rewrite_rule(initial_var_for(vn), int_const_with!([] => 3u128));
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let err = rule(&mut edit, root).expect_err("an unattributable build must be refused");
        assert!(
            format!("{err}").contains("instruction address"),
            "the error must say why: {err}"
        );
        assert_eq!(
            edit.function().graph().all_node_ids().count(),
            nodes_before,
            "the refusal creates no node"
        );
        drop(edit);
        strider_ir::validate::validate(&function).expect("the graph stays valid");

        // A declined RHS is no change, even one that builds before declining.
        let declined = || strider_pattern::int_const_with_fn(|_| Err(strider_pattern::skip()));
        let skips = rewrite_rule(initial_var_for(vn), declined());
        let skips_late = rewrite_rule(
            initial_var_for(vn),
            template::int_add(declined(), int_const_with!([] => 1u128)),
        );
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        assert_eq!(skips(&mut edit, root).unwrap(), None, "a skip is no change");
        assert_eq!(
            super::apply_rules_count(&mut edit, &[skips_late]).unwrap(),
            0,
            "a skip after a built operand is no change"
        );
        assert_eq!(
            edit.function().graph().all_node_ids().count(),
            nodes_before,
            "a declined RHS creates no node"
        );
        drop(edit);

        let x = Capture::new();
        let keep = rewrite_rule(initial_var_for(vn).capture(x), var(x));
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        assert_eq!(
            keep(&mut edit, root).unwrap(),
            None,
            "a bare capture of the root itself is a no-op, not an error"
        );
    }

    /// An RHS node built directly on the graph by `instantiate` must still
    /// land in the cached live set.
    #[test]
    fn rewrite_rule_registers_fresh_node_in_live_set() {
        let c1 = Capture::new();
        let c2 = Capture::new();

        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let a = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let k = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(a, k, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let add_root = {
            let m = Matcher::new(&function);
            let pat = int_add(int_const(c1), int_const(c2)).into_pattern();
            let hits = m.find_all(&pat).unwrap();
            assert!(!hits.is_empty(), "3 + 4 add must match");
            hits[0].root()
        };

        let rule = rewrite_rule(
            int_add(int_const(c1), int_const(c2)),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        );

        // Mirror the pipeline construction path so the cached live/roots
        // state matches a real run.
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, add_root).unwrap();
        assert!(fired.is_some(), "3 + 4 fold must fire");
        let new_value = fired.unwrap();
        let new_node = edit.producer(new_value);

        assert!(
            matches!(edit.node_kind(new_node), NodeKind::IntConst(_))
                && edit.int_const_u128(new_value) == Some(7),
            "RHS built IntConst(7)"
        );
        assert!(
            edit.is_live(new_node),
            "freshly-instantiated RHS node must be registered live"
        );
        assert!(
            edit.live_of_kind(|k| matches!(k, NodeKind::IntConst(_)))
                .any(|n| n == new_node),
            "live_of_kind must surface the fresh node"
        );
        assert!(
            edit.is_root(new_node),
            "input-less fresh const must be a cached root"
        );
    }

    use strider_ir_test_utils::assert_live_matches_reachable;
    use strider_pattern::{template, var};

    fn match_root<L: MatchPat + 'static>(function: &strider_ir::Function, lhs: L) -> super::NodeId {
        let m = Matcher::new(function);
        let pat = lhs.into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert!(!hits.is_empty(), "LHS must match exactly once");
        hits[0].root()
    }

    /// Folding `(var + 1) + 2` to `var + 3` rewrites away the inner Add and
    /// its `IntConst(1)`; neither may linger in `live_nodes`.
    #[test]
    fn track_chain_fold_culls_dead_intermediate() {
        let vn = reg_vn(0x1000, 8);
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let outer = b
            .build_int_binary_operation(inner, k2, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let inner_node = function.producer(inner);
        let k1_node = function.producer(k1);
        let outer_node = function.producer(outer);

        let lhs = int_add(int_add(var(x), int_const(c1)), int_const(c2));
        let root = match_root(&function, lhs);
        assert_eq!(root, outer_node, "matched root is the outer Add");

        let rule = rewrite_rule(
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
            template::int_add(
                var(x),
                int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
            ),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some(), "(var+1)+2 fold must fire");
        edit.clean();

        assert!(!edit.is_live(outer_node), "old outer Add culled");
        assert!(!edit.is_live(inner_node), "dead inner Add culled");
        assert!(!edit.is_live(k1_node), "dead IntConst(1) culled");
        let new_node = edit.producer(fired.unwrap());
        assert!(edit.is_live(new_node), "fresh var+3 Add is live");

        assert_live_matches_reachable(&edit);
    }

    /// AND-distribution, `((a & C1) | (b & C2)) & C3` to
    /// `(a & (C1&C3)) | (b & (C2&C3))`: every fresh node must be tracked and
    /// the old factored subtree culled.
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
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let av = b.read_variable(&a).unwrap();
        let bv = b.read_variable(&bb).unwrap();
        // C1 & C3 == 0 collapses a disjunct, which is what makes the rule
        // fire.
        let k1 = b.build_int_const(0xF0u64, ValueType::int(64)).unwrap();
        let k2 = b.build_int_const(0x0Cu64, ValueType::int(64)).unwrap();
        let k3 = b.build_int_const(0x0Fu64, ValueType::int(64)).unwrap();
        let a_and = b
            .build_int_binary_operation(av, k1, IntBinaryOp::And, ValueType::int(64))
            .unwrap();
        let b_and = b
            .build_int_binary_operation(bv, k2, IntBinaryOp::And, ValueType::int(64))
            .unwrap();
        let or = b
            .build_int_binary_operation(a_and, b_and, IntBinaryOp::Or, ValueType::int(64))
            .unwrap();
        let outer = b
            .build_int_binary_operation(or, k3, IntBinaryOp::And, ValueType::int(64))
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let outer_node = function.producer(outer);
        let or_node = function.producer(or);
        let k3_node = function.producer(k3);

        use strider_pattern::{int_and, int_const, int_or as or_pat};
        let lhs = int_and(
            or_pat(
                int_and(var(ca), int_const(c1)),
                int_and(var(cb), int_const(c2)),
            ),
            int_const(c3),
        );
        let root = match_root(&function, lhs);
        assert_eq!(root, outer_node, "matched root is the outer And");

        let rule = rewrite_rule(
            int_and(
                or_pat(
                    int_and(var(ca), int_const(c1)),
                    int_and(var(cb), int_const(c2)),
                ),
                int_const(c3),
            ),
            template::int_or(
                template::int_and(var(ca), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
                template::int_and(var(cb), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
            ),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some(), "AND-distribution must fire");
        edit.clean();

        assert!(!edit.is_live(outer_node), "old outer And culled");
        assert!(!edit.is_live(or_node), "old Or culled");
        assert!(!edit.is_live(k3_node), "old C3 const culled");
        let new_node = edit.producer(fired.unwrap());
        assert!(edit.is_live(new_node), "fresh Or is live");
        assert!(matches!(
            edit.node_kind(new_node),
            NodeKind::IntBinaryOp(IntBinaryOp::Or)
        ));

        assert_live_matches_reachable(&edit);
    }

    /// With a bare-capture RHS (`x + 0` to `x`) the old `Add` root dies and
    /// the captured `x` survives, since the Return still uses it.
    #[test]
    fn track_identity_fold_keeps_survivor() {
        let vn = reg_vn(0x1000, 8);
        let x = Capture::new();

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        let add_node_val = b
            .build_int_binary_operation(xv, zero, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(add_node_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let add_node = function.producer(add_node_val);
        let zero_node = function.producer(zero);
        let x_node = function.producer(xv);

        use strider_pattern::int_const as int_const_match;
        let lhs = int_add(var(x), int_const_match(0u64));
        let root = match_root(&function, lhs);
        assert_eq!(root, add_node, "matched root is the x+0 Add");

        let rule = rewrite_rule(int_add(var(x), int_const_match(0u64)), var(x));

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some(), "x+0 fold must fire");
        edit.clean();

        assert!(!edit.is_live(add_node), "old Add culled");
        assert!(!edit.is_live(zero_node), "dead IntConst(0) culled");
        assert!(edit.is_live(x_node), "captured survivor x stays live");

        assert_live_matches_reachable(&edit);
    }

    /// A bare-capture identity fold must carry the culled INTERIOR matched
    /// nodes' asm-fingerprints onto the survivor, per the superset-only
    /// proof contract.
    #[test]
    fn identity_fold_carries_interior_fingerprint_onto_survivor() {
        let vn = reg_vn(0x1000, 8);
        let x = Capture::new();

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        // Distinct addresses: interior zero 0xCC, root add 0xDD (which
        // replace_value absorbs either way).
        let xv = b.read_variable(&vn).unwrap();
        b.set_lift_addr(Some(0xCC));
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        b.set_lift_addr(Some(0xDD));
        let add_val = b
            .build_int_binary_operation(xv, zero, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.set_lift_addr(Some(0xEE));
        b.build_return(Some(add_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let x_node = function.producer(xv);

        use strider_pattern::int_const as int_const_match;
        let root = match_root(&function, int_add(var(x), int_const_match(0u64)));
        let rule = rewrite_rule(int_add(var(x), int_const_match(0u64)), var(x));

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        assert!(
            rule(&mut edit, root).unwrap().is_some(),
            "x+0 fold must fire"
        );
        edit.clean();

        let fp = edit.function().side_tables().asm_fingerprint(x_node);
        assert!(
            fp.contains(&0xCC),
            "survivor must absorb the culled interior const's fingerprint 0xCC: {fp:?}"
        );
        assert!(
            fp.contains(&0xDD),
            "survivor must absorb the culled root Add's fingerprint 0xDD: {fp:?}"
        );
    }

    /// When the RHS const dedup-hits an `IntConst(3)` already live in the
    /// graph, that node stays live (not double-counted) and the old root
    /// cone is still culled.
    #[test]
    fn track_dedup_hit_stays_live() {
        let vn = reg_vn(0x1000, 8);
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let outer = b
            .build_int_binary_operation(inner, k2, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        // Kept live by a side-effecting Store so it survives independently
        // of the fold.
        let three = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let store_addr = b.build_int_const(0x4000u64, ValueType::int(64)).unwrap();
        b.build_store(store_addr, three, rsleigh::VnSpace::RAM)
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let inner_node = function.producer(inner);
        let outer_node = function.producer(outer);
        let three_node = function.producer(three);

        let root = match_root(
            &function,
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
        );
        assert_eq!(root, outer_node);

        let rule = rewrite_rule(
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
            template::int_add(
                var(x),
                int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
            ),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some());
        let new_value = fired.unwrap();
        edit.clean();

        let new_add = edit.producer(new_value);
        let const_operand = edit.producer(edit.node_inputs(new_add)[1]);
        assert_eq!(
            const_operand, three_node,
            "RHS const dedup-hit the pre-existing IntConst(3)"
        );
        assert!(edit.is_live(three_node), "deduped const stays live");
        assert!(!edit.is_live(outer_node), "old outer Add culled");
        assert!(!edit.is_live(inner_node), "old inner Add culled");

        assert_live_matches_reachable(&edit);
    }

    /// Every rule is tried at every node, so a node a rule already rewrote is
    /// still offered to the rest: the first fire is what the graph keeps, and a
    /// later rule at the same node redirects nothing.
    #[test]
    fn apply_rules_count_keeps_the_first_fire_at_a_node() {
        let vn = reg_vn(0x1000, 8);
        let (x1, c1) = (Capture::new(), Capture::new());
        let (x2, c2) = (Capture::new(), Capture::new());

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        // Both match `var + <const>`; only the first may take effect.
        let first = rewrite_rule(
            int_add(var(x1), int_const(c1)),
            int_const_with!([c1: uint] => c1.wrapping_add(10)),
        );
        let second = rewrite_rule(
            int_add(var(x2), int_const(c2)),
            int_const_with!([c2: uint] => c2.wrapping_add(20)),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let fired = super::apply_rules_count(&mut edit, &[first, second]).unwrap();
        edit.clean();

        assert_eq!(fired, 1, "the second rule at that node redirects nothing");
        let returned = crate::test_support::return_value(edit.function().graph()).unwrap();
        assert_eq!(
            edit.function().int_const_u128(returned),
            Some(11),
            "the graph keeps the FIRST rule's result"
        );
    }

    /// After `apply_rules_count` plus `clean()`, no accumulated dead cone
    /// may linger in `live_nodes`.
    #[test]
    fn track_apply_rules_count_no_dead_cone() {
        let vn = reg_vn(0x1000, 8);
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

        // `((var+1)+2)` is itself an operand of a further `+ 4`, so apply
        // folds twice.
        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let a1 = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let a2 = b
            .build_int_binary_operation(a1, k2, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let k4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let a3 = b
            .build_int_binary_operation(a2, k4, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(a3), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let a1_node = function.producer(a1);
        let a2_node = function.producer(a2);

        let rule = rewrite_rule(
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
            template::int_add(
                var(x),
                int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
            ),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        // Each call walks once, so loop to a fixed point.
        loop {
            let fired =
                super::apply_rules_count(&mut edit, std::slice::from_ref(&rule)).unwrap() > 0;
            edit.clean();
            if !fired {
                break;
            }
        }

        assert!(!edit.is_live(a1_node), "intermediate Add a1 culled");
        assert!(!edit.is_live(a2_node), "intermediate Add a2 culled");
        assert_live_matches_reachable(&edit);
    }

    /// A multi-output template: the RHS root `Load` consumes a fresh
    /// `Store`'s memory output. Both fresh interior nodes must be tracked.
    /// The rewrite facade roots a template at a value, so it cannot put the
    /// Store on the exit chain; the test threads it to the `Return` itself.
    #[test]
    fn track_multi_output_template_interior() {
        use super::rewrite_rule_runtime;
        use strider_ir::node::ValueType as VT;
        use strider_pattern::load;
        use strider_pattern::template::{TemplateBuilder, TemplateKind};

        // Rewrites `Load(mem, addr)` into
        // `Load(Store(InitialMemory, addr, 7), addr)`.
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x2000u64, VT::int(64)).unwrap();
        let loaded = b
            .build_load(addr, rsleigh::VnSpace::RAM, VT::int(64))
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let load_node = function.producer(loaded);

        // Capture the Load's address so the RHS can re-use it.
        let addr_cap = Capture::new();
        let lhs = load()
            .addr(strider_pattern::anything().capture(addr_cap))
            .build();

        // Raw-builder RHS rooted at the Load, its single value output.
        let rhs = {
            let mut tb = TemplateBuilder::new();
            let a = tb.capture(addr_cap);
            let data = tb.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| Ok(7u128))));
            tb.set_value_ty(data, VT::int(64));
            // The store needs an incoming memory token; this leaf is
            // input-less, so it becomes a root.
            let init_mem_node = tb.node(NodeKind::InitialMemory);
            let init_mem = tb.memory_output(init_mem_node, 0);
            // Store(RAM): inputs [mem, addr, data], output [mem_out].
            let store_node = tb.node(NodeKind::Store(rsleigh::VnSpace::RAM));
            tb.input(store_node, 0, init_mem);
            tb.input(store_node, 1, a);
            tb.input(store_node, 2, data);
            let store_mem = tb.memory_output(store_node, 0);
            // Load(RAM): inputs [mem, addr], output [value].
            let load_n = tb.node(NodeKind::Load(rsleigh::VnSpace::RAM));
            let a2 = tb.capture(addr_cap);
            tb.input(load_n, 0, store_mem);
            tb.input(load_n, 1, a2);
            let out = tb.value_output(load_n, 0);
            tb.set_value_ty(out, VT::int(64));
            tb.finish()
        };

        let rule = rewrite_rule_runtime(lhs, rhs).unwrap();

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, load_node).unwrap();
        assert!(fired.is_some(), "Load -> Load(Store) rewrite must fire");
        edit.clean();

        let new_load = edit.producer(fired.unwrap());
        assert!(matches!(edit.node_kind(new_load), NodeKind::Load(_)));
        assert!(edit.is_live(new_load), "fresh Load is live");

        let mem_in = edit.node_inputs(new_load)[NodeKind::ACCESS_MEM_SLOT];
        let store_node = edit.producer(mem_in);
        assert!(
            matches!(edit.node_kind(store_node), NodeKind::Store(_)),
            "Load's memory input is the fresh Store"
        );
        assert!(
            edit.is_live(store_node),
            "fresh interior Store is tracked live"
        );

        let init_mem_in = edit.node_inputs(store_node)[NodeKind::ACCESS_MEM_SLOT];
        let init_mem_node = edit.producer(init_mem_in);
        assert!(
            edit.is_root(init_mem_node),
            "fresh input-less InitialMemory is a cached root"
        );

        assert_live_matches_reachable(&edit);

        // The matched root's asm-fingerprint must reach every fresh interior
        // RHS node, memory and value alike, stamped at creation.
        let root_fp: Vec<u64> = edit
            .function()
            .side_tables()
            .asm_fingerprint(load_node)
            .into_iter()
            .collect();
        assert!(
            !root_fp.is_empty(),
            "fixture's matched root must carry a fingerprint"
        );
        for n in edit.live_of_kind(|k| {
            matches!(
                k,
                NodeKind::IntBinaryOp(_)
                    | NodeKind::IntConst(_)
                    | NodeKind::Store(_)
                    | NodeKind::Load(_)
            )
        }) {
            let fp = edit.function().side_tables().asm_fingerprint(n);
            assert!(
                root_fp.iter().all(|a| fp.contains(a)),
                "fresh RHS node {n:?} missing root fingerprint"
            );
        }

        // `Return` inputs are [control, mem, ..].
        let ret = edit
            .live_of_kind(|k| matches!(k, NodeKind::Return))
            .next()
            .unwrap();
        let mem_slot = edit.node_input_id_at(ret, 1).unwrap();
        let store_mem = edit.memory_output_of(store_node).unwrap();
        edit.redirect_input(mem_slot, store_mem);
        edit.clean();
        strider_ir::validate::validate(edit.function())
            .map_err(|e| e.report(edit.function()))
            .unwrap();
    }

    /// The RHS const dedup-REVIVES a node built earlier but culled as
    /// unreachable: the dedup-cache hit must re-enter `live_nodes`/`roots`.
    #[test]
    fn track_rhs_dedup_revives_culled_const() {
        let vn = reg_vn(0x1000, 8);
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let outer = b
            .build_int_binary_operation(inner, k2, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        // Wired into nothing, so it starts unreachable and the initial cull
        // removes it. The fold's RHS const (1 + 2) dedups back onto it.
        let dangling_three = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let three_node = function.producer(dangling_three);
        let outer_node = function.producer(outer);

        let root = match_root(
            &function,
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
        );
        assert_eq!(root, outer_node);

        let rule = rewrite_rule(
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
            template::int_add(
                var(x),
                int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
            ),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        assert!(
            !edit.is_live(three_node),
            "dangling IntConst(3) culled as unreachable"
        );

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some(), "(var+1)+2 fold must fire");
        edit.clean();

        let new_add = edit.producer(fired.unwrap());
        let const_operand = edit.producer(edit.node_inputs(new_add)[1]);
        assert_eq!(
            const_operand, three_node,
            "RHS const dedup-hit the pre-existing (culled) IntConst(3)"
        );
        assert!(
            edit.is_live(three_node),
            "dedup-revived const must be re-registered in live_nodes"
        );

        assert_live_matches_reachable(&edit);
    }
}
