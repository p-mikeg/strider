//! SSA-shaped patterns: `phi()` / `phi_for(vn)`, `initial_var()` /
//! `initial_var_for(vn)`, and the `SideTables::arg_index_to_values` side-table
//! `FunctionArgDetect` populates.

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{IRViewer, IntCmpOp};
use strider_pattern::*;

use super::support::{Tb, assertions as a, reg_vn, shapes, stack_vn};

#[test]
fn initial_var_matches_any() {
    let (g, _reg) = shapes::single_initial_var();
    a::matches(&g, initial_var().into_pattern(), 1);
}

#[test]
fn initial_var_for_exact_vn_matches() {
    let (g, reg) = shapes::single_initial_var();
    a::matches(&g, initial_var_for(reg).into_pattern(), 1);
}

#[test]
fn initial_var_for_wrong_vn_rejects() {
    let (g, _reg) = shapes::single_initial_var();
    let other = reg_vn(0x40, 8);
    a::none(&g, initial_var_for(other).into_pattern());
}

/// `InitialVar` carries a per-function `all_vns` index, not the varnode.
/// With two tracked regs, `hi` sorts to `all_vns[1]` (not `[0]`), so a
/// correct `initial_var_for` must resolve each candidate node's index back
/// to its varnode and match by identity, never positionally.
#[test]
fn initial_var_for_resolves_nonzero_index() {
    let lo = reg_vn(0x00, 8);
    let hi = reg_vn(0x40, 8);
    let mut t = Tb::with_vars(&[lo, hi]);
    let lo_v = t.read_var(&lo);
    let hi_v = t.read_var(&hi);
    let sum = t.add(lo_v, hi_v); // keeps both InitialVars reachable
    let g = t.ret_val(sum);
    a::matches(&g, initial_var_for(hi).into_pattern(), 1);
    a::matches(&g, initial_var_for(lo).into_pattern(), 1);
}

#[test]
fn initial_var_capture_binds_value() {
    let (g, _reg) = shapes::single_initial_var();
    let v = Capture::new();
    let m = a::unique(&g, initial_var().capture(v).into_pattern());
    assert!(m.value(v).is_some());
}

/// `if (reg == 0) { reg = 1 } else { reg = 2 }`: after merge, a phi
/// materialises the new value of `reg`.
fn graph_phi_for_reg() -> (strider_ir::Function, rsleigh::Vn) {
    let reg = reg_vn(0, 8);
    let mut t = Tb::bare(vec![reg], &[], &[reg], &[], None, 0);
    let entry = t.region();
    let a_r = t.region();
    let b_r = t.region();
    let merge = t.region();
    t.set_entry(entry);

    t.enter(entry);
    let reg_v = t.read_var(&reg);
    let zero = t.u64(0);
    let neq = t.int_cmp(reg_v, zero, IntCmpOp::Equal);
    t.build_if(neq, a_r, b_r);

    t.enter(a_r);
    let one = t.u64(1);
    t.write_var(&reg, one);
    t.branch(merge);

    t.enter(b_r);
    let two = t.u64(2);
    t.write_var(&reg, two);
    t.branch(merge);

    t.enter(merge);
    let merged = t.read_var(&reg);
    (t.ret_val(merged), reg)
}

#[test]
fn phi_matches_any() {
    let (g, _reg) = graph_phi_for_reg();
    let hits = Matcher::new(&g).find_all(&phi().build()).unwrap();
    assert!(!hits.is_empty(), "expected at least one phi");
}

#[test]
fn phi_for_matches_exact_vn() {
    let (g, reg) = graph_phi_for_reg();
    let hits = Matcher::new(&g).find_all(&phi_for(reg).build()).unwrap();
    assert!(!hits.is_empty(), "phi_for({reg:?}) should match");
}

#[test]
fn phi_for_wrong_vn_rejects() {
    let (g, _reg) = graph_phi_for_reg();
    let other = reg_vn(0x40, 8);
    a::none(&g, phi_for(other).build());
}

// After `FunctionArgDetect`, the underlying `InitialVar` / `Load` nodes
// survive unchanged and are recorded in `SideTables::arg_index_to_values`.
// The tests below check the side-table contents and carrier node kinds.

/// A graph with one stack-arg at sp-relative offset `4`, index `0`.
fn graph_fn_arg_stack() -> strider_ir::Function {
    use strider_orchestrator::opt::FunctionArgDetect;
    let sp = stack_vn();
    // The pass reads its layout from the function's own CC, so the fixture
    // carries `sp` as the SP and a stack-arg layout based at +4.
    let mut t = Tb::from_rs(
        strider_ir_test_utils::RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 4,
                increment: 4,
            })),
    );

    // `read *(sp + 4)`: the first stack arg in cdecl-style.
    let sp_v = t.read_var(&sp);
    let four = t.u64(4);
    let addr = t.add(sp_v, four);
    let v = t.load_ram(addr, ValueType::I64);
    let mut function = t.ret_val(v);

    // Collapse the single-predecessor `read_var(sp)` phi so the stack-arg
    // load is a bare `InitialVar(sp) + 4` terminal (the production shape
    // after PhiCollapse) before the SP-aware post-pass runs.
    let mut pre = strider_orchestrator::opt::OptimizerPipeline::new();
    pre.add(strider_orchestrator::opt::PhiCollapse);
    pre.add(strider_orchestrator::opt::RegionCollapse);
    pre.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("phi collapse");

    strider_orchestrator::opt::run_post(
        &FunctionArgDetect,
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("FunctionArgDetect");
    function
}

/// Register arg 0 is registered in the side-table as an `InitialVar`.
#[test]
fn function_arg_reg_registered_in_side_table() {
    let (g, reg) = shapes::function_arg_reg();
    let carriers = g.side_tables().arg_index_to_values(0);
    assert!(
        !carriers.is_empty(),
        "arg 0 must be registered in the side-table"
    );
    assert_eq!(carriers.len(), 1, "register arg has exactly one carrier");
    assert!(
        matches!(g.node_kind(g.producer(carriers[0])), NodeKind::InitialVar(v) if g.initial_vn(*v) == reg),
        "carrier for register arg 0 must be InitialVar(reg)"
    );
}

#[test]
fn function_arg_reg_carrier_matches_initial_var_for() {
    let (g, reg) = shapes::function_arg_reg();
    a::matches(&g, initial_var_for(reg).into_pattern(), 1);
}

/// Register arg carrier is NOT a different register's InitialVar.
#[test]
fn function_arg_reg_wrong_vn_rejects() {
    let (g, _reg) = shapes::function_arg_reg();
    let other = reg_vn(0x40, 8);
    a::none(&g, initial_var_for(other).into_pattern());
}

/// Stack arg 0 is registered in the side-table as a `Load` node.
#[test]
fn function_arg_stack_registered_in_side_table() {
    let function = graph_fn_arg_stack();
    let carriers = function.side_tables().arg_index_to_values(0);
    assert!(
        !carriers.is_empty(),
        "arg 0 (stack) must be registered in the side-table"
    );
    assert!(
        carriers
            .iter()
            .all(|&v| matches!(function.node_kind(function.producer(v)), NodeKind::Load(_))),
        "all carriers for stack arg 0 must be Load nodes"
    );
}

#[test]
fn function_arg_stack_wrong_offset_absent() {
    let function = graph_fn_arg_stack();
    // Index 1 corresponds to offset 8 in the convention, not present here.
    let carriers_1 = function.side_tables().arg_index_to_values(1);
    assert!(
        carriers_1.is_empty(),
        "arg 1 (offset 8) must not be registered when only offset 4 is present"
    );
}

#[test]
fn function_arg_reg_and_stack_carry_different_kinds() {
    let (g_reg, _reg) = shapes::function_arg_reg();
    let g_stack = graph_fn_arg_stack();

    let reg_carriers = g_reg.side_tables().arg_index_to_values(0);
    assert!(!reg_carriers.is_empty());
    assert!(matches!(
        g_reg.node_kind(g_reg.producer(reg_carriers[0])),
        NodeKind::InitialVar(_)
    ));

    let stack_carriers = g_stack.side_tables().arg_index_to_values(0);
    assert!(!stack_carriers.is_empty());
    assert!(matches!(
        g_stack.node_kind(g_stack.producer(stack_carriers[0])),
        NodeKind::Load(_)
    ));
}

#[test]
fn arg_index_to_values_returns_carriers_for_registered_index() {
    let (g, _reg) = shapes::function_arg_reg();
    assert!(
        !g.side_tables().arg_index_to_values(0).is_empty(),
        "arg 0 must be registered"
    );
}

#[test]
fn arg_index_to_values_empty_for_unregistered() {
    let (g, _reg) = shapes::function_arg_reg();
    assert!(
        g.side_tables().arg_index_to_values(99).is_empty(),
        "arg 99 must not be registered"
    );
}

/// `iter_arg_indices()` returns exactly the set of registered indices.
#[test]
fn arg_indices_iterator_sorted() {
    let (g, _reg) = shapes::function_arg_reg();
    let indices: Vec<u32> = {
        let mut v: Vec<u32> = g.side_tables().iter_arg_indices().collect();
        v.sort();
        v
    };
    assert_eq!(indices, vec![0], "only arg 0 should be registered");
}

/// `JoinConstraint::PhiInputFromEdge` ties a phi's per-branch data input to
/// the control edge leading into that predecessor. On the collapsed diamond
/// `if (reg==0){reg=1}else{reg=2}`, the true edge selects one merged constant
/// and the false edge the other.
#[test]
fn phi_input_from_edge_ties_value_to_its_branch() {
    let (mut function, _reg) = graph_phi_for_reg();
    // Collapse the single-predecessor arms so the If's true/false outputs
    // become the merge region's direct predecessors, the shape the
    // direct-edge constraint keys on.
    let mut pre = strider_orchestrator::opt::OptimizerPipeline::new();
    pre.add(strider_orchestrator::opt::PhiCollapse);
    pre.add(strider_orchestrator::opt::RegionCollapse);
    pre.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("collapse");

    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_else().capture_true(t).capture_false(f).build();
    let phi_p = phi().capture(ph).build();
    let val = int_const(v).into_pattern();

    let read = |edge: Capture| -> u128 {
        let hits = m
            .find_joined_constrained(
                &[&guard, &phi_p, &val],
                &[JoinConstraint::PhiInputFromEdge {
                    phi: ph,
                    edge,
                    value: v,
                }],
            )
            .unwrap();
        assert_eq!(hits.len(), 1, "exactly one branch value per edge");
        let value = hits[0].iter().find_map(|mm| mm.value(v)).unwrap();
        function.int_const_u128(value).unwrap()
    };

    let (true_val, false_val) = (read(t), read(f));
    assert_ne!(
        true_val, false_val,
        "each edge selects its own branch value"
    );
    assert_eq!(
        [true_val, false_val]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [1u128, 2].into_iter().collect()
    );
}

/// `if (reg == 7) { do { reg = reg + 1 } while (reg != 0) }`: a guarded loop.
///
/// The guarded block's header has two control predecessors: the guard's true
/// edge and the loop's own back-edge (the latch). At the exit, a phi merges
/// the untouched `reg` (arriving on the guard's false edge) with the loop's
/// `reg + 1` (arriving on the loop-exit edge).
///
/// Every path that reaches the loop-exit edge went through the guard's true
/// edge, so the guard's true edge DOES dominate that arm. A sole-entry
/// dominance gate misses it: anchoring dominance at the edge's consumer (the
/// loop header) disables the clause because that header has two
/// predecessors, leaving only the direct `==` test, which fails.
fn graph_guarded_loop() -> (strider_ir::Function, rsleigh::Vn) {
    let reg = reg_vn(0, 8);
    let mut t = Tb::bare(vec![reg], &[], &[reg], &[], None, 0);
    let entry = t.region();
    let head = t.region();
    let exit = t.region();
    t.set_entry(entry);

    t.enter(entry);
    let reg_v = t.read_var(&reg);
    // The guard compares against 7, the latch against 0, so the two `If`s
    // are tellable apart by `cond` and a pattern can pin the OUTER guard
    // rather than accidentally matching the latch.
    let seven = t.u64(7);
    let cmp = t.int_cmp(reg_v, seven, IntCmpOp::Equal);
    t.build_if(cmp, head, exit);

    // The loop header's predecessors are the guard's true edge and its own latch.
    t.enter(head);
    let cur = t.read_var(&reg);
    let one = t.u64(1);
    let next = t.add(cur, one);
    t.write_var(&reg, next);
    let zero = t.u64(0);
    let done = t.int_cmp(next, zero, IntCmpOp::Equal);
    t.build_if(done, exit, head); // latch: back to `head`

    t.enter(exit);
    let out = t.read_var(&reg);
    (t.ret_val(out), reg)
}

/// The guarded-loop false negative. The exit phi's loop-side arm is reached
/// only through the guard's true edge, so `phi_input_from_edge` must find it.
///
/// A sole-entry dominance gate returns nothing here: the loop header's second
/// predecessor (its own latch) disables the dominance clause, and the direct
/// `==` clause can't see an arm merged across the loop body. Edge dominance
/// holds regardless: a latch does not make the guard optional.
#[test]
fn phi_input_from_edge_reaches_into_a_guarded_loop() {
    let (function, _reg) = graph_guarded_loop();
    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    // Pin the OUTER guard via its condition (`== 7`), so the latch `If`
    // (`== 0`) cannot stand in for it.
    let guard = if_else()
        .cond(int_cmp(IntCmpOp::Equal, anything(), int_const(7u64)))
        .capture_true(t)
        .capture_false(f)
        .build();
    let phi_p = phi()
        .any_input(int_add(anything(), any_int_const()).capture(v))
        .capture(ph)
        .build();

    // The loop-carried value `reg + 1` arrives at the exit phi from inside
    // the guarded loop, i.e. via the guard's TRUE edge.
    let hits = |edge: Capture| -> usize {
        m.find_joined_constrained(
            &[&guard, &phi_p],
            &[JoinConstraint::PhiInputFromEdge {
                phi: ph,
                edge,
                value: v,
            }],
        )
        .unwrap()
        .len()
    };

    assert!(
        hits(t) > 0,
        "the exit phi's loop-side arm (`reg + 1`) is reached ONLY through the \
         guard's true edge, so phi_input_from_edge must find it: the loop \
         header having a second predecessor (its own latch) does not make the \
         guard optional"
    );
    assert_eq!(
        hits(f),
        0,
        "the guard's FALSE edge skips the loop entirely, so it never merges the \
         loop-carried `reg + 1`"
    );
}

/// `if (reg == 0) { *p = 1 } else { *p = 2 }`: after merge a `MemPhi` merges
/// the two branch stores' memory tokens.
fn graph_memphi_diamond() -> strider_ir::Function {
    let reg = reg_vn(0, 8);
    let mut t = Tb::bare(vec![reg], &[], &[reg], &[], None, 0);
    let entry = t.region();
    let a_r = t.region();
    let b_r = t.region();
    let merge = t.region();
    t.set_entry(entry);

    t.enter(entry);
    let reg_v = t.read_var(&reg);
    let zero = t.u64(0);
    let cond = t.int_cmp(reg_v, zero, IntCmpOp::Equal);
    t.build_if(cond, a_r, b_r);

    t.enter(a_r);
    let addr_a = t.u64(0x40);
    let one = t.u64(1);
    t.store_ram(addr_a, one);
    t.branch(merge);

    t.enter(b_r);
    let addr_b = t.u64(0x40);
    let two = t.u64(2);
    t.store_ram(addr_b, two);
    t.branch(merge);

    t.enter(merge);
    let merged = t.read_var(&reg);
    t.ret_val(merged)
}

/// The `MemPhi` sibling of the value-phi test: `PhiInputFromEdge` ties a
/// `MemPhi`'s per-branch memory input to the control edge. Here `value` binds
/// a memory token (the branch's `Store` output), proving the constraint works
/// for a memory-typed value, not just value phis.
#[test]
fn phi_input_from_edge_ties_memphi_memory_to_its_branch() {
    let mut function = graph_memphi_diamond();
    let mut pre = strider_orchestrator::opt::OptimizerPipeline::new();
    pre.add(strider_orchestrator::opt::PhiCollapse);
    pre.add(strider_orchestrator::opt::RegionCollapse);
    pre.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("collapse");

    let m = Matcher::new(&function);
    let (t, f, mp, sv, dv) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_else().capture_true(t).capture_false(f).build();
    // `mem_phi().capture(mp)` binds `mp` to the MemPhi's memory output; the
    // store's `capture(sv)` binds `sv` to its memory output, the token the
    // MemPhi merges on that predecessor. `dv` reads back which branch.
    let mphi = mem_phi().capture(mp).build();
    let st = store().data(int_const(dv)).capture(sv).build();

    let read = |edge: Capture| -> u128 {
        let hits = m
            .find_joined_constrained(
                &[&guard, &mphi, &st],
                &[JoinConstraint::PhiInputFromEdge {
                    phi: mp,
                    edge,
                    value: sv,
                }],
            )
            .unwrap();
        assert_eq!(hits.len(), 1, "exactly one branch store per edge");
        let data = hits[0].iter().find_map(|mm| mm.value(dv)).unwrap();
        function.int_const_u128(data).unwrap()
    };

    let (true_val, false_val) = (read(t), read(f));
    assert_ne!(
        true_val, false_val,
        "each edge selects its own branch store"
    );
    assert_eq!(
        [true_val, false_val]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [1u128, 2].into_iter().collect()
    );
}

/// The collapsed `if (reg==0){reg=1}else{reg=2}` diamond, shared by the
/// `any_input` tests below.
fn collapsed_phi_diamond() -> strider_ir::Function {
    let (mut function, _reg) = graph_phi_for_reg();
    let mut pre = strider_orchestrator::opt::OptimizerPipeline::new();
    pre.add(strider_orchestrator::opt::PhiCollapse);
    pre.add(strider_orchestrator::opt::RegionCollapse);
    pre.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("collapse");
    function
}

/// An `any_input`-bound arm value selects the same arm the free-floating
/// two-root capture spelling does, without the value ranging over the function.
#[test]
fn phi_input_from_edge_any_input_matches_same_arm() {
    let function = collapsed_phi_diamond();
    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_else().capture_true(t).capture_false(f).build();

    let hits_for = |edge: Capture, k: u64| -> usize {
        let phi_p = phi().any_input(int_const(k).capture(v)).capture(ph).build();
        m.find_joined_constrained(
            &[&guard, &phi_p],
            &[JoinConstraint::PhiInputFromEdge {
                phi: ph,
                edge,
                value: v,
            }],
        )
        .unwrap()
        .len()
    };

    // Each edge merges exactly one of {1, 2}, and the two edges disagree.
    assert_eq!(
        hits_for(t, 1) + hits_for(t, 2),
        1,
        "true edge picks one const"
    );
    assert_eq!(
        hits_for(f, 1) + hits_for(f, 2),
        1,
        "false edge picks one const"
    );
    assert_ne!(
        hits_for(t, 1),
        hits_for(f, 1),
        "the two edges select different constants"
    );
}

#[test]
fn phi_input_from_edge_any_input_capture_is_readable() {
    let function = collapsed_phi_diamond();
    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_else().capture_true(t).capture_false(f).build();
    let phi_p = phi().any_input(int_const(v)).capture(ph).build();

    let read = |edge: Capture| -> u128 {
        let hits = m
            .find_joined_constrained(
                &[&guard, &phi_p],
                &[JoinConstraint::PhiInputFromEdge {
                    phi: ph,
                    edge,
                    value: v,
                }],
            )
            .unwrap();
        assert_eq!(hits.len(), 1, "exactly one branch value per edge");
        let value = hits[0]
            .iter()
            .find_map(|mm| mm.value(v))
            .expect("the any_input capture must be readable from the Match");
        function.int_const_u128(value).unwrap()
    };

    let (true_val, false_val) = (read(t), read(f));
    assert_ne!(true_val, false_val);
    assert_eq!(
        [true_val, false_val]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [1u128, 2].into_iter().collect()
    );
}

/// Three-valued (Kleene) evaluation: a `Not` over a constraint whose capture
/// is unbound in a row must drop that row, not vacuously keep it.
///
/// `ph` is captured only in the phi arm of a `one_of`, so it's absent in the
/// bare-const rows. `dominates(ph, ph)` is always true where `ph` is bound, so
/// `negate(dominates(ph, ph))` is `Some(false)` there. In the `ph`-absent rows
/// the relation is unanswerable (`None`), and `Not(None) == None`, so under
/// Kleene every row drops. Two-valued evaluation reads the absent capture as
/// `false`, flips it to a vacuous `true`, and keeps exactly those rows.
#[test]
fn negate_over_an_unbound_capture_drops_every_row() {
    let function = collapsed_phi_diamond();
    let m = Matcher::new(&function);
    let (ph, v) = (Capture::new(), Capture::new());
    // `ph` binds only in the first arm; the bare-const second arm leaves it
    // absent, so the pattern matches the phi (ph bound) AND every const (ph not).
    let operand = one_of![phi().any_input(int_const(v)).capture(ph), int_const(v),].into_pattern();

    let unconstrained = m.find_joined_constrained(&[&operand], &[]).unwrap().len();
    assert!(
        unconstrained > 0,
        "some rows exist, including bare-const rows where `ph` is unbound"
    );

    // `dominates(ph, ph)` is self-domination: always true where `ph` is bound.
    let negated = m
        .find_joined_constrained(
            &[&operand],
            &[JoinConstraint::Not(Box::new(JoinConstraint::Dominates {
                dominator: ph,
                dominated: ph,
            }))],
        )
        .unwrap()
        .len();
    assert_eq!(
        negated, 0,
        "negate(dominates(ph, ph)) drops the ph-bound rows (Some(false)) AND the \
         ph-unbound rows (None); two-valued evaluation keeps the latter vacuously"
    );
}

/// No match when the arm value is a different constant.
#[test]
fn phi_input_from_edge_any_input_negative() {
    let function = collapsed_phi_diamond();
    let m = Matcher::new(&function);
    let (t, ph, v) = (Capture::new(), Capture::new(), Capture::new());
    let guard = if_else().capture_true(t).build();
    // 0xDEAD is on neither arm.
    let phi_p = phi()
        .any_input(int_const(0xDEADu64).capture(v))
        .capture(ph)
        .build();

    let hits = m
        .find_joined_constrained(
            &[&guard, &phi_p],
            &[JoinConstraint::PhiInputFromEdge {
                phi: ph,
                edge: t,
                value: v,
            }],
        )
        .unwrap();
    assert!(hits.is_empty(), "no arm merges 0xDEAD");
}

/// A capture bound BOTH by a free-floating root and by `any_input` on the phi
/// must agree: the join unifies the two bindings, it never overwrites.
#[test]
fn phi_input_from_edge_any_input_capture_unifies_with_tuple() {
    let function = collapsed_phi_diamond();
    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_else().capture_true(t).capture_false(f).build();
    let phi_p = phi().any_input(int_const(v)).capture(ph).build();
    // `v` is bound by a free-floating root too: the classic two-root spelling.
    let val_root = int_const(v).into_pattern();

    let count = |edge: Capture| -> usize {
        m.find_joined_constrained(
            &[&guard, &phi_p, &val_root],
            &[JoinConstraint::PhiInputFromEdge {
                phi: ph,
                edge,
                value: v,
            }],
        )
        .unwrap()
        .len()
    };

    // The `val_root` root ranges over every int const; the phi's `any_input`
    // also binds `v`. Unification must collapse this to the one arm value per
    // edge; if either binding overwrote the other, every const would survive.
    assert_eq!(count(t), 1, "unification pins `v` to the true arm's value");
    assert_eq!(count(f), 1, "unification pins `v` to the false arm's value");
}

/// `if (reg == 0) { f(); reg = 1 } else { f(); reg = 2 }`: the motivating
/// shape. A `Call` terminates its basic block, so each branch's edge leads
/// into a region that is NOT the merge region's predecessor: an intervening
/// region sits between the `If`'s true/false output and the join. This is
/// the shape behind fixtures where a direct-edge-only constraint sees nothing.
fn graph_phi_across_call() -> (strider_ir::Function, rsleigh::Vn) {
    let reg = reg_vn(0, 8);
    let mut t = Tb::bare(vec![reg], &[], &[reg], &[], None, 0);
    let entry = t.region();
    let (a_r, a_tail) = (t.region(), t.region());
    let (b_r, b_tail) = (t.region(), t.region());
    let merge = t.region();
    t.set_entry(entry);

    t.enter(entry);
    let reg_v = t.read_var(&reg);
    let zero = t.u64(0);
    let cond = t.int_cmp(reg_v, zero, IntCmpOp::Equal);
    t.build_if(cond, a_r, b_r);

    // True side: the call splits the block, so `a_tail`, not `a_r`, is the
    // merge's predecessor, and the If's true edge feeds `a_r`.
    t.enter(a_r);
    t.call_at(0x1000);
    t.branch(a_tail);
    t.enter(a_tail);
    let one = t.u64(1);
    t.write_var(&reg, one);
    t.branch(merge);

    t.enter(b_r);
    t.call_at(0x1000);
    t.branch(b_tail);
    t.enter(b_tail);
    let two = t.u64(2);
    t.write_var(&reg, two);
    t.branch(merge);

    t.enter(merge);
    let merged = t.read_var(&reg);
    (t.ret_val(merged), reg)
}

/// The motivating test: the phi's arms merge across a call, so neither branch
/// edge is a literal control input of the join region. Direct-edge-only
/// matching returns nothing here; reaching through the intervening control
/// must still pin each arm to its branch.
#[test]
fn phi_input_from_edge_reaches_through_intervening_call() {
    let (function, _reg) = graph_phi_across_call();
    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_else().capture_true(t).capture_false(f).build();
    // Pin the MERGE phi (the one the Return consumes): the builder mints a
    // phi per region, and `phi()` alone would also match the branch
    // regions' own single-predecessor phis, whose direct predecessor IS
    // the branch edge.
    let phi_p = ret().ret_val(0, phi().capture(ph)).build();
    let val = int_const(v).into_pattern();

    let read = |edge: Capture| -> u128 {
        let hits = m
            .find_joined_constrained(
                &[&guard, &phi_p, &val],
                &[JoinConstraint::PhiInputFromEdge {
                    phi: ph,
                    edge,
                    value: v,
                }],
            )
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "the arm reached through the call must still be pinned to its edge"
        );
        let value = hits[0].iter().find_map(|mm| mm.value(v)).unwrap();
        function.int_const_u128(value).unwrap()
    };

    let (true_val, false_val) = (read(t), read(f));
    assert_ne!(
        true_val, false_val,
        "each edge selects its own branch value"
    );
    assert_eq!(
        [true_val, false_val]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [1u128, 2].into_iter().collect()
    );
}

/// The `any_input` spelling works through intervening control too, and its
/// capture still binds.
#[test]
fn phi_input_from_edge_any_input_reaches_through_call() {
    let (function, _reg) = graph_phi_across_call();
    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_else().capture_true(t).capture_false(f).build();
    // Pin the MERGE phi (see phi_input_from_edge_reaches_through_intervening_call).
    let phi_p = ret()
        .ret_val(0, phi().any_input(int_const(v)).capture(ph))
        .build();

    let read = |edge: Capture| -> u128 {
        let hits = m
            .find_joined_constrained(
                &[&guard, &phi_p],
                &[JoinConstraint::PhiInputFromEdge {
                    phi: ph,
                    edge,
                    value: v,
                }],
            )
            .unwrap();
        assert_eq!(hits.len(), 1, "exactly one arm per edge, through the call");
        let value = hits[0]
            .iter()
            .find_map(|mm| mm.value(v))
            .expect("the any_input capture must bind through intervening control");
        function.int_const_u128(value).unwrap()
    };

    let (true_val, false_val) = (read(t), read(f));
    assert_ne!(true_val, false_val);
    assert_eq!(
        [true_val, false_val]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [1u128, 2].into_iter().collect()
    );
}

/// Two stacked diamonds: `if (c0) {..} else {..}` merges at `m1`, then
/// `if (c1) {reg=1} else {reg=2}` merges at `m2`. The phi at `m2` has arms
/// whose predecessors are reachable from BOTH of the outer if's edges, so
/// neither outer edge dominates them. Reach is exclusive: neither outer edge
/// may pin an arm of the `m2` phi.
fn graph_stacked_diamonds() -> (strider_ir::Function, rsleigh::Vn) {
    let reg = reg_vn(0, 8);
    let flag = reg_vn(0x40, 8);
    let mut t = Tb::bare(vec![reg, flag], &[], &[reg, flag], &[], None, 0);
    let entry = t.region();
    let (a_r, b_r, m1) = (t.region(), t.region(), t.region());
    let (c_r, d_r, m2) = (t.region(), t.region(), t.region());
    t.set_entry(entry);

    t.enter(entry);
    let reg_v = t.read_var(&reg);
    let zero = t.u64(0);
    let c0 = t.int_cmp(reg_v, zero, IntCmpOp::Equal);
    t.build_if(c0, a_r, b_r);

    t.enter(a_r);
    let ten = t.u64(10);
    t.write_var(&flag, ten);
    t.branch(m1);
    t.enter(b_r);
    let twenty = t.u64(20);
    t.write_var(&flag, twenty);
    t.branch(m1);

    // Inner diamond, reachable from both outer branches.
    t.enter(m1);
    let flag_v = t.read_var(&flag);
    let fifteen = t.u64(15);
    // `Less`, so a pattern can pin the OUTER (`Equal`) if unambiguously.
    let c1 = t.int_cmp(flag_v, fifteen, IntCmpOp::Less);
    t.build_if(c1, c_r, d_r);

    t.enter(c_r);
    let one = t.u64(1);
    t.write_var(&reg, one);
    t.branch(m2);
    t.enter(d_r);
    let two = t.u64(2);
    t.write_var(&reg, two);
    t.branch(m2);

    t.enter(m2);
    let merged = t.read_var(&reg);
    (t.ret_val(merged), reg)
}

/// The exclusivity negative: an arm reachable from BOTH branch edges is
/// pinned to neither. Dominance means "every path goes through it"; a merged
/// arm has paths through both edges, so it belongs to neither.
#[test]
fn phi_input_from_edge_rejects_arm_reachable_from_both_branches() {
    let (function, reg) = graph_stacked_diamonds();
    let m = Matcher::new(&function);
    let (c0_t, c0_f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    // Pin the guard to the OUTER if by its condition operand (a read of `reg`).
    let outer = if_else()
        .cond(int_cmp(IntCmpOp::Equal, anything(), anything()))
        .capture_true(c0_t)
        .capture_false(c0_f)
        .build();
    // The phi of `reg` at m2, the one merging 1 and 2.
    let phi_p = ret().ret_val(0, phi_for(reg).capture(ph)).build();
    let val = int_const(v).into_pattern();

    // Guard against a vacuous pass: an unmatched probe would also give {}.
    // The outer `If` and the merge phi must both really be there.
    assert_eq!(m.find_all(&outer).unwrap().len(), 1, "outer if must match");
    assert_eq!(m.find_all(&phi_p).unwrap().len(), 1, "merge phi must match");

    for edge in [c0_t, c0_f] {
        let hits = m
            .find_joined_constrained(
                &[&outer, &phi_p, &val],
                &[JoinConstraint::PhiInputFromEdge {
                    phi: ph,
                    edge,
                    value: v,
                }],
            )
            .unwrap();
        assert!(
            hits.is_empty(),
            "an arm reachable from both outer branches belongs to neither edge"
        );
    }
}

/// The wildcard probe tells the two empty results apart. An empty result from `PhiInputFromEdge`
/// is ambiguous: either the edge reaches no arm of this phi, or it does and
/// the arm merges a different value. A wildcard `value` cannot fail on value
/// grounds, so an empty result from it proves the edge
/// is not visible.
#[test]
fn phi_input_from_edge_wildcard_probe_discriminates_blind_from_mismatch() {
    // Visible: the across-a-call diamond, a wildcard hits on both edges.
    let (function, _reg) = graph_phi_across_call();
    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_else().capture_true(t).capture_false(f).build();
    let probe = ret()
        .ret_val(0, phi().any_input(anything().capture(v)).capture(ph))
        .build();
    for edge in [t, f] {
        let hits = m
            .find_joined_constrained(
                &[&guard, &probe],
                &[JoinConstraint::PhiInputFromEdge {
                    phi: ph,
                    edge,
                    value: v,
                }],
            )
            .unwrap();
        assert!(
            !hits.is_empty(),
            "a wildcard cannot fail on value grounds: the edge IS visible"
        );
    }

    // ...yet a value on no arm still gives {}, a real mismatch, which the
    // wildcard probe above distinguishes from blindness.
    let dead = ret()
        .ret_val(
            0,
            phi().any_input(int_const(0xDEADu64).capture(v)).capture(ph),
        )
        .build();
    let mismatch = m
        .find_joined_constrained(
            &[&guard, &dead],
            &[JoinConstraint::PhiInputFromEdge {
                phi: ph,
                edge: t,
                value: v,
            }],
        )
        .unwrap();
    assert!(mismatch.is_empty(), "no arm merges 0xDEAD");

    // Invisible: the OUTER if's edges reach no arm of the inner phi, so
    // even a wildcard is empty, which is what blindness looks like.
    let (function2, reg2) = graph_stacked_diamonds();
    let m2 = Matcher::new(&function2);
    let (ot, of, ph2, v2) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let outer = if_else()
        .cond(int_cmp(IntCmpOp::Equal, anything(), anything()))
        .capture_true(ot)
        .capture_false(of)
        .build();
    let phi2 = ret()
        .ret_val(
            0,
            phi_for(reg2).any_input(anything().capture(v2)).capture(ph2),
        )
        .build();
    // The probe itself must match, otherwise {} would be vacuous. The phi
    // probe matches once per arm (its `any_input` binds `v2` to each in
    // turn, the `find_all` enumeration contract), so assert non-vacuity
    // rather than an arity-coupled count.
    assert_eq!(m2.find_all(&outer).unwrap().len(), 1, "outer if must match");
    assert!(
        !m2.find_all(&phi2).unwrap().is_empty(),
        "inner merge phi must match"
    );
    for edge in [ot, of] {
        let hits = m2
            .find_joined_constrained(
                &[&outer, &phi2],
                &[JoinConstraint::PhiInputFromEdge {
                    phi: ph2,
                    edge,
                    value: v2,
                }],
            )
            .unwrap();
        assert!(
            hits.is_empty(),
            "wildcard empty-set proves the edge is not visible: the blind case"
        );
    }
}

/// `if (c0) { if (c1) {reg=1} else {reg=2} } else { reg=3 }`: the true
/// branch's block splits and reaches the merge twice, so two arms qualify
/// for the true edge. Enumerate one binding per qualifying arm; never
/// silently pick one.
fn graph_split_branch() -> (strider_ir::Function, rsleigh::Vn) {
    let reg = reg_vn(0, 8);
    let flag = reg_vn(0x40, 8);
    let mut t = Tb::bare(vec![reg, flag], &[], &[reg, flag], &[], None, 0);
    let entry = t.region();
    let (a_r, b_r, merge) = (t.region(), t.region(), t.region());
    let (a1, a2) = (t.region(), t.region());
    t.set_entry(entry);

    t.enter(entry);
    let reg_v = t.read_var(&reg);
    let zero = t.u64(0);
    let c0 = t.int_cmp(reg_v, zero, IntCmpOp::Equal);
    t.build_if(c0, a_r, b_r);

    // The true block splits in two, and BOTH halves reach the merge.
    t.enter(a_r);
    let flag_v = t.read_var(&flag);
    // `Less`, so a pattern can pin the OUTER (`Equal`) if unambiguously.
    let c1 = t.int_cmp(flag_v, zero, IntCmpOp::Less);
    t.build_if(c1, a1, a2);
    t.enter(a1);
    let one = t.u64(1);
    t.write_var(&reg, one);
    t.branch(merge);
    t.enter(a2);
    let two = t.u64(2);
    t.write_var(&reg, two);
    t.branch(merge);

    t.enter(b_r);
    let three = t.u64(3);
    t.write_var(&reg, three);
    t.branch(merge);

    t.enter(merge);
    let merged = t.read_var(&reg);
    (t.ret_val(merged), reg)
}

/// A split branch reaching the merge twice yields one binding per qualifying
/// arm, the `find_all` enumeration contract, not an arbitrary pick.
#[test]
fn phi_input_from_edge_enumerates_every_qualifying_arm() {
    let (function, reg) = graph_split_branch();
    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let outer = if_else()
        .cond(int_cmp(IntCmpOp::Equal, anything(), anything()))
        .capture_true(t)
        .capture_false(f)
        .build();
    let phi_p = ret().ret_val(0, phi_for(reg).capture(ph)).build();
    let val = int_const(v).into_pattern();

    let vals = |edge: Capture| -> std::collections::BTreeSet<u128> {
        m.find_joined_constrained(
            &[&outer, &phi_p, &val],
            &[JoinConstraint::PhiInputFromEdge {
                phi: ph,
                edge,
                value: v,
            }],
        )
        .unwrap()
        .iter()
        .map(|h| {
            let value = h.iter().find_map(|mm| mm.value(v)).unwrap();
            function.int_const_u128(value).unwrap()
        })
        .collect()
    };

    // The true edge's block splits: BOTH 1 and 2 are exclusively reached
    // through it, so both bind, one tuple each.
    assert_eq!(
        vals(t),
        [1u128, 2].into_iter().collect(),
        "both split-half arms qualify for the true edge"
    );
    // The false edge stays single.
    assert_eq!(vals(f), [3u128].into_iter().collect());
}

/// `if (c0) {} else { flag = 20 }`: the empty-arm shape. The true edge's
/// consumer IS the join `m1`, so a later phi's arms are dominated by `m1`
/// while `m1` is reachable from both edges. Attributing those arms to the
/// true edge would be a false positive: reach must stay exclusive, so the
/// dominance clause only applies where the edge is its target's sole entry.
#[test]
fn phi_input_from_edge_rejects_empty_branch_criss_cross() {
    let reg = reg_vn(0, 8);
    let flag = reg_vn(0x40, 8);
    let mut t = Tb::bare(vec![reg, flag], &[], &[reg, flag], &[], None, 0);
    let entry = t.region();
    let (b_r, m1) = (t.region(), t.region());
    let (c_r, d_r, m2) = (t.region(), t.region(), t.region());
    t.set_entry(entry);

    t.enter(entry);
    let reg_v = t.read_var(&reg);
    let zero = t.u64(0);
    let c0 = t.int_cmp(reg_v, zero, IntCmpOp::Equal);
    // TRUE goes straight to the join: the then-arm is empty.
    t.build_if(c0, m1, b_r);
    t.enter(b_r);
    let twenty = t.u64(20);
    t.write_var(&flag, twenty);
    t.branch(m1);

    t.enter(m1);
    let flag_v = t.read_var(&flag);
    let c1 = t.int_cmp(flag_v, zero, IntCmpOp::Less);
    t.build_if(c1, c_r, d_r);
    t.enter(c_r);
    let one = t.u64(1);
    t.write_var(&reg, one);
    t.branch(m2);
    t.enter(d_r);
    let two = t.u64(2);
    t.write_var(&reg, two);
    t.branch(m2);
    t.enter(m2);
    let merged = t.read_var(&reg);
    let function = t.ret_val(merged);

    let m = Matcher::new(&function);
    let (c0_t, ph, v) = (Capture::new(), Capture::new(), Capture::new());
    let outer = if_else()
        .cond(int_cmp(IntCmpOp::Equal, anything(), anything()))
        .capture_true(c0_t)
        .build();
    let phi_p = ret().ret_val(0, phi_for(reg).capture(ph)).build();
    let val = int_const(v).into_pattern();
    assert_eq!(m.find_all(&outer).unwrap().len(), 1, "outer if must match");
    assert_eq!(m.find_all(&phi_p).unwrap().len(), 1, "merge phi must match");

    let hits = m
        .find_joined_constrained(
            &[&outer, &phi_p, &val],
            &[JoinConstraint::PhiInputFromEdge {
                phi: ph,
                edge: c0_t,
                value: v,
            }],
        )
        .unwrap();
    assert!(
        hits.is_empty(),
        "arms below a join reachable from both edges belong to neither: the \
         edge's target is not entered exclusively through the edge"
    );
}
