//! Control / variadic builder matching: `call` / `call_other` / `ret`
//! / `if_node` / `phi` / `mem_phi` / `value_phi` / `function_arg`.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{
    Capture, MatchPat, Matcher, any, call, call_other, if_node, int_const, load, mem_phi, phi, ret,
    var, value_phi,
};

// ── Call ──────────────────────────────────────────────────────────────────────

/// `call(addr)` then `return` in a fresh post-call region.
fn call_at(addr: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let tgt = b.build_int_const(addr, NodeOutputType::I64).unwrap();
    b.build_call(tgt).unwrap();
    // `build_call` advances the current region's control to the call's
    // output, leaving the same region active — return in place.
    b.build_return(None, &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn call_unconstrained_matches() {
    let function = call_at(0x1234);
    assert_eq!(Matcher::try_new(&function).unwrap().find_all(&call().build()).len(), 1);
}

#[test]
fn call_at_addr_matches_and_rejects() {
    let function = call_at(0x1234);
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(matcher.find_all(&call().at(0x1234).build()).len(), 1);
    assert_eq!(matcher.find_all(&call().at(0x9999).build()).len(), 0);
}

#[test]
fn call_at_any() {
    let function = call_at(0x1234);
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(
        matcher.find_all(&call().at_any([0x1000u64, 0x1234, 0x9999]).build()).len(),
        1
    );
    assert_eq!(matcher.find_all(&call().at_any([0x1000u64, 0x9999]).build()).len(), 0);
    // Empty set is vacuously false.
    assert_eq!(
        matcher.find_all(&call().at_any(std::iter::empty::<u64>()).build()).len(),
        0
    );
}

#[test]
fn call_target_pattern_captures() {
    let function = call_at(0x1234);
    let c = Capture::new();
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&call().target(var(c)).build());
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn call_captures_node() {
    let function = call_at(0x1234);
    let n = Capture::new();
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&call().at(0x1234).capture(n).build());
    assert_eq!(hits.len(), 1);
    let node = hits[0].node(n, &function).expect("node capture");
    assert!(matches!(function.node_kind(node), strider_ir::node::NodeKind::Call));
}

#[test]
fn call_arg_by_index() {
    let arg = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(arg)
        .arg(arg)
        .build_fn_single_region()
        .unwrap();
    let c = b.build_int_const(42u64, NodeOutputType::I64).unwrap();
    b.write_variable(&arg, c).unwrap();
    let tgt = b.build_int_const(0xABCDu64, NodeOutputType::I64).unwrap();
    b.build_call(tgt).unwrap();
    b.build_return(None, &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    assert_eq!(matcher.find_all(&call().arg(0, int_const(42u128)).build()).len(), 1);
    assert_eq!(matcher.find_all(&call().arg(0, int_const(99u128)).build()).len(), 0);
    // Out-of-range arg index → reject.
    assert_eq!(matcher.find_all(&call().arg(99, any()).build()).len(), 0);
}

#[test]
fn call_arg_nests_value_builder_load() {
    // A `Call` whose arg0 is the value loaded from a constant address.
    // The value-producing `load()` builder nests directly as a `Call`
    // arg operand because it implements `MatchPat`.
    let arg = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(arg)
        .arg(arg)
        .build_fn_single_region()
        .unwrap();
    let addr = b.build_int_const(0x40u64, NodeOutputType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::I64)
        .unwrap();
    b.write_variable(&arg, loaded).unwrap();
    let tgt = b.build_int_const(0xABCDu64, NodeOutputType::I64).unwrap();
    b.build_call(tgt).unwrap();
    b.build_return(None, &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    // load() nested in arg(0) matches; a mismatched address load rejects.
    assert_eq!(
        matcher
            .find_all(&call().arg(0, load().addr(int_const(0x40u128))).build())
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&call().arg(0, load().addr(int_const(0x99u128))).build())
            .len(),
        0
    );
}

// ── CallOther ─────────────────────────────────────────────────────────────────

/// A `CallOther` named `name` with op id `op`, then return.
fn call_other_named(name: &str, op: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let (_node, _val, _clob) = b
        .build_call_other_modeled(op, name, &[], None, &[], &[], &[])
        .unwrap();
    b.build_return(None, &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn call_other_unconstrained_matches() {
    let function = call_other_named("rdtsc", 7);
    assert_eq!(
        Matcher::try_new(&function).unwrap().find_all(&call_other().build()).len(),
        1
    );
}

#[test]
fn call_other_name_filter() {
    let function = call_other_named("rdtsc", 7);
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(matcher.find_all(&call_other().name("rdtsc").build()).len(), 1);
    assert_eq!(matcher.find_all(&call_other().name("cpuid").build()).len(), 0);
}

#[test]
fn call_other_user_op_id_filter() {
    let function = call_other_named("rdtsc", 7);
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(matcher.find_all(&call_other().user_op_id(7).build()).len(), 1);
    assert_eq!(matcher.find_all(&call_other().user_op_id(8).build()).len(), 0);
}

// ── Return ────────────────────────────────────────────────────────────────────

fn return_const(v: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let val = b.build_int_const(v, NodeOutputType::I64).unwrap();
    b.build_return(Some(val), &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn ret_unconstrained_matches() {
    let function = return_const(7);
    assert_eq!(Matcher::try_new(&function).unwrap().find_all(&ret().build()).len(), 1);
}

#[test]
fn ret_val_matches_and_captures() {
    let function = return_const(7);
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(matcher.find_all(&ret().ret_val(0, int_const(7u128)).build()).len(), 1);
    assert_eq!(matcher.find_all(&ret().ret_val(0, int_const(0u128)).build()).len(), 0);

    let c = Capture::new();
    let hits = matcher.find_all(&ret().ret_val(0, var(c)).build());
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn ret_without_value_rejects_ret_val() {
    let function = call_at(0x1234); // Return with no value.
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(matcher.find_all(&ret().build()).len(), 1);
    assert_eq!(matcher.find_all(&ret().ret_val(0, any()).build()).len(), 0);
}

#[test]
fn ret_preceded_by_smoke() {
    let function = return_const(7);
    // The Return's ctrl predecessor is a Region; `any()` matches it.
    assert_eq!(
        Matcher::try_new(&function).unwrap().find_all(&ret().preceded_by(any()).build()).len(),
        1
    );
}

#[test]
fn ret_captures_node() {
    let function = return_const(7);
    let n = Capture::new();
    let hits = Matcher::try_new(&function).unwrap().find_all(&ret().capture(n).build());
    assert_eq!(hits.len(), 1);
    let node = hits[0].node(n, &function).expect("ret node capture");
    assert!(matches!(function.node_kind(node), strider_ir::node::NodeKind::Return));
}

// ── If ────────────────────────────────────────────────────────────────────────

fn if_then_else() -> (strider_ir::Function, strider_ir::node::NodeId) {
    let (function, if_node, ()) = RegisterSet::new()
        .build_if_then_else_returns(|b| {
            let c = b.build_boolean_const(false);
            Ok((c, ()))
        })
        .unwrap();
    (function, if_node)
}

#[test]
fn if_unconstrained_matches() {
    let (function, _) = if_then_else();
    assert_eq!(Matcher::try_new(&function).unwrap().find_all(&if_node().build()).len(), 1);
}

#[test]
fn if_cond_captures() {
    let (function, _) = if_then_else();
    let c = Capture::new();
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&if_node().cond(var(c)).build());
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn if_with_true_and_false_branches() {
    let (function, _) = if_then_else();
    // The single consumer of each control output is the branch Region;
    // `any()` matches a real node.
    assert_eq!(
        Matcher::try_new(&function)
            .unwrap()
            .find_all(
                &if_node()
                    .with_true(any().into_pattern())
                    .with_false(any().into_pattern())
                    .build(),
            )
            .len(),
        1
    );
}

#[test]
fn if_captures_node() {
    let (function, if_id) = if_then_else();
    let n = Capture::new();
    let hits = Matcher::try_new(&function).unwrap().find_all(&if_node().capture(n).build());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node(n, &function).expect("if node capture"), if_id);
}

/// White-box: the built `If` pattern carries exactly two control-output
/// vertices (representation invariant — true at slot 0, false at slot 1).
#[test]
fn if_pattern_has_two_control_output_vertices() {
    let pat = if_node().build();
    assert_eq!(
        pat.control_output_count(),
        2,
        "If pattern must declare two control-output vertices"
    );
}

// ── Phi / MemPhi / ValuePhi ───────────────────────────────────────────────────

#[test]
fn mem_phi_matches_region_head() {
    // A freshly created region carries one MemPhi at its head.
    let function = return_const(0);
    assert_eq!(
        Matcher::try_new(&function).unwrap().find_all(&mem_phi().build()).len(),
        1
    );
}

#[test]
fn phi_matches_tagged_phi() {
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let v = b.read_variable(&rax).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    assert_eq!(Matcher::try_new(&function).unwrap().find_all(&phi().build()).len(), 1);
}

#[test]
fn phi_capture_binds_value_output() {
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let v = b.read_variable(&rax).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();
    let c = Capture::new();
    let hits = matcher.find_all(&phi().capture(c).build());
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some(), "phi().capture(c) must bind the matched phi's output");
}

#[test]
fn phi_for_vn_filters() {
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let rbx = strider_ir_test_utils::reg_vn(16, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let v = b.read_variable(&rax).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();
    assert_eq!(matcher.find_all(&phi().for_vn(rax).build()).len(), 1);
    assert_eq!(matcher.find_all(&phi().for_vn(rbx).build()).len(), 0);
}

#[test]
fn value_phi_filters_to_anonymous_only() {
    // A tracked-var read produces a *tagged* Phi; value_phi() (anonymous
    // only) must NOT match it.
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let v = b.read_variable(&rax).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();
    // phi() (no filter) still matches the tagged Phi.
    assert_eq!(matcher.find_all(&phi().build()).len(), 1);
    // value_phi() filters it out.
    assert_eq!(matcher.find_all(&value_phi().build()).len(), 0);
}

// ── function_arg ──────────────────────────────────────────────────────────────

#[test]
fn function_arg_handle_resolves_register_carrier() {
    // The `arg_index_to_nodes` side-table is normally populated by the
    // `FunctionArgDetect` post-pass; in this raw-build unit test we
    // register the carrier directly to exercise the matcher's
    // `function_arg` handle API + Register/Stack source dispatch.
    use strider_ir::node::NodeKind;
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .arg(rax)
        .build_fn_single_region()
        .unwrap();
    let v = b.read_variable(&rax).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let mut function = b.build().unwrap();
    // Find the InitialVar(rax) carrier and register it at arg index 0.
    let carrier = function
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::InitialVar(vn) if *vn == rax))
        .expect("InitialVar(rax) carrier");
    function.register_arg_node(0, carrier);

    let matcher = Matcher::try_new(&function).unwrap();
    let handle = matcher.function_arg(0).expect("arg 0 carrier");
    assert!(matches!(
        function.node_kind(handle.node()),
        NodeKind::InitialVar(_)
    ));
    use strider_pattern::matcher::ArgSource;
    assert_eq!(handle.source(), ArgSource::Register(rax));
    assert_eq!(matcher.function_arg_count(), 1);
}

/// Build a function with a register-passed arg carrier (`InitialVar(rax)`
/// at index 0) and a stack-passed arg carrier (a `Load` at index 1), with
/// the carriers registered directly in `arg_index_to_nodes` (as the
/// `FunctionArgDetect` post-pass would). Returns `(function, rax)`.
fn two_arg_carriers() -> (strider_ir::Function, rsleigh::Vn) {
    use strider_ir::node::NodeKind;
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .arg(rax)
        .build_fn_single_region()
        .unwrap();
    // Register-arg carrier: read the tracked register → InitialVar(rax).
    let v = b.read_variable(&rax).unwrap();
    // Stack-arg carrier: a Load off a constant address.
    let addr = b.build_int_const(0x40u64, NodeOutputType::I64).unwrap();
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::I64).unwrap();
    // Combine both carriers so each stays reachable from the Return.
    let sum = b
        .build_int_binary_operation(v, loaded, strider_ir::IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let mut function = b.build().unwrap();

    let reg_carrier = function
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::InitialVar(vn) if *vn == rax))
        .expect("InitialVar(rax) carrier");
    let stack_carrier = function
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::Load(_)))
        .expect("Load carrier");
    // Stamp the stack-arg offset that `StackOffsetDetect` would record so
    // `function_arg_stack`'s offset enforcement has something to check
    // against. `function_arg_stack` only reads the offset, so any valid
    // base output handle suffices here.
    let base = function.node_outputs(stack_carrier)[0];
    function.set_stack_offset(stack_carrier, base, 0x40);
    function.register_arg_node(0, reg_carrier);
    function.register_arg_node(1, stack_carrier);
    (function, rax)
}

#[test]
fn function_arg_index_matches_carrier() {
    use strider_pattern::function_arg;
    let (function, _rax) = two_arg_carriers();
    let matcher = Matcher::try_new(&function).unwrap();
    // Each index matches exactly its one registered carrier.
    assert_eq!(matcher.find_all(&function_arg(0).build()).len(), 1);
    assert_eq!(matcher.find_all(&function_arg(1).build()).len(), 1);
    // No carrier registered at index 2.
    assert_eq!(matcher.find_all(&function_arg(2).build()).len(), 0);
}

#[test]
fn function_arg_any_matches_every_carrier() {
    use strider_pattern::function_arg_any;
    let (function, _rax) = two_arg_carriers();
    let matcher = Matcher::try_new(&function).unwrap();
    // Both the register and stack carriers are matched.
    assert_eq!(matcher.find_all(&function_arg_any().build()).len(), 2);
}

#[test]
fn function_arg_reg_matches_only_register_carrier() {
    use strider_pattern::{function_arg, function_arg_reg};
    let (function, rax) = two_arg_carriers();
    let matcher = Matcher::try_new(&function).unwrap();
    // Register source at index 0 matches the InitialVar(rax) carrier.
    assert_eq!(matcher.find_all(&function_arg_reg(rax, 0).build()).len(), 1);
    // The stack carrier (index 1) is a Load, not a register source.
    assert_eq!(matcher.find_all(&function_arg_reg(rax, 1).build()).len(), 0);
    // Wrong varnode at index 0 doesn't match.
    let rbx = strider_ir_test_utils::reg_vn(8, 8);
    assert_eq!(matcher.find_all(&function_arg_reg(rbx, 0).build()).len(), 0);
    // Sanity: index 0 with no source filter still matches.
    assert_eq!(matcher.find_all(&function_arg(0).build()).len(), 1);
}

#[test]
fn function_arg_stack_matches_only_stack_carrier() {
    use strider_pattern::function_arg_stack;
    let (function, _rax) = two_arg_carriers();
    let matcher = Matcher::try_new(&function).unwrap();
    // Stack source at index 1 matches the Load carrier.
    assert_eq!(
        matcher
            .find_all(&function_arg_stack(rsleigh::VnSpace::RAM, 0x40, 1).build())
            .len(),
        1
    );
    // The register carrier (index 0) is an InitialVar, not a stack source.
    assert_eq!(
        matcher
            .find_all(&function_arg_stack(rsleigh::VnSpace::RAM, 0x40, 0).build())
            .len(),
        0
    );
}

#[test]
fn function_arg_stack_rejects_wrong_offset() {
    use strider_pattern::function_arg_stack;
    let (function, _rax) = two_arg_carriers();
    let matcher = Matcher::try_new(&function).unwrap();
    // The stack carrier at index 1 has recorded offset 0x40. A pattern
    // with the correct space + index but a DIFFERENT offset must not
    // match — the offset is enforced against `Function::stack_offset`.
    assert_eq!(
        matcher
            .find_all(&function_arg_stack(rsleigh::VnSpace::RAM, 0x48, 1).build())
            .len(),
        0,
        "wrong offset must not match the registered stack carrier"
    );
    // Sanity: the correct offset still matches.
    assert_eq!(
        matcher
            .find_all(&function_arg_stack(rsleigh::VnSpace::RAM, 0x40, 1).build())
            .len(),
        1
    );
}

#[test]
fn function_arg_does_not_match_non_carrier() {
    use strider_pattern::function_arg;
    // A function whose InitialVar is NOT registered as an arg carrier.
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .arg(rax)
        .build_fn_single_region()
        .unwrap();
    let v = b.read_variable(&rax).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();
    // No carrier registered → no match, even though an InitialVar exists.
    assert_eq!(matcher.find_all(&function_arg(0).build()).len(), 0);
}
