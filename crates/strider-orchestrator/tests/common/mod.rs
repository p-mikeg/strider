//! `per_arch_test!("<case>", "<fn_name>", <assertion_fn>)` expands to one
//! `#[test]` per arch in `ALL_ARCHES`. Each loads
//! `fixtures/out/<arch>/<case>.elf`, analyses the named symbol, runs the
//! optimiser pipeline over the ELF's read-only mappings, and hands the
//! resulting `Function` to the assertion.

#![allow(
    clippy::todo,
    clippy::crate_in_macro_def,
    dead_code  // category test files won't use every helper
)]

use object::{Object, ObjectSymbol};
use std::path::PathBuf;
use strider_ir::{IRViewer, IRWalker};
use strider_ir_test_utils::IrWalkerEx;

// Fixture builders for the indirect-branch classifier integration tests
// (see `indirect_resolve_helpers/mod.rs`).
pub(crate) mod indirect_resolve_helpers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arch {
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

/// Every supported `Arch` variant, in `per_arch_test!` order. The canonical
/// list for any test iterating the full arch matrix.
pub(crate) const ALL_ARCHES: &[Arch] = &[
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
    pub(crate) fn name(self) -> &'static str {
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
    pub(crate) fn sleigh(self) -> strider_target::SleighArch {
        match self {
            // x86_kernel uses the same Sleigh spec as x86; only the
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
    pub(crate) fn cc(self) -> strider_target::CallingConvention {
        match self {
            Arch::X86 => strider_target::CallingConvention::x86_cdecl(),
            Arch::X86Kernel => strider_target::CallingConvention::x86_linux_kernel(),
            Arch::X64 => strider_target::CallingConvention::x86_64_systemv(),
            // AAPCS64 is byte-order independent; same CC for LE and BE AArch64.
            Arch::Aarch64 | Arch::Aarch64Be => strider_target::CallingConvention::aarch64_aapcs64(),
            // AAPCS32 is byte-order- and mode-independent; same CC for
            // ARM (LE), ARM-BE, and Thumb.
            Arch::Arm | Arch::ArmBe | Arch::ArmThumb => {
                strider_target::CallingConvention::arm_aapcs()
            }
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
        }
    }
}

/// The `Lifter` owns the `Sleigh`, so it is bound to this one memory reader
/// for its lifetime.
pub(crate) fn driver_for_reader<R: rsleigh::MemReader>(
    arch: Arch,
    reader: R,
) -> (
    strider_orchestrator::Lifter<R>,
    strider_target::BuiltCallingConvention,
) {
    let sleigh_arch = arch.sleigh();
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), reader)
        .expect("create sleigh");
    let driver = strider_orchestrator::Lifter::new(sleigh_arch, sleigh).expect("Lifter::new");
    let cc = arch
        .cc()
        .build(driver.sleigh_regs())
        .expect("build cc against driver regs");
    (driver, cc)
}

pub(crate) fn strider_x86_64<R: rsleigh::MemReader>(
    reader: R,
) -> (
    strider_orchestrator::Lifter<R>,
    strider_target::BuiltCallingConvention,
) {
    driver_for_reader(Arch::X64, reader)
}

/// AAPCS64, the CC used by synthetic fixtures that need a link register.
pub(crate) fn strider_aarch64<R: rsleigh::MemReader>(
    reader: R,
) -> (
    strider_orchestrator::Lifter<R>,
    strider_target::BuiltCallingConvention,
) {
    driver_for_reader(Arch::Aarch64, reader)
}

pub(crate) fn strider_mips32le<R: rsleigh::MemReader>(
    reader: R,
) -> (
    strider_orchestrator::Lifter<R>,
    strider_target::BuiltCallingConvention,
) {
    driver_for_reader(Arch::Mips32le, reader)
}

/// Build a synthetic x86-64 binary: `jmp rax` (2 bytes at `0x1000`)
/// followed by `n_targets` x `ret` (0xc3), padded with 16 x `int3`
/// (0xcc) so speculative look-ahead past the last `ret` doesn't fault
/// the `BufMemReader`.
///
/// Returns `(bytes, base_addr, branch_indirect_addr, target_addrs)`.
/// `branch_indirect_addr == base_addr == 0x1000`; targets are at
/// `0x1002`, `0x1003`, ... (each `ret` is 1 byte).
pub(crate) fn synth_jmp_rax_with_targets(n_targets: usize) -> (Vec<u8>, u64, u64, Vec<u64>) {
    let base = 0x1000u64;
    let mut bytes = vec![0xffu8, 0xe0]; // jmp rax
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

/// Lift `bytes` via `build_ir`, seeding `CfgOptions::known_targets` so the
/// `BranchIndirect` at `branch_indirect_addr` resolves to `Multiple(targets)`.
///
/// Returns `(function, driver, cc)`. Panics on any construction failure.
pub(crate) fn analyze_with_known_targets(
    bytes: &[u8],
    base: u64,
    branch_indirect_addr: u64,
    targets: &[u64],
) -> (
    strider_ir::Function,
    strider_orchestrator::Lifter<rsleigh::mem_readers::BufMemReader<Vec<u8>>>,
    strider_target::BuiltCallingConvention,
) {
    use rustc_hash::FxHashMap;
    use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};

    let reader = rsleigh::mem_readers::BufMemReader::new(bytes.to_vec(), base);
    let (mut driver, cc) = strider_x86_64(reader);

    let mut known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known_targets.insert(
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr::from(branch_indirect_addr),
            insn_index: 0,
        },
        ResolvedTargets::Multiple(targets.iter().copied().map(Into::into).collect()),
    );
    let cfg_opts = strider_cfg::CfgOptions {
        known_targets,
        ..Default::default()
    };
    let cfg = driver
        .build_cfg(MachineInsnAddr::from(base), &cfg_opts, &Default::default())
        .expect("cfg build with Multiple known targets");

    // build_ir consumes cc by value; clone so the caller also gets an
    // owned cc back.
    let function = driver
        .build_ir(&cfg, cc.clone())
        .expect("build_ir")
        .function;
    (function, driver, cc)
}

pub(crate) fn binary_path(arch: Arch, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch.name())
        .join(format!("{case}.elf"))
}

/// Loads the (arch, case) ELF, builds a CFG at `fn_name`, and lifts it. The
/// trailing `ElfFileMemReader` is a second view of the same image for the
/// optimiser's ROM: the first is moved into the `Lifter`.
pub(crate) fn lift_for_pipeline(
    arch: Arch,
    case: &str,
    fn_name: &str,
) -> (
    strider_orchestrator::LiftOutcome,
    strider_orchestrator::Lifter<strider_reader::ElfFileMemReader>,
    strider_target::BuiltCallingConvention,
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
    let obj = obj.file();
    let sleigh_arch = arch.sleigh();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let (mut ana, cc) = driver_for_reader(arch, mem);
    let raw_addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
        .address();
    // The symbol address is passed through with its ARM-Thumb interworking bit
    // intact: that bit IS the entry's ISA mode, and `Lifter::build_cfg` masks
    // it off for decoding itself.  Masking here would decode every Thumb
    // function as ARM.
    let addr = raw_addr;
    let cfg_opts = strider_cfg::CfgOptions {
        allow_code_before_start_addr: true,
        ..Default::default()
    };
    let cfg = ana
        .build_cfg(
            strider_cfg::MachineInsnAddr::from(addr),
            &cfg_opts,
            &Default::default(),
        )
        .unwrap_or_else(|e| panic!("Cfg build for {fn_name}: {e:?}"));
    // build_ir consumes cc by value; clone so the caller also gets an
    // owned cc back.
    let outcome = ana
        .build_ir(&cfg, cc.clone())
        .unwrap_or_else(|e| panic!("build_ir for {fn_name}: {e:?}"));
    let rom_for_opt =
        strider_reader::ElfFileMemReader::from_object(&obj).expect("rom reader (opt)");
    (outcome, ana, cc, sleigh_arch, rom_for_opt)
}

/// [`lift_for_pipeline`] followed by `default_pipeline`.
///
/// The fixtures are compiler-emitted binaries from `fixtures/cases/*.c`, where
/// globals never alias the stack frame, so the default
/// `stack_global_disjoint` holds and the relaxed walker recovers the
/// spill/reload forwarding the assertions depend on. Coverage with the
/// assumption cleared belongs in unit tests with a directly-configured
/// `OptCtx`.
pub(crate) fn analyze(arch: Arch, case: &str, fn_name: &str) -> strider_ir::Function {
    let (outcome, _lifter, _cc, _sleigh_arch, rom_for_opt) = lift_for_pipeline(arch, case, fn_name);
    let mut function = outcome.function;
    // The reader serves raw bytes; `LoadReadOnly` decodes them with the
    // function's own endianness, so big-endian fixtures fold correctly.
    let p = strider_orchestrator::opt::default_pipeline();
    let mut ctx = strider_orchestrator::opt::OptCtx::new(Some(&rom_for_opt));
    p.run(&mut function, &mut ctx)
        .unwrap_or_else(|e| panic!("optimizer pipeline for {fn_name}: {e:?}"));
    function
}

use strider_ir::node::NodeKind;

pub(crate) fn count_kind<F: Fn(&NodeKind) -> bool>(
    function: &strider_ir::Function,
    pred: F,
) -> usize {
    function.count_kind(pred)
}

pub(crate) fn count_int_binop(
    function: &strider_ir::Function,
    op: strider_ir::IntBinaryOp,
) -> usize {
    count_kind(
        function,
        |k| matches!(k, NodeKind::IntBinaryOp(o) if *o == op),
    )
}
pub(crate) fn count_int_unop(function: &strider_ir::Function, op: strider_ir::IntUnaryOp) -> usize {
    count_kind(
        function,
        |k| matches!(k, NodeKind::IntUnaryOp(o) if *o == op),
    )
}
pub(crate) fn count_int_cmp(function: &strider_ir::Function, op: strider_ir::IntCmpOp) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::IntCmpOp(o) if *o == op))
}
pub(crate) fn count_float_binop(
    function: &strider_ir::Function,
    op: strider_ir::FloatBinaryOp,
) -> usize {
    count_kind(
        function,
        |k| matches!(k, NodeKind::FloatBinaryOp(o) if *o == op),
    )
}
pub(crate) fn count_float_unop(
    function: &strider_ir::Function,
    op: strider_ir::FloatUnaryOp,
) -> usize {
    count_kind(
        function,
        |k| matches!(k, NodeKind::FloatUnaryOp(o) if *o == op),
    )
}
pub(crate) fn count_float_cmp(
    function: &strider_ir::Function,
    op: strider_ir::FloatCmpOp,
) -> usize {
    count_kind(
        function,
        |k| matches!(k, NodeKind::FloatCmpOp(o) if *o == op),
    )
}

pub(crate) fn count_calls(function: &strider_ir::Function) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::Call))
}
pub(crate) fn count_ifs(function: &strider_ir::Function) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::If))
}
pub(crate) fn count_returns(function: &strider_ir::Function) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::Return))
}

/// Counts the distinct control-flow paths converging at any `Return` node.
///
/// Some ABIs (PPC, aarch64) share the function epilogue: at `-O0` the
/// compiler routes every source-level `return` through one `blr`/`ret`, so
/// the IR has one `Return` fed by a `Region` merging the individual paths,
/// and `count_returns` would report 1 even with two source-level returns.
/// This counts the `Region`'s immediate fan-in instead (not transitively
/// expanded through a nested `Region`), giving a lower bound on the number
/// of source-level return paths; sufficient for the "at least 2 paths"
/// assertions in this suite.
pub(crate) fn count_return_paths(function: &strider_ir::Function) -> usize {
    let mut total = 0usize;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::Return) {
            continue;
        }
        // Return inputs: [Control, Memory, ...return values].  Slot 0 is the
        // Control predecessor.
        let inputs = function.node_inputs(nid);
        let Some(ctrl_value) = inputs.get(0).copied() else {
            // A Return with no inputs is malformed; the validator would catch
            // it.  Treat as a single path so we don't silently drop it.
            total += 1;
            continue;
        };
        // Step through control-ordered ops interposed on the epilogue (an ARM
        // `bx lr` models `setISAMode` as a CallOther just before the `Return`)
        // to the merging Region, whose immediate fan-in is the path count.
        let mut pred = function.producer(ctrl_value);
        loop {
            match function.node_kind(pred) {
                // Region's control inputs form the leading run of its input
                // list (see node_signature: `inputs: []; in_tail: CTRL`), so
                // the total input count IS the predecessor count.
                NodeKind::Region => {
                    total += function.node_inputs(pred).len();
                    break;
                }
                NodeKind::Call | NodeKind::CallOther { .. } => {
                    match function.node_inputs(pred).get(0).copied() {
                        Some(ctrl) => pred = function.producer(ctrl),
                        None => {
                            total += 1;
                            break;
                        }
                    }
                }
                _ => {
                    total += 1;
                    break;
                }
            }
        }
    }
    total
}
/// Counts loop headers in the lifted CFG.
///
/// A "loop header" here is a `Region` whose predecessor set contains at
/// least one back-edge: a predecessor that is itself reachable from the
/// `Region` via forward control flow.  This is independent of any `VarPhi`
/// count, which can drop to zero when every tracked variable is
/// loop-invariant (e.g. a register read in the loop header but never
/// modified by the body; `PhiCollapse`'s self-ref rule then collapses the
/// phi to the entry value).
pub(crate) fn count_loops(function: &strider_ir::Function) -> usize {
    use entity_utils::DenseEntitySet;
    let mut count = 0;
    let reachable: DenseEntitySet<strider_ir::node::NodeId> = function.walk().collect();
    for n in function.graph().all_node_ids() {
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
        let has_back_edge = preds.iter().any(|&pred_value| {
            let pred = function.producer(pred_value);
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
                    for (consumer, _) in function.graph().value_uses(out) {
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
pub(crate) fn count_loads(function: &strider_ir::Function) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::Load(_)))
}
pub(crate) fn count_stores(function: &strider_ir::Function) -> usize {
    count_kind(function, |k| matches!(k, NodeKind::Store(_)))
}
pub(crate) fn has_kind<F: Fn(&NodeKind) -> bool>(function: &strider_ir::Function, pred: F) -> bool {
    function.has_kind(pred)
}

/// Counts Store/Load nodes whose address decomposed to an SP-relative slot in
/// the `memory_offsets` side-table. `StackOffsetDetect` is the post-pass that
/// decomposes every Store/Load address, so a non-zero count after the pipeline
/// proves it ran; other passes memoize into the same table.
pub(crate) fn count_memory_offsets(function: &strider_ir::Function) -> usize {
    function
        .graph()
        .all_node_ids()
        .filter(|&nid| function.stack_offset(nid).is_some())
        .count()
}

pub(crate) fn has_constant(function: &strider_ir::Function, value: u64) -> bool {
    function.walk().any(|nid| {
        matches!(function.node_kind(nid), NodeKind::IntConst(_))
            && function
                .first_value_output_of(nid)
                .is_some_and(|v| function.int_const_u128(v) == Some(u128::from(value)))
    })
}

/// Generates one `#[test]` per (architecture, function) pair.
///
/// Basic form (every arch in `ALL_ARCHES` runs):
///   per_arch_test!("<case>", "<fn_name>", <assertion_fn>);
///
/// With per-arch ignores (specific archs are `#[ignore = "reason"]`-marked):
///   per_arch_test!("<case>", "<fn_name>", <assertion_fn>, ignore = {
///       Mips32le: "<what fails and why>",
///       Mips32be: "<what fails and why>",
///   });
///
/// Ignore reasons should be a one-line diagnosis, not just a symptom, so
/// a future reader can find the fix path from the reason alone.
///
/// `macro_rules!` has no ident equality, so the ignore block can't be
/// compared directly: it's scanned by dedicated per-arch arms.  The outer
/// macro hands each arch's entry to `__one_arch_test!`, which forwards it
/// to that arch's scanner.
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
                // Resolve each arch's ignore entry and emit the test
                // function.  The inner `__one_arch_test!` macro
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

// Thin dispatcher: forwards the opaque `ignore` block to the matching
// per-arch scanner (`__scan_ignore_x86!` etc.), which emits either an
// `#[ignore = reason] #[test]` or a plain `#[test]`.  `paste!` builds the
// scanner name by lower-casing the arch token (e.g. `Aarch64Be` ->
// `__scan_ignore_aarch64be`).  The literal `$fn:ident` token after the
// function name below isn't a typo: the scanner arms require it in their
// match pattern (see `__define_scan_ignore!`).
#[doc(hidden)]
#[macro_export]
macro_rules! __one_arch_test {
    ($arch:ident, $fn:ident, $case:literal, $fn_name:literal, $assert:ident $ignore_block:tt) => {
        paste::paste! {
            // Scanners are emitted by `__define_scan_ignore!`.  Rust can't
            // resolve `$crate::name` for a macro that is itself
            // macro-generated (rust-lang/rust#52234), so we call it by its
            // unqualified name; `#[macro_export]` hoists it to the crate
            // root regardless.
            [<__scan_ignore_ $arch:lower>]!($fn:ident, $case, $fn_name, $assert, $ignore_block);
        }
    };
}

// Each per-arch scanner needs three arms: match-self (emit an ignored
// test), skip-other (recurse past an entry for a different arch), and
// empty (emit a plain test).  The match-self arm needs the arch as a
// literal token, and `macro_rules!` has no ident equality, so we generate
// one scanner per arch via `__define_scan_ignore!($arch_lower,
// $arch_camel)` (invoked once per arch below).
//
// `$d:tt` is the standard "dollar token" trick for nesting `macro_rules!`
// on stable: an outer macro can't write a bare `$` for an inner macro's
// metavariables (it would bind in the outer scope), so `$d` stands in for
// a literal `$` in the emitted scanner.
//
// The generator runs at definition time, not as part of the runtime
// skip-other recursion, so it adds nothing to `recursion_limit` pressure.
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
