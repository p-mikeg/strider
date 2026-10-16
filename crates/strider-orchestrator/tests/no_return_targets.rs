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

/// A per-address calling convention for the same callee changes how the call
/// is built, not whether it returns.
#[test]
fn a_per_address_cc_on_a_no_return_callee_still_ends_the_region() {
    use strider_ir::IRViewer;
    use strider_ir::node::NodeKind;

    let no_return: rustc_hash::FxHashSet<u64> = [CALLEE].into_iter().collect();
    let cfg_opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x40),
        no_return_targets: &no_return,
        ..CfgOptions::default()
    };
    let (mut strider, cc) =
        common::strider_over_bytes(common::Arch::X64, call_then_jmp_rax(), BASE, None);
    let overrides: rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention> = [(
        CALLEE,
        strider_target::CallingConvention::x86_64_systemv()
            .preserves_all()
            .build(strider.sleigh_regs())
            .expect("override cc"),
    )]
    .into_iter()
    .collect();

    let built = strider.build_cfg(BASE, &cfg_opts).expect("build_cfg");
    assert_eq!(*entry_terminator(&built), RegionTerminator::NoReturn);

    let lift_opts = LiftOptions {
        cfg: cfg_opts,
        per_address_ccs: &overrides,
        ..LiftOptions::default()
    };
    let result = strider
        .analyze(BASE, &cc, &lift_opts, &OptOptions::default(), None)
        .expect("analyze");
    assert_eq!(*entry_terminator(&result.cfg), RegionTerminator::NoReturn);
    assert!(result.unresolved_indirect_branches.is_empty());
    let function = &result.function;
    let call = function
        .graph()
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::Call { .. }))
        .expect("the call is lifted");
    assert_ne!(
        function.get_cc(call),
        function.default_cc(),
        "the call is built with the override"
    );
    assert!(
        function
            .graph()
            .all_node_ids()
            .any(|n| matches!(function.node_kind(n), NodeKind::Unreachable)),
        "the call sinks into Unreachable"
    );
}
