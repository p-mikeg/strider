//! Tests for [`crate::StackOffsetDetect`].
//!
//! Pins: (a) SP-relative stores / loads get a concrete offset stamped
//! on `Function::stack_offsets`, (b) non-SP-rooted addresses leave the
//! side-table untouched, (c) re-running the pass on the same function
//! reports `NoChange`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::{
    Function, IRBuilderExt, IRViewer, IntBinaryOp,
    node::{NodeKind, ValueType},
};
use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, make_sp_fn, stack_vn_x86};

use crate::{StackOffsetDetect, pipeline::PostOptimizerTestExt};

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
    // Canonicalize first (ConstantFold folds the lowered `Sub` = `Add(_, Neg(K))`
    // to `Add(_, IntConst(-K))`, PhiCollapse drops the read_variable(sp) phi),
    // matching the production shape the post-pass sees.
    crate::test_support::cf_rp_pipeline()
        .run(function, &mut crate::OptCtx::new(None))
        .expect("canonicalize must not error");
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

/// A nested Add chain `((sp + 8) + 16) - 4` must be stamped with the
/// summed net offset `+20` — the SP decomposition walks the whole chain,
/// not just the outermost Add.
#[test]
fn nested_add_chain_stamps_summed_offset() {
    let sp = stack_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let sixteen = b.build_int_const(16u64, ValueType::I32)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let a1 = b.build_int_binary_operation(sp_v, eight, IntBinaryOp::Add, ValueType::I32)?;
        let a2 = b.build_int_binary_operation(a1, sixteen, IntBinaryOp::Add, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(a2, four, ValueType::I32)?;
        let data = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, ValueType::I32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap();

    run(&mut f);
    let store = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .expect("store node");
    let (_base, offset) = f.stack_offset(store).expect("nested chain must be stamped");
    assert_eq!(offset, 20, "8 + 16 - 4 = 20");
}

/// A net-NEGATIVE nested chain `(sp + 8) - 12` stamps `-4` — the
/// summation is signed and the lowered-Sub (`Add(_, Neg(K))`) leg
/// subtracts.
#[test]
fn nested_chain_with_negative_net_offset_stamps_negative() {
    let sp = stack_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let twelve = b.build_int_const(12u64, ValueType::I32)?;
        let a1 = b.build_int_binary_operation(sp_v, eight, IntBinaryOp::Add, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(a1, twelve, ValueType::I32)?;
        let data = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, ValueType::I32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap();

    run(&mut f);
    let store = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .expect("store node");
    let (_base, offset) = f
        .stack_offset(store)
        .expect("net-negative chain must be stamped");
    assert_eq!(offset, -4, "8 - 12 = -4");
}

#[test]
fn rerun_after_first_pass_is_idempotent() {
    let sp = stack_vn_x86();
    let mut f = stack_store_load_return(sp);

    run(&mut f);
    let after_first = stamped_count(&f);
    assert!(
        after_first > 0,
        "first run must stamp the SP-relative accesses"
    );
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
    strider_ir::validate::validate(&f).expect("IR must validate");
}
