//! Integration tests: trap-instruction (BUG_ON-class) regions lift
//! cleanly without UnresolvedIndirectBranch errors.
//!
//! Verifies the end-to-end fix for commit_creds, do_exit,
//! do_task_dead, __schedule, etc. on real Linux kernels.  Both arches:
//! a single trap insn lifts to a region whose terminator is
//! [`strider_cfg::RegionTerminator::NoReturn`]; no unresolved indirect
//! branches surface.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
        .build_cfg(MachineInsnAddr::from(entry), &strider_cfg::CfgOptions::default())
        .expect("cfg");

    let outcome = strider.build_ir(&cfg, &cc).expect("build_ir");
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
        .build_cfg(MachineInsnAddr::from(entry), &strider_cfg::CfgOptions::default())
        .expect("cfg");

    let outcome = strider.build_ir(&cfg, &cc).expect("build_ir");
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
