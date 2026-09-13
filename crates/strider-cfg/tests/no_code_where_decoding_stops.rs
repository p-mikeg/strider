//! Bytes that are not code, reached by a fall-through past a call or by a
//! direct branch, are reported instead of failing the function or being
//! decoded: an instruction Sleigh rejects (glibc's PowerPC `abort` word, a
//! gcc traceback table) and a range the image marks as data (an ARM literal
//! pool after a call to an unmarked no-return function).

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{
    Builder, Cfg, CfgOptions, DataRanges, PcodeInsnAddr, RegionTerminator, ResolvedTargets,
};
use strider_target::SleighArch;

fn build(arch: &SleighArch, bytes: Vec<u8>, start: u64, opts: &CfgOptions) -> anyhow::Result<Cfg> {
    let reader = BufMemReader::new(bytes, start);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
    Builder::for_arch(arch, &mut sleigh, start, opts).build()
}

fn be_words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_be_bytes()).collect()
}

fn le_words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn at(addr: u64) -> PcodeInsnAddr {
    PcodeInsnAddr::at_machine_start(addr)
}

fn decoded_addrs(cfg: &Cfg) -> Vec<u64> {
    let mut v: Vec<u64> = cfg
        .regions()
        .flat_map(|r| r.insns.iter().map(|i| i.addr.machine_addr.addr))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn bounded(size: u64) -> CfgOptions {
    CfgOptions {
        fn_max_size: Some(size),
        ..CfgOptions::default()
    }
}

const PPC_BL_PLUS_0X100: u32 = 0x4800_0101;
const PPC_BLR: u32 = 0x4e80_0020;
const PPC_LI_R3_0: u32 = 0x3860_0000;
const PPC_BEQ_PLUS_8: u32 = 0x4182_0008;

#[test]
fn an_undecodable_word_after_a_call_ends_the_call_as_no_return() {
    let cfg = build(
        &SleighArch::ppc32be(),
        be_words(&[PPC_BL_PLUS_0X100, 0, PPC_BLR]),
        0x1000,
        &bounded(0xc),
    )
    .expect("a word past a call that does not decode must not fail the function");
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::NoReturn
    );
    assert_eq!(cfg.undecodable_branch_targets(), &[at(0x1004)]);
    assert!(!decoded_addrs(&cfg).contains(&0x1004));
}

#[test]
fn an_undecodable_word_past_anything_but_a_call_still_fails_the_function() {
    let err = build(
        &SleighArch::ppc32be(),
        be_words(&[PPC_LI_R3_0, 0, PPC_BLR]),
        0x1000,
        &bounded(0xc),
    );
    assert!(
        err.is_err(),
        "a fall-through into bad bytes is no call's return"
    );
}

#[test]
fn a_direct_branch_to_an_undecodable_word_is_reported_not_fatal() {
    let cfg = build(
        &SleighArch::ppc32be(),
        be_words(&[PPC_BEQ_PLUS_8, PPC_BLR, 0]),
        0x1000,
        &bounded(0xc),
    )
    .expect("a branch to a word that does not decode must not fail the function");
    assert_eq!(cfg.undecodable_branch_targets(), &[at(0x1008)]);
    let stub = cfg
        .regions()
        .find(|r| r.start_addr == at(0x1008))
        .expect("the branch keeps an edge");
    assert!(stub.insns.is_empty());
    assert!(matches!(stub.terminator, RegionTerminator::TailCall { .. }));
}

// ARM, little-endian:
//   0x1000 push {r4, lr}
//   0x1004 cmp  r0, #0
//   0x1008 bne  0x1018
//   0x100c bl   0x2000        (an unmarked no-return callee)
//   0x1010 .word 0x00012345   ($d: decodes as andeq)
//   0x1014 .word 0x00010064
//   0x1018 ldr  r1, [pc, #-16]
//   0x101c add  r0, r0, r1
//   0x1020 pop  {r4, pc}
const ARM_MID_POOL: [u32; 9] = [
    0xe92d_4010,
    0xe350_0000,
    0x1a00_0002,
    0xeb00_03fb,
    0x0001_2345,
    0x0001_0064,
    0xe51f_1010,
    0xe080_0001,
    0xe8bd_8010,
];

fn with_pool_marked(mut opts: CfgOptions) -> CfgOptions {
    opts.data_ranges = DataRanges::new(std::iter::once(0x1010..0x1018));
    opts
}

#[test]
fn a_literal_pool_after_a_call_is_never_decoded() {
    let arch = SleighArch::arm();
    let unmarked = build(&arch, le_words(&ARM_MID_POOL), 0x1000, &bounded(0x24)).expect("build");
    assert!(
        decoded_addrs(&unmarked).contains(&0x1010),
        "without the data range the pool decodes as code"
    );

    let cfg = build(
        &arch,
        le_words(&ARM_MID_POOL),
        0x1000,
        &with_pool_marked(bounded(0x24)),
    )
    .expect("build");
    let decoded = decoded_addrs(&cfg);
    assert!(
        !decoded.iter().any(|a| (0x1010..0x1018).contains(a)),
        "decoded inside the data range: {decoded:x?}"
    );
    let call_region = cfg
        .regions()
        .find(|r| r.insns.iter().any(|i| i.addr.machine_addr.addr == 0x100c))
        .expect("the call decodes");
    assert_eq!(call_region.terminator, RegionTerminator::NoReturn);
    assert_eq!(cfg.undecodable_branch_targets(), &[at(0x1010)]);
}

#[test]
fn a_direct_branch_into_a_data_range_is_reported_not_decoded() {
    // 0x1000 b 0x1008 ; 0x1004 bx lr ; 0x1008 .word (data)
    let words = [0xea00_0000, 0xe12f_ff1e, 0xe1a0_0000];
    let mut opts = bounded(0xc);
    opts.data_ranges = DataRanges::new(std::iter::once(0x1008..0x100c));
    let cfg = build(&SleighArch::arm(), le_words(&words), 0x1000, &opts).expect("build");
    assert!(!decoded_addrs(&cfg).contains(&0x1008));
    assert_eq!(cfg.undecodable_branch_targets(), &[at(0x1008)]);
}

#[test]
fn a_seeded_arm_into_a_data_range_is_an_undecodable_seeded_target() {
    // 0x1000 bx r3 ; 0x1004 bx lr ; 0x1008 .word (data)
    let words = [0xe12f_ff13, 0xe12f_ff1e, 0xe1a0_0000];
    let site = PcodeInsnAddr::at_machine_start(0x1000);
    let mut opts = bounded(0xc);
    opts.data_ranges = DataRanges::new(std::iter::once(0x1008..0x100c));
    opts.known_targets.insert(
        site,
        ResolvedTargets::Multiple(vec![0x1004.into(), 0x1008.into()]),
    );
    let cfg = build(&SleighArch::arm(), le_words(&words), 0x1000, &opts).expect("build");
    assert!(!decoded_addrs(&cfg).contains(&0x1008));
    assert_eq!(
        cfg.undecodable_seeded_targets()
            .iter()
            .map(|u| u.target.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![0x1008]
    );
}

#[test]
fn data_ranges_answer_half_open_overlap_across_overlapping_inputs() {
    let ranges = DataRanges::new([0x2000..0x2004, 0x1010..0x1018, 0x1014..0x101c]);
    assert!(ranges.overlaps(0x100e, 0x1012));
    assert!(ranges.overlaps(0x101b, 0x101c));
    assert!(!ranges.overlaps(0x101c, 0x1020));
    assert!(!ranges.overlaps(0x1000, 0x1010));
    assert!(ranges.overlaps(0x2003, 0x2004));
}
