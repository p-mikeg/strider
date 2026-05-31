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

use strider_ir::node::NodeKind;
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_analyze::{run, Config};
use strider_target::SleighArch;

const BASE: u64 = 0x1000;
const TAIL_TARGET: u64 = 0x9000;

/// `mov eax, 5; jmp 0x9000` at 0x1000 (10 bytes).  `jmp` target is
/// 0x9000 (rel32 = 0x9000 - 0x100A = 0x7FF6 = `F6 7F 00 00` LE).
fn synthetic_bytes() -> Vec<u8> {
    let mut bs = vec![0xB8, 0x05, 0x00, 0x00, 0x00, 0xE9, 0xF6, 0x7F, 0x00, 0x00];
    // Pad to a few extra bytes of NOPs so any over-read past the jmp
    // (e.g. the orchestrator probing the next address) finds valid
    // memory rather than a Sleigh decode error that would mask the
    // real bug.
    bs.extend(std::iter::repeat_n(0x90u8, 32));
    bs
}

fn make_sleigh() -> Sleigh<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(synthetic_bytes(), BASE);
    Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new")
}

#[test]
fn bounded_lift_handles_tail_call_terminator() {
    let strider = common::strider_x86_64();
    let config = Config {
        strider: &strider,
        start_addr: BASE.into(),
        sleigh: make_sleigh(),
        rom: None,
        fn_max_size: Some(10),
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs_unbuilt: rustc_hash::FxHashMap::default(),
    };
    let function = run(config).expect("orchestrator must lift TailCall as Call+Return");

    // Post-condition: the graph contains a `Call` whose target operand
    // is an `IntConst(0x9000)`, and a `Return` node downstream.
    let mut had_call_with_target = false;
    let mut had_return = false;
    for nid in function.walk() {
        match function.node_kind(nid) {
            NodeKind::Call => {
                // Call inputs: [ctrl, mem, target, args...].  Slot 2 is the target.
                let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
                if let Some(&target_out) = inputs.get(2)
                    && let NodeKind::IntConst(v) =
                        *function.node_kind(function.node_for_output(target_out))
                    && (v as u64) == TAIL_TARGET
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
                if let Some(&target_out) = inputs.get(2)
                    && let NodeKind::IntConst(v) =
                        *function.node_kind(function.node_for_output(target_out))
                    && (v as u64) == target
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
    //   0x108A..      : NOP padding (over-read safety margin).
    //
    // jmp 0x1000 from 0x1080+5 (insn after `mov`) = rel32 of
    //   0x1000 - (0x1085 + 5) = 0x1000 - 0x108A = -0x8A = 0xFFFFFF76 LE.
    const BASE: u64 = 0x1000;
    const FN_START: u64 = 0x1080;
    const TAIL_TARGET: u64 = 0x1000;
    let mut bs = vec![0x90u8; 0x80]; // 0x1000..0x1080: padding
    bs.extend_from_slice(&[0xB8, 0x05, 0x00, 0x00, 0x00]); // mov eax, 5
    bs.extend_from_slice(&[0xE9, 0x76, 0xFF, 0xFF, 0xFF]); // jmp -0x8A → 0x1000
    bs.extend(std::iter::repeat_n(0x90u8, 32));            // post-fn padding

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bs, BASE);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");

    let strider = common::strider_x86_64();
    let function = run(Config {
        strider: &strider,
        start_addr: FN_START.into(),
        sleigh,
        rom: None,
        fn_max_size: Some(10),
        allow_code_before_start_addr: true,
        compact: true,
        per_address_ccs_unbuilt: rustc_hash::FxHashMap::default(),
    })
    .expect("backward jmp + fn_max_size must classify as tail call regardless of reach-back flag");

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

/// Synthetic dounmount-shape: a function whose body has no explicit
/// terminator inside the bound and whose fall-through crosses
/// `start + fn_max_size` into a multi-pcode-op machine instruction
/// (e.g. `lock cmpxchg`).  Pre-fix the lifter kept fall-through-
/// decoding past the bound and eventually surfaced "invalid tail call
/// at opcode ..." when the OOB instruction's CONST-arm `Branch`
/// produced a non-zero `insn_index` paired with an OOB `machine_addr`.
/// Post-fix, `RegionBuilder::build` truncates at the bound and the IR
/// carries `Call(IntConst(<oob_addr>)) + Return`.
#[test]
fn bounded_lift_truncates_fall_through_past_fn_max_size() {
    // Layout:
    //   0x1000..0x1002: `xor eax, eax`              (2 bytes, ≥1 pcode op).
    //   0x1002..0x1008: `lock cmpxchg %r14, 0x58(%rbx)` (multi-pcode-op,
    //                                                  intra-insn CONST
    //                                                  branches).
    //   0x1008..      : NOP padding.
    const BASE: u64 = 0x1000;
    const TAIL_TARGET: u64 = 0x1002;
    let mut bs = vec![0x31u8, 0xc0];
    bs.extend_from_slice(&[0xF0, 0x4C, 0x0F, 0xB1, 0x73, 0x58]);
    bs.extend(std::iter::repeat_n(0x90u8, 32));

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bs, BASE);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");

    let strider = common::strider_x86_64();
    let function = run(Config {
        strider: &strider,
        start_addr: BASE.into(),
        sleigh,
        rom: None,
        fn_max_size: Some(2),
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs_unbuilt: rustc_hash::FxHashMap::default(),
    })
    .expect("fall-through past fn_max_size must truncate cleanly, not crash on OOB lift");

    assert!(
        graph_has_tail_call_to(&function, TAIL_TARGET),
        "expected Call(IntConst({:#x})) + Return from the fall-through truncation",
        TAIL_TARGET
    );
}

/// Synthetic CondBranch-with-OOB-target: a function ending in a
/// conditional jump whose taken AND fall-through targets both lie
/// past `start + fn_max_size`.  Pre-fix the cfg builder enqueued both
/// OOB addresses onto the work queue and the worker either crashed
/// during the OOB lift or fell through OOB indefinitely on
/// zero-pcode-op padding.  Post-fix the cfg builder pre-classifies
/// both successors and collapses the region to a single `TailCall`
/// when both leave the function — the IR lifts it as
/// `Call(IntConst(<taken_target>)) + Return`.
#[test]
fn bounded_lift_collapses_cond_branch_with_both_targets_oob_to_tail_call() {
    // 0x1000: `je 0x1080` (rel8 = +0x7E, both targets OOB at fn_max_size=2).
    //   taken target: 0x1002 + 0x7E = 0x1080.
    //   fall-through: 0x1002 (also OOB at end_exclusive=0x1002).
    const BASE: u64 = 0x1000;
    const TAKEN_TARGET: u64 = 0x1080;
    let mut bs = vec![0x74u8, 0x7e];
    bs.extend(std::iter::repeat_n(0x90u8, 256));

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bs, BASE);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");

    let strider = common::strider_x86_64();
    let function = run(Config {
        strider: &strider,
        start_addr: BASE.into(),
        sleigh,
        rom: None,
        fn_max_size: Some(2),
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs_unbuilt: rustc_hash::FxHashMap::default(),
    })
    .expect("cond-branch with both OOB targets must collapse to TailCall, not crash");

    assert!(
        graph_has_tail_call_to(&function, TAKEN_TARGET),
        "expected Call(IntConst({:#x})) + Return from the collapsed cond-branch tail call",
        TAKEN_TARGET
    );
}
