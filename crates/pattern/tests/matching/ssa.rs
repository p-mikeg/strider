//! SSA-shaped patterns: `VarPhi`, `InitialVar`, `FunctionArg`.
//!
//! Covers: `phi()` / `phi_for(vn)`, `initial_var()` / `initial_var_for(vn)`,
//! `function_arg(i)` / `_any()` / `_reg(vn)` / `_stack(space, off)`.

use ir::IntCmpOp;
use ir::node::{FunctionArgSource, NodeOutputType};
use pattern::*;

use super::support::{Tb, assertions as a, reg_vn, shapes, sp_vn};

// ── InitialVar ───────────────────────────────────────────────────────────────

#[test]
fn initial_var_matches_any() {
    let (g, _reg) = shapes::single_initial_var();
    a::matches(&g, initial_var(), 1);
}

#[test]
fn initial_var_for_exact_vn_matches() {
    let (g, reg) = shapes::single_initial_var();
    a::matches(&g, initial_var_for(reg), 1);
}

#[test]
fn initial_var_for_wrong_vn_rejects() {
    let (g, _reg) = shapes::single_initial_var();
    let other = reg_vn(0x40, 8); // Different varnode.
    a::none(&g, initial_var_for(other));
}

#[test]
fn initial_var_capture_binds_value() {
    let (g, _reg) = shapes::single_initial_var();
    let v = Capture::new();
    let m = a::unique(&g, initial_var().capture(v));
    assert!(m.output(v).is_some());
}

// ── VarPhi ───────────────────────────────────────────────────────────────────

/// `if (reg != 0) { reg = 1 } else { reg = 2 }` — after merge, a VarPhi
/// materialises the new value of `reg`.
fn graph_phi_for_reg() -> (ir::BuiltFunctionGraph, rsleigh::Vn) {
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
    let hits = Matcher::new(&g).find_all(&phi().into());
    assert!(!hits.is_empty(), "expected at least one phi");
}

#[test]
fn phi_for_matches_exact_vn() {
    let (g, reg) = graph_phi_for_reg();
    let hits = Matcher::new(&g).find_all(&phi_for(reg).into());
    assert!(!hits.is_empty(), "phi_for({reg:?}) should match");
}

#[test]
fn phi_for_wrong_vn_rejects() {
    let (g, _reg) = graph_phi_for_reg();
    let other = reg_vn(0x40, 8);
    a::none(&g, phi_for(other));
}

/// `phi_for(vn).input(idx, p)` must address predecessor
/// slot `idx`, not the raw input index.  Per `node_signature`, the
/// `VarPhi` input layout is `[phi_token, ...per-predecessor values]`,
/// so predecessor 0's value lives at raw input index 1.  Pre-fix the
/// builder pushed `(idx, p)` directly, so `input(0, _)` targeted the
/// phi-token (a `PhiToken`-typed edge no value pattern can match).
#[test]
fn phi_input_addresses_predecessor_slot_not_phi_token() {
    let (g, reg) = graph_phi_for_reg();
    // Predecessor values are u64 1 and u64 2.  At least one of
    // `input(0, int_const(1))` and `input(0, int_const(2))` must match.
    let m1 = Matcher::new(&g)
        .find_all(&phi_for(reg).input(0, int_const(1u64)).into());
    let m2 = Matcher::new(&g)
        .find_all(&phi_for(reg).input(0, int_const(2u64)).into());
    assert!(
        !m1.is_empty() || !m2.is_empty(),
        "phi.input(0, _) must reach predecessor 0's value (got 0 matches for both 1 and 2)"
    );
    // And `input(0, int_const(99))` (a value that is NOT in the phi)
    // must NOT match — proving the index is reaching a value slot, not
    // landing on the phi-token where a value pattern always fails.
    let none_match = Matcher::new(&g)
        .find_all(&phi_for(reg).input(0, int_const(99u64)).into());
    assert!(
        none_match.is_empty(),
        "phi.input(0, int_const(99)) should not match (99 is not a predecessor value)"
    );
}

// ── FunctionArg ──────────────────────────────────────────────────────────────

/// A graph with one stack-arg at sp-relative offset `4`, index `0`.
fn graph_fn_arg_stack() -> ir::BuiltFunctionGraph {
    use opt::{FunctionArgDetect, OptimizerRaw};
    let sp = sp_vn();
    let mut t = Tb::raw(vec![sp], &[], &[sp], &[], None, 0);

    // `read *(sp + 4)` — the first stack arg in cdecl-style.
    let sp_v = t.read_var(&sp);
    let four = t.u64(4);
    let addr = t.add(sp_v, four);
    let v = t.load_ram(addr, NodeOutputType::U64);
    let mut g = t.ret_val(v);

    FunctionArgDetect::new(vec![], sp, vec![4])
        .optimize_raw(&mut g.graph, g.entry)
        .expect("FunctionArgDetect");
    g
}

#[test]
fn function_arg_any_matches() {
    let (g, _reg) = shapes::function_arg_reg();
    a::matches(&g, function_arg_any(), 1);
}

#[test]
fn function_arg_by_index_matches() {
    let (g, _reg) = shapes::function_arg_reg();
    a::matches(&g, function_arg(0), 1);
    a::none(&g, function_arg(99));
}

#[test]
fn function_arg_reg_matches_only_reg_source() {
    let (g, reg) = shapes::function_arg_reg();
    a::matches(&g, function_arg_reg(reg), 1);

    // A stack-source filter should NOT match on a register graph.
    a::none(&g, function_arg_stack(rsleigh::VnSpace::RAM, 0));
}

#[test]
fn function_arg_stack_matches_only_stack_source() {
    let g = graph_fn_arg_stack();
    // Exact space + offset → match.
    a::matches(&g, function_arg_stack(rsleigh::VnSpace::RAM, 4), 1);
    // Wrong offset → reject.
    a::none(&g, function_arg_stack(rsleigh::VnSpace::RAM, 8));
    // A register filter on a stack-sourced arg → reject.
    let some_reg = reg_vn(0, 8);
    a::none(&g, function_arg_reg(some_reg));
}

// ── Matcher::function_arg* index API ─────────────────────────────────────────

#[test]
fn matcher_function_arg_api_returns_handle() {
    let (g, _reg) = shapes::function_arg_reg();
    let matcher = Matcher::new(&g);
    let h = matcher.function_arg(0).expect("fn arg 0");
    assert_eq!(h.index(), 0);
    assert!(matches!(h.source(), FunctionArgSource::Register(_)));
    assert_eq!(matcher.function_arg_count(), 1);
    assert_eq!(matcher.function_arg_len(), 1);
}

#[test]
fn matcher_function_arg_out_of_range_none() {
    let (g, _reg) = shapes::function_arg_reg();
    let matcher = Matcher::new(&g);
    assert!(matcher.function_arg(1).is_none());
    assert!(matcher.function_arg(u32::MAX).is_none());
}

#[test]
fn matcher_function_args_iterator_sorted() {
    let (g, _reg) = shapes::function_arg_reg();
    let matcher = Matcher::new(&g);
    let indices: Vec<u32> = matcher.function_args().map(|(i, _)| i).collect();
    assert_eq!(indices, vec![0]);
}
