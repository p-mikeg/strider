#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_lift::LiftOptions;

#[test]
fn lift_options_default() {
    let d = LiftOptions::default();
    assert_eq!(d.cfg.fn_max_size, None);
    assert!(!d.cfg.allow_code_before_start_addr);
    assert!(d.cfg.known_targets.is_empty());
    assert!(d.per_address_ccs.is_empty());
    assert!(d.compact);
}

#[test]
fn lift_options_embeds_cfg_knobs() {
    let opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(0x1000),
            allow_code_before_start_addr: true,
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    assert_eq!(opts.cfg.fn_max_size, Some(0x1000));
    assert!(opts.cfg.allow_code_before_start_addr);
}

#[test]
fn lift_options_embeds_known_targets_seed() {
    // The orchestrator's analyze loop owns its own map, but the raw lift path
    // passes this one to the CFG builder verbatim.
    use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};

    let site = PcodeInsnAddr {
        machine_addr: MachineInsnAddr::from(0x1000u64),
        insn_index: 0,
    };
    let mut known = rustc_hash::FxHashMap::default();
    known.insert(site, ResolvedTargets::Single(0x2000));
    let opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            known_targets: known,
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    assert_eq!(opts.cfg.known_targets.len(), 1);
    assert_eq!(
        opts.cfg.known_targets.get(&site),
        Some(&ResolvedTargets::Single(0x2000))
    );
}
