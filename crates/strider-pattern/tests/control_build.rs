use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{ExtendOp, FunctionBuilder, IRBuilderExt, IRViewer, IRWalker};
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{
    Capture, CaptureExt, CastMask, MatchPat, Matcher, any_int_const, anything, call, call_other,
    entry, if_else, indirect_branch, int_add, int_const, load, mem_phi, phi, region, ret, store,
    switch, unreachable, var,
};

/// `call(addr)` followed by a `Return`.
fn call_at(addr: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let tgt = b.build_int_const(addr, ValueType::I64).unwrap();
    b.build_call_cc(tgt, None).unwrap();
    // build_call leaves the same region active, so return in place.
    b.build_return(None, &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn call_at_addr_matches_and_rejects() {
    let function = call_at(0x1234);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher.find_all(&call().at(0x1234).build()).unwrap().len(),
        1
    );
    assert_eq!(
        matcher.find_all(&call().at(0x9999).build()).unwrap().len(),
        0
    );
}

#[test]
fn call_target_set() {
    let function = call_at(0x1234);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(
                &call()
                    .target(int_const([0x1000u64, 0x1234, 0x9999]))
                    .build()
            )
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&call().target(int_const([0x1000u64, 0x9999])).build())
            .unwrap()
            .len(),
        0
    );
    // Empty set is vacuously false.
    assert_eq!(
        matcher
            .find_all(&call().target(int_const(Vec::<u64>::new())).build())
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn call_target_pattern_captures() {
    let function = call_at(0x1234);
    let c = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(&call().target(var(c)).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(c).is_some());
}

#[test]
fn call_arg_nests_value_builder_load() {
    // `load()` nests directly as a `Call` arg operand because it is a `MatchPat`.
    let arg = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(arg)
        .arg(arg)
        .build_fn_single_region()
        .unwrap();
    let addr = b.build_int_const(0x40u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.write_variable(&arg, loaded).unwrap();
    let tgt = b.build_int_const(0xABCDu64, ValueType::I64).unwrap();
    b.build_call_cc(tgt, None).unwrap();
    b.build_return(None, &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::new(&function);

    assert_eq!(
        matcher
            .find_all(&call().arg(0, load().addr(int_const(0x40u128))).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&call().arg(0, load().addr(int_const(0x99u128))).build())
            .unwrap()
            .len(),
        0
    );
}

/// A `CallOther` named `name` with op id `op`, then return.
fn call_other_named(name: &str, op: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let (_node, _result) = b
        .build_call_other_abi(
            op,
            name,
            &[],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
                no_return: false,
            },
            None,
            false,
        )
        .unwrap();
    b.build_return(None, &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn call_other_unconstrained_matches() {
    let function = call_other_named("rdtsc", 7);
    assert_eq!(
        Matcher::new(&function)
            .find_all(&call_other().build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn call_other_name_filter() {
    let function = call_other_named("rdtsc", 7);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&call_other().name("rdtsc").build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&call_other().name("cpuid").build())
            .unwrap()
            .len(),
        0
    );
}

fn return_const(v: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let val = b.build_int_const(v, ValueType::I64).unwrap();
    b.build_return(Some(val), &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn ret_val_matches_and_captures() {
    let function = return_const(7);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&ret().ret_val(0, int_const(7u128)).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&ret().ret_val(0, int_const(0u128)).build())
            .unwrap()
            .len(),
        0
    );

    let c = Capture::new();
    let hits = matcher.find_all(&ret().ret_val(0, var(c)).build()).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(c).is_some());
}

#[test]
fn ret_without_value_rejects_ret_val() {
    let function = call_at(0x1234); // Return with no value.
    let matcher = Matcher::new(&function);
    assert_eq!(matcher.find_all(&ret().build()).unwrap().len(), 1);
    assert_eq!(
        matcher
            .find_all(&ret().ret_val(0, anything()).build())
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn ret_ctrl_smoke() {
    let function = return_const(7);
    // The Return's ctrl predecessor is a Region, which `anything()` matches.
    assert_eq!(
        Matcher::new(&function)
            .find_all(&ret().ctrl(anything()).build())
            .unwrap()
            .len(),
        1
    );
}

fn indirect_branch_to(target_addr: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let tgt = b.build_int_const(target_addr, ValueType::I64).unwrap();
    b.build_indirect_branch(tgt).unwrap();
    b.build().unwrap()
}

#[test]
fn indirect_branch_captures_node() {
    let function = indirect_branch_to(0x4000);
    let n = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(&indirect_branch().capture(n).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    let node = hits[0]
        .node(n, function.graph())
        .expect("indirect_branch node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::IndirectBranch
    ));
}

#[test]
fn indirect_branch_target_matches_and_captures() {
    let function = indirect_branch_to(0x4000);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&indirect_branch().target(int_const(0x4000u128)).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&indirect_branch().target(int_const(0u128)).build())
            .unwrap()
            .len(),
        0
    );

    let c = Capture::new();
    let hits = matcher
        .find_all(&indirect_branch().target(var(c)).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(c).is_some());
}

fn unreachable_fn() -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    b.build_unreachable().unwrap();
    b.build().unwrap()
}

#[test]
fn unreachable_matches() {
    let function = unreachable_fn();
    let hits = Matcher::new(&function)
        .find_all(&unreachable().build())
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn unreachable_captures_node() {
    let function = unreachable_fn();
    let n = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(&unreachable().capture(n).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    let node = hits[0]
        .node(n, function.graph())
        .expect("unreachable node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::Unreachable
    ));
}

fn switch_fn(addr: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let dispatch = b.build_int_const(addr, ValueType::I64).unwrap();
    let a = b.create_region_all().unwrap();
    let c = b.create_region_all().unwrap();
    b.build_switch(dispatch, &[(a, 0x1000), (c, 0x1020)])
        .unwrap();
    b.set_region(a);
    b.build_return(None, &[]).unwrap();
    b.set_region(c);
    b.build_return(None, &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn switch_matches_and_captures() {
    let function = switch_fn(0x1000);
    let n = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(&switch().capture(n).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    let node = hits[0]
        .node(n, function.graph())
        .expect("switch node capture");
    assert!(matches!(
        function.node_kind(node),
        strider_ir::node::NodeKind::Switch
    ));
}

#[test]
fn switch_selector_matches_and_captures() {
    let function = switch_fn(0x1000);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&switch().selector(int_const(0x1000u128)).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&switch().selector(int_const(0u128)).build())
            .unwrap()
            .len(),
        0
    );

    let c = Capture::new();
    let hits = matcher
        .find_all(&switch().selector(var(c)).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(c).is_some());
}

/// `Switch` is the one builder whose `.output(slot)` vertex is also its only
/// vertex, so the root lookup has nothing to pick between and the slot has to
/// be pinned on the vertex itself.
#[test]
fn switch_output_slot_pins_one_arm() {
    let function = switch_fn(0x1000);
    let matcher = Matcher::new(&function);
    let arm = |slot: usize| {
        let c = Capture::new();
        let hits = matcher
            .find_all(&switch().output(slot).capture(c).build())
            .unwrap();
        assert_eq!(hits.len(), 1, "output({slot}) must bind one arm");
        hits[0].value(c).expect("arm control edge")
    };
    assert_ne!(arm(0), arm(1));

    // Two arms, so a third slot has no edge to bind.
    assert_eq!(
        matcher
            .find_all(&switch().output(2).capture(Capture::new()).build())
            .unwrap()
            .len(),
        0
    );
}

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
    assert_eq!(
        Matcher::new(&function)
            .find_all(&if_else().build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn if_cond_captures() {
    let (function, _) = if_then_else();
    let c = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(&if_else().cond(var(c)).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(c).is_some());
}

#[test]
fn if_with_true_and_false_branches() {
    let (function, _) = if_then_else();
    // Each control output's single consumer is the branch Region.
    assert_eq!(
        Matcher::new(&function)
            .find_all(
                &if_else()
                    .with_true(anything().into_pattern())
                    .with_false(anything().into_pattern())
                    .build(),
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn if_captures_node() {
    let (function, if_id) = if_then_else();
    let n = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(&if_else().capture(n).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].node(n, function.graph()).expect("if node capture"),
        if_id
    );
}

#[test]
fn if_capture_true_false_bind_distinct_control_outputs() {
    let (function, if_id) = if_then_else();
    let t = Capture::new();
    let f = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(&if_else().capture_true(t).capture_false(f).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    let tv = hits[0].value(t).expect("true control output bound");
    let fv = hits[0].value(f).expect("false control output bound");
    assert_ne!(tv, fv, "true and false outputs are distinct values");
    assert_eq!(function.graph().producer(tv), if_id);
    assert_eq!(function.graph().producer(fv), if_id);
}

#[test]
fn if_else_capture_and_control_output_captures_coexist() {
    // A node capture and the output-vertex captures on the same If must ALL
    // bind: an anchor capture never displaces the node capture.
    let (function, if_id) = if_then_else();
    let (g, t, f) = (Capture::new(), Capture::new(), Capture::new());
    let hits = Matcher::new(&function)
        .find_all(
            &if_else()
                .capture(g)
                .capture_true(t)
                .capture_false(f)
                .build(),
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node(g, function.graph()).unwrap(), if_id);
    assert!(hits[0].value(t).is_some(), "true output still binds");
    assert!(hits[0].value(f).is_some(), "false output still binds");
}

#[test]
fn mem_phi_matches_region_head() {
    // A freshly created region carries one MemPhi at its head.
    let function = return_const(0);
    assert_eq!(
        Matcher::new(&function)
            .find_all(&mem_phi().build())
            .unwrap()
            .len(),
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
    assert_eq!(
        Matcher::new(&function)
            .find_all(&phi().build())
            .unwrap()
            .len(),
        1
    );
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
    let matcher = Matcher::new(&function);
    let c = Capture::new();
    let hits = matcher.find_all(&phi().capture(c).build()).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].value(c).is_some(),
        "phi().capture(c) must bind the matched phi's output"
    );
}

/// Diamond join `if(true){ var=1 } else { var=2 }` then read `var`. The join's
/// `Phi` has `IntConst(1)` at data slot 1 and `IntConst(2)` at slot 2.
fn phi_over_two_consts() -> strider_ir::Function {
    let var_vn = strider_ir_test_utils::reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(var_vn).build_fn().unwrap();

    let entry = b.create_region_all().unwrap();
    let region_t = b.create_region_all().unwrap();
    let region_f = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, region_t, region_f).unwrap();

    b.set_region(region_t);
    let v1 = b.build_int_const(1u64, ValueType::I64).unwrap();
    b.write_variable(&var_vn, v1).unwrap();
    b.build_branch(join).unwrap();

    b.set_region(region_f);
    let v2 = b.build_int_const(2u64, ValueType::I64).unwrap();
    b.write_variable(&var_vn, v2).unwrap();
    b.build_branch(join).unwrap();

    b.set_region(join);
    let phi_val = b.read_variable(&var_vn).unwrap();
    b.build_return(Some(phi_val), &[]).unwrap();
    b.set_lift_addr(None);
    b.build().unwrap()
}

/// Diamond join whose `Phi` feeds `Add(phi, 10)`, putting the phi in a value
/// operand position: the shape `phi()` must nest into.
fn phi_feeding_add() -> strider_ir::Function {
    let var_vn = strider_ir_test_utils::reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(var_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let region_t = b.create_region_all().unwrap();
    let region_f = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, region_t, region_f).unwrap();
    b.set_region(region_t);
    let v1 = b.build_int_const(1u64, ValueType::I64).unwrap();
    b.write_variable(&var_vn, v1).unwrap();
    b.build_branch(join).unwrap();
    b.set_region(region_f);
    let v2 = b.build_int_const(2u64, ValueType::I64).unwrap();
    b.write_variable(&var_vn, v2).unwrap();
    b.build_branch(join).unwrap();
    b.set_region(join);
    let phi_val = b.read_variable(&var_vn).unwrap();
    let ten = b.build_int_const(10u64, ValueType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(phi_val, ten, strider_ir::IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    b.set_lift_addr(None);
    b.build().unwrap()
}

/// A `Phi` produces a value output, so `phi()` must nest as a value operand:
/// `int_add(x, phi())`, and the Python `store(data=phi())`, both wire it into a
/// value slot.
#[test]
fn phi_nests_as_a_value_operand() {
    let function = phi_feeding_add();
    let m = Matcher::new(&function);
    assert_eq!(
        m.find_all(&int_add(phi(), int_const(10u128)).into_pattern())
            .unwrap()
            .len(),
        1,
        "phi must match nested as a value operand of Add"
    );
    let c = Capture::new();
    let hits = m
        .find_all(&int_add(phi().capture(c), anything()).into_pattern())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].node(c, function.graph()).is_some(),
        "captured phi binds out"
    );
}

/// Two-input phi, every data input a `ZeroExtend` of an I32 `Load`. The `Load`
/// is a real interior node carried by no per-region trivial phi, making it a
/// clean cast-walk-through discriminator; a bare `InitialVar` would be shadowed
/// by the trivial single-predecessor phis the builder emits per tracked-var read.
fn phi_over_casts_of_load() -> strider_ir::Function {
    let phi_reg = strider_ir_test_utils::reg_vn(0x10, 8); // I64 phi'd var
    let mut b = RegisterSet::new().tracked(phi_reg).build_fn().unwrap();

    let entry = b.create_region_all().unwrap();
    let region_t = b.create_region_all().unwrap();
    let region_f = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, region_t, region_f).unwrap();

    for region in [region_t, region_f] {
        b.set_region(region);
        let addr = b.build_int_const(0x40u64, ValueType::I64).unwrap();
        let loaded = b
            .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)
            .unwrap();
        let z = b
            .extend_if_needed(loaded, ValueType::I64, ExtendOp::ZeroExtend)
            .unwrap();
        b.write_variable(&phi_reg, z).unwrap();
        b.build_branch(join).unwrap();
    }

    b.set_region(join);
    let phi_val = b.read_variable(&phi_reg).unwrap();
    b.build_return(Some(phi_val), &[]).unwrap();
    b.set_lift_addr(None);
    b.build().unwrap()
}

#[test]
fn phi_any_input_honours_ignore_casts() {
    let function = phi_over_casts_of_load();
    let matcher = Matcher::new(&function);
    // Every phi data input is an I64 `ZeroExtend` with the `Load` one cast down.
    assert_eq!(
        matcher
            .find_all(&phi().any_input(load()).build())
            .unwrap()
            .len(),
        0,
        "any_input must not reach through a cast by default",
    );
    assert_eq!(
        matcher
            .find_all(
                &phi()
                    .any_input(load())
                    .build()
                    .ignore_casts_mask(CastMask::EXTEND)
            )
            .unwrap()
            .len(),
        1,
        "any_input honours ignore_casts like fixed-slot inputs",
    );
}

#[test]
fn phi_any_input_matches_a_data_input_regardless_of_slot() {
    let function = phi_over_two_consts();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&phi().any_input(int_const(2u128)).build())
            .unwrap()
            .len(),
        1,
        "any_input finds the const at a non-first data slot"
    );
    // `IntConst(1)` is the first data slot.
    assert_eq!(
        matcher
            .find_all(&phi().any_input(int_const(1u128)).build())
            .unwrap()
            .len(),
        1,
    );
    assert_eq!(
        matcher
            .find_all(&phi().any_input(int_const(99u128)).build())
            .unwrap()
            .len(),
        0,
        "any_input over an absent value matches nothing"
    );
}

/// `any_input`'s candidate slots are EVERY input slot (no `PhiToken` filter);
/// the sub-pattern is what discriminates. A value-typed sub such as
/// `int_const` can never match the `PhiToken` producer (a `Region`), so it
/// still binds only a data input.
#[test]
fn phi_any_input_value_sub_binds_a_data_input_not_the_phi_token() {
    let function = phi_over_two_consts();
    let m = Matcher::new(&function);

    // Existential: `x` binds either data input, and both are distinct bindings.
    let x = Capture::new();
    let hits = m.find_all(&phi().any_input(int_const(x)).build()).unwrap();
    assert_eq!(hits.len(), 2, "one match per bindable DATA input");
    for hit in &hits {
        let bound = hit.node(x, function.graph()).unwrap();
        assert!(
            matches!(function.node_kind(bound), NodeKind::IntConst(_)),
            "any_input(value sub) must bind a data input (IntConst), got {:?}",
            function.node_kind(bound)
        );
    }
}

/// A bare `any_input(var(..))` discriminates nothing, so with no `PhiToken`
/// filter it also matches the phi's slot-0 `PhiToken` plumbing edge.
#[test]
fn phi_any_input_wildcard_can_reach_the_phi_token() {
    let function = phi_over_two_consts();
    let m = Matcher::new(&function);

    let x = Capture::new();
    let hits = m.find_all(&phi().any_input(var(x)).build()).unwrap();
    assert_eq!(
        hits.len(),
        3,
        "a bare wildcard any_input reaches all 3 input slots (2 data + 1 phi-token)"
    );
    let kinds: Vec<NodeKind> = hits
        .iter()
        .map(|hit| *function.node_kind(hit.node(x, function.graph()).unwrap()))
        .collect();
    assert!(
        kinds.iter().any(|k| matches!(k, NodeKind::IntConst(_))),
        "still reaches the data inputs, kinds: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| matches!(k, NodeKind::Region)),
        "now also reaches the phi-token producer (the owning Region), kinds: {kinds:?}"
    );
}

#[test]
fn phi_multiple_any_input_bind_distinct_slots() {
    // Phi data inputs are the constants 1 and 2, one slot each.
    let function = phi_over_two_consts();
    let m = Matcher::new(&function);
    assert_eq!(
        m.find_all(
            &phi()
                .any_input(int_const(1u128))
                .any_input(int_const(2u128))
                .build()
        )
        .unwrap()
        .len(),
        1,
    );
    assert_eq!(
        m.find_all(
            &phi()
                .any_input(int_const(1u128))
                .any_input(int_const(1u128))
                .build()
        )
        .unwrap()
        .len(),
        0,
        "two any_input must bind two DIFFERENT slots",
    );
    assert_eq!(
        m.find_all(
            &phi()
                .any_input(int_const(1u128))
                .any_input(any_int_const())
                .build()
        )
        .unwrap()
        .len(),
        1,
    );
}

#[test]
fn phi_any_input_binds_captures_out() {
    let function = phi_over_two_consts();
    let matcher = Matcher::new(&function);
    let c = Capture::new();
    let hits = matcher
        .find_all(&phi().any_input(int_const(2u128).capture(c)).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].value(c).is_some(),
        "a capture inside any_input binds out"
    );
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
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher.find_all(&phi().for_vn(rax).build()).unwrap().len(),
        1
    );
    assert_eq!(
        matcher.find_all(&phi().for_vn(rbx).build()).unwrap().len(),
        0
    );
}

/// `phi_token` targets raw slot 0 directly, no `+1` shift. `PhiToken` falls
/// outside `MatchPat`'s value domain, so a typed sub can never bind it.
#[test]
fn phi_token_typed_sub_never_matches() {
    let function = phi_over_two_consts();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&phi().phi_token(int_const(1u128)).build())
            .unwrap()
            .len(),
        0,
        "a typed value sub can never bind the PhiToken input"
    );
}

/// `phi_token` reaches slot 0, the `PhiToken` edge from the owning `Region`.
/// Distinct from `.phi_input(0, _)`, at raw slot 1.
#[test]
fn phi_token_wildcard_binds_the_phi_token_edge() {
    let function = phi_over_two_consts();
    let matcher = Matcher::new(&function);
    let c = Capture::new();
    let hits = matcher.find_all(&phi().phi_token(var(c)).build()).unwrap();
    assert_eq!(hits.len(), 1);
    let bound = hits[0].value(c).unwrap();
    assert!(
        matches!(
            function.value_kind(bound),
            strider_ir::node::ValueKind::PhiToken
        ),
        "phi_token(var(c)) must bind the PhiToken-kind input, got {:?}",
        function.value_kind(bound)
    );

    // The predecessor-indexed accessor reaches a different slot: a typed const
    // sub matches.
    assert_eq!(
        matcher
            .find_all(&phi().phi_input(0, int_const(1u128)).build())
            .unwrap()
            .len(),
        1,
        "phi_input(0, _) must reach predecessor 0's data value, not the PhiToken"
    );
}

/// A store on each branch of an if/else plus a load at the join, forcing a
/// genuine `MemPhi` with two memory predecessors.
fn mem_phi_with_two_stores() -> strider_ir::Function {
    let var_vn = strider_ir_test_utils::reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(var_vn).build_fn().unwrap();

    let entry = b.create_region_all().unwrap();
    let region_t = b.create_region_all().unwrap();
    let region_f = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, region_t, region_f).unwrap();

    for (region, val) in [(region_t, 1u64), (region_f, 2u64)] {
        b.set_region(region);
        let addr = b.build_int_const(0x40u64, ValueType::I64).unwrap();
        let data = b.build_int_const(val, ValueType::I64).unwrap();
        b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
        b.build_branch(join).unwrap();
    }

    b.set_region(join);
    let addr = b.build_int_const(0x48u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    b.set_lift_addr(None);
    b.build().unwrap()
}

/// A wildcard reaches the memory predecessors AND the `PhiToken` slot; a typed
/// value sub reaches neither.
#[test]
fn mem_phi_any_input_general_model() {
    let function = mem_phi_with_two_stores();
    let matcher = Matcher::new(&function);

    let c = Capture::new();
    let hits = matcher
        .find_all(&mem_phi().any_input(var(c)).build())
        .unwrap();
    let kinds: Vec<_> = hits
        .iter()
        .map(|h| function.value_kind(h.value(c).unwrap()))
        .collect();
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, strider_ir::node::ValueKind::Memory)),
        "wildcard any_input must bind a memory predecessor, kinds: {kinds:?}"
    );
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, strider_ir::node::ValueKind::PhiToken)),
        "wildcard any_input must also reach the PhiToken slot, kinds: {kinds:?}"
    );

    assert_eq!(
        matcher
            .find_all(&mem_phi().any_input(int_const(1u128)).build())
            .unwrap()
            .len(),
        0,
        "a typed value sub must not bind a Memory or PhiToken predecessor"
    );
}

/// `phi_token` targets slot 0 directly; `.phi_input(0, _)` addresses memory
/// predecessor 0, at raw slot 1. Every region here carries its own `MemPhi`,
/// so a wildcard `phi_token` matches all four.
#[test]
fn mem_phi_phi_token_targets_slot_zero() {
    let function = mem_phi_with_two_stores();
    let matcher = Matcher::new(&function);

    let mem_phi_count = matcher.find_all(&mem_phi().build()).unwrap().len();

    let c = Capture::new();
    let hits = matcher
        .find_all(&mem_phi().phi_token(var(c)).build())
        .unwrap();
    assert_eq!(
        hits.len(),
        mem_phi_count,
        "a wildcard phi_token must match every MemPhi's slot-0 PhiToken input"
    );
    assert!(
        hits.iter().all(|h| matches!(
            function.value_kind(h.value(c).unwrap()),
            strider_ir::node::ValueKind::PhiToken
        )),
        "phi_token(var(c)) must bind the PhiToken-kind input on every hit"
    );

    // A typed sub can never bind the PhiToken slot.
    assert_eq!(
        matcher
            .find_all(&mem_phi().phi_token(int_const(1u128)).build())
            .unwrap()
            .len(),
        0
    );

    // Only the join's MemPhi has a genuine store as memory predecessor 0; the
    // other three chain to another MemPhi.
    assert_eq!(
        matcher
            .find_all(
                &mem_phi()
                    .phi_input(0, store().data(int_const(1u128)))
                    .build()
            )
            .unwrap()
            .len(),
        1,
        "phi_input(0, _) must reach memory predecessor 0, not the PhiToken"
    );
}

#[test]
fn call_any_input_binds_target() {
    let function = call_at(0x1234);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&call().any_input(int_const(0x1234u128)).build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn call_other_any_input_binds_arg() {
    let function = call_other_named("f", 5);
    let matcher = Matcher::new(&function);
    let hits = matcher
        .find_all(&call_other().any_input(var(Capture::new())).build())
        .unwrap();
    assert!(!hits.is_empty(), "any_input must bind some CallOther input");
}

#[test]
fn ret_any_input_binds_ret_val() {
    let function = return_const(7);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&ret().any_input(int_const(7u128)).build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn indirect_branch_any_input_binds_target() {
    let function = indirect_branch_to(0xBEEF);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&indirect_branch().any_input(int_const(0xBEEFu128)).build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn switch_any_input_binds_address() {
    let function = switch_fn(0xC0DE);
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&switch().any_input(int_const(0xC0DEu128)).build())
            .unwrap()
            .len(),
        1
    );
}

/// Only a wildcard reaches the sole ctrl predecessor: ctrl is not a value edge.
#[test]
fn unreachable_any_input_wildcard_reaches_ctrl() {
    let function = unreachable_fn();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&unreachable().any_input(var(Capture::new())).build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn load_any_input_binds_addr() {
    let var_vn = strider_ir_test_utils::reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(var_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&load().any_input(int_const(0x1000u128)).build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn load_raw_input_slot_is_the_addr_slot() {
    let var_vn = strider_ir_test_utils::reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(var_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::new(&function);
    // `Load` is `[mem(0), addr(1)]`.
    assert_eq!(
        matcher
            .find_all(&load().input(1, int_const(0x1000u128)).build())
            .unwrap()
            .len(),
        1
    );
    // Slot 0 is the memory edge; no value pattern binds it.
    assert_eq!(
        matcher
            .find_all(&load().input(0, int_const(0x1000u128)).build())
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn load_output_and_any_output_reach_the_loaded_value() {
    let var_vn = strider_ir_test_utils::reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(var_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::new(&function);
    let c = Capture::new();
    let hits = matcher
        .find_all(&load().output(0).capture(c).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(c).is_some());
    assert_eq!(
        matcher
            .find_all(&load().any_output().of_width(64).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&load().any_output().of_width(7).build())
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn if_ctrl_matches_the_control_predecessor() {
    let (function, _) = if_then_else();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&if_else().ctrl(anything()).build())
            .unwrap()
            .len(),
        1
    );
    // A value pattern can never bind a Control edge.
    assert_eq!(
        matcher
            .find_all(&if_else().ctrl(int_const(1u128)).build())
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn if_raw_input_slot_is_the_cond_slot() {
    let (function, _) = if_then_else();
    let matcher = Matcher::new(&function);
    // `If` is `[ctrl(0), cond(1)]`; the fixture branches on a false constant.
    assert_eq!(
        matcher
            .find_all(&if_else().input(1, anything()).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&if_else().any_input(anything()).build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn store_any_input_binds_data() {
    let var_vn = strider_ir_test_utils::reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(var_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let data = b.build_int_const(99u64, ValueType::I64).unwrap();
    b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
    b.build_return(None, &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&store().any_input(int_const(99u128)).build())
            .unwrap()
            .len(),
        1
    );
}

/// Register-passed carrier (`InitialVar(rax)`) at index 0 and stack-passed
/// carrier (a `Load`) at index 1, registered directly in `arg_index_to_values`
/// the way the `FunctionArgDetect` post-pass would.
fn two_arg_carriers() -> (strider_ir::Function, rsleigh::Vn) {
    use strider_ir::node::NodeKind;
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .arg(rax)
        .build_fn_single_region()
        .unwrap();
    // Register-arg carrier: InitialVar(rax).
    let v = b.read_variable(&rax).unwrap();
    // Stack-arg carrier.
    let addr = b.build_int_const(0x40u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    // Combine both carriers so each stays reachable from the Return.
    let sum = b
        .build_int_binary_operation(v, loaded, strider_ir::IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let mut function = b.build().unwrap();

    let reg_carrier = function
        .graph()
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::InitialVar(vn) if function.initial_vn(*vn) == rax))
        .expect("InitialVar(rax) carrier");
    let stack_carrier = function
        .graph()
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::Load(_)))
        .expect("Load carrier");
    // Stamp the offset `StackOffsetDetect` would record, so
    // `function_arg_stack`'s offset check has something to read. Only the
    // offset is read, so any valid base output handle works.
    let base = function.node_outputs(stack_carrier)[0];
    // `stack_offset(node)` resolves via the node's address value, so stamp the
    // slot on the Load's address input.
    let carrier_addr = function.node_inputs(stack_carrier)[1];
    function
        .side_tables_mut()
        .set_stack_slot(carrier_addr, base, 0x40);
    let reg_value = function.node_outputs(reg_carrier)[0];
    let stack_value = function.node_outputs(stack_carrier)[0];
    function.side_tables_mut().register_arg_value(0, reg_value);
    function
        .side_tables_mut()
        .register_arg_value(1, stack_value);
    (function, rax)
}

#[test]
fn function_arg_index_matches_carrier() {
    use strider_pattern::function_arg;
    let (function, _rax) = two_arg_carriers();
    let matcher = Matcher::new(&function);
    assert_eq!(matcher.find_all(&function_arg(0).build()).unwrap().len(), 1);
    assert_eq!(matcher.find_all(&function_arg(1).build()).unwrap().len(), 1);
    assert_eq!(matcher.find_all(&function_arg(2).build()).unwrap().len(), 0);
}

#[test]
fn any_function_arg_matches_every_carrier() {
    use strider_pattern::any_function_arg;
    let (function, _rax) = two_arg_carriers();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher.find_all(&any_function_arg().build()).unwrap().len(),
        2
    );
}

/// Integer carrier `InitialVar(rax)` at integer index 0 and float carrier
/// `InitialVar(fp0)` at float index 0: the two index spaces overlap, so index
/// 0 alone does not say which register is meant.
fn int_and_float_carriers() -> (strider_ir::Function, rsleigh::Vn) {
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let fp0 = strider_ir_test_utils::reg_vn(0x100, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .tracked(fp0)
        .arg(rax)
        .build_fn_single_region()
        .unwrap();
    let a = b.read_variable(&rax).unwrap();
    let f0 = b.read_variable(&fp0).unwrap();
    let sum = b
        .build_int_binary_operation(a, f0, strider_ir::IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let mut function = b.build().unwrap();

    let float_carrier = function
        .graph()
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::InitialVar(vn) if function.initial_vn(*vn) == fp0))
        .expect("InitialVar(fp0) carrier");
    let value = function.node_outputs(float_carrier)[0];
    function
        .side_tables_mut()
        .register_float_arg_value(0, value);
    (function, fp0)
}

#[test]
fn function_arg_float_matches_only_the_float_carrier() {
    use strider_pattern::{any_function_arg, function_arg, function_arg_float};
    let (function, fp0) = int_and_float_carriers();
    let matcher = Matcher::new(&function);

    let bound_vn = |pat: strider_pattern::FunctionArgPat| -> Vec<rsleigh::Vn> {
        let c = Capture::new();
        matcher
            .find_all(&pat.capture(c).build())
            .unwrap()
            .iter()
            .map(|hit| {
                let node = function.producer(hit.value(c).expect("carrier capture is bound"));
                match function.node_kind(node) {
                    NodeKind::InitialVar(id) => function.initial_vn(*id),
                    k => panic!("carrier is not an InitialVar: {k:?}"),
                }
            })
            .collect()
    };

    assert_eq!(bound_vn(function_arg_float(0)), vec![fp0]);
    assert_eq!(
        bound_vn(function_arg(0)),
        vec![strider_ir_test_utils::reg_vn(0, 8)],
        "integer index 0 must still be the integer carrier",
    );
    assert_eq!(
        matcher
            .find_all(&function_arg_float(1).build())
            .unwrap()
            .len(),
        0,
        "only one float carrier is registered",
    );
    assert_eq!(
        bound_vn(any_function_arg()).len(),
        2,
        "any_function_arg() spans both classes",
    );
}

#[test]
fn function_arg_reg_matches_only_register_carrier() {
    use strider_pattern::{function_arg, function_arg_reg};
    let (function, rax) = two_arg_carriers();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&function_arg_reg(rax, 0).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&function_arg_reg(rax, 1).build())
            .unwrap()
            .len(),
        0
    );
    let rbx = strider_ir_test_utils::reg_vn(8, 8);
    assert_eq!(
        matcher
            .find_all(&function_arg_reg(rbx, 0).build())
            .unwrap()
            .len(),
        0
    );
    // Sanity: index 0 without a source filter still matches.
    assert_eq!(matcher.find_all(&function_arg(0).build()).unwrap().len(), 1);
}

#[test]
fn function_arg_stack_matches_only_stack_carrier() {
    use strider_pattern::function_arg_stack;
    let (function, _rax) = two_arg_carriers();
    let matcher = Matcher::new(&function);
    assert_eq!(
        matcher
            .find_all(&function_arg_stack(rsleigh::VnSpace::RAM, 0x40, 1).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(&function_arg_stack(rsleigh::VnSpace::RAM, 0x40, 0).build())
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn function_arg_stack_rejects_wrong_offset() {
    use strider_pattern::function_arg_stack;
    let (function, _rax) = two_arg_carriers();
    let matcher = Matcher::new(&function);
    // The carrier at index 1 records offset 0x40, enforced against
    // `Function::stack_offset`.
    assert_eq!(
        matcher
            .find_all(&function_arg_stack(rsleigh::VnSpace::RAM, 0x48, 1).build())
            .unwrap()
            .len(),
        0,
        "wrong offset must not match the registered stack carrier"
    );
    // Sanity: the correct offset still matches.
    assert_eq!(
        matcher
            .find_all(&function_arg_stack(rsleigh::VnSpace::RAM, 0x40, 1).build())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn function_arg_does_not_match_non_carrier() {
    use strider_pattern::function_arg;
    // `rax` is tracked but is not an arg-passing register, so the builder
    // records no carrier at entry despite the InitialVar existing.
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let v = b.read_variable(&rax).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::new(&function);
    assert_eq!(matcher.find_all(&function_arg(0).build()).unwrap().len(), 0);
}

/// A `Call` clobbering a tracked 64-bit register puts a value output at a
/// non-zero slot (`Control@0`, `Memory@1`, clobber value@2). Scaffold for the
/// slot-coupling rule: a root output-vertex width/type constraint applies to
/// whichever output is matched, not always to slot 0.
fn call_with_clobber_retval() -> strider_ir::Function {
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let tgt = b.build_int_const(0x1234u64, ValueType::I64).unwrap();
    b.build_call_cc(tgt, None).unwrap();
    b.build_return(None, &[]).unwrap();
    b.build().unwrap()
}

#[test]
fn width_constraint_applies_to_non_slot_zero_value_output() {
    use strider_ir::node::NodeKind;
    use strider_pattern::{CaptureExt, any_bool};

    let function = call_with_clobber_retval();
    let m = Matcher::new(&function);

    let call = function
        .graph()
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::Call))
        .expect("call node");
    let clobber_value = *function
        .node_outputs(call)
        .iter()
        .find(|&&o| function.value_kind(o).as_value() == Some(ValueType::I64))
        .expect("64-bit clobber value output");

    // The I64 call-target const is the other 64-bit value, hence two matches.
    let c = Capture::new();
    let right = m.find_all(&var(c).of_width(64).into_pattern()).unwrap();
    assert!(
        right.iter().any(|hit| hit.value(c) == Some(clobber_value)),
        "the non-slot-0 64-bit clobber output is matched + bound by of_width(64)",
    );

    // The constraint must be checked at a non-slot-0 output too.
    let c2 = Capture::new();
    assert_eq!(
        m.find_all(&var(c2).of_width(32).into_pattern())
            .unwrap()
            .len(),
        0,
        "no 32-bit value output exists, so of_width(32) does not match",
    );

    // Only Control / Memory / I64 outputs exist here.
    assert_eq!(
        m.find_all(&any_bool().into_pattern()).unwrap().len(),
        0,
        "no I1 value output: any_bool() must not match a value-less or wide node",
    );
}

/// The `Call`'s kind and its slot-2 (clobber / result) I64 value output.
fn call_and_clobber(function: &strider_ir::Function) -> (NodeKind, strider_ir::node::ValueId) {
    let call = function
        .graph()
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::Call))
        .expect("call node");
    let clobber = *function
        .node_outputs(call)
        .iter()
        .find(|&&o| function.value_kind(o).as_value() == Some(ValueType::I64))
        .expect("64-bit clobber value output at slot 2");
    (*function.node_kind(call), clobber)
}

/// The generic leaf sibling-output binding: slot 2 is the clobber / result.
#[test]
fn call_output_slot_binds_sibling_value() {
    let function = call_with_clobber_retval();
    let m = Matcher::new(&function);
    let (_k, clobber) = call_and_clobber(&function);

    let c = Capture::new();
    let hits = m.find_all(&call().output(2).capture(c).build()).unwrap();
    assert_eq!(hits.len(), 1, "one Call, one binding");
    assert_eq!(
        hits[0].value(c),
        Some(clobber),
        "output(2) binds the slot-2 sibling output value",
    );
}

/// A wrong width must fail the whole match, proving the constraint is checked
/// on the secondary output vertex rather than dropped.
#[test]
fn call_output_slot_width_constraint() {
    let function = call_with_clobber_retval();
    let m = Matcher::new(&function);

    assert_eq!(
        m.find_all(&call().output(2).of_width(64).build())
            .unwrap()
            .len(),
        1,
        "slot-2 output is 64 bits wide",
    );
    assert_eq!(
        m.find_all(&call().output(2).of_width(32).build())
            .unwrap()
            .len(),
        0,
        "slot-2 output is not 32 bits: the width constraint rejects the match",
    );
}

/// `output(j).of_type(t)` pins the sibling output's exact value type.
#[test]
fn call_output_slot_type_constraint() {
    let function = call_with_clobber_retval();
    let m = Matcher::new(&function);

    assert_eq!(
        m.find_all(&call().output(2).of_type(ValueType::I64).build())
            .unwrap()
            .len(),
        1,
        "slot-2 output is I64",
    );
    assert_eq!(
        m.find_all(&call().output(2).of_type(ValueType::I32).build())
            .unwrap()
            .len(),
        0,
        "slot-2 output is not I32",
    );
}

/// Entry, if/else, join: four `Region` nodes plus one `Entry`.
fn branching_fn() -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn().unwrap();

    let entry_region = b.create_region_all().unwrap();
    let region_t = b.create_region_all().unwrap();
    let region_f = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();

    b.set_entry_region_all(entry_region).unwrap();
    b.set_region(entry_region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, region_t, region_f).unwrap();

    for region in [region_t, region_f] {
        b.set_region(region);
        b.build_branch(join).unwrap();
    }

    b.set_region(join);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    b.build().unwrap()
}

#[test]
fn entry_matches_exactly_one() {
    let function = branching_fn();
    assert_eq!(
        Matcher::new(&function)
            .find_all(&entry().build())
            .unwrap()
            .len(),
        1,
    );
}

/// Cross-checked against the same `walk_kind` sweep the Python-facing
/// `count_regions` uses.
#[test]
fn region_matches_every_region_node() {
    let function = branching_fn();
    let expected = function
        .walk_kind(|k| matches!(k, NodeKind::Region))
        .count();
    assert_eq!(expected, 4, "sanity: entry + true + false + join regions");
    assert_eq!(
        Matcher::new(&function)
            .find_all(&region().build())
            .unwrap()
            .len(),
        expected,
    );
}

#[test]
fn region_any_input_reaches_entry_predecessor() {
    let function = branching_fn();
    assert_eq!(
        Matcher::new(&function)
            .find_all(&region().any_input(entry()).build())
            .unwrap()
            .len(),
        1,
        "only the entry region has Entry as a direct control predecessor",
    );
}

/// Region has no fixed prefix ahead of its variadic tail, so raw slot 0 is
/// predecessor 0.
#[test]
fn region_input_slot_zero_reaches_entry_predecessor() {
    let function = branching_fn();
    assert_eq!(
        Matcher::new(&function)
            .find_all(&region().input(0, entry()).build())
            .unwrap()
            .len(),
        1,
    );
}

/// A typed value sub can never bind a `Region`'s Control predecessor edge:
/// `any_input` on a control-only family discriminates like it does on `MemPhi`.
#[test]
fn region_any_input_typed_value_sub_matches_nothing() {
    let function = branching_fn();
    assert_eq!(
        Matcher::new(&function)
            .find_all(&region().any_input(int_const(0u128)).build())
            .unwrap()
            .len(),
        0,
    );
}

fn indirect_branch_with_mode(target_addr: u64, mode: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let tgt = b.build_int_const(target_addr, ValueType::I64).unwrap();
    let m = b.build_int_const(mode, ValueType::I8).unwrap();
    b.build_indirect_branch_with_mode(tgt, Some(m)).unwrap();
    b.build().unwrap()
}

/// The ISA-mode input is slot 3 and is absent on a non-switching branch, so
/// pinning it rejects one and matches the other.
#[test]
fn indirect_branch_isa_mode_matches_only_a_switching_branch() {
    let switching = indirect_branch_with_mode(0x4000, 1);
    assert_eq!(
        Matcher::new(&switching)
            .find_all(&indirect_branch().isa_mode(int_const(1u128)).build())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        Matcher::new(&switching)
            .find_all(&indirect_branch().isa_mode(int_const(0u128)).build())
            .unwrap()
            .len(),
        0
    );

    let plain = indirect_branch_to(0x4000);
    assert_eq!(
        Matcher::new(&plain)
            .find_all(&indirect_branch().isa_mode(anything()).build())
            .unwrap()
            .len(),
        0
    );
}
