//! SSA-shaped patterns: `Phi` (formerly VarPhi), `InitialVar`,
//! `FunctionArg` side-table.
//!
//! Covers: `phi()` / `phi_for(vn)`, `initial_var()` / `initial_var_for(vn)`,
//! and the `Function::arg_index_to_nodes` side-table populated by
//! `FunctionArgDetect`.

use strider_pattern::*;
use strider_ir::IntCmpOp;
use strider_ir::node::{NodeKind, ValueType};

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
    let hits = Matcher::try_new(&g).unwrap().find_all(&phi().build());
    assert!(!hits.is_empty(), "expected at least one phi");
}

#[test]
fn phi_for_matches_exact_vn() {
    let (g, reg) = graph_phi_for_reg();
    let hits = Matcher::try_new(&g).unwrap().find_all(&phi_for(reg).build());
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
// survive unchanged and are recorded in `Function::arg_index_to_nodes`.
// Tests below verify the side-table contents and that the carrier nodes
// are the expected kinds.

/// A graph with one stack-arg at sp-relative offset `4`, index `0`.
fn graph_fn_arg_stack() -> strider_ir::Function {
    use strider_analyze::opt::{FunctionArgDetect, Optimizer};
    let sp = stack_vn();
    let mut t = Tb::raw(vec![sp], &[], &[sp], &[], None, 0);

    // `read *(sp + 4)` — the first stack arg in cdecl-style.
    let sp_v = t.read_var(&sp);
    let four = t.u64(4);
    let addr = t.add(sp_v, four);
    let v = t.load_ram(addr, ValueType::I64);
    let mut function = t.ret_val(v);

    FunctionArgDetect::new(vec![], sp, vec![4])
        .optimize(&mut function, &strider_analyze::opt::OptCtx::empty())
        .expect("FunctionArgDetect");
    function
}

/// Register arg 0 is registered in the side-table as an `InitialVar`.
#[test]
fn function_arg_reg_registered_in_side_table() {
    let (g, reg) = shapes::function_arg_reg();
    let carriers = g.arg_index_to_nodes(0);
    assert!(!carriers.is_empty(), "arg 0 must be registered in the side-table");
    assert_eq!(carriers.len(), 1, "register arg has exactly one carrier");
    assert!(
        matches!(g.node_kind(carriers[0]), NodeKind::InitialVar(v) if *v == reg),
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
    let carriers = function.arg_index_to_nodes(0);
    assert!(!carriers.is_empty(), "arg 0 (stack) must be registered in the side-table");
    assert!(
        carriers.iter().all(|&n| matches!(function.node_kind(n), NodeKind::Load(_))),
        "all carriers for stack arg 0 must be Load nodes"
    );
}

/// Stack arg at wrong offset is not registered.
#[test]
fn function_arg_stack_wrong_offset_absent() {
    let function = graph_fn_arg_stack();
    // Index 1 corresponds to offset 8 in the convention — not present.
    let carriers_1 = function.arg_index_to_nodes(1);
    assert!(carriers_1.is_empty(), "arg 1 (offset 8) must not be registered when only offset 4 is present");
}

/// Register arg carrier is not the same kind as a stack arg carrier.
#[test]
fn function_arg_reg_and_stack_carry_different_kinds() {
    let (g_reg, _reg) = shapes::function_arg_reg();
    let g_stack = graph_fn_arg_stack();

    // Register graph: carrier is InitialVar.
    let reg_carriers = g_reg.arg_index_to_nodes(0);
    assert!(!reg_carriers.is_empty());
    assert!(matches!(g_reg.node_kind(reg_carriers[0]), NodeKind::InitialVar(_)));

    // Stack graph: carrier is Load.
    let stack_carriers = g_stack.arg_index_to_nodes(0);
    assert!(!stack_carriers.is_empty());
    assert!(matches!(g_stack.node_kind(stack_carriers[0]), NodeKind::Load(_)));
}

// ── iter_arg_indices / arg_index_to_nodes API ─────────────────────────────────────

/// `arg_index_to_nodes(i)` for a registered index returns a non-empty slice.
#[test]
fn arg_index_to_nodes_returns_carriers_for_registered_index() {
    let (g, _reg) = shapes::function_arg_reg();
    assert!(!g.arg_index_to_nodes(0).is_empty(), "arg 0 must be registered");
}

/// `arg_index_to_nodes(i)` returns empty for an unregistered index.
#[test]
fn arg_index_to_nodes_empty_for_unregistered() {
    let (g, _reg) = shapes::function_arg_reg();
    assert!(g.arg_index_to_nodes(99).is_empty(), "arg 99 must not be registered");
}

/// `iter_arg_indices()` returns exactly the set of registered indices.
#[test]
fn arg_indices_iterator_sorted() {
    let (g, _reg) = shapes::function_arg_reg();
    let indices: Vec<u32> = {
        let mut v: Vec<u32> = g.iter_arg_indices().collect();
        v.sort();
        v
    };
    assert_eq!(indices, vec![0], "only arg 0 should be registered");
}
