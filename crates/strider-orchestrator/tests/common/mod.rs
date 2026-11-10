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

/// Bytes from a string of hex digit pairs.
pub(crate) fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

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

/// `entry` analysed under the default options and pipeline by
/// [`strider_over_bytes`].
pub(crate) fn analyze_bytes(
    arch: Arch,
    bytes: Vec<u8>,
    base: u64,
    rom: Option<Box<dyn strider_orchestrator::opt::ReadOnlyMemory>>,
    entry: u64,
) -> strider_orchestrator::AnalyzeResult {
    let (mut strider, cc) = strider_over_bytes(arch, bytes, base, rom);
    strider
        .analyze(entry, &cc, &Default::default(), None)
        .expect("analyze")
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
    folded_returns(arch, &[bytes], base)[0]
}

/// `images` end to end from `base` at one 64-byte-aligned stride, and where
/// each starts. An image must not address bytes outside itself.
///
/// A `Sleigh` takes tens of milliseconds to build, so a test lifting
/// hundreds of small functions lays them out here and lifts them through one.
pub(crate) fn concat_images(images: &[Vec<u8>], base: u64) -> (Vec<u8>, Vec<u64>) {
    let stride = images
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .next_multiple_of(64);
    let mut bytes = vec![0; stride * images.len()];
    let entries = images
        .iter()
        .enumerate()
        .map(|(i, image)| {
            bytes[i * stride..][..image.len()].copy_from_slice(image);
            base + (i * stride) as u64
        })
        .collect();
    (bytes, entries)
}

/// [`folded_return`] of each of `images`, laid out by [`concat_images`].
pub(crate) fn folded_returns(arch: Arch, images: &[Vec<u8>], base: u64) -> Vec<u128> {
    let (bytes, entries) = concat_images(images, base);
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(
        strider_ir_test_utils::MockRom::raw_bytes(base, bytes.clone()),
    );
    let (mut strider, cc) = strider_over_bytes(arch, bytes, base, Some(rom));
    entries
        .iter()
        .map(|&entry| {
            let f = strider
                .analyze(entry, &cc, &Default::default(), None)
                .expect("analyze")
                .function;
            let v = returned(&f)[0];
            f.int_const_u128(v)
                .unwrap_or_else(|| panic!("return slot 0 did not fold: {:?}", producer_kind(&f, v)))
        })
        .collect()
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
#[macro_export]
macro_rules! per_arch_test {
    ($case:literal, $fn_name:literal, $assert:ident) => {
        paste::paste! {
            mod [<test_ $fn_name>] {
                use super::*;
                $crate::__arch_tests!($case, $fn_name, $assert;
                    X86 x86, X86Kernel x86_kernel, X64 x64, Aarch64 aarch64,
                    Aarch64Be aarch64be, Arm arm, ArmBe arm_be, ArmBe8 arm_be8,
                    ArmThumb arm_thumb, Mips32le mips32le, Mips32be mips32be,
                    Mips64le mips64le, Mips64be mips64be, Ppc32be ppc32be,
                    Ppc32le ppc32le, Ppc64be ppc64be, Ppc64le ppc64le);
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __arch_tests {
    ($case:literal, $fn_name:literal, $assert:ident; $($arch:ident $test:ident),*) => {
        $(
            #[test]
            fn $test() {
                $assert(&$crate::common::analyze($crate::common::Arch::$arch, $case, $fn_name));
            }
        )*
        // A new `Arch` fails to compile here until it is listed above.
        const _: fn($crate::common::Arch) = |arch| match arch {
            $($crate::common::Arch::$arch)|* => {}
        };
    };
}
