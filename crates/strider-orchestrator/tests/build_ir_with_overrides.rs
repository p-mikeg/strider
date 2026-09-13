//! `Lifter::build_ir_with` applies the per-address-cc override at lift time,
//! so a custom pipeline that never goes through `Strider::analyze` still gets
//! it.  Same fixture as `tests/per_address_cc.rs`, which covers the
//! orchestrator path.

use rustc_hash::FxHashMap;
use strider_ir::IRViewer;

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::MachineInsnAddr;
use strider_ir::node::NodeKind;
use strider_orchestrator::LiftOptions;
use strider_target::CallingConvention as TargetCC;

mod common;
use common::x86_64_call_then_ret;

#[test]
fn build_ir_with_applies_per_address_override() {
    let (bytes, entry, call_target) = x86_64_call_then_ret();
    let reader = BufMemReader::new(bytes, entry);
    let (mut strider, _cc) = common::driver_for_reader(common::Arch::X64, reader);
    let cfg = strider
        .build_cfg(
            MachineInsnAddr::from(entry),
            &strider_cfg::CfgOptions::default(),
            &Default::default(),
        )
        .unwrap();

    // Both CCs must be built against the driver's own register table.
    let mut built: FxHashMap<u64, strider_target::BuiltCallingConvention> = FxHashMap::default();
    built.insert(
        call_target,
        TargetCC::x86_64_systemv()
            .preserves_all()
            .build(strider.sleigh_regs())
            .unwrap(),
    );

    let cc = TargetCC::x86_64_systemv()
        .build(strider.sleigh_regs())
        .unwrap();

    let outcome = strider
        .build_ir_with(
            &cfg,
            cc,
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
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call { .. }))
        .expect("function lifts to one Call");
    assert_ne!(
        bfg.get_cc(call_id),
        bfg.default_cc(),
        "override CC must be recorded on the Call (effective CC differs from default)"
    );
    let outs = bfg.node_outputs(call_id);
    assert!(
        outs.iter()
            .skip(2)
            .all(|&v| bfg.get_vn_for_value(v).is_some()),
        "every clobber output must carry its varnode tag"
    );
    let (ret, clob) = strider_ir::cc_ret_and_clobber_vns(&bfg, bfg.get_cc(call_id));
    assert_eq!(
        outs.len(),
        2 + ret.len() + clob.len(),
        "Call's outputs = Control + Memory + the override's ret-val/clobber slots"
    );
}
