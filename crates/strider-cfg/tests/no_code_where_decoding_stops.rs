//! Bytes that are not code, reached by a fall-through past a call or by a
//! direct branch, are reported instead of failing the function or being
//! decoded: an instruction Sleigh rejects (glibc's PowerPC `abort` word, a
//! gcc traceback table) and a range the image marks as data (an ARM literal
//! pool after a call to an unmarked no-return function).

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{
    Cfg, CfgOptions, DataRanges, DecodeEngine, PcodeInsnAddr, RegionTerminator, ResolvedTargets,
};
use strider_target::SleighArch;

fn build(arch: &SleighArch, bytes: Vec<u8>, start: u64, opts: &CfgOptions) -> anyhow::Result<Cfg> {
    let reader = BufMemReader::new(bytes, start);
    DecodeEngine::new(*arch, reader)
        .expect("decode engine")
        .build_cfg(start.into(), opts)
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
        .flat_map(|r| r.insns.iter().map(|i| i.addr.machine()))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn bounded(size: u64) -> CfgOptions<'static> {
    CfgOptions {
        bound: strider_cfg::FunctionBound::size(size),
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
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1004)]
    );
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
fn a_zero_pcode_op_run_no_call_falls_into_still_fails_the_function() {
    // mov eax, 1 ; nop ; bad
    let bytes = vec![0xb8, 0x01, 0x00, 0x00, 0x00, 0x90, 0x06, 0x06, 0x06, 0x06];
    let err = build(&SleighArch::x86_64(), bytes, 0x1000, &CfgOptions::default());
    assert!(
        err.is_err(),
        "a nop between the fall-through and the bad bytes changes nothing"
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
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1008)]
    );
    let stub = cfg
        .regions()
        .find(|r| r.start_addr == at(0x1008))
        .expect("the branch keeps an edge");
    assert!(stub.insns.is_empty());
    assert!(matches!(stub.terminator, RegionTerminator::TailCall { .. }));
}

/// `build` gives every zero-pcode-op instruction its own region, so a
/// `call; nop; <not code>` run reaches the seal from the nop's region, which
/// holds no call. The call ahead of it is the one that does not return.
#[test]
fn a_zero_pcode_op_run_between_a_call_and_bad_bytes_still_ends_the_call() {
    // 0x1000 call 0x2000 (e8 fb 0f 00 00) ; 0x1005 nop (90) ;
    // 0x1006 bytes x86-64 rejects (06 06 06 06).
    let cfg = build(
        &SleighArch::x86_64(),
        vec![0xe8, 0xfb, 0x0f, 0x00, 0x00, 0x90, 0x06, 0x06, 0x06, 0x06],
        0x1000,
        &CfgOptions::default(),
    )
    .expect("a nop between a call and bad bytes must not fail the function");
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::NoReturn
    );
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1006)]
    );
    assert!(!decoded_addrs(&cfg).contains(&0x1006));
}

/// Code decoded past the zero-pcode-op run (a literal pool's words) does not
/// hide the call either.
#[test]
fn code_past_a_zero_pcode_op_run_after_a_call_still_ends_the_call() {
    // 0x1000 call 0x2000 ; 0x1005 nop ; 0x1006 inc eax (ff c0) ; 0x1008 bad.
    let x86 = vec![
        0xe8, 0xfb, 0x0f, 0x00, 0x00, 0x90, 0xff, 0xc0, 0x06, 0x06, 0x06, 0x06,
    ];
    // armhf glibc `dlsym` tail: 0x58d7a bl 0xbdf24 ; 0x58d7e nop ;
    // 0x58d80 literal pool, decoding until 0x58d94.
    let hex = "65f0d3f800bf0c530c008c000000e8000000f6520c0078010000a9ffffffba520c00a0520c00";
    let thumb = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    for (arch, bytes, base, entry, bad) in [
        (SleighArch::x86_64(), x86, 0x1000, 0x1000, 0x1008),
        (SleighArch::arm(), thumb, 0x58d7a, 0x58d7b, 0x58d94),
    ] {
        let cfg = DecodeEngine::new(arch, BufMemReader::new(bytes, base))
            .expect("decode engine")
            .build_cfg(u64::into(entry), &CfgOptions::default())
            .unwrap_or_else(|e| panic!("{base:#x}: {e:#}"));
        assert_eq!(
            cfg.region_graph()[cfg.entry()].terminator,
            RegionTerminator::NoReturn,
            "{base:#x}"
        );
        assert_eq!(
            cfg.incomplete()
                .of(strider_cfg::Reason::UndecodableTarget)
                .collect::<Vec<_>>(),
            &[at(bad)]
        );
        assert_eq!(decoded_addrs(&cfg), &[base], "{base:#x}");
    }
}

/// A branch landing on one of those zero-pcode-op instructions still has a
/// successor: the run keeps its regions, sealed onto the stub at the bytes.
#[test]
fn a_branch_into_the_zero_pcode_op_run_past_a_call_keeps_a_successor() {
    // 0x1000 je 0x1007 (74 05) ; 0x1002 call (e8 00 10 00 00) ; 0x1007 nop ;
    // 0x1008 bytes x86-64 rejects.
    let cfg = build(
        &SleighArch::x86_64(),
        vec![
            0x74, 0x05, 0xe8, 0x00, 0x10, 0x00, 0x00, 0x90, 0x06, 0x06, 0x06, 0x06,
        ],
        0x1000,
        &CfgOptions::default(),
    )
    .expect("build");
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1008)]
    );
    let nop = cfg
        .region_ids()
        .find(|&id| cfg.region_graph()[id].start_addr == at(0x1007))
        .expect("the branch target keeps a region");
    assert_eq!(cfg.region_graph().neighbors(nop).count(), 1);
    let call = cfg
        .regions()
        .find(|r| r.insns.iter().any(|i| i.addr.machine() == 0x1002))
        .expect("the call decodes");
    assert_eq!(call.terminator, RegionTerminator::NoReturn);
}

/// Which predecessor reaches the zero-pcode-op run first does not change the
/// verdict: a branch into the run explored before the call's fall-through.
#[test]
fn a_zero_pcode_op_run_into_bad_bytes_is_decided_whichever_edge_comes_first() {
    // test eax, eax ; je 0x1009 ; call 0x2000 ; 0x1009 nop ; 0x100a bad
    let call_first = [
        0x85, 0xc0, 0x74, 0x05, 0xe8, 0xf7, 0x0f, 0x00, 0x00, 0x90, 0x06, 0x06, 0x06, 0x06,
    ];
    // test eax, eax ; jne 0x1006 ; jmp 0x100b ; call 0x2000 ; 0x100b nop ; 0x100c bad
    let branch_first = [
        0x85, 0xc0, 0x75, 0x02, 0xeb, 0x05, 0xe8, 0xf5, 0x0f, 0x00, 0x00, 0x90, 0x06, 0x06, 0x06,
        0x06,
    ];
    for (bytes, call, bad) in [
        (&call_first[..], 0x1004, 0x100a),
        (&branch_first[..], 0x1006, 0x100c),
    ] {
        let cfg = build(
            &SleighArch::x86_64(),
            bytes.to_vec(),
            0x1000,
            &CfgOptions::default(),
        )
        .expect("a nop run into bad bytes must not fail the function");
        assert_eq!(
            cfg.incomplete()
                .of(strider_cfg::Reason::UndecodableTarget)
                .collect::<Vec<_>>(),
            &[at(bad)]
        );
        let call = cfg
            .regions()
            .find(|r| r.insns.iter().any(|i| i.addr.machine() == call))
            .expect("the call decodes");
        assert_eq!(
            call.terminator,
            RegionTerminator::NoReturn,
            "call at {call:?}"
        );
    }
}

/// The same for a literal pool a `$d` mapping symbol marks, which is what real
/// `gcc -O2 -fPIC` output puts after a call to an unmarked no-return callee.
#[test]
fn a_zero_pcode_op_run_between_a_call_and_a_data_range_still_ends_the_call() {
    // 0x1000 bl 0x2000 ; 0x1004 nop ; 0x1008 .word (data)
    let words = [0x9400_0400, 0xd503_201f, 0x0000_0000];
    let mut opts = bounded(0xc);
    opts.data_ranges = DataRanges::new(std::iter::once(0x1008..0x100c));
    let cfg = build(&SleighArch::aarch64(), le_words(&words), 0x1000, &opts).expect("build");
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::NoReturn
    );
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1008)]
    );
    assert!(
        cfg.regions()
            .all(|r| !matches!(r.terminator, RegionTerminator::TailCall { .. })),
        "no stub is seated at the literal pool",
    );
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
        .find(|r| r.insns.iter().any(|i| i.addr.machine() == 0x100c))
        .expect("the call decodes");
    assert_eq!(call_region.terminator, RegionTerminator::NoReturn);
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1010)]
    );
}

#[test]
fn a_direct_branch_into_a_data_range_is_reported_not_decoded() {
    // 0x1000 b 0x1008 ; 0x1004 bx lr ; 0x1008 .word (data)
    let words = [0xea00_0000, 0xe12f_ff1e, 0xe1a0_0000];
    let mut opts = bounded(0xc);
    opts.data_ranges = DataRanges::new(std::iter::once(0x1008..0x100c));
    let cfg = build(&SleighArch::arm(), le_words(&words), 0x1000, &opts).expect("build");
    assert!(!decoded_addrs(&cfg).contains(&0x1008));
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1008)]
    );
}

#[test]
fn a_seeded_arm_into_a_data_range_is_an_undecodable_seeded_target() {
    // 0x1000 bx r3 ; 0x1004 bx lr ; 0x1008 .word (data)
    let words = [0xe12f_ff13, 0xe12f_ff1e, 0xe1a0_0000];
    let site = PcodeInsnAddr::at_machine_start(0x1000);
    let known = rustc_hash::FxHashMap::from_iter([(
        site,
        ResolvedTargets::table(vec![0x1004.into(), 0x1008.into()]),
    )]);
    let opts = CfgOptions {
        data_ranges: DataRanges::new(std::iter::once(0x1008..0x100c)),
        known_targets: &known,
        ..bounded(0xc)
    };
    let cfg = build(&SleighArch::arm(), le_words(&words), 0x1000, &opts).expect("build");
    assert!(!decoded_addrs(&cfg).contains(&0x1008));
    assert_eq!(
        cfg.undecodable_seeded_targets()
            .iter()
            .map(|u| u.target.machine())
            .collect::<Vec<_>>(),
        vec![0x1008]
    );
}

/// A direct branch and a fall-through reaching one data address report it
/// once: the branch seats the stub from `Builder::build`'s error path, and the
/// fall-through fails `lift_code` before it checks the region map.
#[test]
fn one_data_address_two_edges_reach_is_reported_once() {
    // 0x1000 beq 0x1010 ; 0x1004 b 0x1014 ; 0x1008, 0x100c nop (unreached) ;
    // 0x1010 nop, falling through to the 0x1014 word.
    let words = [
        0x0a00_0002,
        0xea00_0002,
        0xe1a0_0000,
        0xe1a0_0000,
        0xe1a0_0000,
        0x0000_0000,
    ];
    let mut opts = bounded(0x18);
    opts.data_ranges = DataRanges::new(std::iter::once(0x1014..0x1018));
    let cfg = build(&SleighArch::arm(), le_words(&words), 0x1000, &opts).expect("build");
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1014)]
    );
}

// ARM kernel entries written as `.word` under a `$d` mapping symbol.
const ARM_UDF_BUG: u32 = 0xe7f0_01f2;
const ARM_HVC_0: u32 = 0xe140_0070;
const ARM_BX_LR: u32 = 0xe12f_ff1e;
const ARM_NOP: u32 = 0xe1a0_0000;

#[test]
fn an_entry_instruction_inside_a_data_range_is_decoded() {
    let mut opts = bounded(0x8);
    opts.data_ranges = DataRanges::new(std::iter::once(0x1000..0x1004));
    let cfg = build(
        &SleighArch::arm(),
        le_words(&[ARM_UDF_BUG, ARM_NOP]),
        0x1000,
        &opts,
    )
    .expect("the entry instruction decodes whatever the data ranges say");
    assert_eq!(decoded_addrs(&cfg), vec![0x1000]);
    assert!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .next()
            .is_none()
    );

    let cfg = build(
        &SleighArch::arm(),
        le_words(&[ARM_HVC_0, ARM_BX_LR]),
        0x1000,
        &opts,
    )
    .expect("the instruction after a data-marked entry is ordinary code");
    assert_eq!(decoded_addrs(&cfg), vec![0x1000, 0x1004]);
}

#[test]
fn a_data_range_past_the_entry_instruction_still_governs_its_fall_through() {
    let mut opts = bounded(0x8);
    opts.data_ranges = DataRanges::new(std::iter::once(0x1000..0x1008));
    let cfg = build(
        &SleighArch::arm(),
        le_words(&[ARM_NOP, ARM_BX_LR]),
        0x1000,
        &opts,
    )
    .expect("build");
    assert_eq!(decoded_addrs(&cfg), vec![0x1000]);
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x1004)]
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

/// aarch64 `-mcmodel=large`: `bl die` to a no-return function, its literal
/// pool, then `die` itself. With no size bound the call stays a call, cut
/// where the pool begins, not a jump into the next function.
#[test]
fn a_call_before_a_literal_pool_and_the_next_function_stays_a_reported_call() {
    const WORDS: [u32; 13] = [
        0x7100_0c1f, // cmp w0, #3
        0x5400_00cd, // b.le .+0x18
        0x2a00_03e1, // mov w1, w0
        0x9000_0000, // adrp x0, ...
        0xf942_fc00, // ldr x0, [x0, #0x5f8]
        0xb861_d800, // ldr w0, [x0, w1, sxtw #2]
        0xd65f_03c0, // ret
        0xa9bf_7bfd, // stp x29, x30, [sp, #-16]!
        0x9100_03fd, // mov x29, sp
        0x9400_0003, // bl 0x400600 <die>
        0x0041_1030, // literal pool
        0x0000_0000,
        0x1400_0000, // die: b .
    ];
    let cfg = build(
        &SleighArch::aarch64(),
        le_words(&WORDS),
        0x40_05d0,
        &CfgOptions::default(),
    )
    .expect("cfg");
    let call_region = cfg
        .regions()
        .find(|r| r.insns.iter().any(|i| i.addr.machine() == 0x40_05f4))
        .expect("the bl is decoded");
    assert_eq!(call_region.terminator, RegionTerminator::NoReturn);
    assert!(
        call_region
            .insns
            .iter()
            .any(|i| i.addr.machine() == 0x40_05f4 && i.insn.opcode == rsleigh::Opcode::Call)
    );
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
        &[at(0x40_05f8)]
    );
    assert!(!decoded_addrs(&cfg).contains(&0x40_0600));
}
