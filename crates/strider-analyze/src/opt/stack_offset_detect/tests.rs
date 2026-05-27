//! Tests for [`crate::opt::StackOffsetDetect`].
//!
//! Pins: (a) SP-relative stores / loads get a concrete offset stamped
//! on `Function::stack_offsets`, (b) non-SP-rooted addresses leave the
//! side-table untouched, (c) re-running the pass on the same function
//! reports `NoChange`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::Function;
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, make_sp_fn, stack_vn_x86};

use crate::opt::StackOffsetDetect;
use crate::opt::pipeline::{OptimizationResult, Optimizer};

fn run(function: &mut Function, sp: rsleigh::Vn) -> OptimizationResult {
    let pass = StackOffsetDetect::new(sp);
    let entry = function.entry().unwrap();
    pass.optimize(function, entry).expect("must not error")
}

/// `store [sp-4] = 0x42; load [sp-4]; return loaded`.
fn stack_store_load_return(sp: rsleigh::Vn) -> Function {
    make_sp_fn(sp, |b, sp_v| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_sub_as_add_neg(sp_v, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
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

    assert_eq!(run(&mut f, sp), OptimizationResult::Changed);

    let store_offsets: Vec<i64> = f
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .filter_map(|n| f.stack_offset(n))
        .collect();
    let load_offsets: Vec<i64> = f
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Load(_)))
        .filter_map(|n| f.stack_offset(n))
        .collect();

    assert_eq!(store_offsets, vec![-4]);
    assert_eq!(load_offsets, vec![-4]);
}

#[test]
fn non_sp_relative_store_leaves_side_table_untouched() {
    let sp = stack_vn_x86();
    let mut f = make_sp_fn(sp, |b, _sp_v| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap();

    assert_eq!(run(&mut f, sp), OptimizationResult::NoChange);
    let stamped = f.all_node_ids().filter(|&n| f.stack_offset(n).is_some()).count();
    assert_eq!(stamped, 0);
}

/// A store whose address is rooted at an *alignment-masked* base
/// (`And(sp, mask)`, e.g. `and $0xfffffff8, %esp`) is NOT unambiguously
/// `sp + K`: the mask shifts the value by a runtime amount, so the offset
/// relative to entry-SP is unknown.  `stack_offset` is documented to hold a
/// value only for addresses that *are* unambiguously `sp + K`, so such a
/// store must be left unstamped — otherwise its offset is in a different
/// coordinate system from canonical-SP stores and consumers that compare
/// offsets (e.g. CallStackArgCollect's side-table fast path) mix bases.
#[test]
fn alignment_masked_base_store_is_not_stamped() {
    use strider_ir::IntBinaryOp;
    let sp = stack_vn_x86();
    let mut f = make_sp_fn(sp, |b, sp_v| {
        // Simulate `and $0xfffffff8, %esp` then a store at that aligned base.
        let mask = b.build_int_const(0xFFFF_FFF8u64, NodeOutputType::U32)?;
        let aligned = b.build_int_binary_operation(sp_v, mask, IntBinaryOp::And, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(aligned, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        b.build_return(Some(zero), &[])?;
        Ok(())
    })
    .unwrap();

    run(&mut f, sp);
    let stamped = f
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .filter(|&n| f.stack_offset(n).is_some())
        .count();
    assert_eq!(
        stamped, 0,
        "an alignment-masked-base store must not be stamped (not unambiguously sp+K)"
    );
}

#[test]
fn rerun_after_first_pass_reports_no_change() {
    let sp = stack_vn_x86();
    let mut f = stack_store_load_return(sp);

    assert_eq!(run(&mut f, sp), OptimizationResult::Changed);
    assert_eq!(run(&mut f, sp), OptimizationResult::NoChange);
}

#[test]
fn post_pass_function_validates() {
    let sp = stack_vn_x86();
    let mut f = stack_store_load_return(sp);
    run(&mut f, sp);
    let entry = f.entry().unwrap();
    strider_ir::validate::validate(&f, entry).expect("IR must validate");
}
