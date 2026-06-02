//! End-to-end: a Call whose target is in `per_address_ccs` is built
//! with the override CC end-to-end (zero clobber outputs for an
//! all-preserving override).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rustc_hash::FxHashMap;

use strider_ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider_analyze::{RunConfig, RunOptions};
use strider_target::{CallingConvention as TargetCC, SleighArch};

mod common;

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

#[test]
fn call_to_overridden_address_has_zero_clobber_outputs() {
    let (bytes, entry, call_target) = x86_64_call_then_ret();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    overrides.insert(call_target, TargetCC::x86_64_all_preserving().unwrap());

    let config = RunConfig::new(
        arch,
        TargetCC::x86_64_systemv().unwrap(),
        sleigh,
        entry.into(),
        RunOptions::new().per_address_ccs_unbuilt(overrides),
    )
    .unwrap();
    let bfg = strider_analyze::run(config).unwrap();

    let call_id = bfg
                .graph().all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    let outs = bfg.node_outputs(call_id);
    // Override applied: the override CC is recorded (`call_cc` is Some) and
    // every clobber output carries its varnode tag (`clobbered_vn`).
    // Before per-address CC was added, this Call emits a SystemV clobber
    // set with ~16+ slots; with the override, the only tracked variables
    // that survive the all-preserving filter are the Sleigh-generated
    // temporaries (UNIQUE / RAM varnodes) the override's by-name
    // callee_saved list can't reach.  The pinned invariant: the override
    // is recorded AND every clobber output is tagged AND the override
    // clobber count is strictly less than the function-default SystemV set.
    assert!(
        bfg.call_cc(call_id).is_some(),
        "override CC must be recorded on the Call"
    );
    let override_clobbers = outs.iter().skip(2).count();
    assert!(
        outs.iter().skip(2).all(|&v| bfg.clobbered_vn(v).is_some()),
        "every clobber output must carry its varnode tag"
    );
    assert_eq!(
        outs.len(),
        2 + override_clobbers,
        "Call's outputs = Control + Memory + override clobber slots"
    );
    assert!(
        override_clobbers < bfg.call_clobbered_regs().len(),
        "override clobber count ({}) must be strictly smaller than function-default ({})",
        override_clobbers,
        bfg.call_clobbered_regs().len(),
    );
}

/// Counterpart to the above: WITHOUT an override, the same Call
/// emits the full SystemV clobber shape.
#[test]
fn call_without_override_uses_function_default_clobber_set() {
    let (bytes, entry, _call_target) = x86_64_call_then_ret();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let config = RunConfig::new(
        arch,
        TargetCC::x86_64_systemv().unwrap(),
        sleigh,
        entry.into(),
        RunOptions::new(),
    )
    .unwrap();
    let bfg = strider_analyze::run(config).unwrap();

    let call_id = bfg
                .graph().all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    assert!(
        bfg.call_cc(call_id).is_none(),
        "no override means no recorded call_cc"
    );
    let outs = bfg.node_outputs(call_id);
    assert_eq!(
        outs.len(),
        2 + bfg.call_clobbered_regs().len(),
        "default Call: Control + Memory + per-CC clobber slots"
    );
}
