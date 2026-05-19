//! Phase 3 Task 3.7a parity test — `CallStackArgCollect` v1 vs
//! `CallStackArgCollectEgg` v2.
//!
//! `CallStackArgCollect` is a memory-chain post-pass: it walks
//! backward from each `Call`'s memory input through `StackStore` /
//! `Store` nodes, matches each store's offset against the calling
//! convention's `stack_arg_offsets`, and appends the discovered data
//! outputs as positional `Call` inputs.
//!
//! Memory chains are excluded from the egraph's value slice
//! by construction, so v2 is a faithful direct port of v1.  See
//! `crates/strider-analyze/src/opt/call_stack_arg_collect_egg.rs` for
//! the rationale.  Both passes MUST produce structurally identical
//! Call inputs for every supported shape.
//!
//! Each test builds a fixture, runs v1 on one copy and v2 on a fresh
//! copy, and asserts both produce the same Call-input shape — i.e.
//! the same number of inputs and the same `NodeKind` for each
//! appended positional arg.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{
    CallStackArgCollect, ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect,
    call_stack_arg_collect_egg::CallStackArgCollectEgg,
};
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use strider_ir::test_utils::sp_vn_x86_64 as sp_vn;
use strider_ir::{BuiltFunctionGraph, IntBinaryOp};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn find_call(fg: &BuiltFunctionGraph) -> NodeId {
    let calls: Vec<NodeId> = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Call))
        .collect();
    assert_eq!(calls.len(), 1, "fixture must have exactly one Call node");
    calls[0]
}

/// Summarises the Call-input shape: number of inputs and the
/// `NodeKind` of each input's producer (for the appended-arg slots).
#[derive(Debug, PartialEq, Eq)]
struct CallShape {
    input_count: usize,
    arg_kinds: Vec<NodeKind>,
}

fn summarise_call(fg: &BuiltFunctionGraph) -> CallShape {
    let call_id = find_call(fg);
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    // Skip ctrl + memory + target (first 3 inputs); the rest are
    // positional stack args.
    let arg_kinds: Vec<NodeKind> = inputs
        .iter()
        .skip(3)
        .map(|&out| *fg.kind_of_output(out))
        .collect();
    CallShape {
        input_count: inputs.len(),
        arg_kinds,
    }
}

fn run_v1<F>(sp: rsleigh::Vn, stack_arg_offsets: Vec<i64>, build: F) -> BuiltFunctionGraph
where
    F: FnOnce() -> anyhow::Result<BuiltFunctionGraph>,
{
    let mut fg = build().expect("build v1 fixture");
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(stack_arg_offsets, sp));
    pipeline
        .run(&mut fg.graph, fg.entry)
        .expect("v1 pipeline run");
    fg
}

fn run_v2<F>(sp: rsleigh::Vn, stack_arg_offsets: Vec<i64>, build: F) -> BuiltFunctionGraph
where
    F: FnOnce() -> anyhow::Result<BuiltFunctionGraph>,
{
    let mut fg = build().expect("build v2 fixture");
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollectEgg::new(stack_arg_offsets, sp));
    pipeline
        .run(&mut fg.graph, fg.entry)
        .expect("v2 pipeline run");
    fg
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// cdecl-style: `push arg1=22; push arg0=11; call target(0x1000)`.
/// After optimization the Call's inputs should be extended with
/// `[arg0, arg1]` in positional order — and v1/v2 must match.
#[test]
fn parity_cdecl_two_stack_args_collected_in_order() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
            // push arg1 (= 22) at sp - 4
            let four = b.build_int_const(4u64, NodeOutputType::U64)?;
            let sp_v1 = b.build_int_sub(sp_v0, four, NodeOutputType::U64)?;
            b.write_variable(&sp, sp_v1)?;
            let arg1 = b.build_int_const(22u64, NodeOutputType::U64)?;
            b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

            // push arg0 (= 11) at sp - 8
            let sp_v2 = b.build_int_sub(sp_v1, four, NodeOutputType::U64)?;
            b.write_variable(&sp, sp_v2)?;
            let arg0 = b.build_int_const(11u64, NodeOutputType::U64)?;
            b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

            // call 0x1000
            let target = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
            b.build_call(target)?;
            b.build_return(None, &[])?;
            Ok(())
        })
    };
    let fg_v1 = run_v1(sp, vec![0, 4, 8, 12], build);
    let fg_v2 = run_v2(sp, vec![0, 4, 8, 12], build);
    let v1 = summarise_call(&fg_v1);
    let v2 = summarise_call(&fg_v2);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
    // Sanity: both should have collected two args (Call had ctrl+mem+target+2args = 5 inputs).
    assert_eq!(v1.input_count, 5, "expected two args collected, got {v1:?}");
}

/// One store at the anchor offset (slot 0 under an AArch64-style table).
/// v1/v2 should agree on appending exactly one positional arg.
#[test]
fn parity_single_arg_collected() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
            let four = b.build_int_const(4u64, NodeOutputType::U64)?;
            let sp_v1 = b.build_int_sub(sp_v0, four, NodeOutputType::U64)?;
            b.write_variable(&sp, sp_v1)?;
            let only_arg = b.build_int_const(99u64, NodeOutputType::U64)?;
            b.build_store(sp_v1, only_arg, rsleigh::VnSpace::RAM)?;

            let target = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
            b.build_call(target)?;
            b.build_return(None, &[])?;
            Ok(())
        })
    };
    let fg_v1 = run_v1(sp, vec![0, 4], build);
    let fg_v2 = run_v2(sp, vec![0, 4], build);
    assert_eq!(summarise_call(&fg_v1), summarise_call(&fg_v2));
}

/// Slot 1 is filled but slot 0 is empty — `dense_prefix` truncates at
/// the first `None`, so zero args are appended.  v1/v2 must agree.
#[test]
fn parity_missing_slot_zero_skips_collection() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
            let four = b.build_int_const(4u64, NodeOutputType::U64)?;

            // arg1 at sp + 4
            let sp_plus_4 = b.build_int_binary_operation(
                sp_v0,
                four,
                IntBinaryOp::Add,
                NodeOutputType::U64,
            )?;
            let arg1 = b.build_int_const(22u64, NodeOutputType::U64)?;
            b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

            // Implicit `call` ret-addr push at sp - 4 — chain anchor.
            let sp_minus_4 = b.build_int_sub(sp_v0, four, NodeOutputType::U64)?;
            b.write_variable(&sp, sp_minus_4)?;
            let retaddr = b.build_int_const(0x1234u64, NodeOutputType::U64)?;
            b.build_store(sp_minus_4, retaddr, rsleigh::VnSpace::RAM)?;

            let target = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
            b.build_call(target)?;
            b.build_return(None, &[])?;
            Ok(())
        })
    };
    let fg_v1 = run_v1(sp, vec![4, 8], build);
    let fg_v2 = run_v2(sp, vec![4, 8], build);
    assert_eq!(summarise_call(&fg_v1), summarise_call(&fg_v2));
}

/// A call with no stack stores before it — neither v1 nor v2 should
/// add any inputs.
#[test]
fn parity_call_with_no_stack_stores_unchanged() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, _sp_val| {
            let target = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
            b.build_call(target)?;
            b.build_return(None, &[])?;
            Ok(())
        })
    };
    let fg_v1 = run_v1(sp, vec![0, 4, 8], build);
    let fg_v2 = run_v2(sp, vec![0, 4, 8], build);
    assert_eq!(summarise_call(&fg_v1), summarise_call(&fg_v2));
}

/// Empty `stack_arg_offsets` table (register-only convention) — both
/// passes should be no-ops.
#[test]
fn parity_empty_stack_arg_offsets_noop() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
            // Push something, then call — but with no stack-arg
            // offsets, nothing should be collected.
            let four = b.build_int_const(4u64, NodeOutputType::U64)?;
            let sp_v1 = b.build_int_sub(sp_v0, four, NodeOutputType::U64)?;
            b.write_variable(&sp, sp_v1)?;
            let v = b.build_int_const(99u64, NodeOutputType::U64)?;
            b.build_store(sp_v1, v, rsleigh::VnSpace::RAM)?;

            let target = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
            b.build_call(target)?;
            b.build_return(None, &[])?;
            Ok(())
        })
    };
    let fg_v1 = run_v1(sp, vec![], build);
    let fg_v2 = run_v2(sp, vec![], build);
    assert_eq!(summarise_call(&fg_v1), summarise_call(&fg_v2));
}
