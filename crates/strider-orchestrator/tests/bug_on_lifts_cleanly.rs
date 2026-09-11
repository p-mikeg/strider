//! A lone trap instruction (the BUG_ON class) must terminate its region as
//! [`strider_cfg::RegionTerminator::NoReturn`], not as an unresolved indirect
//! branch.  On real Linux kernels the latter surfaces from `commit_creds`,
//! `do_exit`, `do_task_dead` and `__schedule`.
//!
//! MIPS reaches the same place from the other side: its `break` falls through
//! in the sla, so an unterminated function ending in one decodes past its own
//! extent and is rejected whole.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{MachineInsnAddr, RegionTerminator};

mod common;

#[test]
fn x86_64_ud2_terminates_cleanly() {
    let bytes = vec![0x0fu8, 0x0b]; // ud2
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let (mut strider, cc) = common::strider_x86_64(reader);
    let cfg = strider
        .build_cfg(
            MachineInsnAddr::from(entry),
            &strider_cfg::CfgOptions::default(),
            &Default::default(),
        )
        .expect("cfg");

    let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
    assert!(
        outcome.unresolved_branches.is_empty(),
        "ud2 produced {} unresolved branch(es); expected 0",
        outcome.unresolved_branches.len(),
    );
    assert!(
        cfg.region_graph()
            .node_weights()
            .any(|r| matches!(r.terminator, RegionTerminator::NoReturn)),
        "expected at least one NoReturn region in cfg",
    );
}

#[test]
fn aarch64_brk_terminates_cleanly() {
    // brk #0x800 = 0xD4210000 (LE: 00 00 21 D4)
    let bytes = vec![0x00u8, 0x00, 0x21, 0xd4];
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let (mut strider, cc) = common::strider_aarch64(reader);
    let cfg = strider
        .build_cfg(
            MachineInsnAddr::from(entry),
            &strider_cfg::CfgOptions::default(),
            &Default::default(),
        )
        .expect("cfg");

    let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
    assert!(
        outcome.unresolved_branches.is_empty(),
        "brk produced {} unresolved branch(es); expected 0",
        outcome.unresolved_branches.len(),
    );
    assert!(
        cfg.region_graph()
            .node_weights()
            .any(|r| matches!(r.terminator, RegionTerminator::NoReturn)),
        "expected at least one NoReturn region in cfg",
    );
}

#[test]
fn mips_break_terminates_cleanly() {
    // break 0xc = 0x000c000d, Linux MIPS `BUG()`.  Trailing `nop`s: the region
    // must stop AT the break rather than decode them.
    let mut bytes = vec![0x0du8, 0x00, 0x0c, 0x00];
    bytes.extend_from_slice(&[0x00; 16]);
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let (mut strider, cc) = common::strider_mips32le(reader);
    let cfg = strider
        .build_cfg(
            MachineInsnAddr::from(entry),
            &strider_cfg::CfgOptions::default(),
            &Default::default(),
        )
        .expect("cfg");

    let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
    assert!(
        outcome.unresolved_branches.is_empty(),
        "break produced {} unresolved branch(es); expected 0",
        outcome.unresolved_branches.len(),
    );
    assert!(
        cfg.region_graph()
            .node_weights()
            .any(|r| matches!(r.terminator, RegionTerminator::NoReturn)),
        "expected at least one NoReturn region in cfg",
    );
}

/// A caller reclassification reaches the CFG, so the terminator follows it
/// rather than the built-in table.
#[test]
fn call_other_override_restores_the_fall_through() {
    // break 0xc, then `jr ra` + delay-slot nop: reclassified as returning, the
    // decode walks THROUGH the break and terminates on the return instead.
    let mut bytes = vec![0x0du8, 0x00, 0x0c, 0x00];
    bytes.extend_from_slice(&[0x08, 0x00, 0xe0, 0x03]);
    bytes.extend_from_slice(&[0x00; 32]);
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let (mut strider, _cc) = common::strider_mips32le(reader);
    let opts = strider_cfg::CfgOptions {
        call_other_overrides: strider_target::call_other_abi::CallOtherOverrides::new(vec![(
            "trap".to_owned(),
            strider_target::call_other_abi::CallOtherClass::MEM_CLOBBER.into(),
        )])
        .expect("unique override names"),
        ..Default::default()
    };
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(entry), &opts, &Default::default())
        .expect("cfg");

    assert!(
        !cfg.region_graph()
            .node_weights()
            .any(|r| matches!(r.terminator, RegionTerminator::NoReturn)),
        "the override says `trap` returns, so no region may terminate on it",
    );
}
