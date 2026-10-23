//! A `FunctionBound::Size` is the function's exact extent, so a backward `jmp`
//! below `start_addr` is a `RegionTerminator::TailCall` and the lift stays
//! inside `[start_addr, start_addr + size)`.  Following it instead pulls the
//! previous function's body in, which on real binaries surfaces as a live
//! `IndirectBranch` placeholder from a branch in the NEIGHBOUR.
//!
//! The negative half of the invariant: no node in the lifted graph carries an
//! asm-fingerprint address from the previous function's range.
//! `bounded_lift_tail_call.rs` covers the positive shape.

mod common;

use rsleigh::mem_readers::BufMemReader;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::{AnalyzeOptions, Strider};
use strider_target::{CallingConvention, SleighArch};

/// Synthetic layout:
///
/// ```text
/// 0x1000..0x1020:  prev_fn: 32 bytes of `nop`s ending in `ret`.
/// 0x1020..0x102A:  target_fn: `mov eax, 5; jmp 0x1000` (10 bytes).
/// ```
///
/// `jmp 0x1000` from 0x1020+5 (insn after `mov`) = rel32 of
///   `0x1000 - (0x1025 + 5) = 0x1000 - 0x102A = -0x2A = 0xFFFFFFD6 LE`.
const BASE: u64 = 0x1000;
const PREV_FN: u64 = 0x1000;
const PREV_FN_END: u64 = 0x1020;
const TARGET_FN: u64 = 0x1020;
const TARGET_FN_SIZE: u64 = 10;
const TARGET_FN_END: u64 = TARGET_FN + TARGET_FN_SIZE;

fn synthetic_bytes() -> Vec<u8> {
    let mut bs = Vec::new();
    // 0x1000..0x101F: 31 x nop.
    bs.extend(std::iter::repeat_n(0x90u8, 31));
    // 0x101F: ret (prev_fn is a "real" function the cfg builder could
    // plausibly decode if it strayed into this range).
    bs.push(0xC3);
    debug_assert_eq!(bs.len() as u64, PREV_FN_END - PREV_FN);

    // 0x1020: mov eax, 5  (B8 05 00 00 00)
    bs.extend_from_slice(&[0xB8, 0x05, 0x00, 0x00, 0x00]);
    // 0x1025: jmp 0x1000  (E9 D6 FF FF FF)
    bs.extend_from_slice(&[0xE9, 0xD6, 0xFF, 0xFF, 0xFF]);
    debug_assert_eq!(bs.len() as u64, TARGET_FN_END - PREV_FN);

    bs
}

fn make_reader() -> BufMemReader<Vec<u8>> {
    BufMemReader::new(synthetic_bytes(), BASE)
}

/// The cfg builder must not decode any `prev_fn` byte when lifting
/// `target_fn` under a size bound. Proved by walking the lifted graph's
/// asm-fingerprint addresses and asserting every contributor address lies in
/// `[TARGET_FN, TARGET_FN_END)`.
#[test]
fn bounded_lift_does_not_walk_backward_into_prev_fn() {
    let mut strider = Strider::new(SleighArch::x86_64(), make_reader(), None).unwrap();
    let cc = CallingConvention::x86_64_systemv()
        .build(strider.sleigh_regs())
        .expect("build cc");
    let opts = AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            bound: strider_cfg::FunctionBound::size(TARGET_FN_SIZE),
            ..Default::default()
        },
        ..AnalyzeOptions::default()
    };
    let function = strider
        .analyze(TARGET_FN, &cc, &opts, None)
        .expect("bounded lift must complete without reaching prev_fn")
        .function;

    // Filter out the empty-fingerprint kinds (Entry/InitialMemory/
    // InitialVar/Region/MemPhi/Phi; see the asm-fingerprint side-table
    // contract in CLAUDE.md); every remaining address must lie inside
    // target_fn's extent.
    let mut violators: Vec<(u64, &'static str)> = Vec::new();
    for nid in function.walk() {
        for addr in function.side_tables().asm_fingerprint(nid) {
            if !(TARGET_FN..TARGET_FN_END).contains(&addr) {
                let kind_label = match function.node_kind(nid) {
                    strider_ir::node::NodeKind::Call { .. } => "Call",
                    strider_ir::node::NodeKind::Return => "Return",
                    strider_ir::node::NodeKind::IntConst(_) => "IntConst",
                    strider_ir::node::NodeKind::Store(_) => "Store",
                    strider_ir::node::NodeKind::Load(_) => "Load",
                    _ => "other",
                };
                violators.push((addr, kind_label));
            }
        }
    }
    assert!(
        violators.is_empty(),
        "lifted graph contains nodes whose asm-fingerprint references \
         addresses outside target_fn [{TARGET_FN:#x}, {TARGET_FN_END:#x}); \
         violators (addr, kind) = {violators:?}",
    );

    // Sanity floor: an empty graph would satisfy "no violators" vacuously.
    let node_count = function.walk().count();
    assert!(
        node_count >= 3,
        "lifted target_fn graph is suspiciously small ({node_count} nodes); \
         expected at least Entry + Call + Return",
    );
}
