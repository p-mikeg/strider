#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

//! Integration tests for the full analysis pipeline (CFG → IR).
//!
//! Each test verifies that:
//!  - The CFG builder succeeds.
//!  - The IR analyzer succeeds.
//!  - The resulting graph has at least one node.
//!
//! Tests are parameterised over every supported architecture via the
//! `analyzer_arch_tests!` macro.  All architectures share the same test
//! suite; only the binary path and `Analyzer` setup differ.
//!
//! Build the test binaries first:
//!
//!   make -C binary_tests

// ── binary path ───────────────────────────────────────────────────────────────

/// Returns the path to the test binary for `arch`.
///
/// Binaries live at `<workspace_root>/binary_tests/out/<arch>/test.elf`.
fn binary(arch: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../binary_tests/out")
        .join(arch)
        .join("test.elf")
}

// ── analyzer_arch_tests! macro ────────────────────────────────────────────────
//
// Generates a test module for one architecture.
// $setup_fn must be a zero-arg fn that returns Result<(object::File<'static>, Analyzer, SleighArch)>.

macro_rules! analyzer_arch_tests {
    (
        mod $mod_name:ident,
        arch = $arch:literal,
        setup = $setup_fn:expr
        $(, ignore = $reason:literal)?
    ) => {
        mod $mod_name {
            use object::{Object, ObjectSymbol};

            type TestResult = Result<(), Box<dyn std::error::Error>>;

            fn binary_path() -> std::path::PathBuf {
                super::binary($arch)
            }

            /// Runs the full analysis pipeline on `fn_name`.
            /// Returns the number of reachable nodes in the IR graph.
            fn analyze(fn_name: &str) -> Result<usize, Box<dyn std::error::Error>> {
                let setup: fn() -> Result<(object::File<'static>, analyzer::Analyzer, analyzer::SleighArch), Box<dyn std::error::Error>> = $setup_fn;
                let (obj, ana, arch) = setup()?;
                let path = binary_path();

                let sym = obj
                    .symbol_by_name(fn_name)
                    .ok_or_else(|| format!("symbol '{}' not found in {:?}", fn_name, path))?;
                let addr = sym.address();

                let mem_reader = reader::ElfFileMemReader::from_object(&obj)?;

                let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem_reader)?;

                let cfg_opts = cfg::OptionsBuilder::new()
                    .allow_code_before_start_addr()
                    .build();
                let cfg = cfg::Builder::new(sleigh, addr, cfg_opts).build()?;

                let mut graph = ana.analyze_cfg(&cfg)?;
                ana.build_optimizer_pipeline().run(&mut graph)?;

                Ok(graph.preorder().count())
            }

            // ── individual function tests ─────────────────────────────────────

            #[test] $(#[ignore = $reason])?
            fn analyze_add() -> TestResult {
                assert!(analyze("add")? > 0, "'add' IR graph must not be empty");
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_sub() -> TestResult {
                assert!(analyze("sub")? > 0);
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_mul() -> TestResult {
                assert!(analyze("mul")? > 0);
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_bitwise_ops() -> TestResult {
                assert!(analyze("bitwise_ops")? > 0, "'bitwise_ops' IR graph must not be empty");
                Ok(())
            }

            /// `abs_val` contains a conditional branch — IR must be richer than a straight-line fn.
            #[test] $(#[ignore = $reason])?
            fn analyze_abs_val_conditional_branch() -> TestResult {
                assert!(analyze("abs_val")? > 5, "conditional function must have a richer IR graph");
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_max_val() -> TestResult {
                assert!(analyze("max_val")? > 0);
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_clamp_nested_conditionals() -> TestResult {
                assert!(analyze("clamp")? > 0);
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_sum_to_n_loop() -> TestResult {
                assert!(analyze("sum_to_n")? > 0, "loop function must produce a valid IR graph");
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_factorial_loop() -> TestResult {
                assert!(analyze("factorial")? > 0);
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_count_bits_loop_with_shift() -> TestResult {
                assert!(analyze("count_bits")? > 0);
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_array_sum_memory_load() -> TestResult {
                assert!(analyze("array_sum")? > 0);
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_array_fill_memory_store() -> TestResult {
                assert!(analyze("array_fill")? > 0);
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_fib_recursive() -> TestResult {
                assert!(analyze("fib")? > 0, "recursive function must produce a valid IR graph");
                Ok(())
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_g_nested_calls() -> TestResult {
                assert!(analyze("g")? > 0);
                Ok(())
            }
        }
    };
}

// ── per-architecture setup functions ─────────────────────────────────────────

fn setup_x86() -> Result<
    (
        object::File<'static>,
        analyzer::Analyzer,
        analyzer::SleighArch,
    ),
    Box<dyn std::error::Error>,
> {
    let path = binary("x86");
    let obj = reader::load_elf(&path)?;
    let arch = analyzer::SleighArch::x86();
    let ana = make_analyzer(arch, analyzer::CallingConvention::x86_cdecl())?;
    Ok((obj, ana, arch))
}

fn setup_x64() -> Result<
    (
        object::File<'static>,
        analyzer::Analyzer,
        analyzer::SleighArch,
    ),
    Box<dyn std::error::Error>,
> {
    let path = binary("x64");
    let obj = reader::load_elf(&path)?;
    let arch = analyzer::SleighArch::x86_64();
    let ana = make_analyzer(arch, analyzer::CallingConvention::x86_64_systemv_abi())?;
    Ok((obj, ana, arch))
}

fn setup_aarch64() -> Result<
    (
        object::File<'static>,
        analyzer::Analyzer,
        analyzer::SleighArch,
    ),
    Box<dyn std::error::Error>,
> {
    let path = binary("aarch64");
    let obj = reader::load_elf(&path)?;
    let arch = analyzer::SleighArch::aarch64();
    let ana = make_analyzer(arch, analyzer::CallingConvention::aarch64_aapcs64())?;
    Ok((obj, ana, arch))
}

/// Builds an `Analyzer` by probing the Sleigh register table for `arch`.
fn make_analyzer(
    arch: analyzer::SleighArch,
    cc: analyzer::CallingConvention,
) -> Result<analyzer::Analyzer, Box<dyn std::error::Error>> {
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe)?;
    let regs = sleigh.regs()?;
    Ok(analyzer::Analyzer::new(arch, regs, cc)?)
}

// ── architecture instantiations ───────────────────────────────────────────────

analyzer_arch_tests!(
    mod x86,
    arch  = "x86",
    setup = super::setup_x86
);

analyzer_arch_tests!(
    mod x64,
    arch  = "x64",
    setup = super::setup_x64
);

analyzer_arch_tests!(
    mod aarch64,
    arch  = "aarch64",
    setup = super::setup_aarch64
);
