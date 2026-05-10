//! Per-call test: `Strider::analyze_cfg_with` applies the
//! per-address-cc override at lift time without going through
//! `strider::run`.  Mirrors `tests/per_address_cc.rs` but exercises the
//! new options-bag API directly so a strider-py custom pipeline
//! (which calls `analyze_cfg_with` instead of running the orchestrator)
//! gets the same override behaviour.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider::{AnalyzeOptions, SleighArch, Strider};
use target::CallingConvention as TargetCC;

/// Same fixture as `tests/per_address_cc.rs::x86_64_call_then_ret`:
/// `call 0x2000; ret` at 0x1000.
fn x86_64_call_then_ret() -> (Vec<u8>, u64, u64) {
    let bytes = vec![0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3];
    (bytes, 0x1000, 0x2000)
}

fn make_strider() -> Strider {
    strider::test_utils::strider_x86_64()
}

#[test]
fn analyze_cfg_with_applies_per_address_override() {
    let (bytes, entry, call_target) = x86_64_call_then_ret();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();
    let cfg = cfg::Builder::for_arch(&arch, sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .unwrap();

    // Build the override map against the same Sleigh register table the
    // function-default CC was built against.
    let regs = arch.probe_regs().unwrap();
    let mut built: HashMap<u64, target::BuiltCallingConvention> = HashMap::new();
    built.insert(call_target, TargetCC::x86_64_all_preserving().build(&regs).unwrap());

    let outcome = strider
        .analyze_cfg_with(
            &cfg,
            AnalyzeOptions {
                per_address_ccs: &built,
                ..AnalyzeOptions::default()
            },
        )
        .unwrap();
    let bfg = outcome.graph;

    let call_id = bfg
        .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    let override_list = bfg
        .graph
        .call_clobbered_override(call_id)
        .expect("override CC must populate the side-table");
    let outs = bfg.graph.node_outputs(call_id);
    assert_eq!(
        outs.len(),
        2 + override_list.len(),
        "Call's outputs = Control + Memory + override_list.len()"
    );
}

#[test]
fn analyze_cfg_with_default_options_matches_analyze_cfg() {
    let (bytes, entry, _) = x86_64_call_then_ret();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();
    let cfg = cfg::Builder::for_arch(&arch, sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .unwrap();

    let outcome_default = strider.analyze_cfg(&cfg).unwrap();
    let outcome_with = strider
        .analyze_cfg_with(&cfg, AnalyzeOptions::default())
        .unwrap();

    let n_default = outcome_default.graph.all_node_ids().count();
    let n_with = outcome_with.graph.all_node_ids().count();
    assert_eq!(
        n_default, n_with,
        "analyze_cfg_with(default) must produce the same graph shape as analyze_cfg"
    );
}
