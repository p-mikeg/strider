#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

//! CFG integration tests — all architectures in one file.
//!
//! A single `arch_tests!` macro generates a dedicated test module for each
//! supported target.  Every module contains the same suite of structural
//! assertions; only the binary path and Sleigh spec differ.
//!
//! Test binaries live in `binary_tests/out/<arch>` and are built with:
//!
//!   make -C binary_tests
//!
//! ARM 32-bit tests are all `#[ignore]` because `BranchIndirect` is not yet
//! handled as a region terminator — unignore them once that is fixed.

#[path = "helpers.rs"]
mod helpers;

/// Returns the path to the test binary for `arch`.
///
/// Binaries are expected at `<workspace_root>/binary_tests/out/<arch>/test.elf`.
fn binary(arch: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../binary_tests/out")
        .join(arch)
        .join("test.elf")
}

// ── arch_tests! macro ─────────────────────────────────────────────────────────
//
// Generates a test module $mod_name containing the full test suite.
// The optional `ignore = "…"` argument wraps every test with #[ignore].

macro_rules! arch_tests {
    (
        mod $mod_name:ident,
        arch = $arch:literal,
        sla  = $sla:expr,
        pspec = $pspec:expr
        $(, ignore = $reason:literal)?
    ) => {
        mod $mod_name {
            use super::helpers::*;
            use super::helpers;

            fn cfg_of(fn_name: &str) -> cfg::Cfg<reader::ElfFileMemReader> {
                let p = super::binary($arch);
                helpers::build_cfg(p.to_str().unwrap(), fn_name, $sla, $pspec)
            }

            fn bin() -> std::path::PathBuf { super::binary($arch) }

            // ── linear functions ──────────────────────────────────────────────

            #[test] $(#[ignore = $reason])?
            fn add_is_linear() {
                assert_linear_function(&cfg_of("add"), "add");
            }

            #[test] $(#[ignore = $reason])?
            fn sub_is_linear() {
                assert_linear_function(&cfg_of("sub"), "sub");
            }

            #[test] $(#[ignore = $reason])?
            fn mul_is_linear() {
                assert_linear_function(&cfg_of("mul"), "mul");
            }

            #[test] $(#[ignore = $reason])?
            fn bitwise_ops_is_linear() {
                assert_linear_function(&cfg_of("bitwise_ops"), "bitwise_ops");
            }

            // ── single-conditional functions ──────────────────────────────────

            #[test] $(#[ignore = $reason])?
            fn abs_val_has_conditional_edges() {
                assert_single_conditional(&cfg_of("abs_val"), "abs_val");
            }

            #[test] $(#[ignore = $reason])?
            fn max_val_has_conditional_edges() {
                assert_single_conditional(&cfg_of("max_val"), "max_val");
            }

            // ── nested-conditional ────────────────────────────────────────────

            #[test] $(#[ignore = $reason])?
            fn clamp_has_multiple_conditional_regions() {
                let c = cfg_of("clamp");
                assert!(region_count(&c) >= 3, "clamp: expected at least 3 regions");
                assert!(
                    count_edges_of_kind(&c, cfg::RegionEdgeKind::IfCaseTrue) >= 2,
                    "clamp: expected at least 2 IfCaseTrue edges"
                );
                assert!(all_conditional_regions_well_formed(&c), "clamp: pair invariant violated");
            }

            // ── looping functions ─────────────────────────────────────────────

            #[test] $(#[ignore = $reason])?
            fn sum_to_n_has_back_edge() {
                assert_looping_function(&cfg_of("sum_to_n"), "sum_to_n");
            }

            #[test] $(#[ignore = $reason])?
            fn factorial_has_back_edge() {
                assert_looping_function(&cfg_of("factorial"), "factorial");
            }

            #[test] $(#[ignore = $reason])?
            fn count_bits_has_back_edge() {
                assert_looping_function(&cfg_of("count_bits"), "count_bits");
            }

            #[test] $(#[ignore = $reason])?
            fn array_sum_has_back_edge() {
                assert_looping_function(&cfg_of("array_sum"), "array_sum");
            }

            #[test] $(#[ignore = $reason])?
            fn array_fill_has_back_edge() {
                assert_looping_function(&cfg_of("array_fill"), "array_fill");
            }

            // ── recursive function ────────────────────────────────────────────

            /// CFG builder does not follow `Call` opcodes, so `fib` stays bounded.
            #[test] $(#[ignore = $reason])?
            fn fib_builds_and_is_bounded() {
                let c = cfg_of("fib");
                assert!(region_count(&c) >= 2, "fib: expected at least 2 regions");
                assert!(region_count(&c) < 50,  "fib: too many regions — builder may have followed calls");
                assert!(all_conditional_regions_well_formed(&c), "fib: pair invariant violated");
            }

            // ── nested-call function ──────────────────────────────────────────

            /// `g` tail-calls into `add`; with default options the branch is treated
            /// as a tail call and the CFG stays at exactly one region.
            #[test] $(#[ignore = $reason])?
            fn g_is_single_region() {
                assert_eq!(region_count(&cfg_of("g")), 1, "g: expected 1 region (tail-call treated as exit)");
            }

            // ── entry address invariant ───────────────────────────────────────

            /// The entry region's `start_addr` must match the ELF symbol address.
            #[test] $(#[ignore = $reason])?
            fn entry_start_addr_matches_symbol() {
                let b = bin();
                let expected = symbol_addr(b.to_str().unwrap(), "add");
                let c = cfg_of("add");
                assert_eq!(c.graph[c.entry].start_addr.machine_addr.addr, expected);
            }

            // ── API surface: region_if ────────────────────────────────────────

            /// `region_if` on a conditional region must return both successors.
            #[test] $(#[ignore = $reason])?
            fn abs_val_region_if_returns_both_successors() {
                let c = cfg_of("abs_val");
                let has_pair = c.region_ids().any(|id| {
                    let s = c.region_if(id).unwrap();
                    s.if_true_region.is_some() && s.if_false_region.is_some()
                });
                assert!(has_pair, "abs_val: no region has both if-true and if-false successors");
            }

            /// `region_branch` returns `None` on the entry region of a linear function.
            #[test] $(#[ignore = $reason])?
            fn add_entry_region_branch_is_none() {
                let c = cfg_of("add");
                assert!(
                    c.region_branch(c.entry).unwrap().is_none(),
                    "add: linear entry should have no branch successor"
                );
            }

            // ── fallthrough edges ─────────────────────────────────────────────

            /// Looping functions must produce at least one fallthrough edge.
            #[test] $(#[ignore = $reason])?
            fn sum_to_n_has_fallthrough_edges() {
                let c = cfg_of("sum_to_n");
                assert!(
                    count_edges_of_kind(&c, cfg::RegionEdgeKind::Fallthrough) >= 1,
                    "sum_to_n: expected at least one Fallthrough edge"
                );
            }

            // ── global invariants ─────────────────────────────────────────────

            #[test] $(#[ignore = $reason])?
            fn all_functions_satisfy_global_invariants() {
                for name in &[
                    "add", "sub", "mul", "bitwise_ops",
                    "abs_val", "max_val", "clamp",
                    "sum_to_n", "factorial", "count_bits",
                    "array_sum", "array_fill",
                    "fib", "g",
                ] {
                    let c = cfg_of(name);
                    assert_global_invariants(&c, name);
                }
            }
        }
    };
}

// ── Architecture instantiations ───────────────────────────────────────────────

arch_tests!(
    mod x86,
    arch  = "x86",
    sla   = rsleigh::sla_spec::SLA_SPEC_X86,
    pspec = rsleigh::pspec::PSPEC_X86
);

arch_tests!(
    mod x64,
    arch  = "x64",
    sla   = rsleigh::sla_spec::SLA_SPEC_X86_64,
    pspec = rsleigh::pspec::PSPEC_X86_64
);

arch_tests!(
    mod aarch64,
    arch  = "aarch64",
    sla   = rsleigh::sla_spec::SLA_SPEC_AARCH64,
    pspec = rsleigh::pspec::PSPEC_AARCH64
);

arch_tests!(
    mod arm,
    arch  = "arm",
    sla   = rsleigh::sla_spec::SLA_SPEC_ARM8_LE,
    pspec = rsleigh::pspec::PSPEC_ARMCORTEX,
    ignore = "ARM BranchIndirect not yet handled as region terminator"
);
