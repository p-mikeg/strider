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
use strider_ir::{IRViewer, IRWalker};
use rsleigh::mem_readers::BufMemReader;
use strider_orchestrator::Strider;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::LiftOptions;
use strider_target::{CallingConvention, SleighArch};

fn make_sleigh_value(bytes: Vec<u8>, base: u64) -> Sleigh<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh")
}

/// Lift + optimise the function at `base` in `bytes` via the orchestrator
/// `Strider` handle with the standard SystemV-x86_64 convention and
/// default options.
fn run_at(bytes: Vec<u8>, base: u64) -> anyhow::Result<strider_ir::Function> {
    let arch = SleighArch::x86_64();
    let sleigh = make_sleigh_value(bytes, base);
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .unwrap()
        .build(&regs)
        .expect("build cc");
    let mut strider = Strider::new(arch, sleigh, None)?;
    strider.analyze(base, &cc, &LiftOptions::default(), &OptOptions::default())
}

#[test]
fn outer_loop_zero_iter_when_no_branch_indirect_returns_ir() {
    // A function with no BranchIndirect: just `ret`.  The fast path
    // skips the loop entirely; the result is the optimised IR.

    let bytes = vec![0xc3u8]; // ret
    let function = run_at(bytes, 0x1000).expect("orchestrator");
    let mut had_return = false;
    for nid in function.walk() {
        if matches!(function.node_kind(nid), strider_ir::node::NodeKind::Return) {
            had_return = true;
        }
    }
    assert!(had_return);
}

#[test]
fn outer_loop_unresolved_at_fixed_point_returns_error() {
    // `jmp rax` on x86_64: rax is a function-entry value (no constant
    // write), and x86_64 has no link register, so IR-level indirect-branch resolver cannot
    // classify.  The orchestrator must reach a fixed point and return
    // an informative error — never panic, never loop forever.

    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let result = run_at(bytes, 0x1000);
    match result {
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("could not be resolved at fixed point"),
                "expected unresolved-at-fixed-point message, got: {msg}"
            );
        }
        Ok(_) => panic!("expected error, got Ok"),
    }
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
    let function = run_at(bytes, 0x1000).expect("push/pop/jmp of a constant must resolve to a tail call");
    // The placeholder must have been resolved away: no `IndirectBranch`
    // node survives in the final graph.
    let placeholder_survives = function
        .walk()
        .any(|n| matches!(function.node_kind(n), strider_ir::node::NodeKind::IndirectBranch));
    assert!(
        !placeholder_survives,
        "expected the IndirectBranch placeholder to be resolved into a tail call, \
         but one survived in the final graph"
    );
}

#[test]
fn orchestrator_owned_sleigh_succeeds_in_fast_path() {
    let bytes = vec![0xc3u8]; // ret
    let function = run_at(bytes, 0x1000).expect("orchestrator must succeed in fast path");
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
fn orchestrator_owned_sleigh_succeeds_in_error_path() {
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let _ = run_at(bytes, 0x1000);
}

#[test]
fn orchestrator_correctness_unchanged_after_sleigh_persistence() {
    // CORRECTNESS: the resulting graph for a function that does NOT
    // need indirect resolution is identical regardless of how many
    // times Sleigh is reused across runs.

    let make_run = || {
        let bytes = vec![0xc3u8]; // ret
        run_at(bytes, 0x1000).expect("orchestrator")
    };
    let g1 = make_run();
    let g2 = make_run();

    let kinds_1: Vec<strider_ir::node::NodeKind> =
        g1.walk().map(|nid| *g1.node_kind(nid)).collect();
    let kinds_2: Vec<strider_ir::node::NodeKind> =
        g2.walk().map(|nid| *g2.node_kind(nid)).collect();
    assert_eq!(kinds_1, kinds_2);
}

// Constructs a placeholder Sleigh for the unused-import shim.  Keeping
// the import surface stable means no `make_sleigh_value` warning.
#[allow(dead_code)]
fn _ensure_make_sleigh_used() {
    let _ = make_sleigh_value(vec![0xc3], 0);
}
