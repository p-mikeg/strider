//! CFG integration tests for ARM 32-bit (`binary_tests/binary_test_arm`).
//!
//! The binary is compiled with `-marm` to force ARM (non-Thumb) instruction
//! encoding, matched by `SLA_SPEC_ARM8_LE` + `PSPEC_ARMCORTEX`.
//! Build the binary with: `make -C binary_tests binary_test_arm`
//!
//! **All tests are currently ignored.**
//! ARM 32-bit uses `bx lr` for function returns, which the Sleigh lifter emits
//! as `BranchIndirect`. The CFG builder does not yet handle `BranchIndirect` as
//! a region terminator, so it continues decoding past function returns into
//! adjacent functions (including jump-table code that cannot be decoded).
//! Unignore these tests once `BranchIndirect` support is added to the builder.

#[path = "helpers.rs"]
mod helpers;

use helpers::*;

fn binary() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../binary_tests/binary_test_arm")
}

fn cfg(fn_name: &str) -> cfg::Cfg<reader::ElfFileMemReader<'static, 'static>> {
    build_cfg(
        binary(),
        fn_name,
        rsleigh::sla_spec::SLA_SPEC_ARM8_LE,
        rsleigh::pspec::PSPEC_ARMCORTEX,
    )
}

// ── linear functions ──────────────────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn add_is_linear() {
    assert_linear_function(&cfg("add"), "add");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn sub_is_linear() {
    assert_linear_function(&cfg("sub"), "sub");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn mul_is_linear() {
    assert_linear_function(&cfg("mul"), "mul");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn bitwise_ops_is_linear() {
    assert_linear_function(&cfg("bitwise_ops"), "bitwise_ops");
}

// ── single-conditional functions ──────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn abs_val_has_conditional_edges() {
    assert_single_conditional(&cfg("abs_val"), "abs_val");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn max_val_has_conditional_edges() {
    assert_single_conditional(&cfg("max_val"), "max_val");
}

// ── nested-conditional functions ──────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn clamp_has_multiple_conditional_regions() {
    let c = cfg("clamp");
    assert!(region_count(&c) >= 3, "clamp: expected at least 3 regions");
    assert!(count_edges_of_kind(&c, cfg::RegionEdgeKind::IfCaseTrue) >= 2,
        "clamp: expected at least 2 IfCaseTrue edges");
    assert!(all_conditional_regions_well_formed(&c), "clamp: pair invariant violated");
}

// ── looping functions ─────────────────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn sum_to_n_has_back_edge() {
    assert_looping_function(&cfg("sum_to_n"), "sum_to_n");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn factorial_has_back_edge() {
    assert_looping_function(&cfg("factorial"), "factorial");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn count_bits_has_back_edge() {
    assert_looping_function(&cfg("count_bits"), "count_bits");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn array_sum_has_back_edge() {
    assert_looping_function(&cfg("array_sum"), "array_sum");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn array_fill_has_back_edge() {
    assert_looping_function(&cfg("array_fill"), "array_fill");
}

// ── recursive function ────────────────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn fib_builds_and_is_bounded() {
    let c = cfg("fib");
    assert!(region_count(&c) >= 2, "fib: expected at least 2 regions");
    assert!(region_count(&c) < 50, "fib: too many regions");
    assert!(all_conditional_regions_well_formed(&c), "fib: pair invariant violated");
}

// ── nested-call function ──────────────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn g_is_single_region() {
    assert_eq!(region_count(&cfg("g")), 1, "g: expected 1 region");
}

// ── entry address invariant ───────────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn entry_start_addr_matches_symbol() {
    let expected = symbol_addr(binary(), "add");
    let c = cfg("add");
    assert_eq!(c.graph[c.entry].start_addr.machine_addr.addr, expected);
}

// ── API surface ───────────────────────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn abs_val_region_if_returns_both_successors() {
    let c = cfg("abs_val");
    let has_pair = c.region_ids().any(|id| {
        let s = c.region_if(id).unwrap();
        s.if_true_region.is_some() && s.if_false_region.is_some()
    });
    assert!(has_pair, "abs_val: no region has both if successors");
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn add_entry_region_branch_is_none() {
    let c = cfg("add");
    assert!(c.region_branch(c.entry).unwrap().is_none());
}

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
fn sum_to_n_has_fallthrough_edges() {
    let c = cfg("sum_to_n");
    assert!(count_edges_of_kind(&c, cfg::RegionEdgeKind::Fallthrough) >= 1);
}

// ── global invariants ─────────────────────────────────────────────────────────

#[test]
#[ignore = "ARM BranchIndirect not yet handled as region terminator"]
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
