//! Phase 3 Task 3.5a parity test.
//!
//! `StackStoreDetect` (v1, imperative SP-decomposition) vs.
//! `StackStoreDetectEgg` (v2, egg `Analysis::Data`-based) MUST produce
//! structurally identical IR for every supported shape.
//!
//! Each parity case runs both passes to a fixed point on a fresh
//! fixture and compares:
//!   * counts of `StackStore { offset }` and `StackStorePhi { … }` nodes
//!     in the reachable graph
//!   * `stack_phi_offsets` set for the phi case
//!   * whether the original `Store(_)` survived

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{
    ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect,
    stack_store_detect_egg::StackStoreDetectEgg,
};
use strider_ir::node::{NodeId, NodeKind, NodeOutputType};
use strider_ir::test_utils::{SENTINEL_LIFT_ADDR, sp_vn_x86 as sp_vn};
use strider_ir::{BuiltFunctionGraph, FunctionBuilder, IntBinaryOp};

fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}

fn find_phis(fg: &BuiltFunctionGraph) -> Vec<NodeId> {
    strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::StackStorePhi { .. }))
        .collect()
}

fn build_pipeline_v1(sp: rsleigh::Vn) -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(RedundantPhis);
    p.add(StackStoreDetect::new(sp));
    p
}

fn build_pipeline_v2(sp: rsleigh::Vn) -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(RedundantPhis);
    p.add(StackStoreDetectEgg::new(sp));
    p
}

#[derive(Debug, PartialEq, Eq)]
struct ParitySummary {
    stack_store_offsets: Vec<i64>,
    stack_store_phi_offsets: Vec<Vec<i64>>,
    surviving_stores: usize,
}

fn summarise(fg: &BuiltFunctionGraph) -> ParitySummary {
    let mut stack_store_offsets: Vec<i64> = strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .filter_map(|n| match *fg.graph.node_kind(n) {
            NodeKind::StackStore { offset, .. } => Some(offset),
            _ => None,
        })
        .collect();
    stack_store_offsets.sort();
    let mut stack_store_phi_offsets: Vec<Vec<i64>> = find_phis(fg)
        .into_iter()
        .map(|n| {
            let mut o: Vec<i64> = fg.graph.stack_phi_offsets(n).to_vec();
            o.sort();
            o
        })
        .collect();
    stack_store_phi_offsets.sort();
    let surviving_stores = count(fg, |k| matches!(k, NodeKind::Store(_)));
    ParitySummary {
        stack_store_offsets,
        stack_store_phi_offsets,
        surviving_stores,
    }
}

fn run_to_summary<F>(label: &str, sp: rsleigh::Vn, build: F) -> (ParitySummary, ParitySummary)
where
    F: Fn() -> anyhow::Result<BuiltFunctionGraph>,
{
    let mut fg_v1 = build().expect("build v1 fixture");
    build_pipeline_v1(sp)
        .run(&mut fg_v1.graph, fg_v1.entry)
        .unwrap_or_else(|e| panic!("v1 pipeline failed for {label}: {e:?}"));
    let mut fg_v2 = build().expect("build v2 fixture");
    build_pipeline_v2(sp)
        .run(&mut fg_v2.graph, fg_v2.entry)
        .unwrap_or_else(|e| panic!("v2 pipeline failed for {label}: {e:?}"));
    (summarise(&fg_v1), summarise(&fg_v2))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn parity_simple_sp_minus_4() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_val| {
            let four = b.build_int_const(4u64, NodeOutputType::U32)?;
            let addr = b.build_int_sub(sp_val, four, NodeOutputType::U32)?;
            let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
    };
    let (v1, v2) = run_to_summary("simple_sp_minus_4", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_phi_of_offsets_becomes_stack_store_phi() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_a2 = b.build_int_sub(sp_a, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_a2)?;
        b.build_branch(c)?;

        b.set_region(bb);
        let sp_b = b.read_variable(&sp)?;
        let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
        let sp_b2 = b.build_int_sub(sp_b, eight, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_b2)?;
        b.build_branch(c)?;

        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let data = b.build_int_const(0xCCu64, NodeOutputType::U32)?;
        b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        b.build()
    };
    let (v1, v2) = run_to_summary("phi_of_offsets", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_non_sp_store_is_untouched() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, _sp_val| {
            let addr = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
            let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
            b.build_return(None, &[])?;
            Ok(())
        })
    };
    let (v1, v2) = run_to_summary("non_sp_store", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_deep_sp_arith_nesting() {
    // ((sp+16)-4)-4 = sp+8 must reduce.
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_v| {
            let s16 = b.build_int_const(16u64, NodeOutputType::U32)?;
            let s4 = b.build_int_const(4u64, NodeOutputType::U32)?;
            let plus16 = b.build_int_binary_operation(
                sp_v,
                s16,
                IntBinaryOp::Add,
                NodeOutputType::U32,
            )?;
            let minus4a = b.build_int_sub(plus16, s4, NodeOutputType::U32)?;
            let minus4b = b.build_int_sub(minus4a, s4, NodeOutputType::U32)?;
            let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
            b.build_store(minus4b, data, rsleigh::VnSpace::RAM)?;
            b.build_return(None, &[])?;
            Ok(())
        })
    };
    let (v1, v2) = run_to_summary("deep_sp_arith", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_phi_with_equal_offsets_collapses_to_stack_store() {
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_a2 = b.build_int_sub(sp_a, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_a2)?;
        b.build_branch(c)?;

        b.set_region(bb);
        let sp_b = b.read_variable(&sp)?;
        let four2 = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_b2 = b.build_int_sub(sp_b, four2, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_b2)?;
        b.build_branch(c)?;

        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let data = b.build_int_const(0xCCu64, NodeOutputType::U32)?;
        b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        b.build()
    };
    let (v1, v2) = run_to_summary("phi_equal_offsets", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_phi_with_non_sp_pred_does_not_rewrite() {
    // A VarPhi(sp) whose predecessor value is NOT SP-rooted must not be
    // rewritten — `decompose_sp` returns None and StackStoreDetect bails.
    let sp = sp_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_minus_4 = b.build_int_sub(sp_a, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEFu64, NodeOutputType::U32)?;
        b.write_variable(&sp, bogus)?;
        b.build_branch(c)?;

        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let data = b.build_int_const(0xCCu64, NodeOutputType::U32)?;
        b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        b.build()
    };
    let (v1, v2) = run_to_summary("phi_non_sp_pred", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}
