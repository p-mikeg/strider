//! Shared `#[cfg(test)]` fixture helpers for the crate's inline test
//! modules (`builder/mod.rs`, `builder/region_builder.rs`, `query.rs`).
//!
//! These were previously copy-pasted (byte-identically, modulo the
//! `addr`/`addr_at` rename) into each `mod tests`; consolidated here so
//! there is one source of truth for the synthetic `PcodeInsnAddr` /
//! `Insn` / `Region` / `Sleigh` / `Builder` fixtures.

use rsleigh::mem_readers::BufMemReader;
use strider_target::SleighArch;

use crate::builder::Builder;
use crate::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};
use crate::CfgOptions;

/// The in-memory reader used by every synthetic `Sleigh` fixture.
pub(crate) type TestReader = BufMemReader<Vec<u8>>;

/// Build a `PcodeInsnAddr` from a machine address + p-code insn index.
pub(crate) fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
    PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: machine },
        insn_index: insn,
    }
}

/// A minimal no-op p-code instruction (`Copy` with no output / inputs).
pub(crate) fn fake_insn() -> rsleigh::Insn {
    rsleigh::Insn {
        opcode: rsleigh::Opcode::Copy,
        output: None,
        inputs: vec![].into(),
    }
}

/// Build an `Unconditional` region spanning the given `(machine, insn)`
/// addresses.
pub(crate) fn make_region(addrs: &[(u64, u64)]) -> Region {
    let start = addr(addrs[0].0, addrs[0].1);
    let insns = addrs
        .iter()
        .map(|&(m, i)| RegionInstruction {
            addr: addr(m, i),
            insn: fake_insn(),
        })
        .collect();
    Region {
        start_addr: start,
        insns,
        terminator: RegionTerminator::Unconditional,
    }
}

/// An empty x86-64 `Sleigh` over a zero-length reader.
pub(crate) fn make_sleigh() -> rsleigh::Sleigh<TestReader> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
    rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create empty Sleigh")
}

/// An x86-64 CFG `Builder` at `start_addr` with default `CfgOptions`.
pub(crate) fn make_builder<'a>(
    start_addr: u64,
    sleigh: &'a mut rsleigh::Sleigh<TestReader>,
) -> Builder<'a, TestReader> {
    make_builder_opts(start_addr, sleigh, &CfgOptions::default())
}

/// An x86-64 CFG `Builder` at `start_addr` with caller-supplied `CfgOptions`.
pub(crate) fn make_builder_opts<'a>(
    start_addr: u64,
    sleigh: &'a mut rsleigh::Sleigh<TestReader>,
    options: &CfgOptions,
) -> Builder<'a, TestReader> {
    let arch = SleighArch::x86_64();
    Builder::for_arch(&arch, sleigh, start_addr, options)
}
