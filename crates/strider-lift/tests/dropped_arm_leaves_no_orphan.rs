//! A seated arm the build drops takes the region it decoded with it.

const BASE: u64 = 0x1000;
const DISPATCH: u64 = 0x1004;
const ARM: u64 = 0x1008;

/// ```text
/// 1000  beq 0x100c
/// 1004  bx r1                 ; seated with 0x1008 as Thumb, then as ARM
/// 1008  (Thumb) mov r8, r8 ; mov r8, r8   ; falls into 0x100c
/// 100c  add r2, r2, #1        ; loop header, a phi on r2
/// 1010  b 0x100c
/// ```
///
/// The Thumb arm decodes first; the ARM arm then clashes with it and the build
/// strips every arm at 0x1008, leaving the Thumb region with no predecessor.
fn bytes() -> Vec<u8> {
    [
        0x0a00_0001u32,
        0xe12f_ff11,
        0x46c0_46c0,
        0xe282_2001,
        0xeaff_fffd,
    ]
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect()
}

#[test]
fn a_dropped_arm_region_is_removed_and_the_function_lifts() {
    let arch = strider_target::SleighArch::arm();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes(), BASE),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(lifter.sleigh_regs())
        .expect("aapcs");
    let mut opts = strider_lift::LiftOptions::default();
    opts.cfg.known_targets.insert(
        strider_cfg::PcodeInsnAddr::at_machine_start(DISPATCH),
        strider_cfg::ResolvedTargets::Multiple(vec![
            strider_cfg::ResolvedTarget::new(ARM, Some(false)),
            strider_cfg::ResolvedTarget::new(ARM, Some(true)),
        ]),
    );
    let cfg = lifter
        .build_cfg(BASE.into(), &opts.cfg, &opts.per_address_ccs)
        .expect("cfg");

    assert_eq!(
        cfg.isa_mode_conflicts()
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![0x100c, ARM],
        "the shape under test: the Thumb arm ran into ARM code and the ARM arm clashed with it",
    );
    let reachable = petgraph::visit::Walker::iter(
        petgraph::visit::Dfs::new(cfg.region_graph(), cfg.entry()),
        cfg.region_graph(),
    )
    .count();
    assert_eq!(
        reachable,
        cfg.region_ids().count(),
        "every region is reachable from the entry",
    );

    lifter
        .build_ir_with(&cfg, cc, &opts)
        .expect("the function lifts");
}
