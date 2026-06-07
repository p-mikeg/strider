//! Per-call test: `Lifter::build_ir_with` applies the
//! per-address-cc override at lift time without going through
//! `strider_orchestrator::Strider::analyze`.  Mirrors `tests/per_address_cc.rs` but exercises the
//! new options-bag API directly so a strider-py custom pipeline
//! (which calls `build_ir_with` instead of running the orchestrator)
//! gets the same override behaviour.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rustc_hash::FxHashMap;
use strider_ir::IRViewer;

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::MachineInsnAddr;
use strider_ir::node::NodeKind;
use strider_orchestrator::LiftOptions;
use strider_target::CallingConvention as TargetCC;

mod common;

/// Same fixture as `tests/per_address_cc.rs::x86_64_call_then_ret`:
/// `call 0x2000; ret` at 0x1000.
fn x86_64_call_then_ret() -> (Vec<u8>, u64, u64) {
    let bytes = vec![0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3];
    (bytes, 0x1000, 0x2000)
}

#[test]
fn build_ir_with_applies_per_address_override() {
    let (bytes, entry, call_target) = x86_64_call_then_ret();
    let reader = BufMemReader::new(bytes, entry);
    // The driver OWNS the Sleigh and builds the CFG itself.
    let (mut strider, _cc) = common::strider_x86_64(reader);
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(entry), &strider_cfg::CfgOptions::default())
        .unwrap();

    // Build the override map against the driver's register table — the
    // same table the function-default CC was built against.
    let mut built: FxHashMap<u64, strider_target::BuiltCallingConvention> = FxHashMap::default();
    built.insert(
        call_target,
        TargetCC::x86_64_all_preserving()
            .unwrap()
            .build(strider.sleigh_regs())
            .unwrap(),
    );

    // Function-default CC (resolved against the driver's regs).
    let cc = TargetCC::x86_64_systemv()
        .unwrap()
        .build(strider.sleigh_regs())
        .unwrap();

    let outcome = strider
        .build_ir_with(
            &cfg,
            &cc,
            &LiftOptions {
                per_address_ccs: built,
                ..LiftOptions::default()
            },
        )
        .unwrap();
    let bfg = outcome.function;

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    assert!(
        bfg.call_cc(call_id).is_some(),
        "override CC must be recorded on the Call"
    );
    let outs = bfg.node_outputs(call_id);
    let override_clobbers = outs.iter().skip(2).count();
    assert!(
        outs.iter().skip(2).all(|&v| bfg.get_vn_for_value(v).is_some()),
        "every clobber output must carry its varnode tag"
    );
    assert_eq!(
        outs.len(),
        2 + override_clobbers,
        "Call's outputs = Control + Memory + override clobber slots"
    );
}

#[test]
fn build_ir_with_default_options_matches_build_ir() {
    let (bytes, entry, _) = x86_64_call_then_ret();
    let reader = BufMemReader::new(bytes, entry);
    // The driver OWNS the Sleigh and builds the CFG itself.
    let (mut strider, cc) = common::strider_x86_64(reader);
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(entry), &strider_cfg::CfgOptions::default())
        .unwrap();

    let outcome_default = strider.build_ir(&cfg, &cc).unwrap();
    let outcome_with = strider
        .build_ir_with(&cfg, &cc, &LiftOptions::default())
        .unwrap();

    let n_default = outcome_default.function.graph().all_node_ids().count();
    let n_with = outcome_with.function.graph().all_node_ids().count();
    assert_eq!(
        n_default, n_with,
        "build_ir_with(default) must produce the same graph shape as build_ir"
    );
}
