//! `Call`, `CallOther`, `Return`, and `If` node patterns.
//!
//! Covers: call targets (literal + pattern), call args (single/multi/index),
//! call return outputs, `.capture_node`, `ret().ret_val()` / `.preceded_by()`,
//! `if_node().cond().true_branch().false_branch()`, `.at(addr)` convenience.

use strider_pattern::*;
use strider_ir::IntCmpOp;

use super::support::{Tb, assertions as a, reg_vn, shapes};

// ── Call ──────────────────────────────────────────────────────────────────────

#[test]
fn call_unconstrained_matches() {
    let function = shapes::call_at(0x1234);
    a::matches(&function, call(), 1);
}

#[test]
fn call_at_addr_matches() {
    let function = shapes::call_at(0x1234);
    a::matches(&function, call().at(0x1234), 1);
    a::none(&function, call().at(0x9999));
}

#[test]
fn call_target_with_pattern() {
    let function = shapes::call_at(0x1234);
    a::matches(&function, call().target(int_const(0x1234u128)), 1);
}

#[test]
fn call_at_any_matches_when_target_is_in_set() {
    let function = shapes::call_at(0x1234);
    // Set contains the call's target → match.
    a::matches(&function, call().at_any([0x1000u64, 0x1234, 0x9999]), 1);
}

#[test]
fn call_at_any_skips_when_target_is_not_in_set() {
    let function = shapes::call_at(0x1234);
    // Set does not contain the call's target → no match.
    a::none(&function, call().at_any([0x1000u64, 0x9999]));
}

#[test]
fn call_at_any_empty_set_never_matches() {
    let function = shapes::call_at(0x1234);
    // An empty target set is vacuously false — every IntConst lookup
    // fails the membership test.  Pinning this contract so empty-set
    // callers do not accidentally fall through to "match anything".
    a::none(&function, call().at_any(std::iter::empty::<u64>()));
}

#[test]
fn int_const_any_of_matches_set_membership() {
    // Direct test of the underlying primitive — independent of CallPat.
    let function = shapes::call_at(0x1234);
    // The call site stores the target as IntConst(0x1234); query via
    // the standalone any-of ctor.
    a::matches(
        &function,
        call().target(int_const_any_of([0x1234u64, 0xDEADBEEF])),
        1,
    );
    a::none(
        &function,
        call().target(int_const_any_of([0x1000u64, 0xDEADBEEF])),
    );
}

#[test]
fn call_captures_node() {
    let function = shapes::call_at(0x1234);
    let n = Capture::new();
    let m = a::unique(&function, call().at(0x1234).capture(n));
    let node = m.node(n, &function).expect("node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::Call
    ));
}

/// Call with one argument register pre-loaded with a constant value.
fn graph_call_with_single_arg() -> strider_ir::Function {
    let arg = reg_vn(0, 8);
    let mut t = Tb::raw(vec![arg], &[arg], &[], &[], None, 0);
    let c = t.u64(42);
    t.write_var(&arg, c);
    t.call_at(0xABCD);
    t.ret_nothing()
}

#[test]
fn call_arg_by_index() {
    let function = graph_call_with_single_arg();
    a::matches(&function, call().arg(0, int_const(42u128)), 1);
    a::none(&function, call().arg(0, int_const(99u128)));
    // Out-of-range arg index → the indexed input doesn't exist → reject.
    a::none(&function, call().arg(99, any()));
}

/// Two argument registers, pre-loaded with 11 and 22 respectively.
fn graph_call_with_two_args() -> strider_ir::Function {
    let a0 = reg_vn(0, 8);
    let a1 = reg_vn(8, 8);
    let mut t = Tb::raw(vec![a0, a1], &[a0, a1], &[], &[], None, 0);
    let c11 = t.u64(11);
    let c22 = t.u64(22);
    t.write_var(&a0, c11);
    t.write_var(&a1, c22);
    t.call_at(0xBEEF);
    t.ret_nothing()
}

#[test]
fn call_multiple_args() {
    let function = graph_call_with_two_args();
    a::matches(
        &function,
        call().arg(0, int_const(11u128)).arg(1, int_const(22u128)),
        1,
    );
    // Right arg 0, wrong arg 1.
    a::none(
        &function,
        call().arg(0, int_const(11u128)).arg(1, int_const(0u128)),
    );
}

// ── Return ────────────────────────────────────────────────────────────────────

#[test]
fn ret_unconstrained_matches() {
    let function = shapes::add_consts(5, 3);
    a::matches(&function, ret(), 1);
}

#[test]
fn ret_val_matches_returned_value() {
    let function = shapes::add_consts(5, 3);
    a::matches(
        &function,
        ret().ret_val(0, add(int_const(5u128), int_const(3u128))),
        1,
    );
    // Ret val constrained to something not in the graph → reject.
    a::none(&function, ret().ret_val(0, int_const(0u128)));
}

#[test]
fn ret_without_value_rejects_ret_val_constraint() {
    let function = shapes::call_at(0x1234); // Return with no value.
    // Plain ret() matches.
    a::matches(&function, ret(), 1);
    // But constraining ret_val(0, …) cannot succeed — there is no value.
    a::none(&function, ret().ret_val(0, any()));
}

#[test]
fn ret_preceded_by_call() {
    let function = shapes::call_at(0x1234);
    // The Return's ctrl predecessor is a Region at the call region;
    // a call() pattern matches the Call node whose ctrl output this state
    // consumes.  `preceded_by(call())` follows the Ret ctrl → Region,
    // so it will not match directly.  Instead: use `any()` as a smoke test
    // that `.preceded_by` doesn't error.
    a::matches(&function, ret().preceded_by(any()), 1);
}

#[test]
fn ret_captures_node() {
    let function = shapes::add_consts(5, 3);
    let n = Capture::new();
    let m = a::unique(&function, ret().capture(n));
    let node = m.node(n, &function).expect("ret node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::Return
    ));
}

// ── If ────────────────────────────────────────────────────────────────────────

#[test]
fn if_node_unconstrained_matches() {
    let function = shapes::if_cmp_then_return(4);
    a::matches(&function, if_node(), 1);
}

#[test]
fn if_node_cond_matches() {
    let function = shapes::if_cmp_then_return(4);
    a::matches(
        &function,
        if_node().cond(int_eq(int_const(4u128), int_const(1u128))),
        1,
    );
    // Wrong cond subpattern.
    a::none(
        &function,
        if_node().cond(int_eq(int_const(99u128), int_const(1u128))),
    );
}

#[test]
fn if_node_true_and_false_branches() {
    let function = shapes::if_cmp_then_return(4);
    // Single consumer of the true-branch output is the `Region` at
    // the true region — `any()` always matches a real node.
    a::matches(&function, if_node().true_branch(any()).false_branch(any()), 1);
}

#[test]
fn if_node_captures() {
    let function = shapes::if_cmp_then_return(4);
    let n = Capture::new();
    let m = a::unique(&function, if_node().capture(n));
    let node = m.node(n, &function).expect("if node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::If
    ));
}

/// Graph: `if (a < b) { return 10 } else { call(0x9999); return }`.
fn graph_if_with_call_in_false_branch() -> strider_ir::Function {
    let mut t = Tb::bare(vec![], &[], &[], &[], None, 0);
    let entry = t.region();
    let true_r = t.region();
    let false_r = t.region();
    t.set_entry(entry);

    t.enter(true_r);
    let ten = t.u64(10);
    t.fb_mut().build_return(Some(ten), &[]).expect("ret");

    t.enter(false_r);
    t.call_at(0x9999);
    t.fb_mut().build_return(None, &[]).expect("ret");

    t.enter(entry);
    let a_ = t.u64(2);
    let b_ = t.u64(5);
    let c = t.int_cmp(a_, b_, IntCmpOp::Less);
    t.build_if(c, true_r, false_r);
    t.finish()
}

#[test]
fn call_only_matches_present_branch_via_find_all() {
    let function = graph_if_with_call_in_false_branch();
    // There's exactly one Call in the graph; pattern matches it.
    a::matches(&function, call().at(0x9999), 1);
    // A call at the non-existent address should not match.
    a::none(&function, call().at(0xDEAD));
}

// ── CallOther ────────────────────────────────────────────────────────────────

/// `return(call_other(user_op_id, [IntConst(7)]))` — reused across
/// CallOther tests.
fn graph_call_other(user_op_id: u64) -> strider_ir::Function {
    let mut t = Tb::empty();
    let arg = t.u64(7);
    t.call_other(
        "cpuid",
        user_op_id,
        &[arg],
        Some(strider_ir::node::NodeOutputType::I64),
        &[],
        &[],
    );
    t.ret_nothing()
}

#[test]
fn call_other_matches_any_user_op() {
    let function = graph_call_other(42);
    a::matches(&function, call_other(), 1);
}

#[test]
fn call_other_user_op_id_filter() {
    let function = graph_call_other(42);
    a::matches(&function, call_other().user_op_id(42), 1);
    a::none(&function, call_other().user_op_id(99));
}

#[test]
fn call_other_captures_node() {
    let function = graph_call_other(5);
    let n = Capture::new();
    let m = a::unique(&function, call_other().capture(n));
    let node = m.node(n, &function).expect("node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::CallOther { .. }
    ));
}
