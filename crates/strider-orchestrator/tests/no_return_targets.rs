//! A direct call to a `no_return_targets` address ends its region, under
//! `build_cfg` and `analyze` alike.

use strider_cfg::{Cfg, CfgOptions, RegionTerminator};
use strider_orchestrator::LiftOptions;
use strider_orchestrator::opt::OptOptions;

mod common;

const BASE: u64 = 0x1000;
const CALLEE: u64 = 0x1080;

/// `call 0x1080; jmp rax`, the callee a `ret` past the 0x40 bound.
fn call_then_jmp_rax() -> Vec<u8> {
    let mut bytes = vec![0xccu8; 0x81];
    bytes[..7].copy_from_slice(&[0xe8, 0x7b, 0x00, 0x00, 0x00, 0xff, 0xe0]);
    bytes[0x80] = 0xc3;
    bytes
}

fn entry_terminator(cfg: &Cfg) -> &RegionTerminator {
    &cfg.region_graph()[cfg.entry()].terminator
}

#[test]
fn a_no_return_callee_ends_the_region_in_build_cfg_and_analyze() {
    let no_return: rustc_hash::FxHashSet<u64> = [CALLEE].into_iter().collect();
    let cfg_opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x40),
        no_return_targets: &no_return,
        ..CfgOptions::default()
    };
    let (mut strider, cc) =
        common::strider_over_bytes(common::Arch::X64, call_then_jmp_rax(), BASE, None);

    let built = strider.build_cfg(BASE, &cfg_opts).expect("build_cfg");
    assert_eq!(*entry_terminator(&built), RegionTerminator::NoReturn);

    let lift_opts = LiftOptions {
        cfg: cfg_opts,
        ..LiftOptions::default()
    };
    let result = strider
        .analyze(BASE, &cc, &lift_opts, &OptOptions::default(), None)
        .expect("analyze");
    assert_eq!(*entry_terminator(&result.cfg), RegionTerminator::NoReturn);
    assert!(
        result.unresolved_indirect_branches.is_empty(),
        "the jmp rax after the call is never decoded"
    );
}
