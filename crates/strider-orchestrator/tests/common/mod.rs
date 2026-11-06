//! `per_arch_test!("<case>", "<fn_name>", <assertion_fn>)` expands to one
//! `#[test]` per arch in `ALL_ARCHES`. Each loads
//! `fixtures/out/<arch>/<case>.elf`, analyses the named symbol, runs the
//! optimiser pipeline over the ELF's read-only mappings, and hands the
//! resulting `Function` to the assertion.

#![allow(
    clippy::crate_in_macro_def,
    dead_code  // category test files won't use every helper
)]

use strider_ir::{IRViewer, IRWalker};
use strider_ir_test_utils::IrWalkerEx;

// Fixture builders for the indirect-branch classifier integration tests
// (see `indirect_resolve_helpers/mod.rs`).
pub(crate) mod indirect_resolve_helpers;

pub(crate) mod fixtures;

pub(crate) mod cone;

pub(crate) mod quad;

pub(crate) mod round;

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
    /// BE8: little-endian instructions, big-endian data
    /// (`fixtures/arch/arm_be8.mk`).
    ArmBe8,
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

/// Every `Arch`, in `FIXTURE_ARCHES` order.
pub(crate) const ALL_ARCHES: &[Arch] = &[
    Arch::X86,
    Arch::X86Kernel,
    Arch::X64,
    Arch::Aarch64,
    Arch::Aarch64Be,
    Arch::Arm,
    Arch::ArmBe,
    Arch::ArmBe8,
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
            Arch::ArmBe8 => "arm_be8",
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

    pub(crate) fn fixture(self) -> &'static fixtures::FixtureArch {
        fixtures::fixture_arch(self.name())
    }

    pub(crate) fn sleigh(self) -> strider_target::SleighArch {
        (self.fixture().sleigh)()
    }

    pub(crate) fn cc(self) -> strider_target::CallingConvention {
        (self.fixture().cc)()
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
    let driver = strider_orchestrator::Lifter::new(sleigh_arch, reader).expect("Lifter::new");
    let cc = arch
        .cc()
        .build(driver.sleigh_regs())
        .expect("build cc against driver regs");
    (driver, cc)
}

/// A [`strider_orchestrator::Strider`] over `bytes` mapped at `base`, with
/// `arch`'s calling convention built against the `Sleigh` it owns.
pub(crate) fn strider_over_bytes(
    arch: Arch,
    bytes: Vec<u8>,
    base: u64,
    rom: Option<Box<dyn strider_orchestrator::opt::ReadOnlyMemory>>,
) -> (
    strider_orchestrator::Strider<rsleigh::mem_readers::BufMemReader<Vec<u8>>>,
    strider_target::BuiltCallingConvention,
) {
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, base);
    let strider =
        strider_orchestrator::Strider::new(arch.sleigh(), reader, rom).expect("Strider::new");
    let cc = arch
        .cc()
        .build(strider.sleigh_regs())
        .expect("build cc against sleigh regs");
    (strider, cc)
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
/// Panics on any construction failure.
pub(crate) fn analyze_with_known_targets(
    bytes: &[u8],
    base: u64,
    branch_indirect_addr: u64,
    targets: &[u64],
) -> strider_ir::Function {
    use rustc_hash::FxHashMap;
    use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};

    let reader = rsleigh::mem_readers::BufMemReader::new(bytes.to_vec(), base);
    let (mut driver, cc) = driver_for_reader(Arch::X64, reader);

    let mut known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known_targets.insert(
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr::from(branch_indirect_addr),
            insn_index: 0,
        },
        ResolvedTargets::table(
            targets
                .iter()
                .copied()
                .map(strider_cfg::ResolvedTarget::from),
        ),
    );
    let cfg_opts = strider_cfg::CfgOptions {
        known_targets: &known_targets,
        ..Default::default()
    };
    let cfg = driver
        .build_cfg(MachineInsnAddr::from(base), &cfg_opts)
        .expect("cfg build with Multiple known targets");

    driver.build_ir(&cfg, cc).expect("build_ir").function
}

/// [`fixtures::Fixture::strider`] on the (arch, case) ELF, with `fn_name`'s
/// address. The address keeps its ARM-Thumb interworking bit: that bit IS the
/// entry's ISA mode.
pub(crate) fn fixture_strider(
    arch: Arch,
    case: &str,
    fn_name: &str,
) -> (
    strider_orchestrator::Strider<strider_reader::ElfFileMemReader>,
    strider_target::BuiltCallingConvention,
    u64,
) {
    let fixture = fixtures::Fixture::open(arch.fixture(), case);
    let (strider, cc) = fixture.strider();
    (strider, cc, fixture.symbol(fn_name))
}

/// [`fixture_strider`] whose ROM also serves the relocated GOT; see
/// [`fixtures::Fixture::strider_serving_got`].
pub(crate) fn fixture_strider_serving_got(
    arch: Arch,
    case: &str,
    fn_name: &str,
) -> (
    strider_orchestrator::Strider<strider_reader::ElfFileMemReader>,
    strider_target::BuiltCallingConvention,
    u64,
) {
    let fixture = fixtures::Fixture::open(arch.fixture(), case);
    let (strider, cc) = fixture.strider_serving_got();
    (strider, cc, fixture.symbol(fn_name))
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
    strider_target::BuiltCallingConvention,
    strider_reader::ElfFileMemReader,
) {
    let fixture = fixtures::Fixture::open(arch.fixture(), case);
    let mem = strider_reader::ElfFileMemReader::new(
        fixture.elf(),
        strider_reader::elf::RelocationMode::Unapplied,
    )
    .expect("mem reader");
    let (mut ana, cc) = driver_for_reader(arch, mem);
    let addr = fixture.symbol(fn_name);
    let cfg_opts = strider_cfg::CfgOptions {
        bound: strider_cfg::FunctionBound::Open {
            code_before_start: true,
        },
        ..Default::default()
    };
    let cfg = ana
        .build_cfg(strider_cfg::MachineInsnAddr::from(addr), &cfg_opts)
        .unwrap_or_else(|e| panic!("Cfg build for {fn_name}: {e:?}"));
    // build_ir consumes cc by value; clone so the caller also gets an
    // owned cc back.
    let outcome = ana
        .build_ir(&cfg, cc.clone())
        .unwrap_or_else(|e| panic!("build_ir for {fn_name}: {e:?}"));
    let rom_for_opt = strider_reader::ElfFileMemReader::new(
        fixture.elf(),
        strider_reader::elf::RelocationMode::Unapplied,
    )
    .expect("rom reader (opt)");
    (outcome, cc, rom_for_opt)
}

/// [`lift_for_pipeline`] followed by [`optimize`].
pub(crate) fn analyze(arch: Arch, case: &str, fn_name: &str) -> strider_ir::Function {
    let (outcome, _cc, rom_for_opt) = lift_for_pipeline(arch, case, fn_name);
    let mut function = outcome.function;
    optimize(&mut function, &rom_for_opt)
        .unwrap_or_else(|e| panic!("optimizer pipeline for {fn_name}: {e:?}"));
    function
}

/// `default_pipeline` over the ELF's read-only mappings.
///
/// The fixtures are compiler-emitted binaries from `fixtures/cases/*.c`, where
/// globals never alias the stack frame, so the default
/// `stack_global_disjoint` holds and the relaxed walker recovers the
/// spill/reload forwarding the assertions depend on. Coverage with the
/// assumption cleared belongs in unit tests over a fixture of their own.
pub(crate) fn optimize(
    function: &mut strider_ir::Function,
    rom: &strider_reader::ElfFileMemReader,
) -> strider_orchestrator::opt::Result<()> {
    // The reader serves raw bytes; `LoadReadOnly` decodes them with the
    // function's own endianness, so big-endian fixtures fold correctly.
    let p = strider_orchestrator::opt::default_pipeline();
    // A fresh lift claims nothing, so the harness states the claims its
    // assertions rest on: these cases spill arguments across calls, and both
    // detecting the slot afterwards and forwarding it into the callee's
    // argument need the stack to be disjoint from the globals it is
    // interleaved with.
    function.assume(&strider_orchestrator::opt::AssumptionOptions {
        stack_global_disjoint: true,
        assume_incoming_args_survive_calls: true,
        ..strider_orchestrator::opt::AssumptionOptions::none()
    });
    p.run(function, &strider_orchestrator::opt::OptCtx::new(Some(rom)))
}

use strider_ir::node::NodeKind;

/// The `Call` whose target input (slot 2) is `IntConst(target)`.
pub(crate) fn find_call_to(
    function: &strider_ir::Function,
    target: u64,
) -> Option<strider_ir::node::NodeId> {
    function.walk().find(|&nid| {
        matches!(function.node_kind(nid), NodeKind::Call { .. })
            && function
                .node_inputs(nid)
                .into_iter()
                .nth(2)
                .is_some_and(|value| function.int_const_u128(value) == Some(u128::from(target)))
    })
}

pub(crate) fn count_int_binop(
    function: &strider_ir::Function,
    op: strider_ir::IntBinaryOp,
) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::IntBinaryOp(o) if *o == op))
}
pub(crate) fn count_int_cmp(function: &strider_ir::Function, op: strider_ir::IntCmpOp) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::IntCmpOp(o) if *o == op))
}
pub(crate) fn count_float_binop(
    function: &strider_ir::Function,
    op: strider_ir::FloatBinaryOp,
) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::FloatBinaryOp(o) if *o == op))
}

pub(crate) fn count_switches(function: &strider_ir::Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::Switch(_)))
}

/// Panics if zero or more than one `Switch` node is present; either case
/// indicates a fixture-construction bug.
pub(crate) fn find_unique_switch(function: &strider_ir::Function) -> strider_ir::node::NodeId {
    let mut iter = function
        .walk()
        .filter(|&nid| matches!(function.node_kind(nid), NodeKind::Switch(_)));
    let first = iter
        .next()
        .expect("fixture must contain exactly one Switch node");
    assert!(
        iter.next().is_none(),
        "fixture has more than one Switch node"
    );
    first
}

/// Cast selectivity shared by the pattern-query suites: skips the
/// Extend/Truncate width casts some lifters insert between mismatched-width
/// operations.
pub(crate) fn cast_mask() -> strider_pattern::CastMask {
    strider_pattern::CastMask::EXTEND | strider_pattern::CastMask::TRUNCATE
}

pub(crate) fn masked(p: strider_pattern::Pattern) -> strider_pattern::Pattern {
    p.ignore_casts_mask(cast_mask())
}

pub(crate) fn matcher(function: &strider_ir::Function) -> strider_pattern::Matcher<'_> {
    strider_pattern::Matcher::new(function)
}

/// `call 0x2000; ret` at 0x1000, x86-64.  Returns (bytes, entry, call_target).
pub(crate) fn x86_64_call_then_ret() -> (Vec<u8>, u64, u64) {
    let bytes = vec![0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3];
    (bytes, 0x1000, 0x2000)
}

pub(crate) fn count_calls(function: &strider_ir::Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::Call { .. }))
}
pub(crate) fn count_ifs(function: &strider_ir::Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::If))
}
pub(crate) fn count_returns(function: &strider_ir::Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::Return))
}

/// The returned values, past the `Return`'s control and memory inputs.
pub(crate) fn returned(f: &strider_ir::Function) -> Vec<strider_ir::node::ValueId> {
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    f.node_inputs(ret).into_iter().skip(2).collect()
}

/// The constant return slot 0 folds to once `bytes` at `base` is analysed,
/// its bytes also the ROM.
pub(crate) fn folded_return(arch: Arch, bytes: Vec<u8>, base: u64) -> u128 {
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(
        strider_ir_test_utils::MockRom::raw_bytes(base, bytes.clone()),
    );
    let (mut strider, cc) = strider_over_bytes(arch, bytes, base, Some(rom));
    let f = strider
        .analyze(base, &cc, &Default::default(), None)
        .expect("analyze")
        .function;
    let v = returned(&f)[0];
    f.int_const_u128(v)
        .unwrap_or_else(|| panic!("return slot 0 did not fold: {:?}", producer_kind(&f, v)))
}

/// The kind of `v`'s producer.
pub(crate) fn producer_kind(
    f: &strider_ir::Function,
    v: strider_ir::node::ValueId,
) -> &strider_ir::node::NodeKind {
    f.node_kind(f.producer(v))
}

/// The inputs of `v`'s producer.
pub(crate) fn inputs_of(
    f: &strider_ir::Function,
    v: strider_ir::node::ValueId,
) -> Vec<strider_ir::node::ValueId> {
    f.node_inputs(f.producer(v)).into_iter().collect()
}

/// Writes `word` little-endian at `addr` of an image mapped at `base`.
pub(crate) fn put_le32(bytes: &mut [u8], base: u64, addr: u64, word: u32) {
    let off = usize::try_from(addr - base).expect("offset fits usize");
    bytes[off..off + 4].copy_from_slice(&word.to_le_bytes());
}

/// A byte image of 32-bit instruction words in `endian` order.
pub(crate) fn words_image(words: &[u32], endian: strider_target::Endianness) -> Vec<u8> {
    let mut v = Vec::with_capacity(words.len() * 4);
    for w in words {
        match endian {
            strider_target::Endianness::Big => v.extend_from_slice(&w.to_be_bytes()),
            strider_target::Endianness::Little => v.extend_from_slice(&w.to_le_bytes()),
        }
    }
    v
}

pub(crate) fn count_loads(function: &strider_ir::Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::Load(_)))
}
pub(crate) fn count_stores(function: &strider_ir::Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::Store(_)))
}

/// Counts Store/Load nodes whose address decomposed to an SP-relative slot in
/// the `memory_offsets` side-table. Every pipeline run decomposes every
/// Store/Load address after its post-passes, so a non-zero count proves the
/// stamp ran; other passes memoize into the same table.
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
                $crate::__one_arch_test!(ArmBe8,    arm_be8,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(ArmThumb,  arm_thumb, $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Mips32le,  mips32le,  $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Mips32be,  mips32be,  $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Mips64le,  mips64le,  $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Mips64be,  mips64be,  $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Ppc32be,   ppc32be,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Ppc32le,   ppc32le,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Ppc64be,   ppc64be,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                $crate::__one_arch_test!(Ppc64le,   ppc64le,   $case, $fn_name, $assert { $($skip_arch: $reason),* });
                // A new `Arch` fails to compile here until it has an arm above.
                const _: fn($crate::common::Arch) = |arch| match arch {
                    $crate::common::Arch::X86
                    | $crate::common::Arch::X86Kernel
                    | $crate::common::Arch::X64
                    | $crate::common::Arch::Aarch64
                    | $crate::common::Arch::Aarch64Be
                    | $crate::common::Arch::Arm
                    | $crate::common::Arch::ArmBe
                    | $crate::common::Arch::ArmBe8
                    | $crate::common::Arch::ArmThumb
                    | $crate::common::Arch::Mips32le
                    | $crate::common::Arch::Mips32be
                    | $crate::common::Arch::Mips64le
                    | $crate::common::Arch::Mips64be
                    | $crate::common::Arch::Ppc32be
                    | $crate::common::Arch::Ppc32le
                    | $crate::common::Arch::Ppc64be
                    | $crate::common::Arch::Ppc64le => {}
                };
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
__define_scan_ignore!($ armbe8,     ArmBe8);
__define_scan_ignore!($ armthumb,   ArmThumb);
__define_scan_ignore!($ mips32le,   Mips32le);
__define_scan_ignore!($ mips32be,   Mips32be);
__define_scan_ignore!($ mips64le,   Mips64le);
__define_scan_ignore!($ mips64be,   Mips64be);
__define_scan_ignore!($ ppc32be,    Ppc32be);
__define_scan_ignore!($ ppc32le,    Ppc32le);
__define_scan_ignore!($ ppc64be,    Ppc64be);
__define_scan_ignore!($ ppc64le,    Ppc64le);
