//! SSA-shaped patterns: `Phi` (formerly VarPhi), `InitialVar`,
//! `FunctionArg` side-table.
//!
//! Covers: `phi()` / `phi_for(vn)`, `initial_var()` / `initial_var_for(vn)`,
//! and the `Function::arg_index_to_values` side-table populated by
//! `FunctionArgDetect`.

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{IRViewer, IntCmpOp};
use strider_pattern::*;

use super::support::{Tb, assertions as a, reg_vn, shapes, stack_vn};

// ── InitialVar ───────────────────────────────────────────────────────────────

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
    let other = reg_vn(0x40, 8); // Different varnode.
    a::none(&g, initial_var_for(other).into_pattern());
}

/// `InitialVar` carries a per-function `all_vns` index, not the varnode.
/// With two tracked regs, `hi` sorts to `all_vns[1]` (not `[0]`), so a
/// correct `initial_var_for` must resolve each candidate node's index back
/// to its varnode and match by identity — never positionally.
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

// ── Phi (formerly VarPhi) ────────────────────────────────────────────────────

/// `if (reg == 0) { reg = 1 } else { reg = 2 }` — after merge, a phi
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
    // At least one phi exists at the merge region.
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

// ── FunctionArg side-table ───────────────────────────────────────────────────
//
// After `FunctionArgDetect`, the underlying `InitialVar` / `Load` nodes
// survive unchanged and are recorded in `Function::arg_index_to_values`.
// Tests below verify the side-table contents and that the carrier nodes
// are the expected kinds.

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

    // `read *(sp + 4)` — the first stack arg in cdecl-style.
    let sp_v = t.read_var(&sp);
    let four = t.u64(4);
    let addr = t.add(sp_v, four);
    let v = t.load_ram(addr, ValueType::I64);
    let mut function = t.ret_val(v);

    // Collapse the single-predecessor `read_var(sp)` phi so the stack-arg
    // load is a bare `InitialVar(sp) + 4` terminal (production shape after
    // PhiCollapse) before the SP-aware post-pass.
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

/// Register arg carrier is also matchable via `initial_var_for(vn)`.
#[test]
fn function_arg_reg_carrier_matches_initial_var_for() {
    let (g, reg) = shapes::function_arg_reg();
    // The carrier is an InitialVar; it must be findable by the pattern matcher.
    a::matches(&g, initial_var_for(reg).into_pattern(), 1);
}

/// Register arg carrier is NOT a different register's InitialVar.
#[test]
fn function_arg_reg_wrong_vn_rejects() {
    let (g, _reg) = shapes::function_arg_reg();
    let other = reg_vn(0x40, 8); // Different varnode.
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

/// Stack arg at wrong offset is not registered.
#[test]
fn function_arg_stack_wrong_offset_absent() {
    let function = graph_fn_arg_stack();
    // Index 1 corresponds to offset 8 in the convention — not present.
    let carriers_1 = function.side_tables().arg_index_to_values(1);
    assert!(
        carriers_1.is_empty(),
        "arg 1 (offset 8) must not be registered when only offset 4 is present"
    );
}

/// Register arg carrier is not the same kind as a stack arg carrier.
#[test]
fn function_arg_reg_and_stack_carry_different_kinds() {
    let (g_reg, _reg) = shapes::function_arg_reg();
    let g_stack = graph_fn_arg_stack();

    // Register graph: carrier is InitialVar.
    let reg_carriers = g_reg.side_tables().arg_index_to_values(0);
    assert!(!reg_carriers.is_empty());
    assert!(matches!(
        g_reg.node_kind(g_reg.producer(reg_carriers[0])),
        NodeKind::InitialVar(_)
    ));

    // Stack graph: carrier is Load.
    let stack_carriers = g_stack.side_tables().arg_index_to_values(0);
    assert!(!stack_carriers.is_empty());
    assert!(matches!(
        g_stack.node_kind(g_stack.producer(stack_carriers[0])),
        NodeKind::Load(_)
    ));
}

// ── iter_arg_indices / arg_index_to_values API ─────────────────────────────────────

/// `arg_index_to_values(i)` for a registered index returns a non-empty slice.
#[test]
fn arg_index_to_values_returns_carriers_for_registered_index() {
    let (g, _reg) = shapes::function_arg_reg();
    assert!(
        !g.side_tables().arg_index_to_values(0).is_empty(),
        "arg 0 must be registered"
    );
}

/// `arg_index_to_values(i)` returns empty for an unregistered index.
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

/// `JoinConstraint::PhiInputFromEdge` ties a phi's per-branch data input to the
/// control edge that leads into that predecessor.  On the collapsed diamond
/// `if (reg==0){reg=1}else{reg=2}`, the true edge selects one merged constant
/// and the false edge the other.
#[test]
fn phi_input_from_edge_ties_value_to_its_branch() {
    let (mut function, _reg) = graph_phi_for_reg();
    // Collapse the single-predecessor arms so the If's true/false outputs
    // become the merge region's DIRECT predecessors (the converged shape the
    // direct-edge constraint keys on).
    let mut pre = strider_orchestrator::opt::OptimizerPipeline::new();
    pre.add(strider_orchestrator::opt::PhiCollapse);
    pre.add(strider_orchestrator::opt::RegionCollapse);
    pre.run(&mut function, &mut strider_orchestrator::opt::OptCtx::new(None))
        .expect("collapse");

    let m = Matcher::new(&function);
    let (t, f, ph, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_node().capture_true(t).capture_false(f).build();
    let phi_p = phi().capture(ph).build();
    let val = any_int_const().capture(v).into_pattern();

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
    assert_ne!(true_val, false_val, "each edge selects its own branch value");
    assert_eq!(
        [true_val, false_val]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [1u128, 2].into_iter().collect()
    );
}

/// `if (reg == 0) { *p = 1 } else { *p = 2 }` — after merge a `MemPhi` merges
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
/// `MemPhi`'s per-branch MEMORY input to the control edge.  Here `value` binds
/// a memory token (the branch's `Store` output), proving the constraint works
/// for the memory phi with a memory-typed value, not just value phis.
#[test]
fn phi_input_from_edge_ties_memphi_memory_to_its_branch() {
    let mut function = graph_memphi_diamond();
    let mut pre = strider_orchestrator::opt::OptimizerPipeline::new();
    pre.add(strider_orchestrator::opt::PhiCollapse);
    pre.add(strider_orchestrator::opt::RegionCollapse);
    pre.run(&mut function, &mut strider_orchestrator::opt::OptCtx::new(None))
        .expect("collapse");

    let m = Matcher::new(&function);
    let (t, f, mp, sv, dv) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let guard = if_node().capture_true(t).capture_false(f).build();
    // `mem_phi().capture(mp)` binds `mp` to the MemPhi's memory output; the
    // store's `capture(sv)` binds `sv` to its memory output — the very token
    // the MemPhi merges on that predecessor.  `dv` reads back which branch.
    let mphi = mem_phi().capture(mp).build();
    let st = store()
        .data(any_int_const().capture(dv))
        .capture(sv)
        .build();

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
    assert_ne!(true_val, false_val, "each edge selects its own branch store");
    assert_eq!(
        [true_val, false_val]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [1u128, 2].into_iter().collect()
    );
}
