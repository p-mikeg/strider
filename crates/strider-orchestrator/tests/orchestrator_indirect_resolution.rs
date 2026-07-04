//! Integration tests for the strider top-level orchestrator
//! (`strider_orchestrator::Strider::analyze`).
//!
//! Each test:
//!   1. Constructs a `Config` against a synthetic byte sequence +
//!      the standard SystemV-x86_64 calling convention,
//!   2. Calls `strider_orchestrator::Strider::analyze`,
//!   3. Asserts the result matches the spec's per-scenario contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};
use strider_target::{CallingConvention, SleighArch};

fn make_sleigh_value(bytes: Vec<u8>, base: u64) -> Sleigh<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh")
}

/// Lift + optimise the function at `base` in `bytes` via the orchestrator
/// `Strider` handle with the standard SystemV-x86_64 convention and
/// default options.
fn run_at(bytes: Vec<u8>, base: u64) -> anyhow::Result<strider_orchestrator::AnalyzeResult> {
    let arch = SleighArch::x86_64();
    let sleigh = make_sleigh_value(bytes, base);
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    let mut strider = Strider::new(arch, sleigh, None)?;
    strider.analyze(
        base,
        &cc,
        &LiftOptions::default(),
        &OptOptions::default(),
        None,
    )
}

#[test]
fn outer_loop_zero_iter_when_no_branch_indirect_returns_ir() {
    // A function with no BranchIndirect: just `ret`.  The fast path
    // skips the loop entirely; the result is the optimised IR.

    let bytes = vec![0xc3u8]; // ret
    let function = run_at(bytes, 0x1000).expect("orchestrator").function;
    let mut had_return = false;
    for nid in function.walk() {
        if matches!(function.node_kind(nid), strider_ir::node::NodeKind::Return) {
            had_return = true;
        }
    }
    assert!(had_return);
}

#[test]
fn outer_loop_unresolved_branch_is_reported_not_errored() {
    // `jmp rax` on x86_64: rax is a function-entry value (no constant
    // write), and x86_64 has no link register, so the IR-level
    // indirect-branch resolver cannot classify it.  The orchestrator must
    // reach a fixed point and RETURN the result with the branch listed in
    // `unresolved_indirect_branches` (never panic, never loop forever,
    // never error).

    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax — sole machine insn at 0x1000
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let result =
        run_at(bytes, 0x1000).expect("analyze must return Ok even when a branch is unresolvable");
    // EXACTLY the one branch site, at the snippet's only machine
    // instruction (0x1000); `jmp rax` lifts to a single BRANCHIND pcode
    // op, so the pcode insn index is 0.
    assert_eq!(
        result.unresolved_indirect_branches,
        vec![strider_cfg::PcodeInsnAddr {
            machine_addr: strider_cfg::MachineInsnAddr::from(0x1000u64),
            insn_index: 0,
        }],
        "unresolved list must contain exactly the jmp-rax site"
    );
    // The placeholder must still be present in the returned function.
    let placeholder_count = result
        .function
        .walk()
        .filter(|&n| {
            matches!(
                result.function.node_kind(n),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .count();
    assert_eq!(
        placeholder_count, 1,
        "exactly one unresolved IndirectBranch placeholder must remain in the returned IR"
    );
}

#[test]
fn analyze_ignores_pre_seeded_known_targets_in_lift_options() {
    // `Strider::analyze`'s documented contract: the caller's
    // `cfg.known_targets` seed is IGNORED — the resolution loop grows its
    // own map from classifier results only.  Pin that: pre-seeding the
    // unresolvable `jmp rax` site with a valid Single target does NOT
    // short-circuit resolution; the branch is still reported unresolved
    // and the placeholder survives (identical outcome to the unseeded
    // run above).
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax at 0x1000
    bytes.extend(std::iter::repeat_n(0xccu8, 16));

    let site = strider_cfg::PcodeInsnAddr {
        machine_addr: strider_cfg::MachineInsnAddr::from(0x1000u64),
        insn_index: 0,
    };
    let mut known = rustc_hash::FxHashMap::default();
    // 0x1004 is in-range padding — a valid (decodable) seed target.
    known.insert(site, strider_cfg::ResolvedTargets::Single(0x1004));
    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            known_targets: known,
            ..Default::default()
        },
        ..LiftOptions::default()
    };

    let arch = SleighArch::x86_64();
    let sleigh = make_sleigh_value(bytes, 0x1000);
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(0x1000, &cc, &lift_opts, &OptOptions::default(), None)
        .expect("analyze");
    assert_eq!(
        result.unresolved_indirect_branches,
        vec![site],
        "the pre-seeded known_targets map must be ignored by analyze (loop owns its own map)"
    );
    let placeholder_survives = result.function.walk().any(|n| {
        matches!(
            result.function.node_kind(n),
            strider_ir::node::NodeKind::IndirectBranch
        )
    });
    assert!(
        placeholder_survives,
        "placeholder must survive — seed was ignored"
    );
}

#[test]
fn outer_loop_resolves_via_stack_load_forward_for_x86_64_push_pop() {
    // `push imm32; pop rax; jmp rax` — structurally a tail call.
    // After StackOffsetDetect + LoadForward the placeholder's dispatch
    // input folds to IntConst(K); the `IndirectBranchClassify` post-pass
    // reads that live input and classifies `Single(K)`.  K lies OUTSIDE
    // the function range (below `start_addr`), so the orchestrator seats
    // it as a tail call and the rebuild lowers it to `Call(K) + Return`.
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));

    // Must actually resolve — not fall back to the unresolved error.
    let function = run_at(bytes, 0x1000)
        .expect("push/pop/jmp of a constant must resolve to a tail call")
        .function;
    // The placeholder must have been resolved away: no `IndirectBranch`
    // node survives in the final graph.
    let placeholder_survives = function.walk().any(|n| {
        matches!(
            function.node_kind(n),
            strider_ir::node::NodeKind::IndirectBranch
        )
    });
    assert!(
        !placeholder_survives,
        "expected the IndirectBranch placeholder to be resolved into a tail call, \
         but one survived in the final graph"
    );
}

#[test]
fn orchestrator_owned_sleigh_succeeds_in_fast_path() {
    let bytes = vec![0xc3u8]; // ret
    let function = run_at(bytes, 0x1000)
        .expect("orchestrator must succeed in fast path")
        .function;
    let mut had_return = false;
    for nid in function.walk() {
        if matches!(function.node_kind(nid), strider_ir::node::NodeKind::Return) {
            had_return = true;
        }
    }
    assert!(
        had_return,
        "fast-path exit must produce a graph with at least one Return"
    );
}

#[test]
fn orchestrator_owned_sleigh_reports_unresolved_branch() {
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let result =
        run_at(bytes, 0x1000).expect("analyze returns Ok even with an unresolvable branch");
    assert!(
        !result.unresolved_indirect_branches.is_empty(),
        "the unresolvable `jmp rax` must be reported"
    );
}

#[test]
fn orchestrator_correctness_unchanged_after_sleigh_persistence() {
    // CORRECTNESS: the resulting graph for a function that does NOT
    // need indirect resolution is identical regardless of how many
    // times Sleigh is reused across runs.

    let make_run = || {
        let bytes = vec![0xc3u8]; // ret
        run_at(bytes, 0x1000).expect("orchestrator").function
    };
    let g1 = make_run();
    let g2 = make_run();

    let kinds_1: Vec<strider_ir::node::NodeKind> =
        g1.walk().map(|nid| *g1.node_kind(nid)).collect();
    let kinds_2: Vec<strider_ir::node::NodeKind> =
        g2.walk().map(|nid| *g2.node_kind(nid)).collect();
    assert_eq!(kinds_1, kinds_2);
}

#[test]
fn analyze_branch_behind_constant_false_guard_reports_clean() {
    // OR-1 smoke test: a `jmp rax` reachable only through a constant-false
    // flag (`xor eax,eax; test eax,eax; jne <indirect>`).  The CFG/optimizer
    // pipeline never leaves a live, unclassified `IndirectBranch` for this
    // shape, so `analyze` must report an empty unresolved list (no spurious
    // dead-branch report).  The OR-1 liveness/known-targets filter itself is
    // exercised deterministically by the `live_unresolved_*` unit tests in
    // `lib.rs`; this is the end-to-end contract check that no spurious
    // "unresolved" leaks out of the orchestrator for a guarded indirect jump.
    //
    // 0x1000: 31 c0       xor eax,eax     (eax=0, ZF=1)
    // 0x1002: 85 c0       test eax,eax    (ZF stays 1)
    // 0x1004: 75 02       jne 0x1008      (taken iff ZF==0 — never)
    // 0x1006: c3          ret             (live fallthrough)
    // 0x1008: ff e0       jmp rax         (guarded arm)
    let mut bytes: Vec<u8> = vec![0x31, 0xc0, 0x85, 0xc0, 0x75, 0x02, 0xc3, 0xff, 0xe0];
    bytes.extend(std::iter::repeat_n(0xc3u8, 16));
    let result = run_at(bytes, 0x1000).expect("analyze must succeed");
    assert!(
        result.unresolved_indirect_branches.is_empty(),
        "no live, unclassified IndirectBranch → empty unresolved list (got {:?})",
        result.unresolved_indirect_branches
    );
}

#[test]
fn analyze_resolution_loop_beats_single_pass_manual_lift() {
    // OR-5 (EC-5): demonstrate the orchestrator's headline behaviour that
    // the flagship example (which drives `Lifter` directly for a single
    // pass) never exercises — the resolve/re-lift loop.
    //
    // `push K; pop rax; jmp rax` resolves to a tail call only AFTER the
    // classifier folds `Single(K)` into `known_targets` and the CFG is
    // rebuilt.  A single manual `build_cfg + build_ir + pipeline.run`
    // (exactly what `orchestrator_demo` does) leaves the `IndirectBranch`
    // placeholder in the graph; `Strider::analyze`'s loop removes it.
    use strider_orchestrator::Lifter;
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));

    // --- single manual pass (the example's path) ---
    let arch = SleighArch::x86_64();
    let sleigh = make_sleigh_value(bytes.clone(), 0x1000);
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("cc");
    let mut lifter = Lifter::new(arch, sleigh).expect("lifter");
    let cfg = lifter
        .build_cfg(
            strider_cfg::MachineInsnAddr::from(0x1000u64),
            &Default::default(),
            &Default::default(),
        )
        .expect("cfg");
    let mut single = lifter
        .build_ir_with(&cfg, cc, &LiftOptions::default())
        .expect("ir")
        .function;
    let pipeline = strider_orchestrator::opt::default_pipeline();
    let mut ctx = strider_orchestrator::opt::OptCtx::new(None);
    pipeline.run(&mut single, &mut ctx).expect("pipeline");
    let single_pass_has_indirect = single.walk().any(|n| {
        matches!(
            single.node_kind(n),
            strider_ir::node::NodeKind::IndirectBranch
        )
    });
    assert!(
        single_pass_has_indirect,
        "a single manual lift+optimize pass should leave the IndirectBranch placeholder \
         (the example's path does not resolve it)"
    );

    // --- the orchestrator's resolution loop ---
    let resolved = run_at(bytes, 0x1000)
        .expect("analyze must resolve the indirect tail call")
        .function;
    let loop_has_indirect = resolved.walk().any(|n| {
        matches!(
            resolved.node_kind(n),
            strider_ir::node::NodeKind::IndirectBranch
        )
    });
    assert!(
        !loop_has_indirect,
        "Strider::analyze's resolution loop must resolve the placeholder away"
    );
    // And the loop reports no unresolved branches for this case.
    let result_unresolved = {
        let arch = SleighArch::x86_64();
        let mut b2: Vec<u8> = vec![0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0];
        b2.extend(std::iter::repeat_n(0xccu8, 64));
        let sleigh = make_sleigh_value(b2, 0x1000);
        let regs = sleigh.regs().expect("regs");
        let cc = CallingConvention::x86_64_systemv()
            .build(&regs)
            .expect("cc");
        let mut strider = Strider::new(arch, sleigh, None).expect("strider");
        strider
            .analyze(
                0x1000,
                &cc,
                &LiftOptions::default(),
                &OptOptions::default(),
                None,
            )
            .expect("analyze")
            .unresolved_indirect_branches
    };
    assert!(
        result_unresolved.is_empty(),
        "fully-resolved tail call must report no unresolved branches"
    );
}

// Constructs a placeholder Sleigh for the unused-import shim.  Keeping
// the import surface stable means no `make_sleigh_value` warning.
#[allow(dead_code)]
fn _ensure_make_sleigh_used() {
    let _ = make_sleigh_value(vec![0xc3], 0);
}
