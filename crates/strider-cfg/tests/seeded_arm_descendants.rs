//! Code decoded off a seeded jump-table arm is only as certain as the arm:
//! when anything it leads to will not decode, the arm is dropped and reported
//! instead of failing the whole function.

use rsleigh::mem_readers::BufMemReader;
use rustc_hash::FxHashMap;
use strider_cfg::{
    Cfg, CfgOptions, DecodeEngine, PcodeInsnAddr, RegionTerminator, ResolvedTargets,
};
use strider_target::SleighArch;

const BASE: u64 = 0x1000;

fn build(bytes: &[u8], arms: &[u64]) -> anyhow::Result<Cfg> {
    let arch = SleighArch::x86_64();
    let mut code = bytes.to_vec();
    code.resize(bytes.len() + 16, 0xcc);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), BufMemReader::new(code, BASE))
        .expect("create Sleigh");
    let known = FxHashMap::from_iter([(
        PcodeInsnAddr::at_machine_start(BASE),
        ResolvedTargets::table(arms.iter().map(|&a| strider_cfg::ResolvedTarget::from(a))),
    )]);
    let opts = CfgOptions {
        known_targets: &known,
        ..CfgOptions::default()
    };
    DecodeEngine::new(arch, sleigh)
        .expect("decode engine")
        .build_cfg(BASE.into(), &opts)
}

fn switch_arms(cfg: &Cfg) -> Vec<u64> {
    cfg.regions()
        .find_map(|r| match &r.terminator {
            RegionTerminator::Switch { targets, .. } => {
                Some(targets.iter().map(|t| t.addr).collect())
            }
            _ => None,
        })
        .unwrap_or_default()
}

fn undecodable(cfg: &Cfg) -> Vec<u64> {
    cfg.undecodable_seeded_targets()
        .iter()
        .map(|t| t.target.machine_addr.addr)
        .collect()
}

/// The bytes that would not decode, as opposed to the arm that led to them.
fn undecodable_bytes(cfg: &Cfg) -> Vec<u64> {
    cfg.incomplete()
        .of(strider_cfg::Reason::UndecodableTarget)
        .map(|a| a.machine_addr.addr)
        .collect()
}

/// Every edge a region has must land on a region, and every `Switch` arm on a
/// successor starting at it.
fn assert_no_dangling_successor(cfg: &Cfg) {
    for rid in cfg.region_ids() {
        let region = cfg.region_graph().node_weight(rid).unwrap();
        let succs = cfg.region_graph().neighbors(rid).count();
        match &region.terminator {
            RegionTerminator::Unconditional => assert_eq!(succs, 1, "{:?}", region.start_addr),
            RegionTerminator::CondBranch { .. } => {
                assert_eq!(succs, 2, "{:?}", region.start_addr);
            }
            RegionTerminator::Switch { targets, .. } => {
                let arms = cfg.switch_arm_regions(rid);
                for t in targets {
                    assert!(arms.contains_key(&PcodeInsnAddr::at_machine_start(t.addr)));
                }
            }
            _ => {}
        }
    }
}

/// ```text
/// 1000  jmp rax
/// 1002  ret               ; good arm
/// 1003  mov rax, rax
/// 1006  (invalid)
/// 1007  nop               ; bad arm
/// 1008  mov rax, rax
/// 100b  (invalid)
/// ```
const NOP_LED_ARM: [u8; 12] = [
    0xff, 0xe0, 0xc3, 0x48, 0x89, 0xc0, 0x06, 0x90, 0x48, 0x89, 0xc0, 0x06,
];

#[test]
fn an_arm_whose_first_instruction_is_a_nop_is_dropped_like_any_bad_arm() {
    for bad in [0x1003, 0x1007] {
        let cfg = build(&NOP_LED_ARM, &[0x1002, bad])
            .unwrap_or_else(|e| panic!("arm {bad:#x} failed the function: {e:#}"));
        assert_eq!(switch_arms(&cfg), [0x1002], "arm {bad:#x}");
        assert_eq!(undecodable(&cfg), [bad], "arm {bad:#x}");
        // The bytes, not the arm: both arms decode, and the decode fails
        // several instructions past them.
        let bytes = if bad == 0x1003 { 0x1006 } else { 0x100b };
        assert_eq!(undecodable_bytes(&cfg), [bytes], "arm {bad:#x}");
        assert_no_dangling_successor(&cfg);
    }
}

/// ```text
/// 1000  jmp rax
/// 1002  ret               ; good arm
/// 1003  jmp 0x1008        ; bad arm
/// 1005  ret
/// 1006  ret
/// 1007  ret
/// 1008  (invalid)
/// ```
#[test]
fn an_arm_that_branches_into_bytes_that_will_not_decode_is_dropped() {
    let bytes = [0xff, 0xe0, 0xc3, 0xeb, 0x03, 0xc3, 0xc3, 0xc3, 0x06];
    let cfg = build(&bytes, &[0x1002, 0x1003]).expect("one bad arm must not fail the function");
    assert_eq!(switch_arms(&cfg), [0x1002]);
    assert_eq!(undecodable(&cfg), [0x1003]);
    assert_eq!(
        undecodable_bytes(&cfg),
        [0x1008],
        "the address that would not decode is named, not only the arm",
    );
    assert_no_dangling_successor(&cfg);
}

/// Bad arms are reported in the order their decode failed, which for one
/// switch is lowest arm first: the region builder enqueues a switch's arms
/// highest first onto a LIFO queue.
#[test]
fn bad_arms_are_reported_in_the_order_they_failed() {
    let cfg = build(&NOP_LED_ARM, &[0x1002, 0x1003, 0x1007])
        .expect("bad arms must not fail the function");
    assert_eq!(switch_arms(&cfg), [0x1002]);
    assert_eq!(undecodable(&cfg), [0x1003, 0x1007]);
    assert_no_dangling_successor(&cfg);
}

/// A second arm jumping into the first bad arm's code reaches the same
/// undecodable bytes, so it goes too.
///
/// ```text
/// 100c  jmp 0x1007        ; third arm
/// ```
#[test]
fn every_arm_that_reaches_a_region_with_an_undecodable_successor_is_dropped() {
    let mut bytes = NOP_LED_ARM.to_vec();
    bytes.extend_from_slice(&[0xeb, 0xf9]); // 0x100c: jmp 0x1007
    let cfg =
        build(&bytes, &[0x1002, 0x1007, 0x100c]).expect("bad arms must not fail the function");
    assert_eq!(switch_arms(&cfg), [0x1002]);
    let mut dropped = undecodable(&cfg);
    dropped.sort_unstable();
    assert_eq!(dropped, [0x1007, 0x100c]);
    assert_no_dangling_successor(&cfg);
}

/// Bytes the function itself reaches without any arm stay an error.
///
/// ```text
/// 1000  je 0x1007
/// 1002  jmp rax
/// 1004  ret               ; arm
/// 1005  ret
/// 1006  ret
/// 1007  nop
/// 1008  mov rax, rax
/// 100b  (invalid)
/// ```
#[test]
fn undecodable_bytes_reached_without_an_arm_still_fail_the_function() {
    let bytes = [
        0x74, 0x05, 0xff, 0xe0, 0xc3, 0xc3, 0xc3, 0x90, 0x48, 0x89, 0xc0, 0x06,
    ];
    let arch = SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes.to_vec(), BASE),
    )
    .expect("create Sleigh");
    let known = FxHashMap::from_iter([(
        PcodeInsnAddr::at_machine_start(0x1002),
        ResolvedTargets::table(vec![0x1004.into(), 0x1007.into()]),
    )]);
    let opts = CfgOptions {
        known_targets: &known,
        ..CfgOptions::default()
    };
    assert!(
        DecodeEngine::new(arch, sleigh)
            .expect("decode engine")
            .build_cfg(BASE.into(), &opts)
            .is_err()
    );
}

/// Builds over `bytes` with one `known_targets` entry per site.
fn build_sites(bytes: &[u8], size: u64, sites: &[(u64, &[u64])]) -> anyhow::Result<Cfg> {
    let arch = SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes.to_vec(), BASE),
    )
    .expect("create Sleigh");
    let known = FxHashMap::from_iter(sites.iter().map(|&(site, arms)| {
        (
            PcodeInsnAddr::at_machine_start(site),
            ResolvedTargets::table(arms.iter().map(|&a| strider_cfg::ResolvedTarget::from(a))),
        )
    }));
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(size),
        known_targets: &known,
        ..CfgOptions::default()
    };
    DecodeEngine::new(arch, sleigh)
        .expect("decode engine")
        .build_cfg(BASE.into(), &opts)
}

/// The arms left on the `Switch` whose dispatch is at `site`.
fn arms_at(cfg: &Cfg, site: u64) -> Vec<u64> {
    cfg.regions()
        .find_map(|r| match &r.terminator {
            RegionTerminator::Switch { targets, addr, .. } if addr.machine_addr.addr == site => {
                Some(targets.iter().map(|t| t.addr).collect())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// `0xcc` padding, with `at` poked in at each offset from `BASE`.
fn image(size: usize, at: &[(u64, &[u8])]) -> Vec<u8> {
    let mut bytes = vec![0xccu8; size];
    for &(addr, code) in at {
        let off = (addr - BASE) as usize;
        bytes[off..off + code.len()].copy_from_slice(code);
    }
    bytes
}

/// An arm reaching the bad bytes only back through another dispatch is no
/// evidence about its own bound: refusing it would orphan the inner table and
/// every good arm of it.
///
/// ```text
/// 1000  jmp rax           ; outer, arms 0x1002 and 0x100d
/// 1002  jmp rbx           ; inner, arms 0x1004 and 0x1005
/// 1004  ret
/// 1005  je 0x1009
/// 1007  ret
/// 1009  (invalid)
/// 100d  ret
/// ```
#[test]
fn an_inner_tables_bad_arm_does_not_refuse_the_outer_arm_leading_to_it() {
    let bytes = image(
        0x18,
        &[
            (0x1000, &[0xff, 0xe0]),
            (0x1002, &[0xff, 0xe3]),
            (0x1004, &[0xc3]),
            (0x1005, &[0x74, 0x02]),
            (0x1007, &[0xc3]),
            (0x1009, &[0x06, 0x06, 0x06, 0x06]),
            (0x100d, &[0xc3]),
        ],
    );
    let cfg = build_sites(
        &bytes,
        0x18,
        &[(0x1000, &[0x1002, 0x100d]), (0x1002, &[0x1004, 0x1005])],
    )
    .expect("one bad inner arm must not fail the function");
    assert_eq!(arms_at(&cfg, 0x1000), [0x1002, 0x100d]);
    assert_eq!(arms_at(&cfg, 0x1002), [0x1004]);
    assert_eq!(undecodable(&cfg), [0x1005]);
    assert_no_dangling_successor(&cfg);
}

/// An arm looping back to the dispatch reaches the bad bytes only through it,
/// so it survives.
///
/// ```text
/// 1000  jmp rax           ; arms 0x1010, 0x1020, 0x1030
/// 1010  jmp 0x1000
/// 1020  je 0x1024
/// 1022  ret
/// 1024  (invalid)
/// 1030  ret
/// ```
#[test]
fn a_loop_back_edge_arm_is_not_refused_with_the_bad_arm_beside_it() {
    let bytes = image(
        0x38,
        &[
            (0x1000, &[0xff, 0xe0]),
            (0x1010, &[0xeb, 0xee]),
            (0x1020, &[0x74, 0x02]),
            (0x1022, &[0xc3]),
            (0x1024, &[0x06, 0x06, 0x06, 0x06]),
            (0x1030, &[0xc3]),
        ],
    );
    let cfg = build_sites(&bytes, 0x38, &[(0x1000, &[0x1010, 0x1020, 0x1030])])
        .expect("one bad arm must not fail the function");
    assert_eq!(arms_at(&cfg, 0x1000), [0x1010, 0x1030]);
    assert_eq!(undecodable(&cfg), [0x1020]);
    assert_no_dangling_successor(&cfg);
}

/// A direct edge found while decoding an arm is only as certain as that arm,
/// and an earlier arm may already own its target in the other ISA mode: the
/// first decode keeps the bytes and the clash is reported.
///
/// ```text
/// 10000  bx r0                ; ARM, arms 0x10004 (Thumb) and 0x10008
/// 10004  movs r0, #1          ; Thumb
/// 10006  bx lr                ; Thumb
/// 10008  b 0x10004            ; ARM
/// ```
#[test]
fn an_edge_decoded_off_an_arm_does_not_take_bytes_an_earlier_arm_decoded() {
    use strider_cfg::ResolvedTarget;
    let base = 0x10000u64;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xe12f_ff10u32.to_le_bytes());
    bytes.extend_from_slice(&0x2001u16.to_le_bytes());
    bytes.extend_from_slice(&0x4770u16.to_le_bytes());
    bytes.extend_from_slice(&0xeaff_fffdu32.to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);

    let arch = SleighArch::arm();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, base),
    )
    .unwrap();
    let known = FxHashMap::from_iter([(
        PcodeInsnAddr::at_machine_start(base),
        ResolvedTargets::table(vec![
            ResolvedTarget::new(0x10004, Some(true)),
            ResolvedTarget::new(0x10008, None),
        ]),
    )]);
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x20),
        known_targets: &known,
        ..CfgOptions::default()
    };
    // An even entry is ARM state.
    let cfg = DecodeEngine::new(arch, sleigh)
        .unwrap()
        .build_cfg(base.into(), &opts)
        .expect("build");

    assert_eq!(switch_arms(&cfg), [0x10004, 0x10008]);
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::IsaModeConflict)
            .collect::<Vec<_>>(),
        [PcodeInsnAddr::at_machine_start(0x10004)]
    );
    let thumb = cfg
        .regions()
        .find(|r| r.start_addr.machine_addr.addr == 0x10004)
        .expect("region at 0x10004");
    assert_eq!(thumb.insns.first().map(|i| i.len), Some(2));
}

/// `xor eax, eax`, falling through to 0x1012.
const XOR_EAX: &[u8] = &[0x31, 0xc0];
/// `call 0x1100`, returning to 0x1015.
const CALL_OUT: &[u8] = &[0xe8, 0xeb, 0x00, 0x00, 0x00];

/// A `jmp rax` seeded with arms 0x1002 and 0x1010, over `len` bytes, built
/// with `data` marked as data.
///
/// ```text
/// 1000  jmp rax
/// 1002  ret               ; good arm
/// 1010  <arm>             ; falls through
/// ```
fn build_falling_arm(len: usize, arm: &[u8], data: Option<std::ops::Range<u64>>) -> Cfg {
    let mut bytes = image(len, &[(0x1000, &[0xff, 0xe0]), (0x1002, &[0xc3])]);
    bytes[0x10..0x10 + arm.len()].copy_from_slice(arm);
    let arch = SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, BASE),
    )
    .expect("create Sleigh");
    let known = FxHashMap::from_iter([(
        PcodeInsnAddr::at_machine_start(BASE),
        ResolvedTargets::table(vec![0x1002.into(), 0x1010.into()]),
    )]);
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x20),
        known_targets: &known,
        data_ranges: strider_cfg::DataRanges::new(data),
        ..CfgOptions::default()
    };
    DecodeEngine::new(arch, sleigh)
        .expect("decode engine")
        .build_cfg(BASE.into(), &opts)
        .expect("one bad arm must not fail the function")
}

#[test]
fn an_arm_falling_through_into_data_is_dropped() {
    let cfg = build_falling_arm(0x20, XOR_EAX, Some(0x1012..0x1014));
    assert_eq!(switch_arms(&cfg), [0x1002]);
    assert_eq!(undecodable(&cfg), [0x1010]);
    assert_eq!(undecodable_bytes(&cfg), [0x1012]);
    assert!(
        cfg.regions()
            .all(|r| r.start_addr.machine_addr.addr != 0x1012)
    );
    assert_no_dangling_successor(&cfg);
}

#[test]
fn an_arm_falling_through_into_unmapped_bytes_is_dropped() {
    let cfg = build_falling_arm(0x12, XOR_EAX, None);
    assert_eq!(switch_arms(&cfg), [0x1002]);
    assert_eq!(undecodable(&cfg), [0x1010]);
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UnmappedTarget)
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        [0x1012],
    );
    assert!(
        cfg.regions()
            .all(|r| r.start_addr.machine_addr.addr != 0x1012)
    );
    assert_no_dangling_successor(&cfg);
}

/// The call's return address holds no code, so the callee does not return:
/// the arm is kept, ending at the call.
fn assert_arm_sealed_at_its_call(cfg: &Cfg, bytes: strider_cfg::Reason) {
    assert_eq!(switch_arms(cfg), [0x1002, 0x1010]);
    assert_eq!(undecodable(cfg), Vec::<u64>::new());
    let arm = cfg
        .regions()
        .find(|r| r.start_addr.machine_addr.addr == 0x1010)
        .expect("the arm's region");
    assert_eq!(arm.terminator, RegionTerminator::NoReturn);
    assert_eq!(
        cfg.incomplete()
            .of(bytes)
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        [0x1015],
    );
    assert_no_dangling_successor(cfg);
}

#[test]
fn an_arm_calling_before_data_is_kept_and_ends_at_the_call() {
    let cfg = build_falling_arm(0x20, CALL_OUT, Some(0x1015..0x1017));
    assert_arm_sealed_at_its_call(&cfg, strider_cfg::Reason::UndecodableTarget);
}

#[test]
fn an_arm_calling_before_unmapped_bytes_is_kept_and_ends_at_the_call() {
    let cfg = build_falling_arm(0x15, CALL_OUT, None);
    assert_arm_sealed_at_its_call(&cfg, strider_cfg::Reason::UnmappedTarget);
}
