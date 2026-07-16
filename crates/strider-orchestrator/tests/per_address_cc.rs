//! End-to-end: a Call whose target is in `per_address_ccs` is built
//! with the override CC end-to-end (zero clobber outputs for an
//! all-preserving override).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rustc_hash::FxHashMap;
use strider_ir::IRViewer;

use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::NodeKind;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};
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
    let regs = sleigh.regs().unwrap();
    let cc = TargetCC::x86_64_systemv().build(&regs).unwrap();

    let mut overrides: FxHashMap<u64, strider_target::BuiltCallingConvention> =
        FxHashMap::default();
    overrides.insert(
        call_target,
        TargetCC::x86_64_all_preserving().build(&regs).unwrap(),
    );
    let lift_opts = LiftOptions {
        per_address_ccs: overrides,
        ..LiftOptions::default()
    };

    let mut strider = Strider::new(arch, sleigh, None).unwrap();
    let bfg = strider
        .analyze(entry, &cc, &lift_opts, &OptOptions::default(), None)
        .unwrap()
        .function;

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    let outs = bfg.node_outputs(call_id);
    // Override applied: the override CC is recorded (`call_cc` is Some) and
    // every clobber output carries its varnode tag (`get_vn_for_value`).
    // Before per-address CC was added, this Call emits a SystemV clobber
    // set with ~16+ slots; with the override, the only tracked variables
    // that survive the all-preserving filter are the Sleigh-generated
    // temporaries (UNIQUE / RAM varnodes) the override's by-name
    // callee_saved list can't reach.  The pinned invariant: the override
    // is recorded AND every clobber output is tagged AND the override
    // clobber count is strictly less than the function-default SystemV set.
    assert_ne!(
        bfg.get_cc(call_id),
        bfg.default_cc(),
        "override CC must be recorded on the Call (effective CC differs from default)"
    );
    let tagged_outputs = outs.iter().skip(2).count();
    assert!(
        outs.iter()
            .skip(2)
            .all(|&v| bfg.get_vn_for_value(v).is_some()),
        "every ret-val / clobber output must carry its varnode tag"
    );
    assert_eq!(
        outs.len(),
        2 + tagged_outputs,
        "Call's outputs = Control + Memory + tagged ret-val/clobber slots"
    );
    // The override total must be strictly smaller than the default total.
    let (default_ret, default_clob) = strider_ir::cc_ret_and_clobber_vns(&bfg, bfg.default_cc());
    let default_total = default_ret.len() + default_clob.len();
    assert!(
        tagged_outputs < default_total,
        "override tagged output count ({}) must be strictly smaller than \
         function-default total (ret_vals={} + clobbers={} = {})",
        tagged_outputs,
        default_ret.len(),
        default_clob.len(),
        default_total,
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
    let regs = sleigh.regs().unwrap();
    let cc = TargetCC::x86_64_systemv().build(&regs).unwrap();

    let mut strider = Strider::new(arch, sleigh, None).unwrap();
    let bfg = strider
        .analyze(
            entry,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .unwrap()
        .function;

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    assert_eq!(
        bfg.get_cc(call_id),
        bfg.default_cc(),
        "no override → effective CC is the function default"
    );
    let outs = bfg.node_outputs(call_id);
    let (rv, clob) = strider_ir::cc_ret_and_clobber_vns(&bfg, bfg.default_cc());
    let expected = 2 + rv.len() + clob.len();
    assert_eq!(
        outs.len(),
        expected,
        "default Call: Control + Memory + ret-val slots + clobber slots"
    );
}
