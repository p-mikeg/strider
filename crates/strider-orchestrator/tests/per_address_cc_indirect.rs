//! Indirect branch that resolves to `Single(fentry_addr)` as a tail
//! call: the spliced Call must be built with the per-address override.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rustc_hash::FxHashMap;
use strider_ir::IRViewer;

use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::NodeKind;
use strider_orchestrator::Strider;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::LiftOptions;
use strider_target::{CallingConvention as TargetCC, SleighArch};

mod common;

/// Lift + optimise the function at `entry` over `sleigh` with the standard
/// SystemV-x86_64 convention, the given `fn_max_size`, and the per-address
/// CC overrides (preset CCs, built against `sleigh`'s register table).
fn run_at(
    sleigh: rsleigh::Sleigh<BufMemReader<Vec<u8>>>,
    entry: u64,
    fn_max_size: u64,
    overrides: FxHashMap<u64, TargetCC>,
) -> strider_ir::Function {
    let arch = SleighArch::x86_64();
    let regs = sleigh.regs().unwrap();
    let cc = TargetCC::x86_64_systemv().unwrap().build(&regs).unwrap();
    let per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention> = overrides
        .into_iter()
        .map(|(addr, preset)| (addr, preset.build(&regs).unwrap()))
        .collect();
    let lift_opts = LiftOptions {
        fn_max_size: Some(fn_max_size),
        per_address_ccs,
        ..LiftOptions::default()
    };
    let mut strider = Strider::new(arch, sleigh, None).unwrap();
    strider
        .analyze(entry, &cc, &lift_opts, &OptOptions::default())
        .unwrap()
}

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
/// `Single(0x9000)`, and the orchestrator records the resolution in
/// `known_targets`.  On the next CFG rebuild the cfg builder seats a
/// `TailCall(0x9000)` terminator and the per-region driver splices in
/// `Call+Return` honouring the per-address override.
///
/// With `per_address_ccs[0x9000] = override`, the spliced Call must
/// pick up the override's clobber list.
///
///   0x1000:  48 C7 C0 00 90 00 00     mov rax, 0x9000
///   0x1007:  FF E0                    jmp rax
fn x86_64_indirect_jmp_to_const_bytes() -> (Vec<u8>, u64, u64) {
    let bs = vec![
        0x48, 0xC7, 0xC0, 0x00, 0x90, 0x00, 0x00, // mov rax, 0x9000
        0xFF, 0xE0, // jmp rax
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
    let bfg = run_at(sleigh, entry, 9, overrides);

    // The orchestrator's in-place edit spliced in a Call node at the
    // resolved target.
    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("orchestrator must splice a Call after resolving jmp rax to Single(0x9000)");
    assert!(
        bfg.call_cc(call_id).is_some(),
        "orchestrator must record the per-address override CC on the spliced Call"
    );
    let outs = bfg.node_outputs(call_id);
    let tagged_outputs = outs.iter().skip(2).count();
    assert!(
        outs.iter().skip(2).all(|&v| bfg.get_vn_for_value(v).is_some()),
        "every spliced ret-val / clobber output must carry its varnode tag"
    );
    assert_eq!(
        outs.len(),
        2 + tagged_outputs,
        "Call output count = 2 (ctrl + mem) + tagged ret-val/clobber count"
    );
    // The override total must be strictly smaller than the default total.
    let default_total = bfg.call_ret_val_regs().len() + bfg.call_clobbered_regs().len();
    assert!(
        tagged_outputs < default_total,
        "x86_64_all_preserving override tagged outputs ({}) must be strictly smaller than \
         the function-default total (ret_vals={} + clobbers={} = {})",
        tagged_outputs,
        bfg.call_ret_val_regs().len(),
        bfg.call_clobbered_regs().len(),
        default_total,
    );
}

/// Whole-graph `validate` coverage for a resolved per-address-override tail
/// call.  The other in-place-edit tests exercise the editor in isolation and
/// deliberately SKIP `validate`; this one runs the full validator on the
/// resolved function, pinning that the spliced Call+Return shape (arity,
/// vn-tagged outputs, fingerprints) is well-formed end-to-end.
///
/// It also documents the SSoT split that `anchor_calling_context_for`
/// encodes: the spliced **Return** returns from the *current* function to
/// *its* caller, so its ret-val slots come from the function's OWN default CC
/// (`function.default_cc()`), NOT the per-address override (which governs only
/// the tail-callee's Call shape).  For preset CCs the two agree on ret-val
/// count, so this asserts the function-default arity; a custom override with a
/// differing ret-val count would diverge, and sourcing the Return from the
/// override there would fail the validator's `2 + default_ret_val_count`
/// Return check.
#[test]
fn resolved_override_tail_call_passes_whole_graph_validate() {
    let (bytes, entry, call_target) = x86_64_indirect_jmp_to_const_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    // all_preserving differs from SystemV in its (empty) clobber set; it keeps
    // the same ret-val regs, so the spliced Call's clobber group shrinks while
    // the Return's ret-val arity stays at the function default.
    overrides.insert(call_target, TargetCC::x86_64_all_preserving().unwrap());

    let bfg = run_at(sleigh, entry, 9, overrides);

    // The whole-graph validator must pass on the resolved function — the
    // spliced Call+Return arity, vn tags, and fingerprints are all well-formed.
    strider_ir::validate::validate(&bfg, bfg.entry().unwrap())
        .expect("resolved override tail-call must pass whole-graph validation");

    // The spliced Return carries the FUNCTION-DEFAULT ret-val count (it returns
    // from THIS function to its caller), independent of the per-address override.
    let ret_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Return))
        .expect("resolved tail call splices a Return");
    let default_ret_val_count = bfg.ret_val_regs().len();
    assert!(default_ret_val_count > 0, "SystemV default has ret-val regs");
    assert_eq!(
        bfg.node_inputs(ret_id).len(),
        2 + default_ret_val_count,
        "spliced Return arity = 2 (ctrl + mem) + FUNCTION-DEFAULT ret-val count, \
         independent of the per-address override",
    );
}

/// Regression for the **no-override** orchestrator tail-call path: with no
/// per-address CC, `for_anchor` derives the effective convention from
/// `Function::default_cc()` (the SSoT) instead of a threaded `&LiftDriver`.
/// The end-to-end `run` must SUCCEED (the default-CC spliced Call passes
/// `validate` — its ret-val output group makes the arity match the
/// validator's default-`Call` arm) and the spliced Call must not
/// double-count its ret regs.  The other end-to-end tail-call test discards
/// the `run` result, so this pins the default path explicitly.
#[test]
fn indirect_default_cc_tail_call_runs_and_does_not_double_count_ret_regs() {
    let (bytes, entry, _call_target) = x86_64_indirect_jmp_to_const_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    // No per-address overrides → the splice uses the function's stored
    // default SystemV convention.
    //
    // `run_at` unwraps, asserting the analysis completes without error.
    // Note: the analysis does NOT re-validate the graph after in-place
    // indirect-branch edits (`validate` only runs during the initial
    // `FunctionBuilder::build`), so this alone does not prove post-edit
    // Call/Return arity — the explicit node-shape assertions below do.
    let bfg = run_at(sleigh, entry, 9, FxHashMap::default());

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("orchestrator must splice a Call for the default-CC tail call");
    let tagged: Vec<rsleigh::Vn> = bfg
        .node_outputs(call_id)
        .iter()
        .skip(2)
        .filter_map(|&v| bfg.get_vn_for_value(v))
        .collect();
    let distinct: std::collections::HashSet<rsleigh::Vn> = tagged.iter().copied().collect();
    assert_eq!(
        tagged.len(),
        distinct.len(),
        "no register may appear in both the ret-val and clobber groups; tagged = {tagged:?}",
    );
}

/// Regression: an override tail call whose CC declares return registers
/// (e.g. plain SystemV — RAX / XMM0) must put each ret reg in EXACTLY one
/// output group.  The spliced Call's outputs are
/// `[Control, Memory] ++ ret_vals ++ clobbers`; `call_clobbered_for`
/// excludes the ret regs from the clobber group, so no register may appear
/// twice.  A regression where the clobber derivation failed to exclude ret
/// regs (e.g. deriving clobbers via a helper that didn't filter them) would
/// list RAX/XMM0 in BOTH groups — the per-output `value_vn` tags would then
/// contain duplicates.
#[test]
fn indirect_override_with_ret_regs_does_not_double_count_them() {
    let (bytes, entry, _call_target) = x86_64_indirect_jmp_to_const_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    // A *normal* override (SystemV) so the spliced Call carries real ret
    // regs (RAX + XMM0) — unlike the all-preserving override above, whose
    // ret/clobber lists are near-empty and can't expose a double-count.
    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    overrides.insert(_call_target, TargetCC::x86_64_systemv().unwrap());

    let bfg = run_at(sleigh, entry, 9, overrides);

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("orchestrator must splice a Call for the override tail call");

    // Collect the per-output register tags past [Control, Memory].
    let tagged: Vec<rsleigh::Vn> = bfg
        .node_outputs(call_id)
        .iter()
        .skip(2)
        .filter_map(|&v| bfg.get_vn_for_value(v))
        .collect();
    let distinct: std::collections::HashSet<rsleigh::Vn> = tagged.iter().copied().collect();
    assert_eq!(
        tagged.len(),
        distinct.len(),
        "no register may appear in both the ret-val and clobber output groups; \
         tagged outputs = {tagged:?}",
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

    let bfg = run_at(sleigh, entry, 10, overrides);

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("in-place tail call splices in a Call node");
    // Override CC is recorded; every clobber output carries its varnode
    // tag and the clobber count is strictly smaller than the function-
    // default clobber set.
    assert!(
        bfg.call_cc(call_id).is_some(),
        "in-place tail-call edit must record the per-Call override CC"
    );
    let outs = bfg.node_outputs(call_id);
    let tagged_outputs = outs.iter().skip(2).count();
    assert!(
        outs.iter().skip(2).all(|&v| bfg.get_vn_for_value(v).is_some()),
        "every spliced ret-val / clobber output must carry its varnode tag"
    );
    assert_eq!(outs.len(), 2 + tagged_outputs);
    let default_total = bfg.call_ret_val_regs().len() + bfg.call_clobbered_regs().len();
    assert!(
        tagged_outputs < default_total,
        "override tagged outputs ({}) must be strictly smaller than function-default total ({})",
        tagged_outputs,
        default_total,
    );
}
