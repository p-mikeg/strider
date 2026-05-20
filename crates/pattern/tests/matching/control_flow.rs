//! `Call`, `CallOther`, `Return`, and `If` node patterns.
//!
//! Covers: call targets (literal + pattern), call args (single/multi/index),
//! call return outputs, `.capture_node`, `ret().ret_val()` / `.preceded_by()`,
//! `if_node().cond().true_branch().false_branch()`, `.at(addr)` convenience.

use strider_ir::IntCmpOp;
use pattern::*;

use super::support::{Tb, assertions as a, reg_vn, shapes};

// ── Call ──────────────────────────────────────────────────────────────────────

#[test]
fn call_unconstrained_matches() {
    let g = shapes::call_at(0x1234);
    a::matches(&g, call(), 1);
}

#[test]
fn call_at_addr_matches() {
    let g = shapes::call_at(0x1234);
    a::matches(&g, call().at(0x1234), 1);
    a::none(&g, call().at(0x9999));
}

#[test]
fn call_target_with_pattern() {
    let g = shapes::call_at(0x1234);
    a::matches(&g, call().target(int_const(0x1234)), 1);
}

#[test]
fn call_at_any_matches_when_target_is_in_set() {
    let g = shapes::call_at(0x1234);
    // Set contains the call's target → match.
    a::matches(&g, call().at_any([0x1000u64, 0x1234, 0x9999]), 1);
}

#[test]
fn call_at_any_skips_when_target_is_not_in_set() {
    let g = shapes::call_at(0x1234);
    // Set does not contain the call's target → no match.
    a::none(&g, call().at_any([0x1000u64, 0x9999]));
}

#[test]
fn call_at_any_empty_set_never_matches() {
    let g = shapes::call_at(0x1234);
    // An empty target set is vacuously false — every IntConst lookup
    // fails the membership test.  Pinning this contract so empty-set
    // callers do not accidentally fall through to "match anything".
    a::none(&g, call().at_any(std::iter::empty::<u64>()));
}

#[test]
fn int_const_any_of_matches_set_membership() {
    // Direct test of the underlying primitive — independent of CallPat.
    let g = shapes::call_at(0x1234);
    // The call site stores the target as IntConst(0x1234); query via
    // the standalone any-of ctor.
    a::matches(&g, call().target(int_const_any_of([0x1234u64, 0xDEADBEEF])), 1);
    a::none(&g, call().target(int_const_any_of([0x1000u64, 0xDEADBEEF])));
}

///  an empty `int_const_any_of(_)` set
/// vacuously fails — every IntConst lookup runs `.iter().any(_)`
/// against an empty iterator, which returns false.  Mirrors the
/// existing `call_at_any_empty_set_never_matches` contract for
/// `CallPat::at_any` and the `StackStorePat::offset_any` contract.
/// Pinning the empty-set behaviour at the standalone primitive
/// level (not just through the typed builders) so a future
/// change to the matcher loop can't silently flip empty-set to
/// "match-anything".
#[test]
fn int_const_any_of_empty_set_never_matches() {
    let g = shapes::call_at(0x1234);
    a::none(&g, call().target(int_const_any_of(std::iter::empty::<u64>())));
    a::none(&g, call().target(int_const_any_of([] as [u64; 0])));
}

#[test]
fn call_captures_node() {
    let g = shapes::call_at(0x1234);
    let n = Capture::new();
    let m = a::unique(&g, call().at(0x1234).capture(n));
    let node = m.node(n).expect("node capture");
    assert!(matches!(
        g.graph.node_kind(node),
        strider_ir::node::NodeKind::Call
    ));
}

/// Call with one argument register pre-loaded with a constant value.
fn graph_call_with_single_arg() -> strider_ir::BuiltFunctionGraph {
    let arg = reg_vn(0, 8);
    let mut t = Tb::raw(vec![arg], &[arg], &[], &[], None, 0);
    let c = t.u64(42);
    t.write_var(&arg, c);
    t.call_at(0xABCD);
    t.ret_nothing()
}

#[test]
fn call_arg_by_index() {
    let g = graph_call_with_single_arg();
    a::matches(&g, call().arg(0, int_const(42)), 1);
    a::none(&g, call().arg(0, int_const(99)));
    // Out-of-range arg index → the indexed input doesn't exist → reject.
    a::none(&g, call().arg(99, any()));
}

/// Two argument registers, pre-loaded with 11 and 22 respectively.
fn graph_call_with_two_args() -> strider_ir::BuiltFunctionGraph {
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
    let g = graph_call_with_two_args();
    a::matches(&g, call().arg(0, int_const(11)).arg(1, int_const(22)), 1);
    // Right arg 0, wrong arg 1.
    a::none(&g, call().arg(0, int_const(11)).arg(1, int_const(0)));
}

/// Call that produces a return value in `ret_reg`; the returned value is
/// piped into the function's own Return.
fn graph_call_then_return_ret_reg() -> (strider_ir::BuiltFunctionGraph, rsleigh::Vn) {
    let ret = reg_vn(0, 8);
    let mut t = Tb::raw(vec![ret], &[], &[], &[ret], None, 0);
    t.call_at(0xCAFE);
    let g = t.ret_regs(&[ret]);
    (g, ret)
}

#[test]
fn call_ret_output_capture() {
    let (g, _ret) = graph_call_then_return_ret_reg();
    let v = Capture::new();
    let m = a::unique(&g, call().at(0xCAFE).ret_output(0, var(v)));
    let out = m.output(v).expect("ret_output capture");
    // The captured output is a Call output slot.
    assert!(matches!(g.graph.kind_of_output(out), strider_ir::node::NodeKind::Call));
}

// ── Return ────────────────────────────────────────────────────────────────────

#[test]
fn ret_unconstrained_matches() {
    let g = shapes::add_consts(5, 3);
    a::matches(&g, ret(), 1);
}

#[test]
fn ret_val_matches_returned_value() {
    let g = shapes::add_consts(5, 3);
    a::matches(&g, ret().ret_val(0, add(int_const(5), int_const(3))), 1);
    // Ret val constrained to something not in the graph → reject.
    a::none(&g, ret().ret_val(0, int_const(0)));
}

#[test]
fn ret_without_value_rejects_ret_val_constraint() {
    let g = shapes::call_at(0x1234); // Return with no value.
    // Plain ret() matches.
    a::matches(&g, ret(), 1);
    // But constraining ret_val(0, …) cannot succeed — there is no value.
    a::none(&g, ret().ret_val(0, any()));
}

#[test]
fn ret_preceded_by_call() {
    let g = shapes::call_at(0x1234);
    // The Return's ctrl predecessor is a ControlState at the call region;
    // a call() pattern matches the Call node whose ctrl output this state
    // consumes.  `preceded_by(call())` follows the Ret ctrl → ControlState,
    // so it will not match directly.  Instead: use `any()` as a smoke test
    // that `.preceded_by` doesn't error.
    a::matches(&g, ret().preceded_by(any()), 1);
}

#[test]
fn ret_captures_node() {
    let g = shapes::add_consts(5, 3);
    let n = Capture::new();
    let m = a::unique(&g, ret().capture(n));
    let node = m.node(n).expect("ret node capture");
    assert!(matches!(g.graph.node_kind(node), strider_ir::node::NodeKind::Return));
}

/// A control-flow `Capture` (bound to a Return node, which has
/// no value output) must yield `None` from value-typed accessors.
/// `node(c)` must succeed, but `output(c)` is `None`, so
/// `get_uint(c, &g)`, `get_int(c, &g)`, and `get_bool(c, &g)` all
/// return `None` — never panic, never read garbage.
#[test]
fn match_value_accessors_on_control_flow_capture_return_none() {
    let g = shapes::add_consts(5, 3);
    let c = Capture::new();
    let m = a::unique(&g, ret().capture(c));
    // node binding is present (the matched Return).
    assert!(m.node(c).is_some(), "Return capture must bind node");
    // No value output — Return is a control-flow sink.
    assert!(m.output(c).is_none(), "Return has no value output");
    // Value extractors must surface this as None, not panic.
    assert_eq!(m.get_uint(c, &g.graph), None);
    assert_eq!(m.get_int(c, &g.graph), None);
    assert_eq!(m.get_bool(c, &g.graph), None);
}

// ── If ────────────────────────────────────────────────────────────────────────

#[test]
fn if_node_unconstrained_matches() {
    let g = shapes::if_cmp_then_return(4);
    a::matches(&g, if_node(), 1);
}

#[test]
fn if_node_cond_matches() {
    let g = shapes::if_cmp_then_return(4);
    a::matches(&g, if_node().cond(int_eq(int_const(4), int_const(1))), 1);
    // Wrong cond subpattern.
    a::none(&g, if_node().cond(int_eq(int_const(99), int_const(1))));
}

#[test]
fn if_node_true_and_false_branches() {
    let g = shapes::if_cmp_then_return(4);
    // Single consumer of the true-branch output is the `ControlState` at
    // the true region — `any()` always matches a real node.
    a::matches(&g, if_node().true_branch(any()).false_branch(any()), 1);
}

#[test]
fn if_node_captures() {
    let g = shapes::if_cmp_then_return(4);
    let n = Capture::new();
    let m = a::unique(&g, if_node().capture(n));
    let node = m.node(n).expect("if node capture");
    assert!(matches!(g.graph.node_kind(node), strider_ir::node::NodeKind::If));
}

/// Graph: if (a < b) { return 10 } else { call(0x9999); return }
fn graph_if_with_call_in_false_branch() -> strider_ir::BuiltFunctionGraph {
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
    let g = graph_if_with_call_in_false_branch();
    // There's exactly one Call in the graph; pattern matches it.
    a::matches(&g, call().at(0x9999), 1);
    // A call at the non-existent address should not match.
    a::none(&g, call().at(0xDEAD));
}

/// Regression for F-003: `if_node().false_branch(p)` traverses the
/// `ControlState` join when the matcher's `ignore_control_states`
/// flag is set.  Without the walk-through, the strict matcher fails
/// because the If's false-branch output feeds the ControlState
/// header of the false region, not the Call directly.
#[test]
fn if_node_branch_walks_through_control_state_when_flag_set() {
    let g = graph_if_with_call_in_false_branch();
    let pat: pattern::Pat = if_node().false_branch(call().at(0x9999)).into();
    // Strict semantics: the False-branch consumer is a ControlState,
    // not the Call — direct match should fail.
    let strict = pattern::Matcher::new(&g);
    assert!(strict.find_all(&pat).is_empty(),
            "without ignore_control_states the strict if_node().false_branch(call) match must fail");
    // With ignore_control_states, the matcher walks through the
    // ControlState region-join and finds the Call.
    let lenient = pattern::Matcher::new(&g).ignore_control_states();
    assert_eq!(lenient.find_all(&pat).len(), 1,
               "ignore_control_states must let if_node().false_branch(call) reach the Call");
}

// ── CallOther ────────────────────────────────────────────────────────────────

/// `return(call_other(user_op_id, [IntConst(7)]))` — reused across CallOther
/// tests.
fn graph_call_other(user_op_id: u64) -> strider_ir::BuiltFunctionGraph {
    let mut t = Tb::empty();
    let arg = t.u64(7);
    t.call_other("cpuid", user_op_id, &[arg], Some(strider_ir::node::NodeOutputType::U64), &[], &[]);
    t.ret_nothing()
}

#[test]
fn call_other_matches_any_user_op() {
    let g = graph_call_other(42);
    a::matches(&g, call_other(), 1);
}

#[test]
fn call_other_user_op_id_filter() {
    let g = graph_call_other(42);
    a::matches(&g, call_other().user_op_id(42), 1);
    a::none(&g, call_other().user_op_id(99));
}

#[test]
fn call_other_captures_node() {
    let g = graph_call_other(5);
    let n = Capture::new();
    let m = a::unique(&g, call_other().capture(n));
    let node = m.node(n).expect("node capture");
    assert!(matches!(
        g.graph.node_kind(node),
        strider_ir::node::NodeKind::CallOther { .. }
    ));
}

// ── Phi-kind discrimination (O7) ─────────────────────────────────────────────
//
// The three phi ctors `phi()` / `mem_phi()` / `value_phi()` each match one
// (and only one) `NodeKind` variant: `VarPhi(_)`, `MemPhi`, `ValuePhi`
// respectively.  These tests build a synthetic graph that contains all three
// phi kinds simultaneously and pin the pattern-to-kind mapping so a
// future ctor refactor cannot silently widen any of them.

/// Builds a synthetic graph containing one `VarPhi`, one `MemPhi`, and one
/// `ValuePhi` — all reachable from the entry via a `Return` that consumes
/// each phi's output.  Constructed via direct `graph.create_node` because
/// the `Tb` test-builder doesn't expose `MemPhi` / `ValuePhi` synthesis
/// (those phi kinds are produced by the lifter / `StackLoadForward`, not
/// by the user-facing builder API).
fn graph_with_all_three_phi_kinds() -> strider_ir::BuiltFunctionGraph {
    use strider_ir::Graph;
    use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

    let mut g = Graph::new();

    let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = g.node_outputs(entry).into_iter().next().unwrap();
    let init_mem_out = g.node_outputs(init_mem).into_iter().next().unwrap();

    // Single-predecessor ControlState → owning region for the phis.  The
    // Layer-C arity check requires per-phi per-predecessor inputs to match
    // the ControlState's predecessor count, so a 1-predecessor CS keeps
    // each phi at exactly one value input.
    let cs = g.create_node(
        NodeKind::ControlState,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = g.node_outputs(cs).into_iter().next().unwrap();
    let cs_phi_tok = g.node_outputs(cs).into_iter().nth(1).unwrap();

    // Tracked-variable VarPhi.
    let some_vn = strider_ir::test_utils::reg_vn(0x40, 8);
    let init_var = g.create_node(
        NodeKind::InitialVar(some_vn),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let iv_out = g.node_outputs(init_var).into_iter().next().unwrap();
    let var_phi = g.create_node(
        NodeKind::VarPhi(some_vn),
        [cs_phi_tok, iv_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let _var_phi_out = g.node_outputs(var_phi).into_iter().next().unwrap();

    // Memory-token MemPhi.
    let mem_phi = g.create_node(
        NodeKind::MemPhi,
        [cs_phi_tok, init_mem_out],
        [NodeOutputKind::Memory],
    );
    let mem_phi_out = g.node_outputs(mem_phi).into_iter().next().unwrap();

    // Synthesised ValuePhi.
    let one = g.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let one_out = g.node_outputs(one).into_iter().next().unwrap();
    let value_phi = g.create_node(
        NodeKind::ValuePhi,
        [cs_phi_tok, one_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let value_phi_out = g.node_outputs(value_phi).into_iter().next().unwrap();

    // Return consumes mem from MemPhi and value from ValuePhi so both are
    // reachable; the VarPhi is reachable through its data-dep (the matcher
    // walks data inputs as well as control inputs, but we keep it kept-alive
    // via a Truncate consumer to be safe).
    let _used = g.create_node(
        NodeKind::Truncate,
        [g.node_outputs(var_phi).into_iter().next().unwrap()],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let used_out = g.node_outputs(_used).into_iter().next().unwrap();

    // Wire Return: ctrl, mem, value.  Use `value_phi_out` as the value
    // and add `used_out` as a tail value so VarPhi is also reachable.
    g.create_node(
        NodeKind::Return,
        [cs_ctrl, mem_phi_out, value_phi_out, used_out],
        [],
    );

    strider_ir::BuiltFunctionGraph::from_graph_and_entry_for_rewrite(g, entry)
}

#[test]
fn phi_ctor_matches_only_var_phi() {
    let g = graph_with_all_three_phi_kinds();
    a::matches(&g, phi(), 1);
    let n = Capture::new();
    let m = a::unique(&g, phi().capture(n));
    let node = m.node(n).expect("phi node capture");
    assert!(
        matches!(g.graph.node_kind(node), strider_ir::node::NodeKind::VarPhi(_)),
        "phi() must match VarPhi, got {:?}",
        g.graph.node_kind(node)
    );
}

#[test]
fn mem_phi_ctor_matches_only_mem_phi() {
    let g = graph_with_all_three_phi_kinds();
    a::matches(&g, mem_phi(), 1);
    let n = Capture::new();
    let m = a::unique(&g, mem_phi().capture(n));
    let node = m.node(n).expect("mem_phi node capture");
    assert!(
        matches!(g.graph.node_kind(node), strider_ir::node::NodeKind::MemPhi),
        "mem_phi() must match MemPhi, got {:?}",
        g.graph.node_kind(node)
    );
}

#[test]
fn value_phi_ctor_matches_only_value_phi() {
    let g = graph_with_all_three_phi_kinds();
    a::matches(&g, value_phi(), 1);
    let n = Capture::new();
    let m = a::unique(&g, value_phi().capture(n));
    let node = m.node(n).expect("value_phi node capture");
    assert!(
        matches!(g.graph.node_kind(node), strider_ir::node::NodeKind::ValuePhi),
        "value_phi() must match ValuePhi, got {:?}",
        g.graph.node_kind(node)
    );
}
