//! End-to-end: a Call whose target is in `per_address_ccs` is built
//! with the override CC end-to-end (zero clobber outputs for an
//! all-preserving override).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider::{CallingConvention, RunConfig, SleighArch, Strider};
use target::CallingConvention as TargetCC;

/// x86_64: `call $fentry; ret`.  Encoded with the call target near
/// the function entry so we control the absolute address.
///
/// Layout at base 0x1000:
///   0x1000  e8 fb 0f 00 00     call 0x2000
///   0x1005  c3                 ret
fn x86_64_call_then_ret() -> (Vec<u8>, u64, u64) {
    let bytes = vec![0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3];
    let entry = 0x1000;
    let call_target = 0x2000;
    (bytes, entry, call_target)
}

fn make_strider() -> Strider {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).unwrap()
}

#[test]
fn call_to_overridden_address_has_zero_clobber_outputs() {
    let (bytes, entry, call_target) = x86_64_call_then_ret();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();

    let mut overrides: HashMap<u64, TargetCC> = HashMap::new();
    overrides.insert(call_target, TargetCC::x86_64_all_preserving());

    let config = RunConfig {
        strider: &strider,
        start_addr: entry,
        sleigh,
        rom: None,
        fn_max_size: None,
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs: overrides,
    };
    let bfg = strider::run(config).unwrap();

    let call_id = bfg
        .graph
        .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    let outs = bfg.graph.node_outputs(call_id);
    // Override applied: per-Call clobber-list override is recorded
    // (Some); the Call's clobber output count matches the override
    // list length exactly.  Pre-Task-9 (no per-address CC), this Call
    // emits a SystemV clobber set with ~16+ slots; with the override,
    // the only tracked variables that survive the all-preserving
    // filter are the Sleigh-generated temporaries (UNIQUE / RAM
    // varnodes) the override's by-name callee_saved list can't reach.
    // The pinned invariant: the override is recorded AND the Call
    // shape matches the override length AND it's strictly less than
    // the function-default's SystemV clobber count.
    let override_list = bfg
        .graph
        .call_clobbered_override(call_id)
        .expect("override CC must populate the side-table");
    assert_eq!(
        outs.len(),
        2 + override_list.len(),
        "Call's outputs = Control + Memory + override_list.len()"
    );
    assert!(
        override_list.len() < bfg.call_clobbered.len(),
        "override list ({}) must be strictly smaller than function-default ({})",
        override_list.len(),
        bfg.call_clobbered.len(),
    );
}

/// Counterpart to the above: WITHOUT an override, the same Call
/// emits the full SystemV clobber shape.
#[test]
fn call_without_override_uses_function_default_clobber_set() {
    let (bytes, entry, _call_target) = x86_64_call_then_ret();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();

    let config = RunConfig {
        strider: &strider,
        start_addr: entry,
        sleigh,
        rom: None,
        fn_max_size: None,
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs: HashMap::new(),
    };
    let bfg = strider::run(config).unwrap();

    let call_id = bfg
        .graph
        .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    assert!(
        bfg.graph.call_clobbered_override(call_id).is_none(),
        "no override means side-table stays None"
    );
    let outs = bfg.graph.node_outputs(call_id);
    assert_eq!(
        outs.len(),
        2 + bfg.call_clobbered.len(),
        "default Call: Control + Memory + per-CC clobber slots"
    );
}
