//! Integration tests for the analyzer pipeline.
//!
//! These tests compile `binary_tests/test.c` into a 32-bit x86 ELF binary and
//! run the full analysis pipeline (CFG build + IR lift) against every named
//! function in that binary.
//!
//! Each test verifies that:
//! - The CFG builder does not return an error.
//! - The IR analyzer does not return an error.
//! - The resulting graph has at least one node (the Entry node).

use object::{Object, ObjectSymbol};

/// Returns the absolute path to the pre-compiled test binary.
///
/// The binary lives at `<workspace_root>/binary_tests/binary_test`.
/// `CARGO_MANIFEST_DIR` points to the analyzer crate root, so we go up two
/// levels to reach the workspace root.
fn binary_path() -> std::path::PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // crates/analyzer  →  workspace root  →  binary_tests/binary_test
    manifest.join("../../binary_tests/binary_test")
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Builds a Sleigh context and an Analyzer for the x86 cdecl ABI and returns
/// them together with the parsed ELF object.
fn setup() -> (
    object::File<'static>,
    analyzer::Analyzer,
    analyzer::SleighArch,
) {
    let obj = reader::load_elf(binary_path().to_str().unwrap());
    let arch = analyzer::SleighArch::x86();

    // Build an Analyzer (resolves register names from the Sleigh register
    // table — if any name is wrong this panics early with a clear message).
    let probe_reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let probe_sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe_reader)
        .expect("failed to create probe Sleigh context");
    let sleigh_regs = probe_sleigh.regs().expect("failed to fetch register list");
    let ana = analyzer::Analyzer::new(arch, sleigh_regs, analyzer::CallingConvention::x86_cdecl())
        .expect("failed to create Analyzer");

    (obj, ana, arch)
}

/// Runs the full analysis pipeline on the function whose symbol is `fn_name`.
///
/// Returns the number of nodes in the resulting IR graph so callers can
/// assert it is non-trivial.
fn analyze_function(fn_name: &str) -> usize {
    let (obj, ana, arch) = setup();

    let sym = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol '{}' not found in {:?}", fn_name, binary_path()));
    let addr = sym.address();

    let data: Vec<u8> = std::fs::read(binary_path()).expect("read binary");
    let leaked = Box::leak(data.into_boxed_slice());
    let parsed = object::File::parse(&*leaked).expect("parse ELF");
    let mem_reader = reader::ElfFileMemReader::from_elf_sections(&parsed)
        .expect("build mem reader from sections");

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

    // Walk the graph from the entry node to count reachable nodes.
    graph.preorder().count()
}

// ── individual function tests ─────────────────────────────────────────────────

/// The simplest function (add two integers) must produce a non-empty graph.
#[test]
fn analyze_add() {
    let nodes = analyze_function("add");
    assert!(nodes > 0, "IR graph for 'add' must not be empty");
}

/// Subtraction — tests that Sub IR nodes are emitted without errors.
#[test]
fn analyze_sub() {
    let nodes = analyze_function("sub");
    assert!(nodes > 0);
}

/// Multiplication.
#[test]
fn analyze_mul() {
    let nodes = analyze_function("mul");
    assert!(nodes > 0);
}

/// `bitwise_ops` exercises AND, OR, XOR, SHL, SHR, and NOT in one function.
#[test]
fn analyze_bitwise_ops() {
    let nodes = analyze_function("bitwise_ops");
    assert!(nodes > 0, "bitwise_ops IR graph must not be empty");
}

/// `abs_val` contains a conditional branch (if x < 0), exercising the If IR
/// node and two control-flow successors.
#[test]
fn analyze_abs_val_conditional_branch() {
    let nodes = analyze_function("abs_val");
    // A function with an if/else must have more nodes than a straight-line fn
    assert!(nodes > 5, "conditional function must have a richer IR graph");
}

/// `max_val` is another conditional with two separate return paths.
#[test]
fn analyze_max_val() {
    let nodes = analyze_function("max_val");
    assert!(nodes > 0);
}

/// `clamp` has two nested conditionals — exercises multi-branch IR.
#[test]
fn analyze_clamp_nested_conditionals() {
    let nodes = analyze_function("clamp");
    assert!(nodes > 0);
}

/// `sum_to_n` contains a while loop — exercises back edges in the CFG.
#[test]
fn analyze_sum_to_n_loop() {
    let nodes = analyze_function("sum_to_n");
    assert!(nodes > 0, "loop function must produce a valid IR graph");
}

/// `factorial` contains a for loop.
#[test]
fn analyze_factorial_loop() {
    let nodes = analyze_function("factorial");
    assert!(nodes > 0);
}

/// `count_bits` contains a loop with bitwise shift — exercises both loop
/// back edges and the SHR node kind.
#[test]
fn analyze_count_bits_loop_with_shift() {
    let nodes = analyze_function("count_bits");
    assert!(nodes > 0);
}

/// `array_sum` accesses memory through a pointer — exercises Load nodes.
#[test]
fn analyze_array_sum_memory_load() {
    let nodes = analyze_function("array_sum");
    assert!(nodes > 0);
}

/// `array_fill` writes to memory through a pointer — exercises Store nodes.
#[test]
fn analyze_array_fill_memory_store() {
    let nodes = analyze_function("array_fill");
    assert!(nodes > 0);
}

/// `fib` is recursive; the call to itself exercises the Call IR node and
/// ensures the analyzer handles back-call edges without panicking.
#[test]
fn analyze_fib_recursive() {
    let nodes = analyze_function("fib");
    assert!(nodes > 0, "recursive function must produce a valid IR graph");
}

/// `g` makes nested calls, exercising the call-clobbering and argument-
/// passing register logic in the builder.
#[test]
fn analyze_g_nested_calls() {
    let nodes = analyze_function("g");
    assert!(nodes > 0);
}
