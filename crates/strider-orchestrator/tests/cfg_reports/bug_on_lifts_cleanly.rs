//! A lone trap instruction (the BUG_ON class) must terminate its region as
//! [`strider_cfg::RegionTerminator::NoReturn`], not as an unresolved indirect
//! branch.
//!
//! MIPS reaches the same place from the other side: its `break` falls through
//! in the sla, so an unterminated function ending in one decodes past its own
//! extent and is rejected whole.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{MachineInsnAddr, RegionTerminator};

use crate::common;

#[test]
fn a_lone_trap_terminates_cleanly() {
    let cases = [
        // ud2
        ("ud2", common::Arch::X64, vec![0x0fu8, 0x0b]),
        // brk #0x800
        ("brk", common::Arch::Aarch64, vec![0x00, 0x00, 0x21, 0xd4]),
        // break 0xc, Linux MIPS `BUG()`. Trailing `nop`s: the region must stop
        // AT the break rather than decode them.
        ("break", common::Arch::Mips32le, {
            let mut bytes = vec![0x0d, 0x00, 0x0c, 0x00];
            bytes.extend_from_slice(&[0x00; 16]);
            bytes
        }),
    ];
    for (name, arch, bytes) in cases {
        let entry = 0x1000u64;
        let reader = BufMemReader::new(bytes, entry);
        let (mut strider, cc) = common::driver_for_reader(arch, reader);
        let cfg = strider
            .build_cfg(
                MachineInsnAddr::from(entry),
                &strider_cfg::CfgOptions::default(),
            )
            .expect("cfg");

        let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
        assert!(
            outcome.sites.unresolved_branches.is_empty(),
            "{name} produced {} unresolved branch(es); expected 0",
            outcome.sites.unresolved_branches.len(),
        );
        assert!(
            cfg.region_graph()
                .node_weights()
                .any(|r| matches!(r.terminator, RegionTerminator::NoReturn)),
            "{name}: expected at least one NoReturn region in cfg",
        );
    }
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
    let (mut strider, _cc) = common::driver_for_reader(common::Arch::Mips32le, reader);
    let opts = strider_cfg::CfgOptions {
        call_other_overrides: &strider_target::call_other_abi::CallOtherOverrides::new(vec![(
            "trap".to_owned(),
            strider_target::call_other_abi::CallOtherClass::MEM_CLOBBER.into(),
        )])
        .expect("unique override names"),
        ..Default::default()
    };
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(entry), &opts)
        .expect("cfg");

    assert!(
        !cfg.region_graph()
            .node_weights()
            .any(|r| matches!(r.terminator, RegionTerminator::NoReturn)),
        "the override says `trap` returns, so no region may terminate on it",
    );
}
