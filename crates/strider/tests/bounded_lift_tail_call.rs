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
//! Branch / Fallthrough edge.
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

use ir::node::NodeKind;
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider::{run, RunConfig, SleighArch};

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
    Sleigh::new(arch.sla_spec, arch.pspec, reader).expect("Sleigh::new")
}

#[test]
fn bounded_lift_handles_tail_call_terminator() {
    let strider = common::strider_x86_64();
    let config = RunConfig {
        strider: &strider,
        start_addr: BASE,
        sleigh: make_sleigh(),
        rom: None,
        fn_max_size: Some(10),
        allow_code_before_start_addr: false,
    };
    let graph = run(config).expect("orchestrator must lift TailCall as Call+Return");

    // Post-condition: the graph contains a `Call` whose target operand
    // is an `IntConst(0x9000)`, and a `Return` node downstream.
    let mut had_call_with_target = false;
    let mut had_return = false;
    for nid in graph.preorder() {
        match graph.graph.node_kind(nid) {
            NodeKind::Call => {
                // Call inputs: [ctrl, mem, target, args...].  Slot 2 is the target.
                let inputs: Vec<_> = graph.graph.node_inputs(nid).into_iter().collect();
                if let Some(&target_out) = inputs.get(2)
                    && let NodeKind::IntConst(v) =
                        *graph.graph.node_kind(graph.graph.get_node_from_output(target_out))
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
