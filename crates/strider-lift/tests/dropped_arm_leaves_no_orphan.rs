//! A seated arm the build drops takes the region it decoded with it.

const BASE: u64 = 0x1000;
const DISPATCH: u64 = 0x1004;
const ARM: u64 = 0x1008;

/// ```text
/// 1000  beq 0x100c
/// 1004  bx r1                 ; seated with 0x1008 as ARM and as Thumb
/// 1008  (ARM) a conditional strb, falls into 0x100c
///       (Thumb) mov r8, r8 ; mov r8, r8
/// 100c  add r2, r2, #1        ; loop header, a phi on r2
/// 1010  b 0x100c
/// ```
///
/// A table pops its base-mode arm first, so the ARM arm decodes first; the
/// Thumb arm then clashes with it and the build strips every arm at 0x1008,
/// leaving the ARM region with no predecessor.
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
    let mut lifter =
        strider_lift::Lifter::new(arch, rsleigh::mem_readers::BufMemReader::new(bytes(), BASE))
            .expect("lifter");
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(lifter.sleigh_regs())
        .expect("aapcs");
    let known = rustc_hash::FxHashMap::from_iter([(
        strider_cfg::PcodeInsnAddr::at_machine_start(DISPATCH),
        strider_cfg::ResolvedTargets::table(vec![
            strider_cfg::ResolvedTarget::new(ARM, Some(false)),
            strider_cfg::ResolvedTarget::new(ARM, Some(true)),
        ]),
    )]);
    let opts = strider_cfg::CfgOptions {
        known_targets: &known,
        ..Default::default()
    };
    let cfg = lifter.build_cfg(BASE.into(), &opts).expect("cfg");

    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::IsaModeConflict)
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        // Sorted and deduped by the builder: a reason holds a SET of
        // addresses, and one clashing on several edges appeared once per
        // arrival. Work-queue order is the thing this reason exposes, so it
        // must not also decide how the report reads.
        vec![ARM],
        "the shape under test: the Thumb arm clashed with the ARM arm's region",
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
        .build_ir_with(
            &cfg,
            cc,
            &opts,
            &rustc_hash::FxHashMap::default(),
            &strider_ir::AssumptionOptions::none(),
        )
        .expect("the function lifts");
}
