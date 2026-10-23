use rsleigh::mem_readers::BufMemReader;
use strider_target::SleighArch;

use crate::builder::Builder;
use crate::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};
use crate::{CfgOptions, DecodeEngine};

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

/// A [`crate::Cfg`] over a hand-built region graph, complete and with no ISA
/// mode.
pub(crate) fn make_cfg(
    region_graph: crate::types::RegionGraph,
    entry: petgraph::graph::NodeIndex,
) -> crate::Cfg {
    let mut index = crate::region_index::RegionIndex::default();
    for id in region_graph.node_indices() {
        index.insert(&region_graph, id);
    }
    crate::Cfg {
        region_graph,
        entry,
        index,
        undecodable_seeded: Vec::new(),
        incomplete: crate::Incompleteness::default(),
        link_register_seated: Vec::new(),
        tail_call_seated: Vec::new(),
        function_isa_bit: None,
        flowing_isa_bits: std::collections::BTreeMap::new(),
        space_ids: rsleigh::SpaceIds::default(),
    }
}

/// An x86-64 Sleigh reading `bytes` mapped at `base`.
pub(crate) fn make_sleigh_over(bytes: Vec<u8>, base: u64) -> rsleigh::Sleigh<TestReader> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    arch.sleigh(reader).expect("create Sleigh")
}

/// [`make_sleigh_over`] as an engine.
pub(crate) fn make_engine_over(bytes: Vec<u8>, base: u64) -> DecodeEngine<TestReader> {
    DecodeEngine::new(SleighArch::x86_64(), BufMemReader::new(bytes, base))
        .expect("x86-64 decode engine")
}

pub(crate) fn make_engine() -> DecodeEngine<TestReader> {
    make_engine_over(Vec::new(), 0x0)
}

pub(crate) fn make_builder(
    start_addr: u64,
    engine: &mut DecodeEngine<TestReader>,
) -> Builder<'_, TestReader> {
    static DEFAULT_OPTIONS: std::sync::LazyLock<CfgOptions<'static>> =
        std::sync::LazyLock::new(CfgOptions::default);
    make_builder_opts(start_addr, engine, &DEFAULT_OPTIONS)
}

/// The builder production gets: the entry pinned on `engine`.
pub(crate) fn make_builder_opts<'a>(
    start_addr: u64,
    engine: &'a mut DecodeEngine<TestReader>,
    options: &'a CfgOptions<'a>,
) -> Builder<'a, TestReader> {
    engine
        .builder(start_addr.into(), options)
        .expect("pin the entry")
}
