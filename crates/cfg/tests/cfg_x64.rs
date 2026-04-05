//! CFG integration tests for 64-bit x86-64 (`binary_tests/binary_test_x64`).
//!
//! Mirror of `cfg_x86.rs` but using the 64-bit binary and Sleigh spec.

#[path = "helpers.rs"]
mod helpers;

use helpers::*;

fn binary() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../binary_tests/binary_test_x64")
}

fn cfg(fn_name: &str) -> cfg::Cfg<reader::ElfFileMemReader<'static, 'static>> {
    build_cfg(
        binary(),
        fn_name,
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    )
}

// ── linear functions ──────────────────────────────────────────────────────────

#[test]
fn add_is_linear() {
    assert_linear_function(&cfg("add"), "add");
}

#[test]
fn sub_is_linear() {
    assert_linear_function(&cfg("sub"), "sub");
}

#[test]
fn mul_is_linear() {
    assert_linear_function(&cfg("mul"), "mul");
}

#[test]
fn bitwise_ops_is_linear() {
    assert_linear_function(&cfg("bitwise_ops"), "bitwise_ops");
}

// ── single-conditional functions ──────────────────────────────────────────────

#[test]
fn abs_val_has_conditional_edges() {
    assert_single_conditional(&cfg("abs_val"), "abs_val");
}

#[test]
fn max_val_has_conditional_edges() {
    assert_single_conditional(&cfg("max_val"), "max_val");
}

// ── nested-conditional functions ──────────────────────────────────────────────

#[test]
fn clamp_has_multiple_conditional_regions() {
    let c = cfg("clamp");
    assert!(region_count(&c) >= 3, "clamp: expected at least 3 regions");
    assert!(count_edges_of_kind(&c, cfg::RegionEdgeKind::IfCaseTrue) >= 2,
        "clamp: expected at least 2 IfCaseTrue edges");
    assert!(all_conditional_regions_well_formed(&c), "clamp: pair invariant violated");
}

// ── looping functions ─────────────────────────────────────────────────────────

#[test]
fn sum_to_n_has_back_edge() {
    assert_looping_function(&cfg("sum_to_n"), "sum_to_n");
}

#[test]
fn factorial_has_back_edge() {
    assert_looping_function(&cfg("factorial"), "factorial");
}

#[test]
fn count_bits_has_back_edge() {
    assert_looping_function(&cfg("count_bits"), "count_bits");
}

#[test]
fn array_sum_has_back_edge() {
    assert_looping_function(&cfg("array_sum"), "array_sum");
}

#[test]
fn array_fill_has_back_edge() {
    assert_looping_function(&cfg("array_fill"), "array_fill");
}

// ── recursive function ────────────────────────────────────────────────────────

#[test]
fn fib_builds_and_is_bounded() {
    let c = cfg("fib");
    assert!(region_count(&c) >= 2, "fib: expected at least 2 regions");
    assert!(region_count(&c) < 50, "fib: too many regions — builder may have followed calls");
    assert!(all_conditional_regions_well_formed(&c), "fib: pair invariant violated");
}

// ── nested-call function ──────────────────────────────────────────────────────

#[test]
fn g_is_single_region() {
    assert_eq!(region_count(&cfg("g")), 1, "g: expected 1 region");
}

// ── entry address invariant ───────────────────────────────────────────────────

#[test]
fn entry_start_addr_matches_symbol() {
    let expected = symbol_addr(binary(), "add");
    let c = cfg("add");
    assert_eq!(c.graph[c.entry].start_addr.machine_addr.addr, expected);
}

// ── API surface ───────────────────────────────────────────────────────────────

#[test]
fn abs_val_region_if_returns_both_successors() {
    let c = cfg("abs_val");
    let has_pair = c.region_ids().any(|id| {
        let s = c.region_if(id).unwrap();
        s.if_true_region.is_some() && s.if_false_region.is_some()
    });
    assert!(has_pair, "abs_val: no region has both if successors");
}

#[test]
fn add_entry_region_branch_is_none() {
    let c = cfg("add");
    assert!(c.region_branch(c.entry).unwrap().is_none());
}

#[test]
fn sum_to_n_has_fallthrough_edges() {
    let c = cfg("sum_to_n");
    assert!(count_edges_of_kind(&c, cfg::RegionEdgeKind::Fallthrough) >= 1);
}

// ── global invariants ─────────────────────────────────────────────────────────

#[test]
fn all_functions_satisfy_global_invariants() {
    for name in &[
        "add", "sub", "mul", "bitwise_ops",
        "abs_val", "max_val", "clamp",
        "sum_to_n", "factorial", "count_bits",
        "array_sum", "array_fill",
        "fib", "g",
    ] {
        let c = cfg(name);
        assert_global_invariants(&c, name);
    }
}
