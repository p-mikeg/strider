//! A MIPS delay slot is decoded as part of its branch, so it decodes in the
//! branch's ISA mode whatever an earlier decode committed at the slot's own
//! address.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{
    Builder, Cfg, CfgOptions, FlowVars, PcodeInsnAddr, ResolvedTarget, ResolvedTargets,
};
use strider_target::SleighArch;

type Engine = rsleigh::Sleigh<BufMemReader<Vec<u8>>>;

/// fixtures/out/mips32be/control.elf `abs_val`:
///
/// ```text
/// 4007d0  bltz a0, 0x4007e4
/// 4007d4  move v0, a0        ; slot
/// 4007d8  jr ra
/// 4007dc  nop
/// 4007e0  jr ra
/// 4007e4  negu v0, a0        ; slot
/// ```
const ABS_VAL: [u8; 24] = [
    0x04, 0x80, 0x00, 0x03, 0x00, 0x80, 0x10, 0x25, 0x03, 0xe0, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00,
    0x03, 0xe0, 0x00, 0x08, 0x00, 0x04, 0x10, 0x23,
];
const ABS_VAL_BASE: u64 = 0x4007d0;

fn engine(bytes: Vec<u8>, base: u64) -> Engine {
    let arch = SleighArch::mipsbe32();
    rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, base),
    )
    .expect("create Sleigh")
}

/// A cold build at `entry` in ISA `mode`, pinned the way `Lifter::build_cfg`
/// pins one.
fn build(
    s: &mut Engine,
    flow: &FlowVars,
    entry: u64,
    mode: u32,
    opts: &CfgOptions,
) -> anyhow::Result<Cfg> {
    let arch = SleighArch::mipsbe32();
    s.set_context_at(entry, "ISA_MODE", mode).unwrap();
    let function_mode = flow.snapshot(s, entry);
    Builder::for_arch(&arch, s, entry, opts)
        .with_flow_context(flow, function_mode)
        .build()
}

fn bounded(size: u64) -> CfgOptions {
    CfgOptions {
        fn_max_size: Some(size),
        ..CfgOptions::default()
    }
}

/// `(address, machine length)` of every instruction in the region at `start`.
fn lens_at(cfg: &Cfg, start: u64) -> Vec<(u64, u32)> {
    cfg.regions()
        .find(|r| r.start_addr.machine_addr.addr == start)
        .unwrap_or_else(|| panic!("no region at {start:#x}"))
        .insns
        .iter()
        .map(|i| (i.addr.machine_addr.addr, i.len))
        .collect()
}

#[test]
fn a_mips16_build_at_a_slot_address_does_not_change_the_next_mips32_build() {
    let mut fresh = engine(ABS_VAL.to_vec(), ABS_VAL_BASE);
    let flow = FlowVars::discover(&fresh).unwrap();
    let clean = build(&mut fresh, &flow, ABS_VAL_BASE, 0, &bounded(24)).expect("fresh build");

    let mut reused = engine(ABS_VAL.to_vec(), ABS_VAL_BASE);
    let _ = build(&mut reused, &flow, ABS_VAL_BASE + 4, 1, &bounded(64));
    let again = build(&mut reused, &flow, ABS_VAL_BASE, 0, &bounded(24))
        .expect("the same bytes, entry and mode must build on a reused engine");
    assert_eq!(
        lens_at(&again, ABS_VAL_BASE),
        lens_at(&clean, ABS_VAL_BASE),
        "the branch and its slot must decode as on a fresh engine",
    );
}

/// Within one build: a seeded arm claiming MIPS16 at a MIPS32 branch's slot
/// commits `ISA_MODE=1` there before a direct edge decodes that branch.
#[test]
fn a_mips16_arm_at_a_slot_does_not_corrupt_a_later_decode_of_its_branch() {
    let words: [u32; 10] = [
        0x0080_0008, // 0x1000 jr a0
        0x0000_0000, // 0x1004 nop
        0x0480_0003, // 0x1008 bltz a0, 0x1018
        0x0080_1025, // 0x100c move v0, a0    ; slot, and the MIPS16 arm
        0x03e0_0008, // 0x1010 jr ra
        0x0000_0000, // 0x1014 nop
        0x03e0_0008, // 0x1018 jr ra
        0x0004_1023, // 0x101c negu v0, a0
        0x1000_fff9, // 0x1020 b 0x1008       ; the MIPS32 arm
        0x0000_0000, // 0x1024 nop
    ];
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    let run = |arms: Vec<ResolvedTarget>| {
        let mut s = engine(bytes.clone(), 0x1000);
        let flow = FlowVars::discover(&s).unwrap();
        let mut opts = bounded(0x28);
        opts.known_targets.insert(
            PcodeInsnAddr::at_machine_start(0x1000),
            ResolvedTargets::Multiple(arms),
        );
        let cfg = build(&mut s, &flow, 0x1000, 0, &opts).expect("build");
        lens_at(&cfg, 0x1008)
    };
    let clean = run(vec![ResolvedTarget::new(0x1020, None)]);
    let with_mips16_arm = run(vec![
        ResolvedTarget::new(0x100c, Some(true)),
        ResolvedTarget::new(0x1020, None),
    ]);
    assert_eq!(clean.last().map(|l| l.1), Some(8));
    assert_eq!(
        with_mips16_arm, clean,
        "the bltz at 0x1008 decodes as 8 bytes, branch plus a MIPS32 slot",
    );
}
