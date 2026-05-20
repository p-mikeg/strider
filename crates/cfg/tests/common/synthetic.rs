#![allow(dead_code, clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Shared test-fixture helpers (synthetic `Builder`s, regions, addresses,
//! raw-bytes x86-64 snippets).

use cfg::test_api::{
    MachineInsnAddr, Options, PcodeInsnAddr, Region, RegionInstruction, TestRegionBuilder,
};
use cfg::{Builder, OptionsBuilder, RegionTerminator};
use rsleigh::mem_readers::BufMemReader;

pub type TestReader = BufMemReader<Vec<u8>>;

/// Short constructor for a `PcodeInsnAddr`.
#[must_use]
pub fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
    PcodeInsnAddr::new(MachineInsnAddr::new(machine), insn)
}

/// Sleigh backed by an empty buffer — decodes nothing but is enough to
/// construct a `Builder` for tests that never call `Builder::build`.
#[must_use]
pub fn make_sleigh() -> rsleigh::Sleigh<TestReader> {
    let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86,
        rsleigh::pspec::PSPEC_X86,
        reader,
    )
    .expect("failed to create test Sleigh")
}

/// Sleigh backed by `bytes` at `base` (x86-64). Decodes real instructions.
#[must_use]
pub fn make_sleigh_with_bytes(bytes: Vec<u8>, base: u64) -> rsleigh::Sleigh<TestReader> {
    let reader = BufMemReader::new(bytes, base);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        reader,
    )
    .expect("failed to create x86-64 test Sleigh")
}

#[must_use]
pub fn make_builder(start_addr: u64) -> Builder<TestReader> {
    Builder::for_arch(
        &target::SleighArch::x86(),
        make_sleigh(),
        start_addr,
        OptionsBuilder::new().build(),
    )
}

#[must_use]
pub fn make_builder_opts(start_addr: u64, options: Options) -> Builder<TestReader> {
    Builder::for_arch(
        &target::SleighArch::x86(),
        make_sleigh(),
        start_addr,
        options,
    )
}

#[must_use]
pub fn make_builder_with_bytes(bytes: Vec<u8>, start_addr: u64) -> Builder<TestReader> {
    Builder::for_arch(
        &target::SleighArch::x86_64(),
        make_sleigh_with_bytes(bytes, start_addr),
        start_addr,
        OptionsBuilder::new().build(),
    )
    .with_indirect_resolver(test_indirect_resolver())
}

/// Returns the canonical mini-IR indirect-branch resolver (a stateless
/// unit struct) wrapped in an `Arc<dyn IndirectTargetResolver<_>>`.
/// Use this in any test that builds a `Cfg` and expects cfg-time
/// indirect-branch resolution to fire — without it, the builder treats
/// every `BranchIndirect` as deferred via
/// `UnresolvedIndirectBranch`.  Phase 3 Task 3.1.
#[must_use]
pub fn test_indirect_resolver(
) -> std::sync::Arc<dyn cfg::IndirectTargetResolver<TestReader>> {
    std::sync::Arc::new(strider_analyze::opt::indirect_resolver::MiniIrIndirectResolver)
}

/// Minimal dummy pcode instruction (no inputs/outputs, opcode = `Copy`).
#[must_use]
pub fn fake_insn() -> rsleigh::Insn {
    rsleigh::Insn {
        opcode: rsleigh::Opcode::Copy,
        output: None,
        inputs: vec![].into(),
    }
}

/// Synthetic `LiftRes` carrying `n_insns` `Copy` pcode ops and an arbitrary
/// machine length. Intended for tests that exercise code reading only
/// `lift_res.insns.len()` (e.g. `decode_branch_target`'s upper-bound check) —
/// the actual `Insn` content is irrelevant for those paths.
#[must_use]
pub fn fake_lift_res(n_insns: usize) -> rsleigh::LiftRes {
    fake_lift_res_with_len(n_insns, 1)
}

/// Variant of [`fake_lift_res`] with caller-supplied `machine_insn_len`.
/// Needed by tests that exercise the next-machine-address arithmetic in
/// `next_pcode_addr` (e.g. the overflow path).
#[must_use]
pub fn fake_lift_res_with_len(n_insns: usize, machine_insn_len: usize) -> rsleigh::LiftRes {
    rsleigh::LiftRes {
        insns: (0..n_insns).map(|_| fake_insn()).collect(),
        machine_insn_len,
    }
}

/// Builds a `Region` from `(machine_addr, insn_index)` pairs.
///
/// # Panics
/// Panics if `addrs` is empty.
#[must_use]
pub fn make_region(addrs: &[(u64, u64)]) -> Region {
    assert!(!addrs.is_empty(), "make_region requires at least one address");
    let start = addr(addrs[0].0, addrs[0].1);
    let insns: Vec<_> = addrs
        .iter()
        .map(|&(m, i)| RegionInstruction {
            addr: addr(m, i),
            insn: fake_insn(),
        })
        .collect();
    Region {
        start_addr: start,
        insns,
        terminator: RegionTerminator::Fallthrough,
    }
}

/// Builds a `TestRegionBuilder` anchored at `start` with no parent edge.
#[must_use]
pub fn make_region_builder(
    builder: &mut Builder<TestReader>,
    start: PcodeInsnAddr,
) -> TestRegionBuilder<'_, TestReader> {
    TestRegionBuilder::new(builder, start)
}

// ── Raw-bytes x86-64 helpers for synthetic `Builder::build` tests ─────────────

/// `nop; ret` encoded for x86-64.
#[must_use]
pub fn nop_ret_bytes() -> Vec<u8> {
    vec![0x90, 0xc3]
}

/// `ret` alone (single-byte x86-64).
#[must_use]
pub fn ret_bytes() -> Vec<u8> {
    vec![0xc3]
}

/// Unconditional short `jmp rel8` followed by `ret`. Total 3 bytes.
#[must_use]
#[allow(clippy::cast_sign_loss)] // two's-complement byte reinterpretation for x86 short branch displacement
pub fn jmp_rel8_ret_bytes(rel: i8) -> Vec<u8> {
    vec![0xeb, rel as u8, 0xc3]
}

/// `je rel8; ret; ret` — conditional short jump followed by two `ret`s. Total 4 bytes.
#[must_use]
#[allow(clippy::cast_sign_loss)] // two's-complement byte reinterpretation for x86 short branch displacement
pub fn je_rel8_ret_ret_bytes(rel: i8) -> Vec<u8> {
    vec![0x74, rel as u8, 0xc3, 0xc3]
}

/// `jmp rax` (indirect jump through a register) encoded for x86-64.  Sleigh
/// lifts this as `Opcode::BranchIndirect` — the same opcode ARM emits for
/// `bx lr` returns and computed-goto / jump-table dispatches.
#[must_use]
pub fn jmp_rax_bytes() -> Vec<u8> {
    vec![0xff, 0xe0]
}
