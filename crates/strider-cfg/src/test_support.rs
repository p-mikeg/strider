use rsleigh::mem_readers::BufMemReader;
use strider_target::SleighArch;

use crate::CfgOptions;
use crate::builder::Builder;
use crate::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};

pub(crate) type TestReader = BufMemReader<Vec<u8>>;

pub(crate) fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
    PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: machine },
        insn_index: insn,
    }
}

pub(crate) fn fake_insn() -> rsleigh::Insn {
    rsleigh::Insn {
        opcode: rsleigh::Opcode::Copy,
        output: None,
        inputs: vec![].into(),
    }
}

/// An `Unconditional` region spanning the given `(machine, insn)` addresses,
/// every instruction one byte long.
pub(crate) fn make_region(addrs: &[(u64, u64)]) -> Region {
    let start = addr(addrs[0].0, addrs[0].1);
    let insns = addrs
        .iter()
        .map(|&(m, i)| RegionInstruction {
            addr: addr(m, i),
            insn: fake_insn(),
            len: 1,
        })
        .collect();
    Region {
        start_addr: start,
        insns,
        empty_span_len: 0,
        terminator: RegionTerminator::Unconditional,
    }
}

/// An x86-64 Sleigh reading `bytes` mapped at `base`.
pub(crate) fn make_sleigh_over(bytes: Vec<u8>, base: u64) -> rsleigh::Sleigh<TestReader> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh")
}

pub(crate) fn make_sleigh() -> rsleigh::Sleigh<TestReader> {
    make_sleigh_over(Vec::new(), 0x0)
}

pub(crate) fn make_builder<'a>(
    start_addr: u64,
    sleigh: &'a mut rsleigh::Sleigh<TestReader>,
) -> Builder<'a, TestReader> {
    make_builder_opts(start_addr, sleigh, &CfgOptions::default())
}

pub(crate) fn make_builder_opts<'a>(
    start_addr: u64,
    sleigh: &'a mut rsleigh::Sleigh<TestReader>,
    options: &CfgOptions,
) -> Builder<'a, TestReader> {
    let arch = SleighArch::x86_64();
    Builder::for_arch(&arch, sleigh, start_addr, options)
}
