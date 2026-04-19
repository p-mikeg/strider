//! Shared test helpers for the builder submodules.

use std::collections::VecDeque;

use super::region_builder::RegionBuilder;
use super::Builder;
use crate::cfg::options::{Options, OptionsBuilder};
use crate::cfg::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction};

/// Short constructor for a [`PcodeInsnAddr`].
pub(super) fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
    PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: machine },
        insn_index: insn,
    }
}

/// Returns a minimal Sleigh backed by an empty buffer.
///
/// The resulting Sleigh cannot actually decode instructions (the buffer is
/// empty) but is sufficient for constructing a [`Builder`] and testing all
/// methods that do not call `lift_one`.
pub(super) fn make_sleigh() -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86,
        rsleigh::pspec::PSPEC_X86,
        reader,
    )
    .expect("failed to create test Sleigh")
}

/// Returns a [`Builder`] seeded at `start_addr` with default options.
pub(super) fn make_builder(
    start_addr: u64,
) -> Builder<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
    Builder::new(make_sleigh(), start_addr, OptionsBuilder::new().build())
}

/// Returns a [`Builder`] seeded at `start_addr` with the given `options`.
pub(super) fn make_builder_opts(
    start_addr: u64,
    options: Options,
) -> Builder<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
    Builder::new(make_sleigh(), start_addr, options)
}

/// A minimal dummy pcode instruction (no inputs/outputs, opcode = Copy).
pub(super) fn fake_insn() -> rsleigh::Insn {
    rsleigh::Insn {
        opcode: rsleigh::Opcode::Copy,
        output: None,
        inputs: vec![],
    }
}

/// Builds a [`Region`] from a list of `(machine_addr, insn_index)` pairs.
///
/// The first pair is used as `start_addr`; all pairs become instructions.
/// Panics if `addrs` is empty.
pub(super) fn make_region(addrs: &[(u64, u64)]) -> Region {
    assert!(
        !addrs.is_empty(),
        "make_region requires at least one address"
    );
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
        ends_with_tail_call: false,
    }
}

/// Builds a [`RegionBuilder`] for `builder` that starts at `start`.
pub(super) fn make_region_builder<'a>(
    builder: &'a mut Builder<rsleigh::mem_readers::BufMemReader<Vec<u8>>>,
    start: PcodeInsnAddr,
) -> RegionBuilder<'a, rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
    RegionBuilder {
        builder,
        start_addr: start,
        insns: VecDeque::new(),
        parent_edge: None,
    }
}
