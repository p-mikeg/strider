//! Indirect branch that resolves to `Single(fentry_addr)` as a tail
//! call: the spliced Call must be built with the per-address override.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider::{RunConfig, SleighArch, Strider};
use target::CallingConvention as TargetCC;

/// x86_64: `mov eax, 5; jmp $TAIL_TARGET`.  With `fn_max_size = 10`
/// the cfg builder classifies the `jmp` as a `TailCall { target }`
/// terminator (out-of-function direct branch) and the IR-lifter
/// lowers it as `Call(IntConst(target)) + Return`.
///
///   0x1000:  B8 05 00 00 00     mov eax, 5
///   0x1005:  E9 F6 7F 00 00     jmp 0x9000
fn x86_64_tail_call_bytes() -> (Vec<u8>, u64, u64) {
    let mut bs = vec![0xB8, 0x05, 0x00, 0x00, 0x00, 0xE9, 0xF6, 0x7F, 0x00, 0x00];
    // NOP padding so any over-read past the jmp finds valid memory.
    bs.extend(std::iter::repeat_n(0x90u8, 32));
    (bs, 0x1000, 0x9000)
}

fn make_strider() -> Strider {
    strider::test_utils::strider_x86_64()
}

#[test]
fn lift_time_tail_call_to_overridden_address_uses_override_clobber_list() {
    let (bytes, entry, call_target) = x86_64_tail_call_bytes();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let mut overrides: HashMap<u64, TargetCC> = HashMap::new();
    overrides.insert(call_target, TargetCC::x86_64_all_preserving());

    let config = RunConfig {
        strider: &strider,
        start_addr: entry.into(),
        sleigh,
        rom: None,
        fn_max_size: Some(10),
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs: overrides,
    };
    let bfg = strider::run(config).unwrap();

    let call_id = bfg.graph
                .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::Call))
        .expect("in-place tail call splices in a Call node");
    // Per-Call override is recorded; its length matches the Call's
    // clobber output count and is strictly smaller than the function-
    // default clobber set.
    let override_list = bfg.graph
                .call_clobbered_override(call_id)
        .expect("in-place tail-call edit must record per-Call override");
    let outs = bfg.graph.node_outputs(call_id);
    assert_eq!(outs.len(), 2 + override_list.len());
    assert!(
        override_list.len() < bfg.call_clobbered_regs().len(),
        "override list ({}) must be strictly smaller than function-default ({})",
        override_list.len(),
        bfg.call_clobbered_regs().len(),
    );
}
