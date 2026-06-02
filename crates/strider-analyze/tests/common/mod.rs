//! Shared infrastructure for the system-test suite.
//!
//! Every `tests/<category>.rs` declares `mod common;` and uses these helpers.
//! Adding a new test case is mechanical:
//!
//! ```ignore
//! per_arch_test!("arithmetic", "add", graph_must_contain_int_add);
//! fn graph_must_contain_int_add(g: &strider_ir::Function) {
//!     assert!(common::count_int_binop(g, strider_ir::IntBinaryOp::Add) >= 1);
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
    clippy::todo,
    clippy::crate_in_macro_def,
    dead_code  // category test files won't use every helper
)]

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

// Sub-module containing fixture builders for the indirect-branch classifier
// integration tests in `tests/indirect_resolve_classify.rs`.  Kept as a sub-module
// so the rest of the per-arch fixture infrastructure above remains
// unchanged.
pub mod indirect_resolve_helpers;

// Sub-module with the shared lift + temp-dir setup used by the
// `dump_per_region` / `dump_neighborhood` integration tests.
pub mod dump_helpers;

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

/// Every supported `Arch` variant in the same order they appear in
/// `per_arch_test!`.  Use this from any test that wants to iterate
/// the full arch matrix (e.g. cross-arch shape baselines).  Keeping
/// a single canonical list here prevents drift between callers and
/// the `Arch` enum.
pub const ALL_ARCHES: &[Arch] = &[
    Arch::X86,
    Arch::X86Kernel,
    Arch::X64,
    Arch::Aarch64,
    Arch::Aarch64Be,
    Arch::Arm,
    Arch::ArmBe,
    Arch::ArmThumb,
    Arch::Mips32le,
    Arch::Mips32be,
    Arch::Mips64le,
    Arch::Mips64be,
    Arch::Ppc32be,
    Arch::Ppc32le,
    Arch::Ppc64be,
    Arch::Ppc64le,
];

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
    pub fn sleigh(self) -> strider_target::SleighArch {
        match self {
            // x86_kernel uses the same Sleigh spec as x86 — only the
            // calling convention differs.
            Arch::X86 | Arch::X86Kernel => strider_target::SleighArch::x86(),
            Arch::X64 => strider_target::SleighArch::x86_64(),
            Arch::Aarch64 => strider_target::SleighArch::aarch64(),
            Arch::Aarch64Be => strider_target::SleighArch::aarch64be(),
            Arch::Arm => strider_target::SleighArch::arm(),
            Arch::ArmBe => strider_target::SleighArch::arm_be(),
            Arch::ArmThumb => strider_target::SleighArch::arm_thumb(),
            Arch::Mips32le => strider_target::SleighArch::mipsle32(),
            Arch::Mips32be => strider_target::SleighArch::mipsbe32(),
            Arch::Mips64le => strider_target::SleighArch::mipsle64(),
            Arch::Mips64be => strider_target::SleighArch::mipsbe64(),
            Arch::Ppc32be => strider_target::SleighArch::ppc32be(),
            Arch::Ppc32le => strider_target::SleighArch::ppc32le(),
            Arch::Ppc64be => strider_target::SleighArch::ppc64be(),
            Arch::Ppc64le => strider_target::SleighArch::ppc64le(),
        }
    }
    pub fn cc(self) -> strider_target::CallingConvention {
        let preset = match self {
            Arch::X86 => strider_target::CallingConvention::x86_cdecl(),
            Arch::X86Kernel => strider_target::CallingConvention::x86_linux_kernel(),
            Arch::X64 => strider_target::CallingConvention::x86_64_systemv(),
            // AAPCS64 is byte-order independent; same CC for LE and BE AArch64.
            Arch::Aarch64 | Arch::Aarch64Be => strider_target::CallingConvention::aarch64_aapcs64(),
            // AAPCS32 is byte-order- and mode-independent — same CC for
            // ARM (LE), ARM-BE, and Thumb.
            Arch::Arm | Arch::ArmBe | Arch::ArmThumb => strider_target::CallingConvention::arm_aapcs(),
            // O32 ABI is the same on LE and BE 32-bit MIPS Linux.
            Arch::Mips32le | Arch::Mips32be => strider_target::CallingConvention::mips_o32(),
            // N64 ABI is the same on LE and BE 64-bit MIPS Linux.
            Arch::Mips64le | Arch::Mips64be => strider_target::CallingConvention::mips_n64(),
            // PowerPC SysV 32-bit is byte-order independent.
            Arch::Ppc32be | Arch::Ppc32le => strider_target::CallingConvention::powerpc_sysv32(),
            // PPC64: clang+lld defaults to ELFv2 for both BE and LE targets
            // (no function descriptors), so both paths use the v2 CC.  Use
            // the v1 preset only for explicit gcc-built ELFv1 binaries
            // (function-descriptor handling is a future strider feature).
            Arch::Ppc64be | Arch::Ppc64le => strider_target::CallingConvention::powerpc64_elf_v2(),
        };
        preset.expect("CC preset must be registered for this arch")
    }
}

// ── Synthetic-fixture Strider builders ───────────────────────────────────────

/// Construct a `LiftDriver` for x86_64 SystemV.  Used by tests that build
/// hand-assembled byte sequences and don't care about ELF loading.
pub fn strider_x86_64() -> strider_analyze::LiftDriver {
    strider_for(Arch::X64)
}

/// Construct a `LiftDriver` for AArch64 AAPCS64.  Sibling of
/// [`strider_x86_64`] for the handful of synthetic-fixture tests that
/// need an LR-bearing CC (e.g. `bug_on_lifts_cleanly`'s `bx lr`
/// regression case).
pub fn strider_aarch64() -> strider_analyze::LiftDriver {
    strider_for(Arch::Aarch64)
}

/// Construct a `LiftDriver` for `arch` using its preset calling
/// convention.  Probes Sleigh against an empty memory reader to
/// extract the register list.
pub fn strider_for(arch: Arch) -> strider_analyze::LiftDriver {
    let sleigh_arch = arch.sleigh();
    let regs = sleigh_arch.probe_regs().expect("probe sleigh regs");
    strider_analyze::LiftDriver::new(sleigh_arch, regs, arch.cc()).expect("LiftDriver::new")
}

// ── Synthetic x86-64 jump-table fixture builders ─────────────────────────────

/// Build a synthetic x86-64 binary: `jmp rax` (2 bytes at `0x1000`)
/// followed by `n_targets` × `ret` (0xc3), padded with 16 × `int3`
/// (0xcc) so speculative look-ahead past the last `ret` doesn't fault
/// the `BufMemReader`.
///
/// Returns `(bytes, base_addr, branch_indirect_addr, target_addrs)`.
/// `branch_indirect_addr == base_addr == 0x1000`; targets are at
/// `0x1002`, `0x1003`, … (each `ret` is 1 byte).
pub fn synth_jmp_rax_with_targets(n_targets: usize) -> (Vec<u8>, u64, u64, Vec<u64>) {
    let base = 0x1000u64;
    let mut bytes = vec![0xffu8, 0xe0]; // jmp rax — 2 bytes at 0x1000
    let mut target_addrs = Vec::with_capacity(n_targets);
    for i in 0..n_targets {
        let target_addr = base + 2 + i as u64; // 0x1002, 0x1003, ...
        target_addrs.push(target_addr);
        bytes.push(0xc3); // ret
    }
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let branch_indirect_addr = base;
    (bytes, base, branch_indirect_addr, target_addrs)
}

/// Lift `bytes` via `analyze_cfg` with `with_known_targets` seeding the
/// `BranchIndirect` at `branch_indirect_addr` to `Multiple(targets)`.
///
/// Returns `(graph, strider)` so callers can drive the optimizer with
/// the convention-aware pipeline.  Panics on any construction failure.
pub fn analyze_with_known_targets(
    bytes: &[u8],
    base: u64,
    branch_indirect_addr: u64,
    targets: &[u64],
) -> (strider_ir::Function, strider_analyze::LiftDriver) {
    use rustc_hash::FxHashMap;
    use strider_lift::cfg::{Builder, MachineInsnAddr, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};

    let arch = strider_target::SleighArch::x86_64();
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes.to_vec(), base);
    let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .expect("create x86_64 sleigh");
    let mut known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known_targets.insert(
        PcodeInsnAddr { machine_addr: MachineInsnAddr::from(branch_indirect_addr), insn_index: 0 },
        ResolvedTargets::Multiple(targets.to_vec()),
    );
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, opts)
        .with_known_targets(known_targets)
        .build()
        .expect("cfg build with Multiple known targets");

    let strider = strider_x86_64();
    let function = strider.analyze_cfg(&cfg, &sleigh).expect("analyze_cfg").function;
    (function, strider)
}

// ── Binary path resolution ───────────────────────────────────────────────────

pub fn binary_path(arch: Arch, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch.name())
        .join(format!("{case}.elf"))
}

// ── Pipeline runner ──────────────────────────────────────────────────────────

/// Internal helper: load the (arch, case) ELF, build a CFG at `fn_name`,
/// and lift it to IR.  Returns the full [`AnalyzeOutcome`] (so callers
/// that need `unresolved_branches` get it), the strider instance, the
/// sleigh arch (for endianness), and an Arc-shared ROM that callers
/// can use to drive their optimizer pipeline.
///
/// Shared between [`analyze`] (which discards
/// `unresolved_branches`) and `indirect_branch.rs`'s
/// `assert_no_unresolved_indirect_branch` (which needs both halves of
/// the outcome).
pub fn lift_for_pipeline(
    arch: Arch,
    case: &str,
    fn_name: &str,
) -> (
    strider_analyze::AnalyzeOutcome,
    strider_analyze::LiftDriver,
    strider_target::SleighArch,
    strider_reader::ElfFileMemReader,
) {
    let path = binary_path(arch, case);
    if !path.exists() {
        panic!(
            "missing test binary {path:?}; run `make -C fixtures` (or \
             `make -C fixtures ARCH={} CASE={case}` for just this case)",
            arch.name()
        );
    }
    let obj = strider_reader::load_elf(&path)
        .unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
    let sleigh_arch = arch.sleigh();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let ana = strider_analyze::LiftDriver::new(sleigh_arch, regs, arch.cc())
        .expect("LiftDriver::new");
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let mut sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
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
    let rom_for_cfg = strider_reader::ElfFileMemReader::from_object(&obj)
        .expect("rom reader (cfg)");
    let mut cfg_opts_b = strider_lift::cfg::OptionsBuilder::new()
        .allow_code_before_start_addr();
    if let Some(lr) = ana.calling_convention().link_register_vn {
        cfg_opts_b = cfg_opts_b.set_link_register(lr);
    }
    let cfg_opts = cfg_opts_b.build();
    // Use `for_arch` so both endianness AND `ArchPreset` are derived
    // from `sleigh_arch` atomically.  (Earlier `Builder::new` /
    // `Builder::with_endianness` ctors silently defaulted the preset
    // to `X86_64`; they are no longer exposed.)
    let cfg = strider_lift::cfg::Builder::for_arch(&sleigh_arch, &mut sleigh, addr, cfg_opts)
        .with_read_only_memory(&rom_for_cfg)
        .build()
        .unwrap_or_else(|e| panic!("Cfg build for {fn_name}: {e:?}"));
    let outcome = ana.analyze_cfg(&cfg, &sleigh)
        .unwrap_or_else(|e| panic!("analyze_cfg for {fn_name}: {e:?}"));
    let rom_for_opt = strider_reader::ElfFileMemReader::from_object(&obj)
        .expect("rom reader (opt)");
    (outcome, ana, sleigh_arch, rom_for_opt)
}

/// Loads the (arch, case) ELF, builds a CFG starting at `fn_name`, runs the
/// production optimiser pipeline
/// ([`LiftDriver::build_optimizer_pipeline`] + `LoadReadOnly`) over the
/// lifted IR, and returns the resulting graph.
///
/// Test fixtures are well-behaved compiler-emitted binaries (gcc/clang
/// at -O0/-O2 from `fixtures/cases/*.c`), so this helper opts into
/// [`crate::opt::AliasMode::AssumeStackGlobalDisjoint`] — globals never
/// alias the stack frame in such binaries, and the relaxed walker
/// recovers the spill/reload forwarding the assertions depend on.
/// Tests of the strict default belong in unit tests with a directly-
/// constructed pipeline.
///
/// Panics on any failure — system tests are pass/fail end-to-end checks.  If
/// the binary is missing, the panic carries an actionable message including
/// the `make -C fixtures` instruction.
pub fn analyze(arch: Arch, case: &str, fn_name: &str) -> strider_ir::Function {
    let (outcome, ana, sleigh_arch, rom_for_opt) =
        lift_for_pipeline(arch, case, fn_name);
    let ana = ana.with_alias_mode(strider_analyze::opt::AliasMode::AssumeStackGlobalDisjoint);
    let mut function = outcome.function;
    let mut p = ana.build_optimizer_pipeline();
    p.add(strider_analyze::opt::LoadReadOnly);
    // Decode ROM bytes per the fixture's target byte order — the reader
    // no longer decodes, so the OptCtx must carry the run's endianness
    // (big-endian fixtures like arm_be / mips_be otherwise mis-fold).
    let ctx =
        strider_analyze::opt::OptCtx::with_rom_endian(&rom_for_opt, sleigh_arch.endianness());
    p.run(&mut function, &ctx)
        .unwrap_or_else(|e| panic!("optimizer pipeline for {fn_name}: {e:?}"));
    function
}

// ── Assertion vocabulary ─────────────────────────────────────────────────────
//
// All counters walk the graph in pre-order and filter on the node kind.
// Naming convention: `count_<thing>` returns a `usize`; `has_<thing>` returns a `bool`.

use strider_ir::node::NodeKind;

// Re-export the canonical `Function::count_kind` / `Function::has_kind` under
// their bare names so existing test call-sites need no qualification.
pub fn count_kind<F: Fn(&NodeKind) -> bool>(function: &strider_ir::Function, pred: F) -> usize {
    function.count_kind(pred)
}

pub fn count_int_binop(function: &strider_ir::Function, op: strider_ir::IntBinaryOp) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::IntBinaryOp(o) if *o == op))
}
pub fn count_int_unop(function: &strider_ir::Function, op: strider_ir::IntUnaryOp) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::IntUnaryOp(o) if *o == op))
}
pub fn count_int_cmp(function: &strider_ir::Function, op: strider_ir::IntCmpOp) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::IntCmpOp(o) if *o == op))
}
pub fn count_float_binop(function: &strider_ir::Function, op: strider_ir::FloatBinaryOp) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::FloatBinaryOp(o) if *o == op))
}
pub fn count_float_unop(function: &strider_ir::Function, op: strider_ir::FloatUnaryOp) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::FloatUnaryOp(o) if *o == op))
}
pub fn count_float_cmp(function: &strider_ir::Function, op: strider_ir::FloatCmpOp) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::FloatCmpOp(o) if *o == op))
}

pub fn count_calls (function: &strider_ir::Function) -> usize { count_kind(function, |k| matches!(k, NodeKind::Call)) }
pub fn count_ifs   (function: &strider_ir::Function) -> usize { count_kind(function, |k| matches!(k, NodeKind::If)) }
pub fn count_returns(function: &strider_ir::Function) -> usize { count_kind(function, |k| matches!(k, NodeKind::Return)) }

/// Counts the distinct control-flow paths converging at any `Return` node.
///
/// Some ABIs (PPC, aarch64) share the function epilogue: at `-O0` the compiler
/// still routes every source-level `return` through a single `blr`/`ret`, so
/// the IR has one `Return` node fed by a `Region` that merges the
/// individual paths.  `count_returns` reports `1` here even though there are
/// two source-level return statements.  This helper counts those merged
/// predecessors instead, giving a compiler-independent lower bound on the
/// number of source-level return paths.
///
/// Algorithm: for each `Return` node, look at its first input (the Control
/// predecessor — see `node_signature::expected_signature` for `Return`).  If
/// that producer is a `Region`, contribute its *immediate* fan-in;
/// otherwise contribute 1.  Sum across all reachable Return nodes.  Deeper
/// joins (a `Region` whose own predecessor is another `Region`)
/// are not transitively expanded — the result is therefore a lower bound on
/// the number of source-level return paths, sufficient for the
/// "≥ 2 return paths" assertions in this suite.
pub fn count_return_paths(function: &strider_ir::Function) -> usize {
    let mut total = 0usize;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::Return) {
            continue;
        }
        // Return inputs: [Control, Memory, ...return values].  Slot 0 is the
        // Control predecessor.
        let inputs = function.node_inputs(nid);
        let Some(ctrl_out) = inputs.get(0).copied() else {
            // A Return with no inputs is malformed; the validator would catch
            // it.  Treat as a single path so we don't silently drop it.
            total += 1;
            continue;
        };
        let pred = function.producer(ctrl_out);
        match function.node_kind(pred) {
            // Region's control inputs form the leading run of its input
            // list (see node_signature: `inputs: []; in_tail: CTRL`), so the
            // total input count IS the predecessor count.
            NodeKind::Region => total += function.node_inputs(pred).len(),
            _ => total += 1,
        }
    }
    total
}
/// Counts loop headers in the lifted CFG.
///
/// A "loop header" here is a `Region` whose predecessor set contains
/// at least one back-edge — a predecessor that is itself reachable from
/// the `Region` via forward control flow.  This is independent of
/// any `VarPhi` count, which can drop to zero when *every* tracked
/// variable is loop-invariant (e.g. a register that's read in the loop
/// header but never modified by the body — `PhiCollapse`'s self-ref
/// rule then collapses the phi to the entry value).
pub fn count_loops(function: &strider_ir::Function) -> usize {
    use entity_utils::DenseEntitySet;
    let mut count = 0;
    let reachable: DenseEntitySet<strider_ir::node::NodeId> = function.walk().collect();
    for n in function.all_node_ids() {
        if !reachable.contains(n) {
            continue;
        }
        if !matches!(function.node_kind(n), NodeKind::Region) {
            continue;
        }
        // Back-edge detection: from each predecessor, walk forward
        // through Control outputs.  If we land back on `n`, that
        // predecessor closes a loop.
        let preds: Vec<_> = function.node_inputs(n).into_iter().collect();
        let has_back_edge = preds.iter().any(|&pred_out| {
            let pred = function.producer(pred_out);
            let mut seen: DenseEntitySet<strider_ir::node::NodeId> = DenseEntitySet::new();
            let mut stack = vec![pred];
            while let Some(cur) = stack.pop() {
                if !seen.insert(cur) {
                    continue;
                }
                for &out in function.node_outputs(cur) {
                    if !function.value_kind(out).is_control() {
                        continue;
                    }
                    for (consumer, _) in function.value_uses(out) {
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
pub fn count_loads (function: &strider_ir::Function) -> usize { count_kind(function, |k| matches!(k, NodeKind::Load(_))) }
pub fn count_stores(function: &strider_ir::Function) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::Store(_)))
}
pub fn count_popcount(function: &strider_ir::Function) -> usize { count_kind(function, |k| matches!(k, NodeKind::Popcount)) }
pub fn count_lzcount (function: &strider_ir::Function) -> usize { count_kind(function, |k| matches!(k, NodeKind::Lzcount)) }
pub fn count_int_consts(function: &strider_ir::Function) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::IntConst(_)))
}

pub fn has_kind<F: Fn(&NodeKind) -> bool>(function: &strider_ir::Function, pred: F) -> bool {
    function.has_kind(pred)
}

pub fn has_constant(function: &strider_ir::Function, value: u64) -> bool {
    // IntConst stores u128; compare against the u64 value widened to u128.
    has_kind(function, |k| matches!(k, NodeKind::IntConst(c) if *c == u128::from(value)))
}

/// Locates the unique `If` node in `g`.  Panics if zero or more than one
/// is present — either case indicates a fixture-construction bug.  Use this
/// helper when the test asserts on the condition of a known-unique `If` node
/// rather than counting `If` nodes via [`count_ifs`].
pub fn find_unique_if(function: &strider_ir::Function) -> strider_ir::node::NodeId {
    let mut iter = function
        .all_node_ids()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::If));
    let first = iter.next().expect("fixture must contain exactly one If node");
    assert!(
        iter.next().is_none(),
        "fixture has more than one If node",
    );
    first
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
            // Per-arch scanners are emitted by `__define_scan_ignore!` (a
            // macro-generated `#[macro_export] macro_rules!`).  Rust does
            // not allow `$crate::name` lookup for macros that are
            // themselves the output of macro expansion (issue #52234), so
            // we reach the scanner by its unqualified name — `#[macro_export]`
            // hoists it to the test crate's root prelude.
            [<__scan_ignore_ $arch:lower>]!($fn:ident, $case, $fn_name, $assert, $ignore_block);
        }
    };
}

// Per-arch scanners.  Each scanner needs three arms: match-self (head of
// the ignore list names this arch — emit an ignored test); skip-other
// (head names some other arch — recurse on the tail); empty (no entry —
// emit a plain test).  The match-self arm needs its arch ident as a
// literal token because `macro_rules!` lacks ident equality — so we
// generate one scanner per arch.
//
// `__define_scan_ignore!($arch_lower, $arch_camel)` stamps out one
// scanner.  All 16 scanners are listed in the invocations below; adding
// a new arch is a one-line addition.
//
// The `$d:tt` parameter is the classic "dollar token" trick for nesting
// `macro_rules!` definitions on stable Rust: when an outer macro emits
// an inner `macro_rules!`, we cannot write a bare `$` for the inner
// metavariables (it would bind in the outer scope), so we accept `$` as
// a token parameter and use `$d` wherever we need a literal `$` in the
// emitted inner macro.
//
// (Earlier comments worried that adding a generator macro on top of the
// existing per-arch scanners would deepen the recursion past the default
// `recursion_limit`.  That concern only applied to the *runtime* dispatch
// — the skip-other arm recurses through the ignore list once per arch.
// `__define_scan_ignore!` runs at macro definition time and does not
// participate in the runtime recursion chain.)
#[doc(hidden)]
#[macro_export]
macro_rules! __define_scan_ignore {
    ($d:tt $arch_lower:ident, $arch_camel:ident) => {
        paste::paste! {
            #[doc(hidden)]
            #[macro_export]
            macro_rules! [<__scan_ignore_ $arch_lower>] {
                ($d fn:ident : ident, $d case:literal, $d fn_name:literal, $d assert:ident,
                 { $arch_camel: $d reason:literal $d(, $d($d _rest:tt)*)? }) => {
                    #[test] #[ignore = $d reason]
                    fn $d fn() {
                        let function = $d crate::common::analyze($d crate::common::Arch::$arch_camel, $d case, $d fn_name);
                        $d assert(&function);
                    }
                };
                ($d fn:ident : ident, $d case:literal, $d fn_name:literal, $d assert:ident,
                 { $d _skip:ident: $d _r:literal $d(, $d($d rest:tt)*)? }) => {
                    [<__scan_ignore_ $arch_lower>]!($d fn:ident, $d case, $d fn_name, $d assert, { $d($d($d rest)*)? });
                };
                ($d fn:ident : ident, $d case:literal, $d fn_name:literal, $d assert:ident, { $d(,)? }) => {
                    #[test]
                    fn $d fn() {
                        let function = $d crate::common::analyze($d crate::common::Arch::$arch_camel, $d case, $d fn_name);
                        $d assert(&function);
                    }
                };
            }
        }
    };
}

__define_scan_ignore!($ x86,        X86);
__define_scan_ignore!($ x86kernel,  X86Kernel);
__define_scan_ignore!($ x64,        X64);
__define_scan_ignore!($ aarch64,    Aarch64);
__define_scan_ignore!($ aarch64be,  Aarch64Be);
__define_scan_ignore!($ arm,        Arm);
__define_scan_ignore!($ armbe,      ArmBe);
__define_scan_ignore!($ armthumb,   ArmThumb);
__define_scan_ignore!($ mips32le,   Mips32le);
__define_scan_ignore!($ mips32be,   Mips32be);
__define_scan_ignore!($ mips64le,   Mips64le);
__define_scan_ignore!($ mips64be,   Mips64be);
__define_scan_ignore!($ ppc32be,    Ppc32be);
__define_scan_ignore!($ ppc32le,    Ppc32le);
__define_scan_ignore!($ ppc64be,    Ppc64be);
__define_scan_ignore!($ ppc64le,    Ppc64le);
