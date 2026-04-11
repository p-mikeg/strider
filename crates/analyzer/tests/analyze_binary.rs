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
// $setup_fn must be a zero-arg fn that returns (object::File<'static>, Analyzer, SleighArch).

macro_rules! analyzer_arch_tests {
    (
        mod $mod_name:ident,
        arch = $arch:literal,
        setup = $setup_fn:expr
        $(, ignore = $reason:literal)?
    ) => {
        mod $mod_name {
            use object::{Object, ObjectSymbol};

            fn binary_path() -> std::path::PathBuf {
                super::binary($arch)
            }

            /// Runs the full analysis pipeline on `fn_name`.
            /// Returns the number of reachable nodes in the IR graph.
            fn analyze(fn_name: &str) -> usize {
                let setup: fn() -> (object::File<'static>, analyzer::Analyzer, analyzer::SleighArch) = $setup_fn;
                let (obj, ana, arch) = setup();
                let path = binary_path();

                let sym = obj
                    .symbol_by_name(fn_name)
                    .unwrap_or_else(|| panic!("symbol '{}' not found in {:?}", fn_name, path));
                let addr = sym.address();

                let data: Vec<u8> = std::fs::read(&path).expect("read binary");
                let leaked = Box::leak(data.into_boxed_slice());
                let parsed = object::File::parse(&*leaked).expect("parse ELF");
                let mem_reader = reader::ElfFileMemReader::from_elf_sections(&parsed)
                    .expect("build mem reader");

                let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem_reader)
                    .expect("create Sleigh context");

                let cfg_opts = cfg::OptionsBuilder::new()
                    .allow_code_before_start_addr()
                    .build();
                let cfg = cfg::Builder::new(sleigh, addr, cfg_opts)
                    .build()
                    .unwrap_or_else(|e| panic!("CFG build failed for '{}': {e:?}", fn_name));

                let graph = ana
                    .analyze_cfg(&cfg)
                    .unwrap_or_else(|e| panic!("IR analysis failed for '{}': {e:?}", fn_name));

                graph.preorder().count()
            }

            // ── individual function tests ─────────────────────────────────────

            #[test] $(#[ignore = $reason])?
            fn analyze_add() {
                assert!(analyze("add") > 0, "'add' IR graph must not be empty");
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_sub() {
                assert!(analyze("sub") > 0);
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_mul() {
                assert!(analyze("mul") > 0);
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_bitwise_ops() {
                assert!(analyze("bitwise_ops") > 0, "'bitwise_ops' IR graph must not be empty");
            }

            /// `abs_val` contains a conditional branch — IR must be richer than a straight-line fn.
            #[test] $(#[ignore = $reason])?
            fn analyze_abs_val_conditional_branch() {
                assert!(analyze("abs_val") > 5, "conditional function must have a richer IR graph");
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_max_val() {
                assert!(analyze("max_val") > 0);
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_clamp_nested_conditionals() {
                assert!(analyze("clamp") > 0);
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_sum_to_n_loop() {
                assert!(analyze("sum_to_n") > 0, "loop function must produce a valid IR graph");
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_factorial_loop() {
                assert!(analyze("factorial") > 0);
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_count_bits_loop_with_shift() {
                assert!(analyze("count_bits") > 0);
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_array_sum_memory_load() {
                assert!(analyze("array_sum") > 0);
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_array_fill_memory_store() {
                assert!(analyze("array_fill") > 0);
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_fib_recursive() {
                assert!(analyze("fib") > 0, "recursive function must produce a valid IR graph");
            }

            #[test] $(#[ignore = $reason])?
            fn analyze_g_nested_calls() {
                assert!(analyze("g") > 0);
            }
        }
    };
}

// ── per-architecture setup functions ─────────────────────────────────────────

fn setup_x86() -> (object::File<'static>, analyzer::Analyzer, analyzer::SleighArch) {
    let path = binary("x86");
    let obj = reader::load_elf(path.to_str().unwrap());
    let arch = analyzer::SleighArch::x86();
    let ana = make_analyzer(arch, analyzer::CallingConvention::x86_cdecl());
    (obj, ana, arch)
}

fn setup_x64() -> (object::File<'static>, analyzer::Analyzer, analyzer::SleighArch) {
    let path = binary("x64");
    let obj = reader::load_elf(path.to_str().unwrap());
    let arch = analyzer::SleighArch::x86_64();
    let ana = make_analyzer(arch, analyzer::CallingConvention::x86_64_systemv_abi());
    (obj, ana, arch)
}

fn setup_aarch64() -> (object::File<'static>, analyzer::Analyzer, analyzer::SleighArch) {
    let path = binary("aarch64");
    let obj = reader::load_elf(path.to_str().unwrap());
    let arch = analyzer::SleighArch::aarch64();
    let ana = make_analyzer(arch, analyzer::CallingConvention::aarch64_aapcs64());
    (obj, ana, arch)
}

/// Builds an `Analyzer` by probing the Sleigh register table for `arch`.
fn make_analyzer(arch: analyzer::SleighArch, cc: analyzer::CallingConvention) -> analyzer::Analyzer {
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe)
        .expect("probe Sleigh context");
    let regs = sleigh.regs().expect("fetch register list");
    analyzer::Analyzer::new(arch, regs, cc).expect("create Analyzer")
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
