//! Indirect branch that resolves to `Single(fentry_addr)` as a tail
//! call: the spliced Call must be built with the per-address override.

use rustc_hash::FxHashMap;
use strider_ir::IRViewer;

use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::NodeKind;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};
use strider_target::{CallingConvention as TargetCC, SleighArch};

mod common;

/// Lift and optimise the function at `entry` over `sleigh` with the standard
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
    let cc = TargetCC::x86_64_systemv().build(&regs).unwrap();
    let per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention> = overrides
        .into_iter()
        .map(|(addr, preset)| (addr, preset.build(&regs).unwrap()))
        .collect();
    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(fn_max_size),
            ..Default::default()
        },
        per_address_ccs,
        ..LiftOptions::default()
    };
    let mut strider = Strider::new(arch, sleigh, None).unwrap();
    strider
        .analyze(entry, &cc, &lift_opts, &OptOptions::default(), None)
        .unwrap()
        .function
}

/// `mov eax, 5; jmp 0x9000` at 0x1000. With `fn_max_size = 10` the cfg
/// builder classifies the `jmp` as a `TailCall { target }` terminator
/// (out-of-function direct branch) and the IR-lifter lowers it as
/// `Call(IntConst(target)) + Return`.
fn x86_64_tail_call_bytes() -> (Vec<u8>, u64, u64) {
    let bs = vec![0xB8, 0x05, 0x00, 0x00, 0x00, 0xE9, 0xF6, 0x7F, 0x00, 0x00];
    (bs, 0x1000, 0x9000)
}

/// `mov rax, 0x9000; jmp rax` at 0x1000: the indirect jump lifts as a
/// placeholder IndirectBranch; once KnownBits + ConstantFold prove
/// `rax == 0x9000`, the CFG rebuild seats `TailCall(0x9000)` and the
/// per-region driver splices in `Call+Return`.
fn x86_64_indirect_jmp_to_const_bytes() -> (Vec<u8>, u64, u64) {
    let bs = vec![
        0x48, 0xC7, 0xC0, 0x00, 0x90, 0x00, 0x00, // mov rax, 0x9000
        0xFF, 0xE0, // jmp rax
    ];
    (bs, 0x1000, 0x9000)
}

/// `SpecialTerm::TailCall::skips_opcode` covers `BranchIndirect` alongside
/// `Branch`/`CondBranch`: lifting a `BranchIndirect` insn in the per-insn loop
/// emits an `IndirectBranch` and terminates the region, after which
/// `handle_tail_call` panics with "attempted to insert into terminated
/// region 0".
#[test]
fn indirect_resolves_to_intra_fn_overridden_address_uses_override_clobber_list() {
    let (bytes, entry, call_target) = x86_64_indirect_jmp_to_const_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    overrides.insert(call_target, TargetCC::x86_64_systemv().preserves_all());

    // 9 bytes covers `mov rax, imm` + `jmp rax` exactly.
    let bfg = run_at(sleigh, entry, 9, overrides);

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("orchestrator must splice a Call after resolving jmp rax to Single(0x9000)");
    assert_ne!(
        bfg.get_cc(call_id),
        bfg.default_cc(),
        "orchestrator must record the per-address override CC on the spliced Call"
    );
    let outs = bfg.node_outputs(call_id);
    let tagged_outputs = outs.iter().skip(2).count();
    assert!(
        outs.iter()
            .skip(2)
            .all(|&v| bfg.get_vn_for_value(v).is_some()),
        "every spliced ret-val / clobber output must carry its varnode tag"
    );
    // Against the OVERRIDE's own lists: `2 + outs.len() - 2` would hold for
    // any arity, including a splice that emitted no ret-val group at all.
    let (ret, clob) = strider_ir::cc_ret_and_clobber_vns(&bfg, bfg.get_cc(call_id));
    assert_eq!(
        outs.len(),
        2 + ret.len() + clob.len(),
        "Call output count = 2 (ctrl + mem) + the override's ret-val/clobber count"
    );
    let (default_ret, default_clob) = strider_ir::cc_ret_and_clobber_vns(&bfg, bfg.default_cc());
    let default_total = default_ret.len() + default_clob.len();
    assert!(
        tagged_outputs < default_total,
        "x86_64_all_preserving override tagged outputs ({}) must be strictly smaller than \
         the function-default total (ret_vals={} + clobbers={} = {})",
        tagged_outputs,
        default_ret.len(),
        default_clob.len(),
        default_total,
    );
}

/// Runs the full validator on the resolved function, pinning that the spliced
/// Call+Return shape (arity, vn-tagged outputs, fingerprints) is well-formed
/// end-to-end; the other in-place-edit tests exercise the editor in isolation.
///
/// Also documents the SSoT split `target_calling_context_for` encodes: the
/// spliced **Return** returns from the *current* function to *its*
/// caller, so its ret-val slots come from the function's OWN default CC,
/// NOT the per-address override (which governs only the tail-callee's
/// Call shape). For preset CCs the two agree on ret-val count; a custom
/// override with a differing count would diverge, and sourcing the
/// Return from the override there would fail the validator's
/// `2 + default_ret_val_count` check.
#[test]
fn resolved_override_tail_call_passes_whole_graph_validate() {
    let (bytes, entry, call_target) = x86_64_indirect_jmp_to_const_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    // all_preserving differs from SystemV in its (empty) clobber set but
    // keeps the same ret-val regs, so the spliced Call's clobber group
    // shrinks while the Return's ret-val arity stays at the function default.
    overrides.insert(call_target, TargetCC::x86_64_systemv().preserves_all());

    let bfg = run_at(sleigh, entry, 9, overrides);

    strider_ir::validate::validate(&bfg)
        .expect("resolved override tail-call must pass whole-graph validation");

    // Return carries the FUNCTION-DEFAULT ret-val count (it returns from THIS
    // function to its caller), independent of the per-address override.
    let ret_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Return))
        .expect("resolved tail call splices a Return");
    let default_ret_val_count = bfg.ret_val_regs().len();
    assert!(
        default_ret_val_count > 0,
        "SystemV default has ret-val regs"
    );
    assert_eq!(
        bfg.node_inputs(ret_id).len(),
        2 + default_ret_val_count,
        "spliced Return arity = 2 (ctrl + mem) + FUNCTION-DEFAULT ret-val count, \
         independent of the per-address override",
    );
}

/// The **no-override** path: with no per-address CC, `build_cc_call` falls back
/// to `Function::default_cc()`, the SSoT.
/// Pins that the default-CC spliced Call passes `validate` and does not
/// double-count its ret regs; the other end-to-end tail-call test
/// discards the `run` result, so this is the one that checks the default
/// path explicitly.
#[test]
fn indirect_default_cc_tail_call_runs_and_does_not_double_count_ret_regs() {
    let (bytes, entry, _call_target) = x86_64_indirect_jmp_to_const_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    // No per-address overrides: the splice uses the function's stored
    // default SystemV convention. `run_at`'s unwrap only proves analysis
    // completes without error; the analysis does NOT re-validate the graph
    // after in-place indirect-branch edits (`validate` runs only during the
    // initial `FunctionBuilder::build`), so the explicit node-shape
    // assertions below are what actually prove post-edit Call/Return arity.
    let bfg = run_at(sleigh, entry, 9, FxHashMap::default());

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("orchestrator must splice a Call for the default-CC tail call");
    let outs = bfg.node_outputs(call_id);
    let (ret, clob) = strider_ir::cc_ret_and_clobber_vns(&bfg, bfg.default_cc());
    assert_eq!(
        outs.len(),
        2 + ret.len() + clob.len(),
        "post-edit Call arity = 2 (ctrl + mem) + the default CC's ret-val/clobber count"
    );
    let tagged: Vec<rsleigh::Vn> = outs
        .iter()
        .skip(2)
        .filter_map(|&v| bfg.get_vn_for_value(v))
        .collect();
    assert_eq!(
        tagged.len(),
        outs.len() - 2,
        "an untagged output would silently drop out of the duplicate check",
    );
    let distinct: std::collections::HashSet<rsleigh::Vn> = tagged.iter().copied().collect();
    assert_eq!(
        tagged.len(),
        distinct.len(),
        "no register may appear in both the ret-val and clobber groups; tagged = {tagged:?}",
    );
}

/// Regression: an override CC declaring return registers (e.g. plain
/// SystemV: RAX / XMM0) must put each ret reg in EXACTLY one output
/// group. The spliced Call's outputs are
/// `[Control, Memory] ++ ret_vals ++ clobbers`; `call_clobbered_for`
/// excludes the ret regs from the clobber group. A clobber derivation
/// that failed to filter them would list RAX/XMM0 in BOTH groups, so the
/// per-output `value_vn` tags would contain duplicates.
#[test]
fn indirect_override_with_ret_regs_does_not_double_count_them() {
    let (bytes, entry, _call_target) = x86_64_indirect_jmp_to_const_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();

    // A normal override (SystemV) gives the spliced Call real ret regs
    // (RAX + XMM0); the all-preserving override above can't expose a
    // double-count since its ret/clobber lists are near-empty.
    let mut overrides: FxHashMap<u64, TargetCC> = FxHashMap::default();
    overrides.insert(_call_target, TargetCC::x86_64_systemv());

    let bfg = run_at(sleigh, entry, 9, overrides);

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("orchestrator must splice a Call for the override tail call");

    // Register tags for outputs past [Control, Memory].
    let outs = bfg.node_outputs(call_id);
    let tagged: Vec<rsleigh::Vn> = outs
        .iter()
        .skip(2)
        .filter_map(|&v| bfg.get_vn_for_value(v))
        .collect();
    assert_eq!(
        tagged.len(),
        outs.len() - 2,
        "an untagged output would silently drop out of the duplicate check",
    );
    assert!(
        !tagged.is_empty(),
        "SystemV declares ret regs, so the group cannot be empty",
    );
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
    overrides.insert(call_target, TargetCC::x86_64_systemv().preserves_all());

    let bfg = run_at(sleigh, entry, 10, overrides);

    let call_id = bfg
        .graph()
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("in-place tail call splices in a Call node");
    assert_ne!(
        bfg.get_cc(call_id),
        bfg.default_cc(),
        "in-place tail-call edit must record the per-Call override CC"
    );
    let outs = bfg.node_outputs(call_id);
    let tagged_outputs = outs.iter().skip(2).count();
    assert!(
        outs.iter()
            .skip(2)
            .all(|&v| bfg.get_vn_for_value(v).is_some()),
        "every spliced ret-val / clobber output must carry its varnode tag"
    );
    let (ret, clob) = strider_ir::cc_ret_and_clobber_vns(&bfg, bfg.get_cc(call_id));
    assert_eq!(
        outs.len(),
        2 + ret.len() + clob.len(),
        "Call output count = 2 (ctrl + mem) + the override's ret-val/clobber count"
    );
    let (default_ret, default_clob) = strider_ir::cc_ret_and_clobber_vns(&bfg, bfg.default_cc());
    let default_total = default_ret.len() + default_clob.len();
    assert!(
        tagged_outputs < default_total,
        "override tagged outputs ({}) must be strictly smaller than function-default total ({})",
        tagged_outputs,
        default_total,
    );
}
