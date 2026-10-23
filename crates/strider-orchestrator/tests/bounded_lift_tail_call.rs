//! With the size bound set, the cfg builder classifies any direct `jmp` whose
//! target lies outside `[start, start+size)` as
//! `RegionTerminator::TailCall { target }` (no successor edge), which the IR
//! layer lowers as `Call(IntConst(target)) + Return`.  Routing the trailing
//! `Opcode::Branch` through the generic `handle_branch` path instead errors
//! with "invalid region index N": a TailCall region has no Unconditional edge.
//!
//! Synthetic x86_64 function:
//!
//! ```text
//! 0x1000:  B8 05 00 00 00     mov eax, 5
//! 0x1005:  E9 F6 7F 00 00     jmp 0x9000        ; out-of-fn tail call
//! ```

mod common;

use rsleigh::mem_readers::BufMemReader;
use strider_ir::IRWalker;
use strider_ir::node::NodeKind;
use strider_ir_test_utils::IrWalkerEx;
use strider_orchestrator::{AnalyzeOptions, Strider};
use strider_target::{CallingConvention, SleighArch};

const BASE: u64 = 0x1000;
const TAIL_TARGET: u64 = 0x9000;

/// The function at `entry` under x86_64 SystemV and `opts`.
fn run_at(
    reader: BufMemReader<Vec<u8>>,
    entry: u64,
    opts: &AnalyzeOptions,
) -> anyhow::Result<strider_ir::Function> {
    let mut strider = Strider::new(SleighArch::x86_64(), reader, None)?;
    let cc = CallingConvention::x86_64_systemv()
        .build(strider.sleigh_regs())
        .expect("build cc");
    strider.analyze(entry, &cc, opts, None).map(|r| r.function)
}

/// `mov eax, 5; jmp 0x9000` at 0x1000 (10 bytes).  `jmp` target is
/// 0x9000 (rel32 = 0x9000 - 0x100A = 0x7FF6 = `F6 7F 00 00` LE).
fn synthetic_bytes() -> Vec<u8> {
    vec![0xB8, 0x05, 0x00, 0x00, 0x00, 0xE9, 0xF6, 0x7F, 0x00, 0x00]
}

fn make_reader() -> BufMemReader<Vec<u8>> {
    BufMemReader::new(synthetic_bytes(), BASE)
}

#[test]
fn bounded_lift_handles_tail_call_terminator() {
    let opts = AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            bound: strider_cfg::FunctionBound::size(10),
            ..Default::default()
        },
        ..AnalyzeOptions::default()
    };
    let function =
        run_at(make_reader(), BASE, &opts).expect("orchestrator must lift TailCall as Call+Return");

    assert!(
        common::find_call_to(&function, TAIL_TARGET).is_some(),
        "expected a Call(IntConst({TAIL_TARGET:#x})) node from the lifted tail call"
    );
    assert!(
        function.has_kind(|k| matches!(k, NodeKind::Return)),
        "expected a Return node downstream of the tail-call Call"
    );
}

/// Synthetic vmspace_exitfree shape: a small function ending with a backward
/// `jmp` whose target is a *different* function (below `start_addr`).
/// A `FunctionBound::Size` is the function's exact extent, so the backward
/// target is a tail call, and the IR carries
/// `Call(IntConst(<backward_target>)) + Return`. Following the jmp into the
/// adjacent bytes instead balloons the graph to tens of thousands of nodes.
#[test]
fn bounded_lift_backward_jmp_with_size_bound_classifies_as_tail_call() {
    // 0x1000..0x1080: NOP padding (the "previous function").
    // 0x1080..0x108A: our function: `mov eax, 5; jmp 0x1000`.
    // jmp 0x1000 from 0x1080+5 (insn after `mov`) = rel32 of
    //   0x1000 - (0x1085 + 5) = 0x1000 - 0x108A = -0x8A = 0xFFFFFF76 LE.
    const BASE: u64 = 0x1000;
    const FN_START: u64 = 0x1080;
    const TAIL_TARGET: u64 = 0x1000;
    let mut bs = vec![0x90u8; 0x80]; // 0x1000..0x1080: padding
    bs.extend_from_slice(&[0xB8, 0x05, 0x00, 0x00, 0x00]); // mov eax, 5
    bs.extend_from_slice(&[0xE9, 0x76, 0xFF, 0xFF, 0xFF]); // jmp -0x8A to 0x1000

    let reader = BufMemReader::new(bs, BASE);

    let opts = AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            bound: strider_cfg::FunctionBound::size(10),
            ..Default::default()
        },
        ..AnalyzeOptions::default()
    };
    let function = run_at(reader, FN_START, &opts)
        .expect("backward jmp under a size bound must classify as tail call");

    assert!(
        common::find_call_to(&function, TAIL_TARGET).is_some()
            && function.has_kind(|k| matches!(k, NodeKind::Return)),
        "expected Call(IntConst({TAIL_TARGET:#x})) + Return from the backward-jmp tail call"
    );
    // A 10-byte function tail-calling out stays tight.
    let node_count = function.walk().count();
    assert!(
        node_count < 200,
        "lifted graph should be tight (~tens of nodes); got {node_count}",
    );
}

/// A fall-through past `start + size` is a tail call to the end, reported
/// as `PastFunctionEnd`, and `analyze` returns the function as usual.
#[test]
fn bounded_lift_fall_through_past_size_bound_tail_calls_the_end() {
    // 0x1000..0x1002: `xor eax, eax`, then a `lock cmpxchg %r14, 0x58(%rbx)`
    // past the bound.
    let mut bs = vec![0x31u8, 0xc0];
    bs.extend_from_slice(&[0xF0, 0x4C, 0x0F, 0xB1, 0x73, 0x58]);

    let mut strider = Strider::new(SleighArch::x86_64(), BufMemReader::new(bs, BASE), None)
        .expect("Strider::new");
    let cc = CallingConvention::x86_64_systemv()
        .build(strider.sleigh_regs())
        .expect("build cc");
    let opts = AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            bound: strider_cfg::FunctionBound::size(2),
            ..Default::default()
        },
        ..AnalyzeOptions::default()
    };
    let result = strider
        .analyze(BASE, &cc, &opts, None)
        .expect("a fall-through past the bound is no error");

    assert_eq!(
        result.incomplete().iter().collect::<Vec<_>>(),
        [(
            strider_cfg::PcodeInsnAddr::at_machine_start(0x1002),
            strider_orchestrator::Reason::PastFunctionEnd
        )]
    );
    assert!(
        common::find_call_to(&result.function, 0x1002).is_some()
            && result.function.has_kind(|k| matches!(k, NodeKind::Return)),
        "expected Call(IntConst(0x1002)) + Return from the fall-through"
    );
}

/// Conditional branch whose taken AND fall-through targets both lie past
/// `start + size`. The conditional must SURVIVE: the cfg builder
/// lowers each OOB arm as a synthetic tail-call stub, so the IR carries
/// an `If` dispatching between two `Call(IntConst(target)) + Return` arms
/// with distinct targets. Each Call's asm fingerprint names the
/// conditional-branch instruction that proves the call happens.
#[test]
fn bounded_lift_keeps_cond_branch_with_both_targets_oob_as_two_tail_call_arms() {
    // 0x1000: `je 0x1080` (rel8 = +0x7E, both targets OOB at a size bound of 2).
    //   taken target: 0x1002 + 0x7E = 0x1080.
    //   fall-through: 0x1002 (also OOB at end_exclusive=0x1002).
    // The condition (ZF) is an InitialVar, so no pass can fold the If.
    const BASE: u64 = 0x1000;
    const TAKEN_TARGET: u64 = 0x1080;
    const FALLTHROUGH_TARGET: u64 = 0x1002;
    let bs = vec![0x74u8, 0x7e];

    let reader = BufMemReader::new(bs, BASE);

    let opts = AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            bound: strider_cfg::FunctionBound::size(2),
            ..Default::default()
        },
        ..AnalyzeOptions::default()
    };
    let function = run_at(reader, BASE, &opts)
        .expect("cond-branch with both OOB targets must lift as a conditional tail call");

    assert!(
        function.has_kind(|k| matches!(k, NodeKind::If)),
        "the conditional must survive as an If node"
    );
    let taken_call = common::find_call_to(&function, TAKEN_TARGET)
        .expect("taken arm must carry Call(IntConst(0x1080))");
    let fallthrough_call = common::find_call_to(&function, FALLTHROUGH_TARGET)
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
/// conditional must survive as an `If` whose taken arm is a synthetic tail
/// call (`Call(IntConst(<oob>)) + Return`) and whose fall-through arm is the
/// function's normal in-range `ret`. Dropping the conditional and folding the
/// region onto the in-range arm would present the branch as never taken.
#[test]
fn bounded_lift_oob_taken_arm_lifts_as_conditional_tail_call() {
    // 0x1000: 85 FF   test edi, edi   (condition depends on the arg reg,
    //                                  no pass can constant-fold the If)
    // 0x1002: 74 7C   je 0x1080       (taken 0x1004+0x7C=0x1080, OOB at
    //                                  a size bound of 0x10)
    // 0x1004: C3      ret             (in-range fall-through)
    const BASE: u64 = 0x1000;
    const JE_ADDR: u64 = 0x1002;
    const OOB_TARGET: u64 = 0x1080;
    let bs = vec![0x85u8, 0xFF, 0x74, 0x7C, 0xC3];

    let reader = BufMemReader::new(bs, BASE);

    let opts = AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            bound: strider_cfg::FunctionBound::size(0x10),
            ..Default::default()
        },
        ..AnalyzeOptions::default()
    };
    let function = run_at(reader, BASE, &opts)
        .expect("cond-branch with one OOB arm must lift as a conditional tail call");

    assert!(
        function.has_kind(|k| matches!(k, NodeKind::If)),
        "the conditional must survive as an If node"
    );
    let call = common::find_call_to(&function, OOB_TARGET)
        .expect("the OOB arm must carry Call(IntConst(0x1080))");
    assert!(
        function
            .side_tables()
            .asm_fingerprint(call)
            .contains(&JE_ADDR),
        "stub Call fingerprint must name the cond-branch insn at {JE_ADDR:#x}; got {:?}",
        function.side_tables().asm_fingerprint(call)
    );
    assert_eq!(
        function.count_kind(|k| matches!(k, NodeKind::Call { .. })),
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
