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
    Capture, MatchPat, Matcher, Pattern, Result, Template, TemplatePat, instantiate, is_skip,
};

/// Builds a rewrite-rule closure from a typed LHS and a buildable typed
/// RHS. A wildcard RHS does not implement [`TemplatePat`], so it is a
/// compile error.
///
/// The returned closure attempts the match at a candidate root and on
/// success materialises the RHS and redirects the root's value output to
/// it. Returns `Ok(Some(new_out))` when at least one use was redirected,
/// `Ok(None)` for a failed match, a skipped RHS, or nothing to redirect.
///
/// # Single-value-output constraint
///
/// The LHS root must have exactly one value output, since the rule
/// redirects that output's uses. Rooting on a multi-output node errors.
///
/// # Panics
///
/// Panics if the RHS references a [`Capture`] the LHS does not bind.
#[allow(clippy::expect_used)]
pub fn rewrite_rule<L: MatchPat + 'static, T: TemplatePat + 'static>(lhs: L, rhs: T) -> BoxedRule {
    let lhs_pat = lhs.into_pattern();
    let rhs_tpl = rhs.into_template();
    check_capture_coverage(&lhs_pat, &rhs_tpl).expect("rewrite_rule: RHS capture not bound by LHS");
    Box::new(rewrite_rule_impl(lhs_pat, rhs_tpl))
}

/// The dynamic (FFI / scripted) counterpart of [`rewrite_rule`], taking an
/// already-built [`Pattern`] and [`Template`].
///
/// # Output-signature validity is author-owned
///
/// [`instantiate`] calls `Graph::create_node` with the [`Template`]'s
/// DECLARED output signature and never runs [`strider_ir::validate`], so
/// the RHS author owns two invariants nothing downstream checks:
///
/// * Each template node's declared output signature must match its
///   `NodeKind`'s real `expected_signature` (kind, slot count, types).
/// * No two producers may be wired into the same input slot; a duplicate
///   silently drops the earlier edge.
///
/// The typed `template::` builders guarantee both; the raw
/// [`TemplateBuilder`](strider_pattern::template::TemplateBuilder) does not.
///
/// # Errors
///
/// Errors if the RHS references a capture the LHS does not bind.
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
        // Keep the matcher borrow tight so the function can be mutated
        // afterwards. The snapshotted footprint (root, interior, captured
        // leaves) is the rewrite's proof: the interior nodes get culled, so
        // their asm-fingerprints must be carried onto the RHS.
        let (bindings, matched_nodes) = {
            let matcher = Matcher::new(edit.function());
            match matcher.match_at(node, &lhs)? {
                Some(m) => (m.bindings_clone(), m.matched_nodes().to_vec()),
                None => return Ok(None),
            }
        };

        let (root_value, root_ty) = edit.function().single_value_output(node)?;

        // Threading the matched footprint as the proof-node set makes EVERY
        // fresh node, not just the root output, absorb the matched subgraph's
        // fingerprints at creation. A closure in the tree may opt out via
        // `skip()`, which becomes "no change" here.
        let new_value = match instantiate(&rhs, edit, &bindings, node, &matched_nodes, root_ty) {
            Ok(value) => value,
            Err(e) if is_skip(&e) => return Ok(None),
            Err(e) => return Err(e),
        };

        // Redundant for a fresh-node RHS, but load-bearing for a
        // BARE-CAPTURE RHS such as `add(x, 0) -> x`: that returns the
        // LHS-bound value verbatim, so nothing else carries the culled
        // interior nodes' addresses onto the survivor.
        let new_producer = edit.producer(new_value);
        for &matched in &matched_nodes {
            edit.function_mut()
                .side_tables_mut()
                .extend_asm_fingerprint_from(new_producer, matched);
        }

        // `replace_value` absorbs the old root's fingerprint, redirects every
        // use, and enqueues the orphaned old root for the cull.
        let changed = edit.replace_value(root_value, new_value)?;
        Ok(changed.then_some(new_value))
    }
}

/// Asserts every capture the RHS references also appears in the LHS.
fn check_capture_coverage(lhs: &Pattern, rhs: &Template) -> Result<()> {
    let lhs_caps: rustc_hash::FxHashSet<Capture> = lhs.bound_captures().collect();
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

/// Applies `rules` round-robin at every reachable node, returning the total
/// per-`(node, rule)` fire count.
///
/// Rules are tried in order at each node, and a firing rule redirects the
/// matched root's uses, so a later rule at that node sees the rewritten
/// graph.
///
/// # Errors
///
/// Propagates the first non-skip error returned by any rule.
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
            }
        }
    }
    Ok(applied)
}

/// Composes rewrite-rule closures into one that tries every rule at the same
/// root. Once a rule fires the root's uses are redirected, so subsequent
/// rules see the new graph state and may no longer apply.
///
/// `Ok(Some(new_out))` names the output of the LAST rule to fire, whose
/// redirect won.
pub fn apply_rules_in_order<R>(
    rules: &[R],
) -> impl for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>> + '_
where
    R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
{
    move |edit, node| {
        let mut last: Option<ValueId> = None;
        for r in rules {
            if let Some(out) = r(edit, node)? {
                last = Some(out);
            }
        }
        Ok(last)
    }
}

/// The common trait-object type both rule constructors box into.
pub type BoxedRule = Box<dyn for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>>;

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]
mod tests {
    //! Every fixture builds a BUILT `Function` (entry set).

    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{EditFunction, FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp};
    use strider_ir_test_utils::{RegisterSet, reg_vn};

    /// `replace_value` unions the old producer's asm-fingerprint into the
    /// new one and redirects every use.
    #[test]
    fn replace_value_absorbs_fingerprint_and_redirects_uses() {
        let mut b: FunctionBuilder = RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region");

        b.set_lift_addr(Some(0xAA));
        let old_value = b.build_int_const(10u64, ValueType::I64).unwrap();
        b.set_lift_addr(Some(0xBB));
        let new_value = b.build_int_const(20u64, ValueType::I64).unwrap();
        // Add(old, old): two uses of old_value.
        let sink = b
            .build_int_binary_operation(old_value, old_value, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(sink), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let new_node = function.producer(new_value);
        let sink_node = function.producer(sink);

        let mut edit = EditFunction::new(&mut function);
        let changed = edit.replace_value(old_value, new_value).unwrap();
        assert!(changed, "a live use existed → changed");

        let fp = function.side_tables().asm_fingerprint(new_node);
        assert!(
            fp.contains(&0xAA),
            "absorbed old's fingerprint 0xAA: {fp:?}"
        );
        assert!(
            fp.contains(&0xBB),
            "kept new's own fingerprint 0xBB: {fp:?}"
        );

        let sink_inputs: Vec<_> = function.node_inputs(sink_node).into_iter().collect();
        assert_eq!(
            sink_inputs,
            vec![new_value, new_value],
            "sink inputs must now point at new_value"
        );

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

        // old is wired to nothing; only new_value is used, by the Return.
        b.set_lift_addr(Some(0xAA));
        let old_value = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.set_lift_addr(Some(0xBB));
        let new_value = b.build_int_const(2u64, ValueType::I64).unwrap();
        b.build_return(Some(new_value), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let new_node = function.producer(new_value);

        let mut edit = EditFunction::new(&mut function);
        let changed = edit.replace_value(old_value, new_value).unwrap();
        assert!(!changed, "no uses of old → changed must be false");

        let fp = function.side_tables().asm_fingerprint(new_node);
        assert!(
            fp.contains(&0xAA),
            "fingerprint absorbed even when no uses redirected: {fp:?}"
        );
        assert!(
            fp.contains(&0xBB),
            "kept new's own fingerprint 0xBB: {fp:?}"
        );
    }

    /// Removing predecessor 0 of a 2-predecessor `Region` strips both its
    /// first control slot and the matching value slot from the `Phi` over
    /// it, leaving `[token, surviving_value]`.
    #[test]
    fn remove_region_predecessors_strips_ctrl_and_phi_slot() {
        // `if (true) { var = 1 } else { var = 2 }; return var;` gives the
        // join Region two control predecessors and a 2-value VarPhi.
        let var = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(var).arg(var).build_fn().unwrap();
        let entry = b.create_region_all().unwrap();
        let true_r = b.create_region_all().unwrap();
        let false_r = b.create_region_all().unwrap();
        let join = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();

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

        // Filtering on input count 3 skips any single-predecessor VarPhi the
        // builder may have produced for an intermediate region.
        let phi = function
            .graph()
            .all_node_ids()
            .find(|&n| {
                matches!(function.node_kind(n), NodeKind::Phi)
                    && function.get_vn_for_value(function.node_outputs(n)[0]) == Some(var)
                    && function.node_inputs(n).len() == 3
            })
            .expect("2-value VarPhi at the join must exist");
        let phi_token = function.node_inputs(phi)[0];
        let region = function.producer(phi_token);
        assert!(
            matches!(function.node_kind(region), NodeKind::Region),
            "phi token producer must be the join Region"
        );

        assert_eq!(
            function.node_inputs(region).len(),
            2,
            "join region starts with 2 control predecessors"
        );
        let pre_phi_inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
        assert_eq!(pre_phi_inputs.len(), 3, "phi: [token, val0, val1]");
        let pred1_val = pre_phi_inputs[2];

        let mut edit = EditFunction::new(&mut function);
        edit.remove_region_predecessors(region, &[0])
            .expect("remove_region_predecessors must succeed");

        assert_eq!(
            function.node_inputs(region).len(),
            1,
            "region drops to 1 ctrl input"
        );

        let phi_inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
        assert_eq!(phi_inputs.len(), 2, "phi: [token, surviving value]");
        assert_eq!(phi_inputs[1], pred1_val, "surviving slot is pred 1's value");
    }

    use strider_ir::IntUnaryOp;

    /// Killing the sole consumer and draining `clean()` recursively culls
    /// every operand-cone node that thereby loses its last use.
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
        // Return a different live value so the cone above is orphaned once
        // `add` is killed.
        let ret_val = b.build_int_const(99u64, ValueType::I64).unwrap();
        b.build_return(Some(ret_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let neg_node = function.producer(neg);
        let k_node = function.producer(k);
        let k2_node = function.producer(k2);
        let add_node = function.producer(add);

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        edit.kill_node(add_node);
        edit.clean();

        assert!(!edit.is_live(add_node), "add was killed");
        assert!(!edit.is_live(neg_node), "neg orphaned → culled");
        assert!(!edit.is_live(k_node), "k orphaned → culled");
        assert!(!edit.is_live(k2_node), "k2 orphaned → culled");
    }

    /// Killing `add(k, k)` must leave `k` with zero uses and get it culled,
    /// even though the operand is repeated across both input edges.
    #[test]
    fn kill_node_culls_repeated_operand() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        // One `k` fed into BOTH operands, so the add holds the same
        // `ValueId` twice.
        let k = b.build_int_const(5u64, ValueType::I64).unwrap();
        let add = b
            .build_int_binary_operation(k, k, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // Returning the add keeps `k` entry-reachable, so `cull_dead` leaves
        // it alone and the test exercises the manual `kill_node` path.
        b.build_return(Some(add), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let k_node = function.producer(k);
        let add_node = function.producer(add);

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        assert!(edit.is_live(k_node), "k starts live (reachable via add)");

        edit.kill_node(add_node);
        edit.clean();

        assert!(!edit.is_live(add_node), "add was killed");
        assert!(!edit.is_live(k_node), "repeated operand k must be culled");
    }

    /// Dropping the use of one add must NOT cull a shared operand: its other
    /// consumer keeps it live.
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
        // The Return keeps add2, k and other live. add1 shares k/other but
        // feeds nothing reachable.
        b.build_return(Some(add2), &[]).unwrap();
        let _ = add1;
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let k_node = function.producer(k);
        let add1_node = function.producer(add1);
        let add2_node = function.producer(add2);

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        // add1 was unreachable, so the initial cull removed it and detached
        // its operands. `will_detach_value(k)` saw add2 still using k, so k
        // was never enqueued.
        assert!(
            !edit.is_live(add1_node),
            "unreachable add1 culled by initial cull"
        );
        assert!(edit.is_live(add2_node), "add2 stays live (returned)");
        assert!(edit.is_live(k_node), "shared operand k kept live by add2");

        // A further drain changes nothing: k still has add2's use.
        edit.clean();
        assert!(edit.is_live(k_node), "k still live after an extra clean");
    }

    use strider_ir::node::{ValueId, ValueKind};

    /// An input-less node is marked live AND recorded as a root; one with
    /// inputs is live but NOT a root.
    #[test]
    fn create_node_marks_live_and_tracks_root() {
        let mut function = {
            // Terminate the entry region so the built function satisfies the
            // control invariant that every control edge reaches a terminator.
            let mut b = RegisterSet::new().build_fn_single_region().unwrap();
            b.build_return(None, &[]).unwrap();
            b.build().unwrap()
        };
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        use strider_ir::IRBuilderExt;
        let kv = edit.build_int_const(5u64, ValueType::I64).unwrap();
        let k = edit.producer(kv);
        assert!(edit.is_live(k), "fresh const is live");
        assert!(edit.is_root(k), "input-less const is a root");

        let k2v = edit.build_int_const(6u64, ValueType::I64).unwrap();
        let add = edit.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [kv, k2v],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert!(edit.is_live(add), "fresh Add is live");
        assert!(!edit.is_root(add), "Add has inputs → not a root");
    }

    /// `add_node_input` on a previously input-less node drops it from `roots`.
    #[test]
    fn add_node_input_drops_root_when_node_gains_input() {
        let mut function = {
            // Terminate the entry region so the built function satisfies the
            // control invariant that every control edge reaches a terminator.
            let mut b = RegisterSet::new().build_fn_single_region().unwrap();
            b.build_return(None, &[]).unwrap();
            b.build().unwrap()
        };
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let region = edit.create_node(NodeKind::Region, [], [ValueKind::Control]);
        assert!(edit.is_root(region), "input-less Region is a root");

        let entry = edit.entry();
        let entry_ctrl = edit.node_outputs(entry)[0];
        edit.add_node_input(region, entry_ctrl).unwrap();
        assert!(
            !edit.is_root(region),
            "Region with an input is no longer a root"
        );
    }

    /// `replace_value(old, new)` enqueues old's producer; a following `clean()`
    /// culls it once it has lost its last use.
    #[test]
    fn replace_value_enqueues_old_producer_and_clean_culls_it() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        // A non-side-effecting Neg whose value the Return consumes.
        let k = b.build_int_const(5u64, ValueType::I64).unwrap();
        let old = b
            .build_int_unary_operation(k, IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        let new = b.build_int_const(9u64, ValueType::I64).unwrap();
        b.build_return(Some(old), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let old_node = function.producer(old);
        let new_node = function.producer(new);
        let k_node = function.producer(k);

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        assert!(
            !edit.is_live(new_node),
            "new const was unreachable pre-replace"
        );
        // Re-creating it dedups back to the same node, which re-enters the
        // live set.
        let new_v: ValueId = edit.build_int_const(9u64, ValueType::I64).unwrap();
        let new_node = edit.producer(new_v);
        assert!(edit.is_live(new_node), "re-made new const is live");

        let changed = edit.replace_value(old, new_v).unwrap();
        assert!(changed, "the Return's use of old was redirected");
        edit.clean();

        assert!(!edit.is_live(old_node), "old producer enqueued + culled");
        assert!(!edit.is_live(k_node), "old's orphaned operand culled too");
        assert!(edit.is_live(new_node), "new producer stays live");
    }

    /// `live_of_kind` filters the cached live set without re-walking.
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

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        use cranelift_entity::EntityRef;
        let mut consts: Vec<_> = edit
            .live_of_kind(|k| matches!(k, NodeKind::IntConst(_)))
            .collect();
        consts.sort_unstable_by_key(|n| n.index());
        let mut expected = vec![k1_node, k2_node];
        expected.sort_unstable_by_key(|n| n.index());
        assert_eq!(consts, expected, "exactly the two IntConsts");
    }

    use super::rewrite_rule;
    use strider_pattern::{
        Capture, CaptureExt, MatchPat, Matcher, add, any_int_const, int_const_with,
    };

    /// An RHS node built directly on the graph by `instantiate` must still
    /// land in the cached live set.
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
            let m = Matcher::new(&function);
            let pat = add(any_int_const().capture(c1), any_int_const().capture(c2)).into_pattern();
            let hits = m.find_all(&pat).unwrap();
            assert!(!hits.is_empty(), "3 + 4 add must match");
            hits[0].root()
        };

        let rule = rewrite_rule(
            add(any_int_const().capture(c1), any_int_const().capture(c2)),
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

    use std::collections::BTreeSet;
    use strider_pattern::{template, var};

    /// After any edits plus `clean()`, the cached live/roots state must
    /// equal a fresh `GraphWalkInfo::compute_full(entry)` AS SETS: root
    /// ORDER legitimately differs.
    fn assert_live_matches_reachable(edit: &EditFunction) {
        use cranelift_entity::EntityRef;
        let entry = edit.entry();
        let info = strider_ir::walk::GraphWalkInfo::compute_full(edit.function().graph(), entry);

        let fresh_live: BTreeSet<usize> = info.live_nodes.iter().map(|n| n.index()).collect();
        let cached_live: BTreeSet<usize> = edit.live_snapshot().iter().map(|n| n.index()).collect();
        assert_eq!(
            cached_live, fresh_live,
            "cached live_nodes must equal the entry-reachable set"
        );

        let fresh_roots: BTreeSet<usize> = info.roots.into_iter().map(|n| n.index()).collect();
        let cached_roots: BTreeSet<usize> =
            edit.roots_snapshot().iter().map(|n| n.index()).collect();
        assert_eq!(
            cached_roots, fresh_roots,
            "cached roots must equal the input-less reachable set"
        );
    }

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
        let zero = b.build_int_const(0u64, ValueType::I64).unwrap();
        b.set_lift_addr(Some(0xDD));
        let add_val = b
            .build_int_binary_operation(xv, zero, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.set_lift_addr(Some(0xEE));
        b.build_return(Some(add_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let x_node = function.producer(xv);

        use strider_pattern::int_const as int_const_match;
        let root = match_root(&function, add(var(x), int_const_match(0u64)));
        let rule = rewrite_rule(add(var(x), int_const_match(0u64)), var(x));

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
        let k1 = b.build_int_const(1u64, ValueType::I64).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::I64).unwrap();
        let outer = b
            .build_int_binary_operation(inner, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // Kept live by a side-effecting Store so it survives independently
        // of the fold.
        let three = b.build_int_const(3u64, ValueType::I64).unwrap();
        let store_addr = b.build_int_const(0x4000u64, ValueType::I64).unwrap();
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
    #[test]
    fn track_multi_output_template_interior() {
        use super::rewrite_rule_runtime;
        use strider_ir::node::ValueType as VT;
        use strider_pattern::load;
        use strider_pattern::matcher::KindSpec;
        use strider_pattern::template::{TemplateBuilder, TemplateKind};

        // Rewrites `Load(addr)` into `Load(addr, Store(addr, data, mem))`.
        // Forwards nothing; purely a structural template exercise.
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x2000u64, VT::I64).unwrap();
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, VT::I64).unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let load_node = function.producer(loaded);

        // Capture the Load's address so the RHS can re-use it.
        let addr_cap = Capture::new();
        let lhs = load()
            .addr(strider_pattern::any().capture(addr_cap))
            .build();

        // Raw-builder RHS rooted at the Load, its single value output.
        let rhs = {
            let mut tb = TemplateBuilder::new();
            let a = tb.capture(addr_cap);
            let data = tb.leaf(KindSpec::Any);
            tb.set_template_kind(data, TemplateKind::FnIntConst(Box::new(|_| Ok(7u128))));
            tb.set_value_ty(data, VT::I64);
            // The store needs an incoming memory token; this leaf is
            // input-less, so it becomes a root.
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

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, load_node).unwrap();
        assert!(fired.is_some(), "Load → Load(Store) rewrite must fire");
        edit.clean();

        let new_load = edit.producer(fired.unwrap());
        assert!(matches!(edit.node_kind(new_load), NodeKind::Load(_)));
        assert!(edit.is_live(new_load), "fresh Load is live");

        let mem_in = edit.node_inputs(new_load)[1];
        let store_node = edit.producer(mem_in);
        assert!(
            matches!(edit.node_kind(store_node), NodeKind::Store(_)),
            "Load's memory input is the fresh Store"
        );
        assert!(
            edit.is_live(store_node),
            "fresh interior Store is tracked live"
        );

        let init_mem_in = edit.node_inputs(store_node)[2];
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
    }

    /// A direct `replace_value` plus `clean()` must also leave the cached
    /// state equal to the entry-reachable walk.
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

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let new_v = edit.build_int_const(9u64, ValueType::I64).unwrap();
        let new_node = edit.producer(new_v);
        let changed = edit.replace_value(neg, new_v).unwrap();
        assert!(changed);
        edit.clean();

        assert!(!edit.is_live(neg_node), "old Neg culled");
        assert!(!edit.is_live(k_node), "Neg's orphaned operand culled");
        assert!(edit.is_live(new_node), "fresh const live");

        assert_live_matches_reachable(&edit);
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
        let k1 = b.build_int_const(1u64, ValueType::I64).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let k2 = b.build_int_const(2u64, ValueType::I64).unwrap();
        let outer = b
            .build_int_binary_operation(inner, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // Wired into nothing, so it starts unreachable and the initial cull
        // removes it. The fold's RHS const (1 + 2) dedups back onto it.
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
