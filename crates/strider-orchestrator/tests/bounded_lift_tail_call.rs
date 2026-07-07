//! Regression: bounded lift must not crash on a `RegionTerminator::TailCall`.
//!
//! When `fn_max_size` is set, the cfg builder classifies any direct `jmp`
//! whose target lies outside `[start, start+fn_max_size)` as
//! `RegionTerminator::TailCall { target }` (no successor edge).  The
//! terminator's doc-comment promises the IR layer lowers it as
//! `Call(IntConst(target)) + Return`, but historically nothing did:
//! the per-insn loop processed the trailing `Opcode::Branch` through
//! the generic `handle_branch` path, which errors with
//! "invalid region index N" because a TailCall region has no
//! Unconditional edge.
//!
//! This test pins the fix.  Synthetic x86_64 function:
//!
//! ```text
//! 0x1000:  B8 05 00 00 00     mov eax, 5
//! 0x1005:  E9 F6 7F 00 00     jmp 0x9000        ← out-of-fn tail call
//! ```
//!
//! With `fn_max_size = 10` and `allow_code_before_start_addr = false`,
//! the cfg builder emits `RegionTerminator::TailCall { target: 0x9000 }`.
//! The IR must lift it as `Call(IntConst(0x9000)) + Return` — i.e. the
//! lifted graph must contain at least one `Call` node whose target is
//! `IntConst(0x9000)` AND a `Return` node downstream of it.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

mod common;

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_ir_test_utils::IrWalkerEx;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};
use strider_target::{CallingConvention, SleighArch};

const BASE: u64 = 0x1000;
const TAIL_TARGET: u64 = 0x9000;

/// Lift + optimise the function at `entry` over `sleigh` with the standard
/// SystemV-x86_64 convention, the caller-supplied `lift_opts`, and default
/// opt options.
fn run_at(
    sleigh: Sleigh<BufMemReader<Vec<u8>>>,
    entry: u64,
    lift_opts: &LiftOptions,
) -> anyhow::Result<strider_ir::Function> {
    let arch = SleighArch::x86_64();
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    let mut strider = Strider::new(arch, sleigh, None)?;
    strider
        .analyze(entry, &cc, lift_opts, &OptOptions::default(), None)
        .map(|r| r.function)
}

/// `mov eax, 5; jmp 0x9000` at 0x1000 (10 bytes).  `jmp` target is
/// 0x9000 (rel32 = 0x9000 - 0x100A = 0x7FF6 = `F6 7F 00 00` LE).
fn synthetic_bytes() -> Vec<u8> {
    vec![0xB8, 0x05, 0x00, 0x00, 0x00, 0xE9, 0xF6, 0x7F, 0x00, 0x00]
}

fn make_sleigh() -> Sleigh<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(synthetic_bytes(), BASE);
    Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new")
}

#[test]
fn bounded_lift_handles_tail_call_terminator() {
    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(10),
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    let function = run_at(make_sleigh(), BASE, &lift_opts)
        .expect("orchestrator must lift TailCall as Call+Return");

    // Post-condition: the graph contains a `Call` whose target operand
    // is an `IntConst(0x9000)`, and a `Return` node downstream.
    let mut had_call_with_target = false;
    let mut had_return = false;
    for nid in function.walk() {
        match function.node_kind(nid) {
            NodeKind::Call => {
                // Call inputs: [ctrl, mem, target, sp, args...].  Slot 2 is the target.
                let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
                if let Some(&target_value) = inputs.get(2)
                    && function.int_const_u128(target_value) == Some(u128::from(TAIL_TARGET))
                {
                    had_call_with_target = true;
                }
            }
            NodeKind::Return => had_return = true,
            _ => {}
        }
    }
    assert!(
        had_call_with_target,
        "expected a Call(IntConst({:#x})) node from the lifted tail call",
        TAIL_TARGET
    );
    assert!(
        had_return,
        "expected a Return node downstream of the tail-call Call"
    );
}

/// Helper: walks the lifted graph and returns whether it contains a
/// `Call(IntConst(target)) + Return` pair.  Mirrors the verifier in
/// `bounded_lift_handles_tail_call_terminator` so the new tests can
/// share the same shape assertion.
fn graph_has_tail_call_to(function: &strider_ir::Function, target: u64) -> bool {
    let mut had_call = false;
    let mut had_return = false;
    for nid in function.walk() {
        match function.node_kind(nid) {
            NodeKind::Call => {
                let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
                if let Some(&target_value) = inputs.get(2)
                    && function.int_const_u128(target_value) == Some(u128::from(target))
                {
                    had_call = true;
                }
            }
            NodeKind::Return => had_return = true,
            _ => {}
        }
    }
    had_call && had_return
}

/// Synthetic vmspace_exitfree-shape: a small function ending with a
/// backward `jmp` whose target is a *different* function (below
/// `start_addr`).  Pre-fix, with `allow_code_before_start_addr=true`
/// AND `fn_max_size` set, the cfg builder followed the backward jmp
/// into adjacent bytes — ballooning the lifted graph to tens of
/// thousands of nodes.  Post-fix the backward target is classified as
/// a tail call regardless of the reach-back flag (since `fn_max_size`
/// defines the function's exact extent), and the IR carries
/// `Call(IntConst(<backward_target>)) + Return`.
#[test]
fn bounded_lift_backward_jmp_with_fn_max_size_classifies_as_tail_call() {
    // Layout:
    //   0x1000..0x1080: NOP padding (the "previous function").
    //   0x1080..0x108A: our function — `mov eax, 5; jmp 0x1000`.
    //
    // jmp 0x1000 from 0x1080+5 (insn after `mov`) = rel32 of
    //   0x1000 - (0x1085 + 5) = 0x1000 - 0x108A = -0x8A = 0xFFFFFF76 LE.
    const BASE: u64 = 0x1000;
    const FN_START: u64 = 0x1080;
    const TAIL_TARGET: u64 = 0x1000;
    let mut bs = vec![0x90u8; 0x80]; // 0x1000..0x1080: padding
    bs.extend_from_slice(&[0xB8, 0x05, 0x00, 0x00, 0x00]); // mov eax, 5
    bs.extend_from_slice(&[0xE9, 0x76, 0xFF, 0xFF, 0xFF]); // jmp -0x8A → 0x1000

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bs, BASE);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");

    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(10),
            allow_code_before_start_addr: true,
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    let function = run_at(sleigh, FN_START, &lift_opts).expect(
        "backward jmp + fn_max_size must classify as tail call regardless of reach-back flag",
    );

    assert!(
        graph_has_tail_call_to(&function, TAIL_TARGET),
        "expected Call(IntConst({:#x})) + Return from the backward-jmp tail call",
        TAIL_TARGET
    );
    // Sanity: a 10-byte function tail-calling out should produce a
    // small graph — not the tens-of-thousands-of-nodes pre-fix shape.
    let node_count = function.walk().count();
    assert!(
        node_count < 200,
        "lifted graph should be tight (~tens of nodes); got {node_count}",
    );
}

/// A function whose body has no explicit terminator inside the bound
/// and whose fall-through crosses `start + fn_max_size` is a
/// **function-boundary error**, not a tail call: a legitimate tail call
/// has an explicit `jmp <oob>` / `je <oob>` opcode, which reaches
/// `is_branch_tail_call_nocheck` via `process_branch` /
/// `process_cond_branch` and classifies correctly.  Sequential
/// fall-through past the bound means the user's `fn_max_size` is too
/// small or the function is unterminated — silently classifying it as
/// a tail call hides the bug (the user-reported `tzcount.o` reproducer
/// surfaces here).  The cfg builder must surface an error with a clear
/// "function-boundary error" / "sequential decoding overflowed"
/// message instead.
#[test]
fn bounded_lift_fall_through_past_fn_max_size_is_function_boundary_error() {
    // Layout:
    //   0x1000..0x1002: `xor eax, eax`              (2 bytes, ≥1 pcode op).
    //   0x1002..0x1008: `lock cmpxchg %r14, 0x58(%rbx)` (multi-pcode-op,
    //                                                  intra-insn CONST
    //                                                  branches).
    const BASE: u64 = 0x1000;
    let mut bs = vec![0x31u8, 0xc0];
    bs.extend_from_slice(&[0xF0, 0x4C, 0x0F, 0xB1, 0x73, 0x58]);

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bs, BASE);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");

    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(2),
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    let err = match run_at(sleigh, BASE, &lift_opts) {
        Ok(_) => panic!(
            "fall-through past fn_max_size must surface as a function-boundary error, not Ok"
        ),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("function-boundary error"),
        "expected function-boundary error message; got {msg}"
    );
    assert!(
        msg.contains("sequential decoding overflowed"),
        "expected overflow detail in error message; got {msg}"
    );
}

/// Finds a `Call` node whose target operand is an `IntConst(target)`.
/// Call input slots per `node_signature`: [control, memory, target, args…];
/// the target sits at slot 2.
fn find_call_to(function: &strider_ir::Function, target: u64) -> Option<strider_ir::node::NodeId> {
    function.walk().find(|&nid| {
        matches!(function.node_kind(nid), NodeKind::Call)
            && function
                .node_inputs(nid)
                .into_iter()
                .nth(2)
                .is_some_and(|target_value| {
                    function.int_const_u128(target_value) == Some(u128::from(target))
                })
    })
}

/// Conditional branch whose taken AND fall-through targets both lie
/// past `start + fn_max_size`.  The conditional must SURVIVE: the cfg
/// builder lowers each OOB arm as a synthetic tail-call stub, so the
/// IR carries an `If` dispatching between two
/// `Call(IntConst(target)) + Return` arms with distinct targets.
/// Each Call's asm fingerprint names the conditional-branch
/// instruction — the insn that proves the call happens.
#[test]
fn bounded_lift_keeps_cond_branch_with_both_targets_oob_as_two_tail_call_arms() {
    // 0x1000: `je 0x1080` (rel8 = +0x7E, both targets OOB at fn_max_size=2).
    //   taken target: 0x1002 + 0x7E = 0x1080.
    //   fall-through: 0x1002 (also OOB at end_exclusive=0x1002).
    // The condition (ZF) is an InitialVar, so no pass can fold the If.
    const BASE: u64 = 0x1000;
    const TAKEN_TARGET: u64 = 0x1080;
    const FALLTHROUGH_TARGET: u64 = 0x1002;
    let bs = vec![0x74u8, 0x7e];

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bs, BASE);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");

    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(2),
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    let function = run_at(sleigh, BASE, &lift_opts)
        .expect("cond-branch with both OOB targets must lift as a conditional tail call");

    assert!(
        function.has_kind(|k| matches!(k, NodeKind::If)),
        "the conditional must survive as an If node"
    );
    let taken_call =
        find_call_to(&function, TAKEN_TARGET).expect("taken arm must carry Call(IntConst(0x1080))");
    let fallthrough_call = find_call_to(&function, FALLTHROUGH_TARGET)
        .expect("fall-through arm must carry Call(IntConst(0x1002))");
    for call in [taken_call, fallthrough_call] {
        assert!(
            function.side_tables().asm_fingerprint(call).contains(&BASE),
            "stub Call fingerprint must name the cond-branch insn at {BASE:#x}; got {:?}",
            function.side_tables().asm_fingerprint(call)
        );
    }
    assert_eq!(
        function.count_kind(|k| matches!(k, NodeKind::Return)),
        2,
        "each tail-call arm carries its own Return"
    );
    strider_ir::validate::validate(&function)
        .expect("lifted conditional-tail-call graph must validate");
}

/// Conditional branch with ONLY the taken target out-of-bounds: the
/// conditional must survive as an `If` whose taken arm is a synthetic
/// tail call (`Call(IntConst(<oob>)) + Return`) and whose fall-through
/// arm is the function's normal in-range `ret`.  Pre-fix the cfg
/// builder silently deleted the conditional, folding the region onto
/// the in-range arm — analysis then believed the branch was never
/// taken.
#[test]
fn bounded_lift_oob_taken_arm_lifts_as_conditional_tail_call() {
    // 0x1000: 85 FF   test edi, edi   (condition depends on the arg reg —
    //                                  no pass can constant-fold the If)
    // 0x1002: 74 7C   je 0x1080       (taken 0x1004+0x7C=0x1080, OOB at
    //                                  fn_max_size=0x10)
    // 0x1004: C3      ret             (in-range fall-through)
    const BASE: u64 = 0x1000;
    const JE_ADDR: u64 = 0x1002;
    const OOB_TARGET: u64 = 0x1080;
    let bs = vec![0x85u8, 0xFF, 0x74, 0x7C, 0xC3];

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bs, BASE);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");

    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(0x10),
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    let function = run_at(sleigh, BASE, &lift_opts)
        .expect("cond-branch with one OOB arm must lift as a conditional tail call");

    assert!(
        function.has_kind(|k| matches!(k, NodeKind::If)),
        "the conditional must survive as an If node"
    );
    let call =
        find_call_to(&function, OOB_TARGET).expect("the OOB arm must carry Call(IntConst(0x1080))");
    assert!(
        function
            .side_tables()
            .asm_fingerprint(call)
            .contains(&JE_ADDR),
        "stub Call fingerprint must name the cond-branch insn at {JE_ADDR:#x}; got {:?}",
        function.side_tables().asm_fingerprint(call)
    );
    assert_eq!(
        function.count_kind(|k| matches!(k, NodeKind::Call)),
        1,
        "only the OOB arm carries a Call"
    );
    assert_eq!(
        function.count_kind(|k| matches!(k, NodeKind::Return)),
        2,
        "one Return on the tail-call arm, one for the in-range ret"
    );
    strider_ir::validate::validate(&function)
        .expect("lifted conditional-tail-call graph must validate");
}
