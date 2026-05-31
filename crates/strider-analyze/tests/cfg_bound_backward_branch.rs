//! Regression: `function_max_size` must define the function's exact
//! extent, so the cfg builder never reaches back into an adjacent
//! function via a backward direct branch — *even with*
//! `allow_code_before_start_addr=true`.
//!
//! Pre-fix behaviour: with `allow_code_before_start_addr=true` and
//! `function_max_size` set, the cfg builder followed a backward `jmp`
//! whose target lay below `start_addr` into the previous function's
//! body.  On real binaries this surfaced as
//! `UnresolvedIndirectBranchError` from an indirect branch inside the
//! *neighbouring* function — completely unrelated to the function the
//! user asked to lift.
//!
//! Post-fix: `is_addr_tail_call` honours `fn_max_size` as the exact
//! extent regardless of the legacy reach-back flag, so the backward
//! `jmp` is classified as a `RegionTerminator::TailCall` and the lift
//! stays inside `[start_addr, start_addr + fn_max_size)`.
//!
//! The companion test in `bounded_lift_tail_call.rs` asserts the
//! *positive* shape (the backward jmp becomes a `Call + Return`); this
//! test asserts the *negative* invariant — no node in the lifted
//! graph carries an asm-fingerprint address from the previous
//! function's range, i.e. the cfg builder really didn't decode any
//! `prev_fn` instructions.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

mod common;

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_analyze::{run, RunConfig, RunOptions};
use strider_target::{CallingConvention, SleighArch};

/// Synthetic layout:
///
/// ```text
/// 0x1000..0x1020:  prev_fn — 32 bytes of `nop`s ending in `ret`.
/// 0x1020..0x102A:  target_fn — `mov eax, 5; jmp 0x1000` (10 bytes).
/// 0x102A..      :  trailing `nop` padding (Sleigh over-read safety).
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
    // 0x1000..0x101F: 31 × nop.
    bs.extend(std::iter::repeat_n(0x90u8, 31));
    // 0x101F: ret (so prev_fn is a "real" function the cfg builder
    // could plausibly decode if it strayed into this range).
    bs.push(0xC3);
    debug_assert_eq!(bs.len() as u64, PREV_FN_END - PREV_FN);

    // 0x1020: mov eax, 5  (B8 05 00 00 00)
    bs.extend_from_slice(&[0xB8, 0x05, 0x00, 0x00, 0x00]);
    // 0x1025: jmp 0x1000  (E9 D6 FF FF FF)
    bs.extend_from_slice(&[0xE9, 0xD6, 0xFF, 0xFF, 0xFF]);
    debug_assert_eq!(bs.len() as u64, TARGET_FN_END - PREV_FN);

    // Post-fn padding so any Sleigh over-read past the jmp finds
    // valid memory rather than a decode error masking the real bug.
    bs.extend(std::iter::repeat_n(0x90u8, 32));
    bs
}

fn make_sleigh() -> Sleigh<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(synthetic_bytes(), BASE);
    Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new")
}

/// The cfg builder must not decode any `prev_fn` byte when lifting
/// `target_fn` under `function_max_size`, even with the reach-back
/// flag on.  We prove this by walking the lifted graph's
/// asm-fingerprint addresses and asserting every contributor address
/// lies in `[TARGET_FN, TARGET_FN_END)`.
#[test]
fn bounded_lift_does_not_walk_backward_into_prev_fn() {
    // The bug only surfaced with the reach-back flag ON — without
    // it the lower-bound check alone would have blocked the
    // backward jmp.  This pin is what guards against regression
    // of the `allow_code_before_start_addr && fn_max_size` combo.
    let config = RunConfig::new(
        SleighArch::x86_64(),
        CallingConvention::x86_64_systemv().unwrap(),
        make_sleigh(),
        TARGET_FN.into(),
        RunOptions::new()
            .fn_max_size(TARGET_FN_SIZE)
            .allow_code_before_start_addr(),
    )
    .unwrap();
    let function = run(config)
        .expect("bounded lift with reach-back flag must complete without reaching prev_fn");

    // Walk every reachable node and collect every asm-fingerprint
    // contributor address.  Filter out the empty-fingerprint nodes
    // (Entry/InitialMemory/InitialVar/Region/MemPhi/Phi — see the
    // "Asm-fingerprint side-table" contract in CLAUDE.md).  Every
    // remaining address MUST lie inside `target_fn`'s extent.
    let mut violators: Vec<(u64, &'static str)> = Vec::new();
    for nid in function.walk() {
        for &addr in function.asm_fingerprint(nid) {
            if !(TARGET_FN..TARGET_FN_END).contains(&addr) {
                let kind_label = match function.node_kind(nid) {
                    strider_ir::node::NodeKind::Call => "Call",
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

    // Sanity floor: the lift must have produced a non-trivial graph
    // (Entry + Call + Return at minimum) — an empty graph would
    // satisfy the "no violators" check vacuously.
    let node_count = function.walk().count();
    assert!(
        node_count >= 3,
        "lifted target_fn graph is suspiciously small ({node_count} nodes); \
         expected at least Entry + Call + Return",
    );
}
