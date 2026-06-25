//! End-to-end test for `Config::compact`.
//!
//! Drives `strider_orchestrator::Strider::analyze` on a small inline-byte function under both
//! compact=true and compact=false; asserts the compact graph has no
//! more node ids than the non-compact graph AND identical
//! pattern-match counts on a representative query.

#![allow(clippy::unwrap_used)]

use rsleigh::mem_readers::BufMemReader;
use strider_orchestrator::{LiftOptions, Strider, opt::OptOptions};
use strider_target::{CallingConvention, SleighArch};

mod common;

/// Minimal x86_64 function: `mov rax, 42; ret`.
fn x86_64_call_then_ret_bytes() -> (Vec<u8>, u64) {
    // 48 c7 c0 2a 00 00 00     mov rax, 42
    // c3                        ret
    let bytes = vec![0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3];
    let entry = 0x1000;
    (bytes, entry)
}

fn run_with(compact: bool) -> strider_ir::Function {
    let (bytes, entry) = x86_64_call_then_ret_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).unwrap();
    let regs = sleigh.regs().unwrap();
    let cc = CallingConvention::x86_64_systemv()
        .unwrap()
        .build(&regs)
        .unwrap();
    let lift_opts = LiftOptions {
        compact,
        ..LiftOptions::default()
    };
    let mut strider = Strider::new(arch, sleigh, None).unwrap();
    strider
        .analyze(entry, &cc, &lift_opts, &OptOptions::default(), None)
        .unwrap()
        .function
}

#[test]
fn compact_yields_no_more_node_ids_than_non_compact() {
    let compact_function = run_with(true);
    let noncompact_function = run_with(false);
    let compact_count = compact_function.graph().all_node_ids().count();
    let noncompact_count = noncompact_function.graph().all_node_ids().count();
    assert!(
        compact_count <= noncompact_count,
        "compact={compact_count} must not exceed non-compact={noncompact_count}"
    );
}

#[test]
fn compact_preserves_reachable_pattern_matches() {
    use strider_pattern::{Matcher, ret};

    let compact_function = run_with(true);
    let noncompact_function = run_with(false);

    let pat = ret().build();
    let compact_matches = Matcher::try_new(&compact_function)
        .unwrap()
        .find_all(&pat)
        .unwrap()
        .len();
    let noncompact_matches = Matcher::try_new(&noncompact_function)
        .unwrap()
        .find_all(&pat)
        .unwrap()
        .len();
    assert_eq!(
        compact_matches, noncompact_matches,
        "ret() match count must be invariant under compaction"
    );
}
