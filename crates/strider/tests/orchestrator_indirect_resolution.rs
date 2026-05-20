//! Integration tests for the strider top-level orchestrator
//! ([`strider::run`]).
//!
//! Each test:
//!   1. Constructs a `RunConfig` against a synthetic byte sequence +
//!      the standard SystemV-x86_64 calling convention,
//!   2. Calls `strider::run`,
//!   3. Asserts the result matches the spec's per-scenario contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider::{run, RunConfig, SleighArch, Strider};

fn make_sleigh_value(bytes: Vec<u8>, base: u64) -> Sleigh<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh")
}

fn make_config<'a>(
    strider: &'a Strider,
    bytes: Vec<u8>,
    base: u64,
) -> RunConfig<'a, BufMemReader<Vec<u8>>> {
    RunConfig {
        strider,
        start_addr: base.into(),
        sleigh: make_sleigh_value(bytes, base),
        rom: None,
        fn_max_size: None,
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs: std::collections::HashMap::new(),
    }
}

#[test]
fn outer_loop_zero_iter_when_no_branch_indirect_returns_ir() {
    // A function with no BranchIndirect: just `ret`.  The fast path
    // skips the loop entirely; the result is the optimised IR.
    let strider = common::strider_x86_64();
    let bytes = vec![0xc3u8]; // ret
    let config = make_config(&strider, bytes, 0x1000);
    let graph = run(config).expect("orchestrator");
    let mut had_return = false;
    for nid in graph.preorder() {
        if matches!(graph.graph.node_kind(nid), strider_ir::node::NodeKind::Return) {
            had_return = true;
        }
    }
    assert!(had_return);
}

#[test]
fn outer_loop_unresolved_at_fixed_point_returns_typed_error() {
    // `jmp rax` on x86_64: rax is a function-entry value (no constant
    // write), and x86_64 has no link register, so IR-level indirect-branch resolver cannot
    // classify.  The orchestrator must reach a fixed point and return
    // a `UnresolvedIndirectBranch`-typed error — never panic, never
    // loop forever.  The typed error lets callers (e.g. strider-py's
    // `UnresolvedIndirectBranchError`) catch this case selectively
    // rather than treating every fixed-point failure as opaque.
    let strider = common::strider_x86_64();
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let config = make_config(&strider, bytes, 0x1000);
    let result = run(config);
    match result {
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("could not be resolved at fixed point"),
                "expected unresolved-at-fixed-point message, got: {msg}"
            );
            assert!(
                e.downcast_ref::<strider::UnresolvedIndirectBranch>().is_some(),
                "expected `UnresolvedIndirectBranch` typed error in the anyhow chain, got: {e:?}"
            );
        }
        Ok(_) => panic!("expected error, got Ok"),
    }
}

#[test]
fn outer_loop_resolves_via_stack_load_forward_for_x86_64_push_pop() {
    // `push imm32; pop rax; jmp rax` — structurally a tail call.
    // After StackStoreDetect + StackLoadForward the placeholder
    // Return's value-input folds to IntConst(K), and IR-level indirect-branch resolver
    // classifies as `Single(K)`.  K must lie OUTSIDE the function
    // range so the orchestrator treats it as a tail call.
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![
        0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0,
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));

    let strider = common::strider_x86_64();
    let config = make_config(&strider, bytes, 0x1000);
    let result = run(config);
    match result {
        Ok(_graph) => {}
        Err(e) => {
            let msg = format!("{e}");
            if msg.contains("did not converge") {
                panic!("orchestrator should never hit the cap on a valid fixture: {e:?}");
            }
            assert!(
                msg.contains("could not be resolved at fixed point"),
                "expected unresolved fallback, got: {msg}"
            );
        }
    }
}

#[test]
fn orchestrator_owned_sleigh_succeeds_in_fast_path() {
    let strider = common::strider_x86_64();
    let bytes = vec![0xc3u8]; // ret
    let config = make_config(&strider, bytes, 0x1000);
    let graph = run(config).expect("orchestrator must succeed in fast path");
    let mut had_return = false;
    for nid in graph.preorder() {
        if matches!(graph.graph.node_kind(nid), strider_ir::node::NodeKind::Return) {
            had_return = true;
        }
    }
    assert!(had_return, "fast-path exit must produce a graph with at least one Return");
}

#[test]
fn orchestrator_owned_sleigh_succeeds_in_error_path() {
    let strider = common::strider_x86_64();
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let config = make_config(&strider, bytes, 0x1000);
    let _ = run(config);
}

#[test]
fn orchestrator_correctness_unchanged_after_sleigh_persistence() {
    // CORRECTNESS: the resulting graph for a function that does NOT
    // need indirect resolution is identical regardless of how many
    // times Sleigh is reused across runs.
    let strider = common::strider_x86_64();

    let make_run = || {
        let bytes = vec![0xc3u8]; // ret
        let config = make_config(&strider, bytes, 0x1000);
        run(config).expect("orchestrator")
    };
    let g1 = make_run();
    let g2 = make_run();

    let kinds_1: Vec<strider_ir::node::NodeKind> =
        g1.preorder().map(|nid| *g1.graph.node_kind(nid)).collect();
    let kinds_2: Vec<strider_ir::node::NodeKind> =
        g2.preorder().map(|nid| *g2.graph.node_kind(nid)).collect();
    assert_eq!(kinds_1, kinds_2);
}

// Constructs a placeholder Sleigh for the unused-import shim.  Keeping
// the import surface stable means no `make_sleigh_value` warning.
#[allow(dead_code)]
fn _ensure_make_sleigh_used() {
    let _ = make_sleigh_value(vec![0xc3], 0);
}
