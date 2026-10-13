//! Code decoded off a seeded jump-table arm is only as certain as the arm:
//! when anything it leads to will not decode, the arm is dropped and reported
//! instead of failing the whole function.

use rsleigh::mem_readers::BufMemReader;
use rustc_hash::FxHashMap;
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
    let known = FxHashMap::from_iter([(
        PcodeInsnAddr::at_machine_start(BASE),
        ResolvedTargets::Multiple(arms.iter().map(|&a| a.into()).collect()),
    )]);
    let opts = CfgOptions {
        known_targets: &known,
        ..CfgOptions::default()
    };
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

/// The bytes that would not decode, as opposed to the arm that led to them.
fn undecodable_bytes(cfg: &Cfg) -> Vec<u64> {
    cfg.undecodable_branch_targets()
        .iter()
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
    let known = FxHashMap::from_iter([(
        PcodeInsnAddr::at_machine_start(0x1002),
        ResolvedTargets::Multiple(vec![0x1004.into(), 0x1007.into()]),
    )]);
    let opts = CfgOptions {
        known_targets: &known,
        ..CfgOptions::default()
    };
    assert!(
        Builder::for_arch(&arch, &mut sleigh, BASE, &opts)
            .build()
            .is_err()
    );
}

/// Builds over `bytes` with one `known_targets` entry per site.
fn build_sites(bytes: &[u8], size: u64, sites: &[(u64, &[u64])]) -> anyhow::Result<Cfg> {
    let arch = SleighArch::x86_64();
    let mut sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes.to_vec(), BASE),
    )
    .expect("create Sleigh");
    let known = FxHashMap::from_iter(sites.iter().map(|&(site, arms)| {
        (
            PcodeInsnAddr::at_machine_start(site),
            ResolvedTargets::Multiple(arms.iter().map(|&a| a.into()).collect()),
        )
    }));
    let opts = CfgOptions {
        fn_max_size: Some(size),
        known_targets: &known,
        ..CfgOptions::default()
    };
    Builder::for_arch(&arch, &mut sleigh, BASE, &opts).build()
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
    use strider_cfg::{FlowVars, ResolvedTarget};
    let base = 0x10000u64;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xe12f_ff10u32.to_le_bytes());
    bytes.extend_from_slice(&0x2001u16.to_le_bytes());
    bytes.extend_from_slice(&0x4770u16.to_le_bytes());
    bytes.extend_from_slice(&0xeaff_fffdu32.to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);

    let arch = SleighArch::arm();
    let mut sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, base),
    )
    .unwrap();
    let flow = FlowVars::discover(&sleigh).unwrap();
    sleigh.set_context_at(base, "TMode", 0).unwrap();
    let mode = flow.snapshot(&sleigh, base);
    let known = FxHashMap::from_iter([(
        PcodeInsnAddr::at_machine_start(base),
        ResolvedTargets::Multiple(vec![
            ResolvedTarget::new(0x10004, Some(true)),
            ResolvedTarget::new(0x10008, None),
        ]),
    )]);
    let opts = CfgOptions {
        fn_max_size: Some(0x20),
        known_targets: &known,
        ..CfgOptions::default()
    };
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .with_flow_context(&flow, mode)
        .build()
        .expect("build");

    assert_eq!(switch_arms(&cfg), [0x10004, 0x10008]);
    assert_eq!(
        cfg.isa_mode_conflicts(),
        [PcodeInsnAddr::at_machine_start(0x10004)]
    );
    let thumb = cfg
        .regions()
        .find(|r| r.start_addr.machine_addr.addr == 0x10004)
        .expect("region at 0x10004");
    assert_eq!(thumb.insns.first().map(|i| i.len), Some(2));
}
