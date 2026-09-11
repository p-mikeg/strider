use strider_ir::{IRViewer, IntCmpOp};
use strider_pattern::matcher::{KindSpec, MatcherBuilder};
use strider_pattern::*;

use super::support::{Tb, assertions as a, reg_vn, shapes};

use rsleigh::VnSpace;

#[test]
fn call_unconstrained_matches() {
    let function = shapes::call_at(0x1234);
    a::matches(&function, call().build(), 1);
}

#[test]
fn call_at_addr_matches() {
    let function = shapes::call_at(0x1234);
    a::matches(&function, call().at(0x1234).build(), 1);
    a::none(&function, call().at(0x9999).build());
}

#[test]
fn call_target_with_pattern() {
    let function = shapes::call_at(0x1234);
    a::matches(&function, call().target(int_const(0x1234u128)).build(), 1);
}

#[test]
fn call_target_set_matches_when_target_is_in_set() {
    let function = shapes::call_at(0x1234);
    a::matches(
        &function,
        call()
            .target(int_const([0x1000u64, 0x1234, 0x9999]))
            .build(),
        1,
    );
}

#[test]
fn call_target_set_skips_when_target_is_not_in_set() {
    let function = shapes::call_at(0x1234);
    a::none(
        &function,
        call().target(int_const([0x1000u64, 0x9999])).build(),
    );
}

#[test]
fn call_target_set_empty_never_matches() {
    let function = shapes::call_at(0x1234);
    // An empty set is vacuously false. Pinned so empty-set callers never fall
    // through to "match anything".
    a::none(
        &function,
        call().target(int_const(Vec::<u64>::new())).build(),
    );
}

#[test]
fn int_const_set_matches_set_membership() {
    // Exercises the set form itself, independent of CallPat: the call site
    // stores its target as IntConst(0x1234).
    let function = shapes::call_at(0x1234);
    a::matches(
        &function,
        call().target(int_const([0x1234u64, 0xDEADBEEF])).build(),
        1,
    );
    a::none(
        &function,
        call().target(int_const([0x1000u64, 0xDEADBEEF])).build(),
    );
}

#[test]
fn call_captures_node() {
    let function = shapes::call_at(0x1234);
    let n = Capture::new();
    let m = a::unique(&function, call().at(0x1234).capture(n).build());
    let node = m.node(n, function.graph()).expect("node capture");
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
    a::matches(&function, call().arg(0, int_const(42u128)).build(), 1);
    a::none(&function, call().arg(0, int_const(99u128)).build());
    // Out-of-range arg index: the indexed input doesn't exist, so reject.
    a::none(&function, call().arg(99, anything()).build());
}

/// Two argument registers pre-loaded with 11 and 22.
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
        call()
            .arg(0, int_const(11u128))
            .arg(1, int_const(22u128))
            .build(),
        1,
    );
    // Right arg 0, wrong arg 1.
    a::none(
        &function,
        call()
            .arg(0, int_const(11u128))
            .arg(1, int_const(0u128))
            .build(),
    );
}

#[test]
fn with_root_post_match_filters_control_pattern() {
    // A root post-match guard on a finished control `Pattern` runs on the root
    // node and can reject the match: the finished-pattern analogue of
    // `.when_match` on a value builder.
    let function = shapes::call_at(0x1234);

    a::matches(&function, call().build(), 1);

    let rejecting = call()
        .build()
        .with_root_post_match(Box::new(|_m, _node, _ty, _b| false));
    a::none(&function, rejecting);

    let accepting = call()
        .build()
        .with_root_post_match(Box::new(|_m, _node, _ty, _b| true));
    a::matches(&function, accepting, 1);
}

#[test]
fn with_root_post_match_composes_with_an_existing_guard() {
    // A second guard narrows the first rather than replacing it.
    let function = shapes::call_at(0x1234);
    let both = call()
        .build()
        .with_root_post_match(Box::new(|_m, _node, _ty, _b| false))
        .with_root_post_match(Box::new(|_m, _node, _ty, _b| true));
    a::none(&function, both);
}

#[test]
fn with_root_post_match_sees_root_node() {
    let function = shapes::call_at(0x1234);
    let guarded = call()
        .build()
        .with_root_post_match(Box::new(|m, node, _ty, _b| {
            matches!(
                m.function().node_kind(node),
                strider_ir::node::NodeKind::Call
            )
        }));
    a::matches(&function, guarded, 1);
}

#[test]
fn ret_unconstrained_matches() {
    let function = shapes::add_consts(5, 3);
    a::matches(&function, ret().build(), 1);
}

#[test]
fn ret_val_matches_returned_value() {
    let function = shapes::add_consts(5, 3);
    a::matches(
        &function,
        ret()
            .ret_val(0, int_add(int_const(5u128), int_const(3u128)))
            .build(),
        1,
    );
    a::none(&function, ret().ret_val(0, int_const(0u128)).build());
}

#[test]
fn ret_without_value_rejects_ret_val_constraint() {
    let function = shapes::call_at(0x1234); // Return with no value.
    a::matches(&function, ret().build(), 1);
    a::none(&function, ret().ret_val(0, anything()).build());
}

#[test]
fn ret_ctrl_call() {
    let function = shapes::call_at(0x1234);
    // The Ret's ctrl predecessor is the join Region, not the Call, so
    // `ctrl(call())` would not match. `anything()` is a smoke test that
    // `.ctrl` doesn't error.
    a::matches(&function, ret().ctrl(anything()).build(), 1);
}

#[test]
fn ret_captures_node() {
    let function = shapes::add_consts(5, 3);
    let n = Capture::new();
    let m = a::unique(&function, ret().capture(n).build());
    let node = m.node(n, function.graph()).expect("ret node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::Return
    ));
}

#[test]
fn if_else_unconstrained_matches() {
    let function = shapes::if_cmp_then_return(4);
    a::matches(&function, if_else().build(), 1);
}

#[test]
fn if_else_cond_matches() {
    let function = shapes::if_cmp_then_return(4);
    a::matches(
        &function,
        if_else()
            .cond(int_eq(int_const(4u128), int_const(1u128)))
            .build(),
        1,
    );
    a::none(
        &function,
        if_else()
            .cond(int_eq(int_const(99u128), int_const(1u128)))
            .build(),
    );
}

#[test]
fn if_else_true_and_false_branches() {
    let function = shapes::if_cmp_then_return(4);
    // The branch consumer is the join Region; `anything()` matches it.
    a::matches(
        &function,
        if_else()
            .with_true(anything().into_pattern())
            .with_false(anything().into_pattern())
            .build(),
        1,
    );
}

#[test]
fn if_else_captures() {
    let function = shapes::if_cmp_then_return(4);
    let n = Capture::new();
    let m = a::unique(&function, if_else().capture(n).build());
    let node = m.node(n, function.graph()).expect("if node capture");
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
    a::matches(&function, call().at(0x9999).build(), 1);
    a::none(&function, call().at(0xDEAD).build());
}

#[test]
fn if_branch_slot_accepts_built_control_pattern() {
    // `with_true` / `with_false` accept a finished control `Pattern`, routed
    // through the node-wise branch-consumer walk. In unoptimised IR that
    // consumer is the join `Region`, so a `call()` pattern does not match it
    // while `anything()` does.
    let function = graph_if_with_call_in_false_branch();
    let m = Matcher::new(&function);

    assert_eq!(
        m.find_all(&if_else().with_false(anything().into_pattern()).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        m.find_all(&if_else().with_false(call().at(0x9999).build()).build())
            .unwrap()
            .len(),
        0
    );
}

/// A malformed (multi-sink / rootless) branch pattern must be rejected loudly
/// at `with_true` build time, so a typo surfaces instead of reading as "branch
/// did not match".
#[test]
#[should_panic(expected = "If branch pattern is not matchable")]
fn with_true_multi_sink_branch_pattern_panics_not_silently_skips() {
    // Two unconsumed leaf sinks make `root()` error.
    let mut mb = MatcherBuilder::new();
    let _a = mb.leaf(KindSpec::Any);
    let _b = mb.leaf(KindSpec::Any);
    let bad = mb.finish();
    let _ = if_else().with_true(bad).build();
}

/// A capture bound inside an If branch sub-pattern binds in the enclosing
/// match. See `if_branch_captures` for the agreement cases.
#[test]
fn with_true_branch_capture_reaches_the_outer_match() {
    let function = shapes::if_cmp_then_return(4);
    let branch_cap = Capture::new();
    let pat = if_else()
        .with_true(anything().capture(branch_cap).into_pattern())
        .build();
    let m = a::unique(&function, pat);
    assert!(matches!(
        function.node_kind(m.root()),
        strider_ir::node::NodeKind::If
    ));
    assert!(
        m.node(branch_cap, function.graph()).is_some(),
        "branch capture must reach the outer match"
    );
}

/// `return(call_other(user_op_id, [IntConst(7)]))`, reused across CallOther
/// tests.
fn graph_call_other(user_op_id: u64) -> strider_ir::Function {
    // The builder writes the result back via `write_reg_vn`, so the CallOther
    // output vn must be a tracked register.
    let out_vn = rsleigh::Vn {
        size: 8,
        addr_off: 0x100,
        addr_space: VnSpace::REGISTER,
    };
    let mut t = Tb::with_vars(&[out_vn]);
    let arg = t.u64(7);
    t.call_other("cpuid", user_op_id, &[arg], Some(out_vn), &[], &[]);
    t.ret_nothing()
}

#[test]
fn call_other_matches_any_user_op() {
    let function = graph_call_other(42);
    a::matches(&function, call_other().build(), 1);
}

#[test]
fn call_other_user_op_id_filter() {
    let function = graph_call_other(42);
    a::matches(&function, call_other().user_op_id(42).build(), 1);
    a::none(&function, call_other().user_op_id(99).build());
}

#[test]
fn call_other_captures_node() {
    let function = graph_call_other(5);
    let n = Capture::new();
    let m = a::unique(&function, call_other().capture(n).build());
    let node = m.node(n, function.graph()).expect("node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::CallOther { .. }
    ));
}
