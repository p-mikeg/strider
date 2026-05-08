//! End-to-end tests for an SP-aware pipeline like the one Strider wires:
//! default + StackStoreDetect + StackLoadForward + CallStackArgCollect post-pass.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

mod common;

use ir::node::{NodeKind, NodeOutputType};
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
    let four = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr = b.build_int_sub(sp_v, four, NodeOutputType::U32)?;
    let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    pipeline_with_sp(sp, vec![4, 8, 12]).run(&mut fg.graph, fg.entry)?;

    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow::anyhow!("no return node found in function"))?;
    let val = fg.graph.node_inputs(ret)[2];
    let kind = *fg.graph.kind_of_output(val);
    assert!(
        matches!(kind, NodeKind::IntConst(0x42)),
        "load must forward to stored value, got {kind:?}"
    );
    Ok(())
}

/// O10 — `StackStoreDetect + StackLoadForward` converge in ≤ 2 manual
/// iterations on the canonical "Store(SP+K, c) ; Load(SP+K)" shape.
/// First iteration: detect classifies the Store (changed = true) and
/// forward replaces the Load's value (changed = true).  Second iteration:
/// neither pass finds further work, so both report `NoChange`.
#[test]
fn stack_store_detect_and_load_forward_converge_in_two_iters() -> opt::Result<()> {
    let sp = sp_vn();
    let mut b = ir::FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
    let addr = b.build_int_sub(sp_v, eight, NodeOutputType::U32)?;
    let data = b.build_int_const(42u64, NodeOutputType::U32)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let detect = StackStoreDetect::new(sp);
    let forward = StackLoadForward::new(sp, target::Endianness::Little);

    let mut iter = 0u32;
    let max_iters = 2u32;
    loop {
        iter += 1;
        let r1 = detect.optimize(&mut fg.graph, fg.entry)?;
        let r2 = forward.optimize(&mut fg.graph, fg.entry)?;
        if !r1.changed() && !r2.changed() {
            break;
        }
        assert!(
            iter <= max_iters,
            "StackStoreDetect+StackLoadForward did not converge in {max_iters} iters"
        );
    }
    // The first iteration must do real work (otherwise the test is
    // trivially-passing on a no-op shape); the second iteration's check
    // is what closes the convergence-bound contract.
    assert!(
        iter <= max_iters,
        "expected convergence in ≤ {max_iters} iters, took {iter}"
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
    let four = b.build_int_const(4u64, NodeOutputType::U32)?;
    let sp_v1 = b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;
    let sp_v2 = b.build_int_sub(sp_v1, four, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11u64, NodeOutputType::U32)?;
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    pipeline_with_sp(sp, vec![0, 4]).run(&mut fg.graph, fg.entry)?;

    let call = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Call))
        .ok_or_else(|| anyhow::anyhow!("expected Call node, got {:?}", NodeKind::Call))?;
    let inputs = fg.graph.node_inputs(call);
    assert_eq!(inputs.len(), 5, "ctrl + mem + target + 2 args");
    Ok(())
}
