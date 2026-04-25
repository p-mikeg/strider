//! Shared infrastructure for the system-test suite.
//!
//! Every `tests/<category>.rs` declares `mod common;` and uses these helpers.
//! Adding a new test case is mechanical:
//!
//! ```ignore
//! per_arch_test!("arithmetic", "add", graph_must_contain_int_add);
//! fn graph_must_contain_int_add(g: &ir::BuiltFunctionGraph) {
//!     assert!(common::count_int_binop(g, ir::IntBinaryOp::Add) >= 1);
//! }
//! ```
//!
//! The macro expands to six `#[test]` functions, one per supported arch
//! (`x86`, `x64`, `aarch64`, `arm`, `mips32le`, `mips32be`).  Each arch test
//! loads its binary at `fixtures/out/<arch>/<case>.elf`, analyses the named
//! symbol, runs the optimiser pipeline (with `LoadReadOnly` wired to the
//! binary's `.rodata`), and invokes the user-provided assertion closure.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    dead_code  // category test files won't use every helper
)]

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

// ── Architecture enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch { X86, X64, Aarch64, Arm, Mips32le, Mips32be }

impl Arch {
    pub fn name(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X64 => "x64",
            Arch::Aarch64 => "aarch64",
            Arch::Arm => "arm",
            Arch::Mips32le => "mips32le",
            Arch::Mips32be => "mips32be",
        }
    }
    pub fn sleigh(self) -> analyzer::SleighArch {
        match self {
            Arch::X86 => analyzer::SleighArch::x86(),
            Arch::X64 => analyzer::SleighArch::x86_64(),
            Arch::Aarch64 => analyzer::SleighArch::aarch64(),
            Arch::Arm => analyzer::SleighArch::arm(),
            Arch::Mips32le => analyzer::SleighArch::mipsle32(),
            Arch::Mips32be => analyzer::SleighArch::mipsbe32(),
        }
    }
    pub fn cc(self) -> analyzer::CallingConvention {
        match self {
            Arch::X86 => analyzer::CallingConvention::x86_cdecl(),
            Arch::X64 => analyzer::CallingConvention::x86_64_systemv_abi(),
            Arch::Aarch64 => analyzer::CallingConvention::aarch64_aapcs64(),
            Arch::Arm => analyzer::CallingConvention::arm_aapcs(),
            // O32 ABI is the same on LE and BE 32-bit MIPS Linux.
            Arch::Mips32le | Arch::Mips32be => analyzer::CallingConvention::mips_o32(),
        }
    }
}

// ── Binary path resolution ───────────────────────────────────────────────────

pub fn binary_path(arch: Arch, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch.name())
        .join(format!("{case}.elf"))
}

// ── Pipeline runner ──────────────────────────────────────────────────────────

/// Loads the (arch, case) ELF, builds a CFG starting at `fn_name`, runs the
/// full analyzer + optimiser pipeline (with `LoadReadOnly` against the same
/// ELF) and returns the resulting graph.
///
/// Panics on any failure — system tests are pass/fail end-to-end checks.  If
/// the binary is missing, the panic carries an actionable message including
/// the `make -C fixtures` instruction.
pub fn analyze(arch: Arch, case: &str, fn_name: &str) -> ir::BuiltFunctionGraph {
    let path = binary_path(arch, case);
    if !path.exists() {
        panic!(
            "missing test binary {path:?}; run `make -C fixtures` (or \
             `make -C fixtures ARCH={} CASE={case}` for just this case)",
            arch.name()
        );
    }
    let obj = reader::load_elf(&path)
        .unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
    let sleigh_arch = arch.sleigh();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec, sleigh_arch.pspec, probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let ana = analyzer::Analyzer::new(sleigh_arch, regs, arch.cc())
        .expect("Analyzer::new");
    let mem = reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec, sleigh_arch.pspec, mem)
        .expect("real sleigh new");
    let addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
        .address();
    let cfg = cfg::Builder::new(
        sleigh, addr,
        cfg::OptionsBuilder::new().allow_code_before_start_addr().build()
    )
    .build()
    .unwrap_or_else(|e| panic!("Cfg build for {fn_name}: {e:?}"));
    let mut graph = ana.analyze_cfg(&cfg)
        .unwrap_or_else(|e| panic!("analyze_cfg for {fn_name}: {e:?}"));
    let rom = reader::ElfFileMemReader::from_object(&obj).expect("rom reader");
    let mut p = ana.build_optimizer_pipeline();
    p.add(opt::LoadReadOnly(rom));
    p.run(&mut graph)
        .unwrap_or_else(|e| panic!("optimizer pipeline for {fn_name}: {e:?}"));
    graph
}

// ── Assertion vocabulary ─────────────────────────────────────────────────────
//
// All counters walk the graph in pre-order and filter on the node kind.
// Naming convention: `count_<thing>` returns a `usize`; `has_<thing>` returns a `bool`.

use ir::node::NodeKind;

pub fn count_kind<F: Fn(&NodeKind) -> bool>(g: &ir::BuiltFunctionGraph, pred: F) -> usize {
    g.preorder().filter(|nid| pred(g.graph.node_kind(*nid))).count()
}

pub fn count_int_binop(g: &ir::BuiltFunctionGraph, op: ir::IntBinaryOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::IntBinaryOp(o) if *o == op))
}
pub fn count_int_unop(g: &ir::BuiltFunctionGraph, op: ir::IntUnaryOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::IntUnaryOp(o) if *o == op))
}
pub fn count_int_cmp(g: &ir::BuiltFunctionGraph, op: ir::IntCmpOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::IntCmpOp(o) if *o == op))
}
pub fn count_float_binop(g: &ir::BuiltFunctionGraph, op: ir::FloatBinaryOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::FloatBinaryOp(o) if *o == op))
}
pub fn count_float_unop(g: &ir::BuiltFunctionGraph, op: ir::FloatUnaryOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::FloatUnaryOp(o) if *o == op))
}
pub fn count_float_cmp(g: &ir::BuiltFunctionGraph, op: ir::FloatCmpOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::FloatCmpOp(o) if *o == op))
}

pub fn count_calls (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Call)) }
pub fn count_ifs   (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::If)) }
pub fn count_returns(g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Return)) }
pub fn count_loops (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::ControlPhi(_))) }
pub fn count_loads (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Load(_))) }
pub fn count_stores(g: &ir::BuiltFunctionGraph) -> usize {
    // Both raw Store and StackStore count as "writes to memory" from the user's POV.
    count_kind(g, |k| matches!(k, NodeKind::Store(_) | NodeKind::StackStore { .. }))
}
pub fn count_stack_stores(g: &ir::BuiltFunctionGraph) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::StackStore { .. }))
}
pub fn count_popcount(g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Popcount)) }
pub fn count_lzcount (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Lzcount)) }
pub fn count_int_consts(g: &ir::BuiltFunctionGraph) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::IntConst(_)))
}

pub fn has_kind<F: Fn(&NodeKind) -> bool>(g: &ir::BuiltFunctionGraph, pred: F) -> bool {
    count_kind(g, pred) > 0
}

pub fn has_constant(g: &ir::BuiltFunctionGraph, value: u64) -> bool {
    has_kind(g, |k| matches!(k, NodeKind::IntConst(c) if *c == value))
}

// ── per_arch_test! macro ─────────────────────────────────────────────────────

/// Generates one `#[test]` per (architecture, function) pair.
///
/// Usage:
///   per_arch_test!("<case>", "<fn_name>", <assertion_fn>);
///
/// Expands to a module named `test_<fn_name>` containing six tests
/// (`x86`, `x64`, `aarch64`, `arm`, `mips32le`, `mips32be`).
#[macro_export]
macro_rules! per_arch_test {
    ($case:literal, $fn_name:literal, $assert:ident) => {
        paste::paste! {
            mod [<test_ $fn_name>] {
                use super::*;
                #[test] fn x86()      { let g = $crate::common::analyze($crate::common::Arch::X86,      $case, $fn_name); $assert(&g); }
                #[test] fn x64()      { let g = $crate::common::analyze($crate::common::Arch::X64,      $case, $fn_name); $assert(&g); }
                #[test] fn aarch64()  { let g = $crate::common::analyze($crate::common::Arch::Aarch64,  $case, $fn_name); $assert(&g); }
                #[test] fn arm()      { let g = $crate::common::analyze($crate::common::Arch::Arm,      $case, $fn_name); $assert(&g); }
                #[test] fn mips32le() { let g = $crate::common::analyze($crate::common::Arch::Mips32le, $case, $fn_name); $assert(&g); }
                #[test] fn mips32be() { let g = $crate::common::analyze($crate::common::Arch::Mips32be, $case, $fn_name); $assert(&g); }
            }
        }
    };
}
