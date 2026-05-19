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

// Sub-module containing fixture builders for the indirect-branch classifier
// integration tests in `tests/indirect_resolve_classify.rs`.  Kept as a sub-module
// so the rest of the per-arch fixture infrastructure above remains
// unchanged.
pub mod indirect_resolve_helpers;

// ── Architecture enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86,
    /// x86 32-bit compiled with `-mregparm=3` and analysed under the
    /// Linux kernel-internal CC (`x86_linux_kernel`).  Same Sleigh
    /// spec as `X86`; only the CC differs.  Fixtures live under
    /// `fixtures/out/x86_kernel/` (see `fixtures/arch/x86_kernel.mk`).
    X86Kernel,
    X64,
    Aarch64,
    Aarch64Be,
    Arm,
    ArmBe,
    ArmThumb,
    Mips32le,
    Mips32be,
    Mips64le,
    Mips64be,
    Ppc32be,
    Ppc32le,
    Ppc64be,
    Ppc64le,
}

impl Arch {
    pub fn name(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X86Kernel => "x86_kernel",
            Arch::X64 => "x64",
            Arch::Aarch64 => "aarch64",
            Arch::Aarch64Be => "aarch64be",
            Arch::Arm => "arm",
            Arch::ArmBe => "arm_be",
            Arch::ArmThumb => "arm_thumb",
            Arch::Mips32le => "mips32le",
            Arch::Mips32be => "mips32be",
            Arch::Mips64le => "mips64le",
            Arch::Mips64be => "mips64be",
            Arch::Ppc32be => "ppc32be",
            Arch::Ppc32le => "ppc32le",
            Arch::Ppc64be => "ppc64be",
            Arch::Ppc64le => "ppc64le",
        }
    }
    pub fn sleigh(self) -> strider::SleighArch {
        match self {
            // x86_kernel uses the same Sleigh spec as x86 — only the
            // calling convention differs.
            Arch::X86 | Arch::X86Kernel => strider::SleighArch::x86(),
            Arch::X64 => strider::SleighArch::x86_64(),
            Arch::Aarch64 => strider::SleighArch::aarch64(),
            Arch::Aarch64Be => strider::SleighArch::aarch64be(),
            Arch::Arm => strider::SleighArch::arm(),
            Arch::ArmBe => strider::SleighArch::arm_be(),
            Arch::ArmThumb => strider::SleighArch::arm_thumb(),
            Arch::Mips32le => strider::SleighArch::mipsle32(),
            Arch::Mips32be => strider::SleighArch::mipsbe32(),
            Arch::Mips64le => strider::SleighArch::mipsle64(),
            Arch::Mips64be => strider::SleighArch::mipsbe64(),
            Arch::Ppc32be => strider::SleighArch::ppc32be(),
            Arch::Ppc32le => strider::SleighArch::ppc32le(),
            Arch::Ppc64be => strider::SleighArch::ppc64be(),
            Arch::Ppc64le => strider::SleighArch::ppc64le(),
        }
    }
    pub fn cc(self) -> strider::CallingConvention {
        match self {
            Arch::X86 => strider::CallingConvention::x86_cdecl(),
            Arch::X86Kernel => strider::CallingConvention::x86_linux_kernel(),
            Arch::X64 => strider::CallingConvention::x86_64_systemv(),
            // AAPCS64 is byte-order independent; same CC for LE and BE AArch64.
            Arch::Aarch64 | Arch::Aarch64Be => strider::CallingConvention::aarch64_aapcs64(),
            // AAPCS32 is byte-order- and mode-independent — same CC for
            // ARM (LE), ARM-BE, and Thumb.
            Arch::Arm | Arch::ArmBe | Arch::ArmThumb => strider::CallingConvention::arm_aapcs(),
            // O32 ABI is the same on LE and BE 32-bit MIPS Linux.
            Arch::Mips32le | Arch::Mips32be => strider::CallingConvention::mips_o32(),
            // N64 ABI is the same on LE and BE 64-bit MIPS Linux.
            Arch::Mips64le | Arch::Mips64be => strider::CallingConvention::mips_n64(),
            // PowerPC SysV 32-bit is byte-order independent.
            Arch::Ppc32be | Arch::Ppc32le => strider::CallingConvention::powerpc_sysv32(),
            // PPC64: clang+lld defaults to ELFv2 for both BE and LE targets
            // (no function descriptors), so both paths use the v2 CC.  Use
            // the v1 preset only for explicit gcc-built ELFv1 binaries
            // (function-descriptor handling is a future strider feature).
            Arch::Ppc64be | Arch::Ppc64le => strider::CallingConvention::powerpc64_elf_v2(),
        }
    }
}

// ── Synthetic-fixture Strider builders ───────────────────────────────────────

/// Construct a `Strider` for x86_64 SystemV.  Used by tests that build
/// hand-assembled byte sequences and don't care about ELF loading.
pub fn strider_x86_64() -> strider::Strider {
    strider_for(Arch::X64)
}

/// Construct a `Strider` for `arch` using its preset calling
/// convention.  Probes Sleigh against an empty memory reader to
/// extract the register list.
pub fn strider_for(arch: Arch) -> strider::Strider {
    let sleigh_arch = arch.sleigh();
    let regs = sleigh_arch.probe_regs().expect("probe sleigh regs");
    strider::Strider::new(sleigh_arch, regs, arch.cc()).expect("Strider::new")
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
/// full strider + optimiser pipeline (with `LoadReadOnly` against the same
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
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let ana = strider::Strider::new(sleigh_arch, regs, arch.cc())
        .expect("Strider::new");
    let mem = reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("real sleigh new");
    let raw_addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
        .address();
    // ARM-Thumb interworking: a Thumb function symbol's address has the LSB
    // set as a "Thumb mode" marker (the actual instructions live at
    // `addr & !1` and are 2-byte aligned).  Sleigh expects the aligned
    // address; mask the marker off for ARM-class targets.
    let addr = match arch {
        Arch::Arm | Arch::ArmThumb => raw_addr & !1u64,
        _ => raw_addr,
    };
    // Thread the calling-convention link-register varnode and an
    // `Arc<dyn ReadOnlyMemory>` view of the ELF into the cfg builder so
    // the indirect-branch resolver can classify `bx lr` as Return and
    // fold rodata-stored jump targets.  The optimiser's `LoadReadOnly`
    // pass takes its own `ElfFileMemReader` (its `M: 'static` bound
    // requires an owned concrete type) — both readers see the same
    // ELF, just via separate Arcs.
    let rom_for_cfg: std::sync::Arc<dyn opt::ReadOnlyMemory> = std::sync::Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom reader (cfg)"),
    );
    let mut cfg_opts_b = cfg::OptionsBuilder::new()
        .allow_code_before_start_addr()
        .set_read_only_memory(rom_for_cfg);
    if let Some(lr) = ana.calling_convention().link_register_vn() {
        cfg_opts_b = cfg_opts_b.set_link_register(lr);
    }
    let cfg_opts = cfg_opts_b.build();
    // Use `for_arch` so both endianness AND `ArchPreset` are derived
    // from `sleigh_arch` atomically.  (The earlier `Builder::new` /
    // `Builder::with_endianness` ctors silently defaulted the preset
    // to `X86_64` and were deleted in round 12 W5c.)
    let cfg = cfg::Builder::for_arch(&sleigh_arch, sleigh, addr, cfg_opts)
        .build()
        .unwrap_or_else(|e| panic!("Cfg build for {fn_name}: {e:?}"));
    let mut graph = ana.analyze_cfg(&cfg)
        .unwrap_or_else(|e| panic!("analyze_cfg for {fn_name}: {e:?}"))
        .graph;
    let rom_for_opt = reader::ElfFileMemReader::from_object(&obj).expect("rom reader (opt)");
    let mut p = ana.build_optimizer_pipeline();
    p.add(opt::LoadReadOnly(rom_for_opt));
    p.run(&mut graph.graph, graph.entry)
        .unwrap_or_else(|e| panic!("optimizer pipeline for {fn_name}: {e:?}"));
    graph
}

/// Phase 3 Task 3.8 parity entry point.  Same shape as [`analyze`] but
/// runs the v2 [`opt::pipeline_v2::PipelineV2`] (interleaved
/// destructive+nondestructive fixed-point) instead of v1's
/// `Strider::build_optimizer_pipeline()`.
///
/// Returns the analyzed graph PLUS the v2 iteration count to
/// convergence (for reporting; the parity test averages across
/// fixtures).
pub fn analyze_v2(
    arch: Arch,
    case: &str,
    fn_name: &str,
) -> (ir::BuiltFunctionGraph, u32) {
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
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let ana = strider::Strider::new(sleigh_arch, regs, arch.cc())
        .expect("Strider::new");
    let mem = reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("real sleigh new");
    let raw_addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
        .address();
    let addr = match arch {
        Arch::Arm | Arch::ArmThumb => raw_addr & !1u64,
        _ => raw_addr,
    };
    let rom_for_cfg: std::sync::Arc<dyn opt::ReadOnlyMemory> = std::sync::Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom reader (cfg)"),
    );
    let mut cfg_opts_b = cfg::OptionsBuilder::new()
        .allow_code_before_start_addr()
        .set_read_only_memory(rom_for_cfg);
    if let Some(lr) = ana.calling_convention().link_register_vn() {
        cfg_opts_b = cfg_opts_b.set_link_register(lr);
    }
    let cfg_opts = cfg_opts_b.build();
    let cfg = cfg::Builder::for_arch(&sleigh_arch, sleigh, addr, cfg_opts)
        .build()
        .unwrap_or_else(|e| panic!("Cfg build for {fn_name}: {e:?}"));
    let mut graph = ana.analyze_cfg(&cfg)
        .unwrap_or_else(|e| panic!("analyze_cfg for {fn_name}: {e:?}"))
        .graph;

    // v2 pipeline: build PipelineV2 with the same ROM that v1's
    // `LoadReadOnly` overlay uses.  `Arc<dyn ReadOnlyMemory>` erases
    // the concrete reader so PipelineV2 doesn't need to be generic.
    let rom_for_opt: std::sync::Arc<dyn opt::ReadOnlyMemory> = std::sync::Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom reader (opt)"),
    );
    let pipeline = opt::pipeline_v2::PipelineV2::with_rom(
        ana.calling_convention(),
        sleigh_arch.endianness(),
        rom_for_opt,
    );
    let iters = pipeline
        .run(&mut graph.graph, graph.entry)
        .unwrap_or_else(|e| panic!("PipelineV2 for {fn_name}: {e:?}"));
    (graph, iters)
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

/// Counts the distinct control-flow paths converging at any `Return` node.
///
/// Some ABIs (PPC, aarch64) share the function epilogue: at `-O0` the compiler
/// still routes every source-level `return` through a single `blr`/`ret`, so
/// the IR has one `Return` node fed by a `ControlState` that merges the
/// individual paths.  `count_returns` reports `1` here even though there are
/// two source-level return statements.  This helper counts those merged
/// predecessors instead, giving a compiler-independent lower bound on the
/// number of source-level return paths.
///
/// Algorithm: for each `Return` node, look at its first input (the Control
/// predecessor — see `node_signature::expected_signature` for `Return`).  If
/// that producer is a `ControlState`, contribute its *immediate* fan-in;
/// otherwise contribute 1.  Sum across all reachable Return nodes.  Deeper
/// joins (a `ControlState` whose own predecessor is another `ControlState`)
/// are not transitively expanded — the result is therefore a lower bound on
/// the number of source-level return paths, sufficient for the
/// "≥ 2 return paths" assertions in this suite.
pub fn count_return_paths(g: &ir::BuiltFunctionGraph) -> usize {
    let mut total = 0usize;
    for nid in g.preorder() {
        if !matches!(g.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        // Return inputs: [Control, Memory, ...return values].  Slot 0 is the
        // Control predecessor.
        let inputs = g.graph.node_inputs(nid);
        let Some(ctrl_out) = inputs.get(0).copied() else {
            // A Return with no inputs is malformed; the validator would catch
            // it.  Treat as a single path so we don't silently drop it.
            total += 1;
            continue;
        };
        let pred = g.graph.get_node_from_output(ctrl_out);
        match g.graph.node_kind(pred) {
            // ControlState's control inputs form the leading run of its input
            // list (see node_signature: `inputs: []; in_tail: CTRL`), so the
            // total input count IS the predecessor count.
            NodeKind::ControlState => total += g.graph.node_inputs(pred).len(),
            _ => total += 1,
        }
    }
    total
}
/// Counts loop headers in the lifted CFG.
///
/// A "loop header" here is a `ControlState` whose predecessor set contains
/// at least one back-edge — a predecessor that is itself reachable from
/// the `ControlState` via forward control flow.  This is independent of
/// any `VarPhi` count, which can drop to zero when *every* tracked
/// variable is loop-invariant (e.g. a register that's read in the loop
/// header but never modified by the body — `RedundantPhis`'s self-ref
/// rule then collapses the phi to the entry value).
pub fn count_loops(g: &ir::BuiltFunctionGraph) -> usize {
    use std::collections::HashSet;
    let mut count = 0;
    let reachable: HashSet<_> = g.preorder().collect();
    for n in g.all_node_ids() {
        if !reachable.contains(&n) {
            continue;
        }
        if !matches!(g.graph.node_kind(n), NodeKind::ControlState) {
            continue;
        }
        // Back-edge detection: from each predecessor, walk forward
        // through Control outputs.  If we land back on `n`, that
        // predecessor closes a loop.
        let preds: Vec<_> = g.graph.node_inputs(n).into_iter().collect();
        let has_back_edge = preds.iter().any(|&pred_out| {
            let pred = g.graph.get_node_from_output(pred_out);
            let mut seen: HashSet<_> = HashSet::new();
            let mut stack = vec![pred];
            while let Some(cur) = stack.pop() {
                if !seen.insert(cur) {
                    continue;
                }
                for out in g.graph.node_outputs(cur) {
                    if !g.graph.output_kind(out).is_control() {
                        continue;
                    }
                    for (consumer, _) in g.graph.output_uses(out) {
                        if consumer == n {
                            return true;
                        }
                        stack.push(consumer);
                    }
                }
            }
            false
        });
        if has_back_edge {
            count += 1;
        }
    }
    count
}
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
    // IntConst stores u128; compare against the u64 value widened to u128.
    has_kind(g, |k| matches!(k, NodeKind::IntConst(c) if *c == u128::from(value)))
}

// ── per_arch_test! macro ─────────────────────────────────────────────────────

/// Generates one `#[test]` per (architecture, function) pair.
///
/// Basic form (all six archs run):
///   per_arch_test!("<case>", "<fn_name>", <assertion_fn>);
///
/// With per-arch ignores (specific archs are `#[ignore = "reason"]`-marked):
///   per_arch_test!("<case>", "<fn_name>", <assertion_fn>, ignore = {
///       Mips32le: "BUG-N: <one-line reason>",
///       Mips32be: "BUG-N: <one-line reason>",
///   });
///
/// Ignore reasons should reference an entry in
/// docs/superpowers/plans/2026-04-25-analyzer-known-issues.md so a future
/// reader can find the diagnosis and the fix path.
///
/// Implementation note: because Rust `macro_rules!` does not support ident
/// equality matching, the ignore block is parsed with individual per-arch
/// arms.  The outer macro converts each arch's entry into either a
/// `[ignored "reason"]` or `[run]` group, then the inner `__one_arch_test!`
/// helper uses that group as its last argument.
#[macro_export]
macro_rules! per_arch_test {
    // No-ignore shorthand.
    ($case:literal, $fn_name:literal, $assert:ident) => {
        per_arch_test!($case, $fn_name, $assert, ignore = {});
    };
    // Full form: parse the per-arch ignore list into a canonical group per arch.
    ($case:literal, $fn_name:literal, $assert:ident, ignore = { $($skip_arch:ident: $reason:literal),* $(,)? }) => {
        paste::paste! {
            mod [<test_ $fn_name>] {
                use super::*;
                // Resolve each arch's ignore entry (or lack thereof) and emit
                // the test function.  The inner `__one_arch_test!` macro
                // receives the ignore list verbatim and scans it for a
                // matching entry using dedicated per-arch arms.
                $crate::__one_arch_test!(X86,       x86,        $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(X86Kernel, x86_kernel, $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(X64,       x64,        $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Aarch64,   aarch64,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Aarch64Be, aarch64be, $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Arm,       arm,       $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(ArmBe,     arm_be,    $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(ArmThumb,  arm_thumb, $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Mips32le,  mips32le,  $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Mips32be,  mips32be,  $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Mips64le,  mips64le,  $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Mips64be,  mips64be,  $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Ppc32be,   ppc32be,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Ppc32le,   ppc32le,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Ppc64be,   ppc64be,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Ppc64le,   ppc64le,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
            }
        }
    };
}

// `__one_arch_test!` is a thin dispatcher.  The `ignore` block is an
// opaque `{ ... }` group; we forward it to a per-arch scanner
// (`__scan_ignore_x86!` etc.) which digs into the braces and either emits
// `#[ignore = $r] #[test] fn $fn() { ... }` or a plain `#[test] fn $fn() { ... }`.
//
// `paste!` builds the inner-macro name by lower-casing the arch token
// (e.g. `Aarch64Be` → `__scan_ignore_aarch64be`).  This collapses what
// used to be 15 hand-written dispatcher arms into a single arm.
//
// The `$fn:ident` token sequence after the function name is a literal
// type-tag that the per-arch scanners require in their patterns; see
// each `__scan_ignore_<arch>!` definition below.

#[doc(hidden)]
#[macro_export]
macro_rules! __one_arch_test {
    ($arch:ident, $fn:ident, $case:literal, $fn_name:literal, $assert:ident $ignore_block:tt) => {
        paste::paste! {
            $crate::[<__scan_ignore_ $arch:lower>]!($fn:ident, $case, $fn_name, $assert, $ignore_block);
        }
    };
}

// Per-arch scanners.  Each macro scans its `{ ... }` group for its own
// arch key.  Arms: found (emit ignored test) | skip one entry | empty (emit
// plain test).  Using `{ ... }` groups means the outer `,` in the list is
// inside braces and is NOT part of the macro argument separator — so there is
// no ambiguity for the `:` token inside each entry.

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_x86 {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { X86: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::X86, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_x86!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::X86, $case, $fn_name);
            $assert(&g);
        }
    };
}

// `paste!` lower-cases `X86Kernel` to `x86kernel` (no underscore), so
// the dispatcher's name is `__scan_ignore_x86kernel` — not
// `__scan_ignore_x86_kernel`.  The fixture path / fn-name suffix
// uses the underscored form (`x86_kernel`) via the `$fn` argument.
#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_x86kernel {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { X86Kernel: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::X86Kernel, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_x86kernel!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::X86Kernel, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_x64 {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { X64: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::X64, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_x64!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::X64, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_aarch64 {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Aarch64: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Aarch64, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_aarch64!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Aarch64, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_arm {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Arm: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Arm, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_arm!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Arm, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_mips32le {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Mips32le: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Mips32le, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_mips32le!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Mips32le, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_mips32be {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Mips32be: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Mips32be, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_mips32be!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Mips32be, $case, $fn_name);
            $assert(&g);
        }
    };
}

// ── New-arch scanners (factored via `__scan_ignore_for!` for brevity) ──────
//
// Each scanner needs three arms: match-self / skip-other / empty.  The
// pattern is identical for every arch — the arch ident and the `Arch::*`
// variant are the only differences.  Defining a helper macro that takes
// these as arguments would deepen the macro recursion (and hit the
// recursion limit on long-chain ignore lists).  Since the per-arch
// pattern is mechanical, we keep it explicit per arch — same shape as
// the existing ones.

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_aarch64be {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Aarch64Be: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Aarch64Be, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_aarch64be!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Aarch64Be, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_armthumb {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { ArmThumb: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::ArmThumb, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_armthumb!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::ArmThumb, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_armbe {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { ArmBe: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::ArmBe, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_armbe!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::ArmBe, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_mips64le {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Mips64le: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Mips64le, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_mips64le!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Mips64le, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_mips64be {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Mips64be: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Mips64be, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_mips64be!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Mips64be, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_ppc32be {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Ppc32be: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Ppc32be, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_ppc32be!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Ppc32be, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_ppc32le {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Ppc32le: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Ppc32le, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_ppc32le!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    // Empty list: run normally.  The ppc32le.mk build flags work around
    // Debian's BE-only libgcc by linking with `-nodefaultlibs
    // --unresolved-symbols=ignore-all` so we get real LE executables
    // (with stub relocations for libgcc helpers we never call into).
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Ppc32le, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_ppc64be {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Ppc64be: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Ppc64be, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_ppc64be!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    // Empty list: run normally.  The ppc64be.mk build flags switched to
    // clang+lld which DEFAULTS to ELFv2 (no function descriptors) even
    // for the BE target — sidesteps the gcc-side ELFv1 .opd problem.
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Ppc64be, $case, $fn_name);
            $assert(&g);
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_ppc64le {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { Ppc64le: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Ppc64le, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_ppc64le!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::Ppc64le, $case, $fn_name);
            $assert(&g);
        }
    };
}
