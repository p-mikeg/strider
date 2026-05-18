//! Phase 3 Task 3.5b parity test.
//!
//! `StackLoadForward` (v1, imperative memory-chain walker) vs.
//! `StackLoadForwardEgg` (v2, egg-informed-but-imperative) MUST produce
//! structurally identical IR for every supported shape.
//!
//! Each parity case runs both passes (sequenced after ConstantFold,
//! RedundantPhis, and the v1 StackStoreDetect so the memory chain is
//! StackStore-classified the same way for both) and compares:
//!   * count of reachable `Load` nodes (forwarded loads vanish)
//!   * count of reachable `ValuePhi` nodes (phi-of-stores resolution)
//!   * `NodeKind` of the return-value producer

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{
    ConstantFold, OptimizerPipeline, RedundantPhis, StackLoadForward, StackStoreDetect,
    stack_load_forward_egg::StackLoadForwardEgg,
};
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{BuiltFunctionGraph, FunctionBuilder, IntBinaryOp};
use target::Endianness;

fn sp32_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    }
}

fn reachable_count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}

fn return_value_kind(fg: &BuiltFunctionGraph) -> NodeKind {
    let ret = strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("must have Return");
    let inputs = fg.graph.node_inputs(ret);
    // Return inputs: [ctrl, memory, value?]
    if inputs.len() >= 3 {
        let producer = fg.graph.get_node_from_output(inputs[2]);
        *fg.graph.node_kind(producer)
    } else {
        NodeKind::Return
    }
}

fn build_pipeline_common(sp: rsleigh::Vn) -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(RedundantPhis);
    p.add(StackStoreDetect::new(sp));
    p
}

fn build_pipeline_v1(sp: rsleigh::Vn) -> OptimizerPipeline {
    let mut p = build_pipeline_common(sp);
    p.add(StackLoadForward::new(sp, Endianness::Little));
    p
}

fn build_pipeline_v2(sp: rsleigh::Vn) -> OptimizerPipeline {
    let mut p = build_pipeline_common(sp);
    p.add(StackLoadForwardEgg::new(sp, Endianness::Little));
    p
}

#[derive(Debug, PartialEq, Eq)]
struct ParitySummary {
    reachable_loads: usize,
    reachable_value_phis: usize,
    return_kind: NodeKind,
}

fn summarise(fg: &BuiltFunctionGraph) -> ParitySummary {
    ParitySummary {
        reachable_loads: reachable_count(fg, |k| matches!(k, NodeKind::Load(_))),
        reachable_value_phis: reachable_count(fg, |k| matches!(k, NodeKind::ValuePhi)),
        return_kind: return_value_kind(fg),
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
fn parity_forward_through_single_store() {
    let sp = sp32_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_val| {
            let four = b.build_int_const(4u64, NodeOutputType::U32)?;
            let addr = b.build_int_binary_operation(
                sp_val,
                four,
                IntBinaryOp::Add,
                NodeOutputType::U32,
            )?;
            let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
    };
    let (v1, v2) = run_to_summary("forward_single_store", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_bail_on_call_between() {
    let sp = sp32_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_val| {
            let four = b.build_int_const(4u64, NodeOutputType::U32)?;
            let addr = b.build_int_binary_operation(
                sp_val,
                four,
                IntBinaryOp::Add,
                NodeOutputType::U32,
            )?;
            let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
            // Call between — clobbers memory.
            let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
            b.build_call(target)?;
            let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
    };
    let (v1, v2) = run_to_summary("bail_on_call", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_phi_of_stores_at_same_offset() {
    let sp = sp32_vn();
    // Both branches write the same offset; load from sp+4 in the join
    // region.  v1 emits a ValuePhi over the two data slots.
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

        b.set_region(entry);
        b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        // a: store 0xAA at sp+4
        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let four_a = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr_a = b.build_int_binary_operation(
            sp_a,
            four_a,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let data_a = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
        b.build_store(addr_a, data_a, rsleigh::VnSpace::RAM)?;
        b.build_branch(c)?;

        // b: store 0xBB at sp+4
        b.set_region(bb);
        let sp_b = b.read_variable(&sp)?;
        let four_b = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr_b = b.build_int_binary_operation(
            sp_b,
            four_b,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let data_b = b.build_int_const(0xBBu64, NodeOutputType::U32)?;
        b.build_store(addr_b, data_b, rsleigh::VnSpace::RAM)?;
        b.build_branch(c)?;

        // c: load from sp+4 → expect ValuePhi over the two data slots.
        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let four_c = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr_c = b.build_int_binary_operation(
            sp_c,
            four_c,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let loaded = b.build_load(addr_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        b.build()
    };
    let (v1, v2) = run_to_summary("phi_same_offset", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_phi_where_one_branch_missing_store_bails() {
    let sp = sp32_vn();
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

        b.set_region(entry);
        b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        // a: store 0xAA at sp+4
        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let four_a = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr_a = b.build_int_binary_operation(
            sp_a,
            four_a,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let data_a = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
        b.build_store(addr_a, data_a, rsleigh::VnSpace::RAM)?;
        b.build_branch(c)?;

        // b: no store
        b.set_region(bb);
        b.build_branch(c)?;

        // c: load from sp+4 — only one branch stores, must bail.
        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let four_c = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr_c = b.build_int_binary_operation(
            sp_c,
            four_c,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let loaded = b.build_load(addr_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        b.build()
    };
    let (v1, v2) = run_to_summary("phi_missing_branch", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_type_mismatch_wider_load_skipped() {
    let sp = sp32_vn();
    // Store U16 at sp+4, load U32 from sp+4.  v1 only forwards
    // narrow-load-from-wider-store; wider-load-from-narrower-store
    // bails (no zero-extend synthesis).
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_val| {
            let four = b.build_int_const(4u64, NodeOutputType::U32)?;
            let addr = b.build_int_binary_operation(
                sp_val,
                four,
                IntBinaryOp::Add,
                NodeOutputType::U32,
            )?;
            // Narrow store.
            let data = b.build_int_const(0x11u64, NodeOutputType::U16)?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
            // Wider load — type mismatch, v1 bails.
            let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
    };
    let (v1, v2) = run_to_summary("type_mismatch_wider_load", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}

#[test]
fn parity_multi_store_chain_with_non_aliasing() {
    let sp = sp32_vn();
    // Store at sp+8 (irrelevant), then store at sp+4, then load at sp+4.
    // Walker must skip the +8 store.
    let build = || -> anyhow::Result<BuiltFunctionGraph> {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
            let addr8 = b.build_int_binary_operation(
                sp_val,
                eight,
                IntBinaryOp::Add,
                NodeOutputType::U32,
            )?;
            let data8 = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
            b.build_store(addr8, data8, rsleigh::VnSpace::RAM)?;
            let four = b.build_int_const(4u64, NodeOutputType::U32)?;
            let addr4 = b.build_int_binary_operation(
                sp_val,
                four,
                IntBinaryOp::Add,
                NodeOutputType::U32,
            )?;
            let data4 = b.build_int_const(0x42u64, NodeOutputType::U32)?;
            b.build_store(addr4, data4, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
    };
    let (v1, v2) = run_to_summary("multi_store_chain", sp, build);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
}
