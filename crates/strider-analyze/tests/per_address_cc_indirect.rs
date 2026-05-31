//! Indirect branch that resolves to `Single(fentry_addr)` as a tail
//! call: the spliced Call must be built with the per-address override.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rustc_hash::FxHashMap;

use strider_ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider_analyze::{RunConfig, RunOptions};
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
    let bs = vec![0xB8, 0x05, 0x00, 0x00, 0x00, 0xE9, 0xF6, 0x7F, 0x00, 0x00];
    (bs, 0x1000, 0x9000)
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
    let bs = vec![
        0x48, 0xC7, 0xC0, 0x00, 0x90, 0x00, 0x00, // mov rax, 0x9000
        0xFF, 0xE0,                                // jmp rax
    ];
    (bs, 0x1000, 0x9000)
}

/// Indirect-branch-via-known-targets path: the first iteration sees
/// `jmp rax` as an `UnresolvedIndirectBranch`.  Once the orchestrator
/// resolves `rax = 0x9000` via constant-fold + classify_anchor, the
/// CFG rebuild seeds `known_targets` and the cfg builder treats the
/// `jmp rax` as a `TailCall(0x9000)`.  The per-region driver's
/// `handle_tail_call` then splices in `Call+Return` honouring the
/// per-address override.
///
/// Regression guard for the bug where `SpecialTerm::TailCall::
/// skips_opcode` only skipped `Branch`/`CondBranch`, so the
/// `BranchIndirect` insn was lifted by the per-insn loop (emitting
/// `IndirectBranch` + terminating the region) and `handle_tail_call`
/// crashed with "attempted to insert into terminated region 0".
/// Fixed by extending the skip-set to include `BranchIndirect`.
#[test]
fn indirect_resolves_to_intra_fn_overridden_address_uses_override_clobber_list() {
    let (bytes, entry, call_target) = x86_64_indirect_jmp_to_const_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    overrides.insert(call_target, TargetCC::x86_64_all_preserving().unwrap());

    // 9 bytes covers `mov rax, imm` + `jmp rax` exactly; any further
    // memory access is via the orchestrator's resolver.
    let config = RunConfig::new(
        arch,
        TargetCC::x86_64_systemv().unwrap(),
        sleigh,
        entry.into(),
        RunOptions::new()
            .fn_max_size(9)
            .per_address_ccs_unbuilt(overrides),
    )
    .unwrap();
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
        RunOptions::new()
            .fn_max_size(10)
            .per_address_ccs_unbuilt(overrides),
    )
    .unwrap();
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
