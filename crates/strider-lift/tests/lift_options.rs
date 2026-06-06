#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Tests for `LiftOptions` — the composed binary → IR lift options
//! (an embedded [`strider_cfg::CfgOptions`] plus the IR-lift knobs
//! `all_vns` / `per_address_ccs`).

use strider_lift::LiftOptions;

#[test]
fn lift_options_default() {
    let d = LiftOptions::default();
    // Embedded CFG knobs default to unbounded / no-pre-start / no targets.
    assert_eq!(d.cfg.fn_max_size, None);
    assert!(!d.cfg.allow_code_before_start_addr);
    assert!(d.cfg.known_targets.is_empty());
    // IR-lift knobs default to scan-for-vns / no CC overrides.
    assert!(d.all_vns.is_none());
    assert!(d.per_address_ccs.is_empty());
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
