//! Code decoded off a seeded jump-table arm is only as certain as the arm:
//! when anything it leads to will not decode, the arm is dropped and reported
//! instead of failing the whole function.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, Cfg, CfgOptions, PcodeInsnAddr, RegionTerminator, ResolvedTargets};
use strider_target::SleighArch;

const BASE: u64 = 0x1000;

fn build(bytes: &[u8], arms: &[u64]) -> anyhow::Result<Cfg> {
    let arch = SleighArch::x86_64();
    let mut code = bytes.to_vec();
    code.resize(bytes.len() + 16, 0xcc);
    let mut sleigh =
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), BufMemReader::new(code, BASE))
            .expect("create Sleigh");
    let mut opts = CfgOptions::default();
    opts.known_targets.insert(
        PcodeInsnAddr::at_machine_start(BASE),
        ResolvedTargets::Multiple(arms.iter().map(|&a| a.into()).collect()),
    );
    Builder::for_arch(&arch, &mut sleigh, BASE, &opts).build()
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
    let mut sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes.to_vec(), BASE),
    )
    .expect("create Sleigh");
    let mut opts = CfgOptions::default();
    opts.known_targets.insert(
        PcodeInsnAddr::at_machine_start(0x1002),
        ResolvedTargets::Multiple(vec![0x1004.into(), 0x1007.into()]),
    );
    assert!(
        Builder::for_arch(&arch, &mut sleigh, BASE, &opts)
            .build()
            .is_err()
    );
}
