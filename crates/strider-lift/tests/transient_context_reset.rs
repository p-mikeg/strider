//! A `noflow` context var that changes the decode must not survive from one
//! function to the next on a reused engine.

use rsleigh::mem_readers::BufMemReader;
use strider_lift::lift::Lifter;
use strider_target::SleighArch;

/// `mov lr, pc` commits ARM's `LRset` at the NEXT address, and `ARMinstructions
/// .sinc` picks `call [pc]` over `goto [pc]` for a `bx` under it. `LRset` is
/// `noflow`, so it is outside the flowing set `pin_at` restores: analysing the
/// function at 0x1000 first must not turn 0x1004's `bx r0` into a call when
/// 0x1004 is later analysed as its own cold entry.
#[test]
fn a_noflow_decode_var_does_not_leak_into_the_next_function() {
    let arch = SleighArch::arm();
    let mut bytes = vec![0u8; 0x40];
    bytes[0x00..0x0c].copy_from_slice(&[
        0x0f, 0xe0, 0xa0, 0xe1, // 0x1000: mov lr, pc
        0x10, 0xff, 0x2f, 0xe1, // 0x1004: bx r0
        0x1e, 0xff, 0x2f, 0xe1, // 0x1008: bx lr
    ]);
    let sleigh = |b: Vec<u8>| {
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), BufMemReader::new(b, 0x1000))
            .expect("create Sleigh")
    };
    let terminators = |lifter: &mut Lifter<BufMemReader<Vec<u8>>>, entry: u64| -> Vec<String> {
        let cfg = lifter
            .build_cfg(entry.into(), &Default::default(), &Default::default())
            .expect("build_cfg");
        cfg.regions()
            .map(|r| format!("{:?}", r.terminator))
            .collect()
    };

    let mut cold = Lifter::new(arch, sleigh(bytes.clone())).unwrap();
    let alone = terminators(&mut cold, 0x1004);
    assert!(
        alone
            .iter()
            .any(|t| t.starts_with("UnresolvedIndirectBranch")),
        "a cold `bx r0` is an indirect branch; got {alone:?}",
    );

    let mut reused = Lifter::new(arch, sleigh(bytes)).unwrap();
    let _ = terminators(&mut reused, 0x1000);
    assert_eq!(
        terminators(&mut reused, 0x1004),
        alone,
        "the previous function's `mov lr,pc` must not change this decode",
    );
}

/// ```text
/// ARM, 0x1000:                     Thumb, 0x1020:
/// 1000  push {r4, lr}              1020  push {r4, lr}
/// 1004  cmp r0, #0                 1022  cmp r0, #0
/// 1008  beq 0x1010                 1024  beq 0x1028
/// 100c  b 0x1014                   1026  b 0x102a
/// 1010  mov lr, pc                 1028  mov lr, pc
/// 1014  bx r3                      102a  bx r3
/// 1018  pop {r4, pc}               102c  pop {r4, pc}
/// ```
///
/// The `b` decodes `bx r3` as a region of its own before `mov lr, pc` commits
/// `LRset` at it, so the first build reads it as an indirect branch. The
/// commit stays on the engine at a region start that is not the entry.
fn lr_set_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 0x40];
    for (i, word) in [
        0xe92d_4010u32,
        0xe350_0000,
        0x0a00_0000,
        0xea00_0000,
        0xe1a0_e00f,
        0xe12f_ff13,
        0xe8bd_8010,
    ]
    .iter()
    .enumerate()
    {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    for (i, half) in [0xb510u16, 0x2800, 0xd000, 0xe000, 0x46fe, 0x4718, 0xbd10]
        .iter()
        .enumerate()
    {
        bytes[0x20 + i * 2..0x22 + i * 2].copy_from_slice(&half.to_le_bytes());
    }
    bytes
}

#[test]
fn a_noflow_decode_var_does_not_leak_into_a_second_build_of_the_same_function() {
    let arch = SleighArch::arm();
    for (entry, size) in [(0x1000u64, 0x1c), (0x1021, 0xe)] {
        let mut lifter = Lifter::new(
            arch,
            rsleigh::Sleigh::new(
                arch.sla_spec(),
                arch.pspec(),
                BufMemReader::new(lr_set_bytes(), 0x1000),
            )
            .expect("create Sleigh"),
        )
        .unwrap();
        let opts = strider_cfg::CfgOptions {
            fn_max_size: Some(size),
            ..Default::default()
        };
        let mut terminators = || -> Vec<String> {
            let cfg = lifter
                .build_cfg(entry.into(), &opts, &Default::default())
                .expect("build_cfg");
            let mut t: Vec<String> = cfg
                .regions()
                .map(|r| format!("{:?} {:?}", r.start_addr, r.terminator))
                .collect();
            t.sort();
            t
        };
        let first = terminators();
        assert!(
            first.iter().any(|t| t.contains("UnresolvedIndirectBranch")),
            "{entry:#x}: the `bx r3` reached by the `b` is an indirect branch; got {first:?}",
        );
        assert_eq!(terminators(), first, "{entry:#x}: the second build");
    }
}

fn arm_lifter(bytes: Vec<u8>) -> Lifter<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::arm();
    Lifter::new(
        arch,
        rsleigh::Sleigh::new(
            arch.sla_spec(),
            arch.pspec(),
            BufMemReader::new(bytes, 0x1000),
        )
        .expect("create Sleigh"),
    )
    .unwrap()
}

fn sorted_terminators(
    lifter: &mut Lifter<BufMemReader<Vec<u8>>>,
    entry: u64,
    size: u64,
) -> anyhow::Result<Vec<String>> {
    let opts = strider_cfg::CfgOptions {
        fn_max_size: Some(size),
        ..Default::default()
    };
    let cfg = lifter.build_cfg(entry.into(), &opts, &Default::default())?;
    let mut t: Vec<String> = cfg
        .regions()
        .map(|r| format!("{:?} {:?}", r.start_addr, r.terminator))
        .collect();
    t.sort();
    Ok(t)
}

/// ```text
/// 1000  strbmi r0, [lr], r0    ; ARM; its high halfword 0x46fe is Thumb `mov lr, pc`
/// 1004  bx r0
/// 1008  bx lr
/// ```
///
/// A Thumb decode through 0x1002 commits `LRset` at 0x1004, which the ARM
/// stream reaches sequentially from an instruction that commits nothing.
#[test]
fn a_noflow_commit_from_another_stream_does_not_reach_a_sequential_decode() {
    let mut bytes = vec![0u8; 0x40];
    bytes[0x00..0x0c].copy_from_slice(&[
        0x00, 0x00, 0xfe, 0x46, // 0x1000
        0x10, 0xff, 0x2f, 0xe1, // 0x1004: bx r0
        0x1e, 0xff, 0x2f, 0xe1, // 0x1008: bx lr
    ]);
    let alone = sorted_terminators(&mut arm_lifter(bytes.clone()), 0x1000, 0xc).expect("build");
    assert!(
        alone.iter().any(|t| t.contains("UnresolvedIndirectBranch")),
        "a cold `bx r0` is an indirect branch; got {alone:?}",
    );

    let mut reused = arm_lifter(bytes);
    let _ = sorted_terminators(&mut reused, 0x1003, 0x10);
    assert_eq!(
        sorted_terminators(&mut reused, 0x1000, 0xc).expect("build"),
        alone
    );
}

/// ```text
/// 1000  mov lr, pc
/// 1004  bx r0        ; a call under the `LRset` the `mov` commits
/// 1008  bx lr
/// ```
#[test]
fn a_sequential_decode_keeps_the_noflow_commit_of_the_instruction_before_it() {
    let mut bytes = vec![0u8; 0x40];
    bytes[0x00..0x0c].copy_from_slice(&[
        0x0f, 0xe0, 0xa0, 0xe1, // 0x1000: mov lr, pc
        0x10, 0xff, 0x2f, 0xe1, // 0x1004: bx r0
        0x1e, 0xff, 0x2f, 0xe1, // 0x1008: bx lr
    ]);
    let mut lifter = arm_lifter(bytes);
    let first = sorted_terminators(&mut lifter, 0x1000, 0xc).expect("build");
    assert!(
        !first.iter().any(|t| t.contains("UnresolvedIndirectBranch")),
        "`mov lr, pc; bx r0` is a call; got {first:?}",
    );
    assert_eq!(
        sorted_terminators(&mut lifter, 0x1000, 0xc).expect("build"),
        first
    );
}
