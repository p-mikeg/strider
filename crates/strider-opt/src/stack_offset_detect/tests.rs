//! Tests for [`crate::StackOffsetDetect`].
//!
//! Pins: (a) SP-relative stores / loads get a concrete offset stamped
//! on `Function::stack_offsets`, (b) non-SP-rooted addresses leave the
//! side-table untouched, (c) re-running the pass on the same function
//! reports `NoChange`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::Function;
use strider_ir::IRBuilderExt;
use strider_ir::IRViewer;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, make_sp_fn, stack_vn_x86};

use crate::StackOffsetDetect;
use crate::pipeline::PostOptimizerTestExt;

/// Count the nodes that currently carry a stamped stack offset.
fn stamped_count(function: &Function) -> usize {
    function
        .graph()
        .all_node_ids()
        .filter(|&n| function.stack_offset(n).is_some())
        .count()
}

/// Collapse phis (so SP addresses are bare `InitialVar(sp) + k` terminals —
/// the shape the SP-aware pass sees in production once PhiCollapse has run)
/// and run the `StackOffsetDetect` post-pass.  The pass reads the stack
/// pointer from the function's own calling convention and returns no
/// Change/NoChange — tests assert directly on the `stack_offsets` side-table.
fn run(function: &mut Function) {
    let mut pre = crate::OptimizerPipeline::new();
    pre.add(crate::PhiCollapse);
    pre.add(crate::RegionCollapse);
    pre.run(function, &mut crate::OptCtx::new(None))
        .expect("phi collapse must not error");
    StackOffsetDetect
        .run_one(function, &mut crate::OptCtx::new(None))
        .expect("must not error");
}

/// `store [sp-4] = 0x42; load [sp-4]; return loaded`.
fn stack_store_load_return(sp: rsleigh::Vn) -> Function {
    make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_v, four, ValueType::I32)?;
        let data = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })
    .unwrap()
}

#[test]
fn sp_relative_store_and_load_get_offset_stamped() {
    let sp = stack_vn_x86();
    let mut f = stack_store_load_return(sp);

    run(&mut f);

    let store_offsets: Vec<i64> = f
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .filter_map(|n| f.stack_offset(n).map(|(_, off)| off))
        .collect();
    let load_offsets: Vec<i64> = f
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Load(_)))
        .filter_map(|n| f.stack_offset(n).map(|(_, off)| off))
        .collect();

    assert_eq!(store_offsets, vec![-4]);
    assert_eq!(load_offsets, vec![-4]);
}

#[test]
fn non_sp_relative_store_leaves_side_table_untouched() {
    let sp = stack_vn_x86();
    let mut f = make_sp_fn(sp, |b, _sp_v| {
        let addr = b.build_int_const(0x1000u64, ValueType::I32)?;
        let data = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, ValueType::I32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap();

    run(&mut f);
    assert_eq!(stamped_count(&f), 0);
}

/// A store whose address is rooted at an *alignment-masked* base
/// (`And(sp, mask)`, e.g. `and $0xfffffff8, %esp`) IS a stack access — just
/// in a different coordinate system from entry-SP.  `StackOffsetDetect`
/// stamps it with that aligned base (offset 0 here, no `Add`), so aligned
/// frames are covered; the recorded `base` keeps its offset from being
/// conflated with an entry-SP offset.
#[test]
fn alignment_masked_base_store_is_stamped_with_aligned_base() {
    use strider_ir::IntBinaryOp;
    let sp = stack_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        // Simulate `and $0xfffffff8, %esp` then a store at that aligned base.
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        let aligned = b.build_int_binary_operation(sp_v, mask, IntBinaryOp::And, ValueType::I32)?;
        let data = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(aligned, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, ValueType::I32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap();

    run(&mut f);
    // The aligned-base store IS stamped, and its base is the `And` node's
    // output (NOT the canonical `InitialVar(sp)`).
    let store = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .expect("store node");
    let (base, offset) = f
        .stack_offset(store)
        .expect("aligned store must be stamped");
    assert_eq!(offset, 0, "store at the aligned base directly => offset 0");
    let base_node = f.producer(base);
    assert!(
        matches!(
            f.node_kind(base_node),
            NodeKind::IntBinaryOp(IntBinaryOp::And)
        ),
        "recorded base must be the alignment `And` node, not InitialVar(sp)"
    );
}

#[test]
fn rerun_after_first_pass_is_idempotent() {
    let sp = stack_vn_x86();
    let mut f = stack_store_load_return(sp);

    run(&mut f);
    let after_first = stamped_count(&f);
    assert!(after_first > 0, "first run must stamp the SP-relative accesses");
    // Re-running the post-pass must not stamp anything new (the
    // already-known offsets are skipped) — the stamped set is stable.
    run(&mut f);
    assert_eq!(
        stamped_count(&f),
        after_first,
        "re-run must be idempotent: no new stamps"
    );
}

#[test]
fn post_pass_function_validates() {
    let sp = stack_vn_x86();
    let mut f = stack_store_load_return(sp);
    run(&mut f);
    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("IR must validate");
}
