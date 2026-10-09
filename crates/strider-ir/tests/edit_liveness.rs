//! `EditFunction`'s cached live set, roots and fingerprint absorption.
//!
//! Every fixture builds a BUILT `Function` (entry set). An integration test
//! because `strider_ir_test_utils` links its own compilation of `strider-ir`,
//! so its `FunctionBuilder` is a different type from the crate's own unit
//! tests'.

use std::collections::BTreeSet;

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{EditFunction, FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp};
use strider_ir_test_utils::{RegisterSet, reg_vn};

/// The cached live set and roots against a fresh walk of the same graph.
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
    let cached_roots: BTreeSet<usize> = edit.roots_snapshot().iter().map(|n| n.index()).collect();
    assert_eq!(
        cached_roots, fresh_roots,
        "cached roots must equal the input-less reachable set"
    );
}

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
    assert!(changed, "a live use existed -> changed");

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
    assert!(!changed, "no uses of old -> changed must be false");

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
    assert!(!edit.is_live(neg_node), "neg orphaned -> culled");
    assert!(!edit.is_live(k_node), "k orphaned -> culled");
    assert!(!edit.is_live(k2_node), "k2 orphaned -> culled");
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
    assert!(!edit.is_root(add), "Add has inputs -> not a root");
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
    assert_live_matches_reachable(&edit);
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
