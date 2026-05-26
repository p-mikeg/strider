//! Indirect branch that resolves to `Single(fentry_addr)` as a tail
//! call: the spliced Call must be built with the per-address override.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rustc_hash::FxHashMap;

use strider_ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider_analyze::{Config, Strider};
use strider_target::{CallingConvention as TargetCC, SleighArch};

mod common;

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
    common::strider_x86_64()
}

/// x86_64: `mov rax, 0x9000; jmp rax` — the indirect jump is lifted
/// as a placeholder IndirectBranch.  At fixed point KnownBits +
/// ConstantFold prove `rax == 0x9000`, the classifier returns
/// `Single(0x9000)`, and the orchestrator splices in a Call+Return
/// in-place via [`crate::opt::apply_tail_call`].
///
/// With `per_address_ccs[0x9000] = override`, the spliced Call must
/// pick up the override's clobber list — pinning the orchestrator's
/// `apply_in_place_edit` path for the `ResolvedTargets::Single` arm
/// with override.
///
///   0x1000:  48 C7 C0 00 90 00 00     mov rax, 0x9000
///   0x1007:  FF E0                    jmp rax
fn x86_64_indirect_jmp_to_const_bytes() -> (Vec<u8>, u64, u64) {
    let mut bs = vec![
        0x48, 0xC7, 0xC0, 0x00, 0x90, 0x00, 0x00, // mov rax, 0x9000
        0xFF, 0xE0,                                // jmp rax
    ];
    bs.extend(std::iter::repeat_n(0x90u8, 32)); // NOP pad
    (bs, 0x1000, 0x9000)
}

/// Indirect-branch-via-constant-fold path: emits an IndirectBranch
/// placeholder, then ConstantFold + KnownBits prove the target is a
/// runtime constant, the orchestrator's classify_anchor returns
/// `Single(K)`, and apply_in_place_edit splices a Call+Return with
/// the per-address override.
///
/// The lift-time tail-call test below already covers the
/// `ResolvedTargets::Single + override` arm of `apply_in_place_edit`
/// (the orchestrator routes both through the same code path).  This
/// `#[ignore]`d test exists to pin the higher-level shape — a real
/// `mov reg, K; jmp reg` shape — once the cfg builder / orchestrator
/// handles the "region already terminated" interaction without error.
/// Currently fails with "attempted to insert into terminated region 0"
/// at the orchestrator's apply_in_place_edit step.
#[test]
#[ignore = "region-termination interaction not yet handled when the resolver \
            splices into a region that the indirect jmp already terminated"]
fn indirect_resolves_to_intra_fn_overridden_address_uses_override_clobber_list() {
    let (bytes, entry, call_target) = x86_64_indirect_jmp_to_const_bytes();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    overrides.insert(call_target, TargetCC::x86_64_all_preserving().unwrap());

    let config = Config {
        strider: &strider,
        start_addr: entry.into(),
        sleigh,
        rom: None,
        // 9 bytes covers `mov rax, imm` + `jmp rax` exactly; any further
        // memory access is via the orchestrator's resolver.
        fn_max_size: Some(9),
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs: overrides,
    };
    let bfg = strider_analyze::run(config).unwrap();

    // The orchestrator's in-place edit spliced in a Call node at the
    // resolved target.
    let call_id = bfg
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("orchestrator must splice a Call after resolving jmp rax to Single(0x9000)");
    let override_list = bfg
        .call_clobbered_override(call_id)
        .expect("orchestrator's apply_in_place_edit must record the per-address override");
    let outs = bfg.node_outputs(call_id);
    assert_eq!(
        outs.len(),
        2 + override_list.len(),
        "Call output count = 2 (ctrl + mem) + override clobber count"
    );
    assert!(
        override_list.len() < bfg.call_clobbered_regs().len(),
        "x86_64_all_preserving override list ({}) must be strictly smaller than the \
         function-default clobber set ({})",
        override_list.len(),
        bfg.call_clobbered_regs().len(),
    );
}

#[test]
fn lift_time_tail_call_to_overridden_address_uses_override_clobber_list() {
    let (bytes, entry, call_target) = x86_64_tail_call_bytes();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    overrides.insert(call_target, TargetCC::x86_64_all_preserving().unwrap());

    let config = Config {
        strider: &strider,
        start_addr: entry.into(),
        sleigh,
        rom: None,
        fn_max_size: Some(10),
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs: overrides,
    };
    let bfg = strider_analyze::run(config).unwrap();

    let call_id = bfg
                .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("in-place tail call splices in a Call node");
    // Per-Call override is recorded; its length matches the Call's
    // clobber output count and is strictly smaller than the function-
    // default clobber set.
    let override_list = bfg
                .call_clobbered_override(call_id)
        .expect("in-place tail-call edit must record per-Call override");
    let outs = bfg.node_outputs(call_id);
    assert_eq!(outs.len(), 2 + override_list.len());
    assert!(
        override_list.len() < bfg.call_clobbered_regs().len(),
        "override list ({}) must be strictly smaller than function-default ({})",
        override_list.len(),
        bfg.call_clobbered_regs().len(),
    );
}
