//! End-to-end tests for an SP-aware pipeline like the one Analyzer wires:
//! default + StackStoreDetect + StackLoadForward + CallStackArgCollect post-pass.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

mod common;

use ir::node::{NodeKind, NodeOutputType};
use ir::IntBinaryOp;
use opt::*;

use common::sp_vn;

fn pipeline_with_sp(sp: rsleigh::Vn, stack_offsets: Vec<i64>) -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(KnownBits);
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p.add(StackStoreDetect::new(sp));
    p.add(StackLoadForward::new(sp, target::Endianness::Little));
    p.add_post_pass(CallStackArgCollect::new(stack_offsets, sp));
    p
}

/// Push a constant onto the stack, load it back: the load must be forwarded.
#[test]
fn store_then_load_at_same_offset_forwarded() -> opt::Result<()> {
    let sp = sp_vn();
    let mut b = ir::FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(sp_v, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let data = b.build_int_const(0x42u64, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    pipeline_with_sp(sp, vec![4, 8, 12]).run(&mut fg)?;

    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or(opt::ErrorKind::NoReturnNode)?;
    let val = fg.graph.node_inputs(ret)[2];
    let kind = *fg.graph.node_kind(fg.graph.get_node_from_output(val));
    assert!(
        matches!(kind, NodeKind::IntConst(0x42)),
        "load must forward to stored value, got {kind:?}"
    );
    Ok(())
}

/// Two cdecl-style pushes followed by a Call — `CallStackArgCollect` post-pass
/// must extend the Call's input list with both arg values.
#[test]
fn full_call_pipeline_collects_args() -> opt::Result<()> {
    let sp = sp_vn();
    let mut b = ir::FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, NodeOutputType::U32);
    let sp_v1 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22u64, NodeOutputType::U32);
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;
    let sp_v2 = b.build_int_binary_operation(sp_v1, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11u64, NodeOutputType::U32);
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000u64, NodeOutputType::U32);
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    pipeline_with_sp(sp, vec![0, 4]).run(&mut fg)?;

    let call = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Call))
        .ok_or(opt::ErrorKind::ExpectedNodeNotFound("Call", NodeKind::Call))?;
    let inputs = fg.graph.node_inputs(call);
    assert_eq!(inputs.len(), 5, "ctrl + mem + target + 2 args");
    Ok(())
}
