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
//! Test binaries live in `fixtures/out/<arch>` and are built with:
//!
//!   make -C `fixtures`
//!
//! ARM 32-bit tests are all `#[ignore]` because `BranchIndirect` is not yet
//! handled as a region terminator — unignore them once that is fixed.

mod common;

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
        $(, ignore_fallthrough = $fallthrough_reason:literal)?
    ) => {
        mod $mod_name {
            use super::common::*;
            use super::common;

            fn cfg_of(fn_name: &str) -> cfg::Cfg<reader::ElfFileMemReader> {
                let p = super::common::binary($arch, fn_name);
                common::build_cfg(p.to_str().unwrap(), fn_name, $sla, $pspec)
            }

            fn bin_for(fn_name: &str) -> std::path::PathBuf {
                super::common::binary($arch, fn_name)
            }

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

            // (bitwise_ops was a single function in the legacy fixture; the
            // per-category fixtures split it into bit_and/or/xor/not/shl etc.
            // bit_and stands in as the linear-bitwise-arithmetic case.)
            #[test] $(#[ignore = $reason])?
            fn bit_and_is_linear() {
                assert_linear_function(&cfg_of("bit_and"), "bit_and");
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

            /// CFG builder does not follow `Call` opcodes, so a recursive
            /// function stays bounded.
            #[test] $(#[ignore = $reason])?
            fn fib_recursive_builds_and_is_bounded() {
                let c = cfg_of("fib_recursive");
                assert!(region_count(&c) >= 2, "fib_recursive: expected at least 2 regions");
                assert!(region_count(&c) < 50,  "fib_recursive: too many regions — builder may have followed calls");
                assert!(all_conditional_regions_well_formed(&c), "fib_recursive: pair invariant violated");
            }

            // ── entry address invariant ───────────────────────────────────────

            /// The entry region's `start_addr` must match the ELF symbol address.
            #[test] $(#[ignore = $reason])?
            fn entry_start_addr_matches_symbol() {
                let b = bin_for("add");
                let expected = symbol_addr(b.to_str().unwrap(), "add");
                let c = cfg_of("add");
                assert_eq!(c.graph[c.entry].start_addr.machine_addr.addr, expected);
            }

            // ── fallthrough edges ─────────────────────────────────────────────

            /// Looping functions must produce at least one fallthrough edge.
            /// `ignore_fallthrough` overrides the module-wide `ignore` for
            /// arches where clang at -O0 emits explicit `b <next-instr>`
            /// (BUG-25 — `aarch64be`, `ppc32le`).
            #[test]
            $(#[ignore = $fallthrough_reason])?
            $(#[ignore = $reason])?
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
                    "add", "sub", "mul", "bit_and",
                    "abs_val", "max_val", "clamp",
                    "sum_to_n", "factorial", "count_bits",
                    "array_sum", "array_fill",
                    "fib_recursive",
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
    // PSPEC_ARM_V45 (32-bit ARM mode) matches the `-marm` build flag.
    // PSPEC_ARMCORTEX is Thumb-only (Cortex-M) and would mis-decode the
    // 4-byte ARM instructions as 2-byte Thumb halfwords.
    pspec = rsleigh::pspec::PSPEC_ARM_V45
);

arch_tests!(
    mod arm_thumb,
    arch  = "arm_thumb",
    sla   = rsleigh::sla_spec::SLA_SPEC_ARM8_LE,
    // PSPEC_ARMCORTEX selects Thumb-2 decoding for the `-mthumb` fixtures.
    pspec = rsleigh::pspec::PSPEC_ARMCORTEX,
    // The cfg_integration assertions (specific region counts, edge kinds)
    // were tuned for 4-byte ARM instructions; Thumb's 2-byte / 4-byte mix
    // produces different CFG shapes (a Thumb conditional cluster is
    // typically more regions than the ARM IT-block equivalent).  Per-
    // arch assertion tuning is its own follow-up.
    ignore = "ARM Thumb fixtures produce different region shapes than the ARM-tuned cfg assertions"
);

arch_tests!(
    mod aarch64be,
    arch  = "aarch64be",
    sla   = rsleigh::sla_spec::SLA_SPEC_AARCH64BE,
    pspec = rsleigh::pspec::PSPEC_AARCH64
);

arch_tests!(
    mod mips64le,
    arch  = "mips64le",
    sla   = rsleigh::sla_spec::SLA_SPEC_MIPS64LE,
    pspec = rsleigh::pspec::PSPEC_MIPS64
);

arch_tests!(
    mod mips64be,
    arch  = "mips64be",
    sla   = rsleigh::sla_spec::SLA_SPEC_MIPS64BE,
    pspec = rsleigh::pspec::PSPEC_MIPS64
);

arch_tests!(
    mod ppc32be,
    arch  = "ppc32be",
    sla   = rsleigh::sla_spec::SLA_SPEC_PPC_32_BE,
    pspec = rsleigh::pspec::PSPEC_PPC_32
);

arch_tests!(
    mod ppc32le,
    arch  = "ppc32le",
    sla   = rsleigh::sla_spec::SLA_SPEC_PPC_32_LE,
    pspec = rsleigh::pspec::PSPEC_PPC_32
);

arch_tests!(
    mod ppc64be,
    arch  = "ppc64be",
    sla   = rsleigh::sla_spec::SLA_SPEC_PPC_64_BE,
    pspec = rsleigh::pspec::PSPEC_PPC_64
);

arch_tests!(
    mod ppc64le,
    arch  = "ppc64le",
    sla   = rsleigh::sla_spec::SLA_SPEC_PPC_64_LE,
    pspec = rsleigh::pspec::PSPEC_PPC_64
);
