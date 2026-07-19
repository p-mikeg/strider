use super::*;
use crate::error::Result;
use crate::test_support::cf_rp_pipeline;
use anyhow::anyhow;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Graph, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::{RegisterSet, stack_vn_x86 as stack_vn};

fn is_const(fg: &strider_ir::Function, v: ValueId, expected: u64) -> bool {
    matches!(fg.kind_of_value(v), NodeKind::IntConst(_))
        && fg.int_const_u128(v) == Some(u128::from(expected))
}

fn const_val(fg: &strider_ir::Function, v: ValueId, ctx: &str) -> u128 {
    fg.int_const_u128(v).unwrap_or_else(|| {
        panic!(
            "collected arg should be an IntConst, got {:?} — {ctx}",
            fg.kind_of_value(v)
        )
    })
}

/// Prologue zero-init writes and a `push ebx` save land in a later call's
/// arg-slot window, and once lowered to memory they are indistinguishable from
/// argument pushes.  So all 7 are collected here and disambiguation is left to
/// the caller.  The old chain-order heuristic that stopped after arg 1 was
/// dropped because it could equally drop real args.
#[test]
fn local_inits_in_arg_window_are_collected_too() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp0 = b.read_variable(&sp)?;
    // `push ebx`, `sub esp, 16`, 4x zero-init, push arg1, push arg0, then the
    // implicit call ret-push.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sixteen = b.build_int_const(16u64, ValueType::I32)?;

    let sp_after_push_ebx = b.build_sub_as_add_neg(sp0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_after_push_ebx)?;
    let init_ebx = b.build_int_const(0xEBu64, ValueType::I32)?;
    b.build_store(sp_after_push_ebx, init_ebx, rsleigh::VnSpace::RAM)?;

    let sp_after_sub = b.build_sub_as_add_neg(sp_after_push_ebx, sixteen, ValueType::I32)?;
    b.write_variable(&sp, sp_after_sub)?;

    // buf[0..16] at esp+0,+4,+8,+12, i.e. [-20, -16, -12, -8].
    let zero = b.build_int_const(0u64, ValueType::I32)?;
    for k in 0..4 {
        let off = b.build_int_const((k * 4) as u64, ValueType::I32)?;
        let addr =
            b.build_int_binary_operation(sp_after_sub, off, IntBinaryOp::Add, ValueType::I32)?;
        b.build_store(addr, zero, rsleigh::VnSpace::RAM)?;
    }

    // push arg1 = 1 at [sp - 24].
    let sp_push_arg1 = b.build_sub_as_add_neg(sp_after_sub, four, ValueType::I32)?;
    b.write_variable(&sp, sp_push_arg1)?;
    let arg1 = b.build_int_const(1u64, ValueType::I32)?;
    b.build_store(sp_push_arg1, arg1, rsleigh::VnSpace::RAM)?;

    // push arg0 = 42 at [sp - 28].
    let sp_push_arg0 = b.build_sub_as_add_neg(sp_push_arg1, four, ValueType::I32)?;
    b.write_variable(&sp, sp_push_arg0)?;
    let arg0 = b.build_int_const(42u64, ValueType::I32)?;
    b.build_store(sp_push_arg0, arg0, rsleigh::VnSpace::RAM)?;

    // Implicit ret-addr push at [sp - 32], mimicking x86 `call`.
    let sp_call = b.build_sub_as_add_neg(sp_push_arg0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_call)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_call, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // x86 cdecl: ret addr at offset 0, args at +4, +8, +12, ...
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + 7 collected args = 11 inputs.
    let collected: Vec<u128> = inputs[4..]
        .iter()
        .map(|&v| const_val(&fg, v, "local_inits_in_arg_window_are_collected_too"))
        .collect();
    assert_eq!(
        collected,
        vec![42, 1, 0, 0, 0, 0, 0xEB],
        "every plausible stack-arg store in the contiguous window is collected"
    );
    Ok(())
}

/// 32-bit cdecl `f(double a, int b)`: the 8-byte store at `sp+0` spans two
/// 4-byte slots and must be ONE argument, with the cursor advancing past both
/// so `b` lands next.  The old within-slot `index_of` rejected the wide store
/// entirely and dropped both args.
#[test]
fn outgoing_wide_arg_store_collected_as_one_arg() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // a = double stored as I64 at sp+0, covering slots 0 and 1.
    let a = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, ValueType::I64)?;
    b.build_store(sp_v0, a, rsleigh::VnSpace::RAM)?;
    // b = int stored as I32 at sp+8, slot 2.
    let eight = b.build_int_const(8u64, ValueType::I32)?;
    let sp_plus_8 = b.build_int_binary_operation(sp_v0, eight, IntBinaryOp::Add, ValueType::I32)?;
    let bv = b.build_int_const(7u64, ValueType::I32)?;
    b.build_store(sp_plus_8, bv, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + exactly 2 args (the wide double + the int).
    assert_eq!(
        inputs.len(),
        6,
        "wide store = one arg; cursor advances past both slots it covers so the \
         int lands as arg 1; got inputs={inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 0xDEAD_BEEF_CAFE_BABE),
        "arg0 should be the 8-byte double value, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    assert!(
        is_const(&fg, inputs[5], 7),
        "arg1 should be the int 7, got {:?}",
        fg.kind_of_value(inputs[5])
    );
    Ok(())
}

/// Span 4: an `I128` store at `sp+0` covers four 4-byte slots and must still be
/// exactly ONE Call input, not one per covered slot, so the following `I32` at
/// `sp+16` lands as arg 1.
#[test]
fn outgoing_span_four_wide_arg_store_collected_as_one_arg() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // a = 16-byte value at sp+0, covering slots 0..3.
    let const_id_a = b
        .function_mut()
        .intern_int_const(0xABCD_u128, ValueType::I128);
    let a = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::IntConst(const_id_a),
        [],
        [strider_ir::node::ValueKind::Typed(ValueType::I128)],
    );
    let a_val = b.function().node_outputs_exact::<1>(a).unwrap()[0];
    b.build_store(sp_v0, a_val, rsleigh::VnSpace::RAM)?;
    // b = int at sp+16, slot 4.
    let sixteen = b.build_int_const(16u64, ValueType::I32)?;
    let sp_plus_16 =
        b.build_int_binary_operation(sp_v0, sixteen, IntBinaryOp::Add, ValueType::I32)?;
    let bv = b.build_int_const(7u64, ValueType::I32)?;
    b.build_store(sp_plus_16, bv, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + exactly 2 args, not four inputs for the four
    // slots the I128 covers.
    assert_eq!(
        inputs.len(),
        6,
        "wide I128 store = one arg; cursor advances past all four slots it \
         covers so the int lands as arg 1; got inputs={inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 0xABCD),
        "arg0 should be the 16-byte I128 value, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    assert!(
        is_const(&fg, inputs[5], 7),
        "arg1 should be the int 7, got {:?}",
        fg.kind_of_value(inputs[5])
    );
    Ok(())
}

/// Odd span 3, between the covered 2- and 4-slot cases: an `I80` at `sp+0`
/// covers three slots, so the following `I32` at `sp+12` must land as arg 1,
/// neither absorbed into the span nor mis-indexed.
#[test]
fn outgoing_span_three_wide_arg_store_collected_as_one_arg() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // a = 10-byte value at sp+0, covering slots 0..2.
    let const_id_a = b
        .function_mut()
        .intern_int_const(0xABCD_u128, ValueType::I80);
    let a = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::IntConst(const_id_a),
        [],
        [strider_ir::node::ValueKind::Typed(ValueType::I80)],
    );
    let a_val = b.function().node_outputs_exact::<1>(a).unwrap()[0];
    b.build_store(sp_v0, a_val, rsleigh::VnSpace::RAM)?;
    // b = int at sp+12, slot 3.
    let twelve = b.build_int_const(12u64, ValueType::I32)?;
    let sp_plus_12 =
        b.build_int_binary_operation(sp_v0, twelve, IntBinaryOp::Add, ValueType::I32)?;
    let bv = b.build_int_const(7u64, ValueType::I32)?;
    b.build_store(sp_plus_12, bv, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + exactly 2 args, not three inputs for the
    // three slots the I80 covers, and not absorbing the int.
    assert_eq!(
        inputs.len(),
        6,
        "I80 store spans 3 slots = one arg; the int lands as arg 1 (slot 3); \
         got inputs={inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 0xABCD),
        "arg0 should be the 10-byte I80 value, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    assert!(
        is_const(&fg, inputs[5], 7),
        "arg1 should be the int 7, got {:?}",
        fg.kind_of_value(inputs[5])
    );
    Ok(())
}

fn find_call(graph: &Graph) -> Result<NodeId> {
    graph
        .all_node_ids()
        .find(|&n| matches!(graph.node_kind(n), NodeKind::Call))
        .ok_or_else(|| anyhow!("expected Call node, got {:?}", NodeKind::Call))
}

/// `push arg1=22; push arg0=11; call 0x1000` must extend the Call's inputs
/// with `[arg0, arg1]` in positional order.
#[test]
fn cdecl_two_stack_args_collected_in_order() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

    let sp_v2 = b.build_sub_as_add_neg(sp_v1, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // cdecl has no arg-passing registers, so indices 4 and 5 are the stack args.
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+sp+2 stack args; got {inputs:?}"
    );

    assert!(
        is_const(&fg, inputs[4], 11),
        "arg0 should be 11, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    assert!(
        is_const(&fg, inputs[5], 22),
        "arg1 should be 22, got {:?}",
        fg.kind_of_value(inputs[5])
    );
    Ok(())
}

/// Ten push-style stack args, more than any old fixed offset-list length,
/// proving `StackArgs` has no upper bound on collection.
#[test]
fn collects_ten_stack_args() -> Result<()> {
    const N: usize = 10;
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let mut sp_cur = b.read_variable(&sp)?;
    // Each `push` decrements SP and stores, so pushing argN..arg0 leaves the
    // most recent (arg0) at the chain head, slot 0.  Value `100 + i` identifies
    // arg `i`.
    for i in (0..N).rev() {
        sp_cur = b.build_sub_as_add_neg(sp_cur, four, ValueType::I32)?;
        b.write_variable(&sp, sp_cur)?;
        let arg = b.build_int_const((100 + i) as u64, ValueType::I32)?;
        b.build_store(sp_cur, arg, rsleigh::VnSpace::RAM)?;
    }

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + N stack args.
    assert_eq!(
        inputs.len(),
        4 + N,
        "expected ctrl+mem+target+sp+{N} stack args; got {inputs:?}"
    );
    for i in 0..N {
        assert!(
            is_const(&fg, inputs[4 + i], (100 + i) as u64),
            "stack arg {i} should be {}, got {:?}",
            100 + i,
            fg.kind_of_value(inputs[4 + i])
        );
    }
    Ok(())
}

/// Slots 0 and 2 filled, slot 1 not.  Only the dense prefix is collected, so
/// exactly one arg is wired despite slot 2 holding a plausible store.
/// Over-collection applies WITHIN a contiguous window; a hole truncates it.
#[test]
fn slot_hole_truncates_collection_to_dense_prefix() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v = b.read_variable(&sp)?;
    let arg0 = b.build_int_const(0xA0u64, ValueType::I32)?;
    b.build_store(sp_v, arg0, rsleigh::VnSpace::RAM)?;
    // Slot 2 at sp+8; slot 1 (sp+4) is left unfilled.
    let eight = b.build_int_const(8u64, ValueType::I32)?;
    let addr8 = b.build_int_binary_operation(sp_v, eight, IntBinaryOp::Add, ValueType::I32)?;
    let arg2 = b.build_int_const(0xA2u64, ValueType::I32)?;
    b.build_store(addr8, arg2, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // The hole at slot 1 truncates the window before slot 2.
    assert_eq!(
        inputs.len(),
        5,
        "only the dense prefix (slot 0) is collected across the hole"
    );
    assert!(
        is_const(&fg, inputs[4], 0xA0),
        "the collected arg must be slot 0's 0xA0, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    Ok(())
}

/// One store at slot 0 under an AArch64-style `[0, 4]` table, so exactly one
/// positional arg is appended even with the higher slots missing.
#[test]
fn single_arg_collected_when_higher_slot_missing() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v1)?;
    let only_arg = b.build_int_const(99u64, ValueType::I32)?;
    b.build_store(sp_v1, only_arg, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + memory + target + sp + the single stack arg.
    assert_eq!(inputs.len(), 5, "only one stack arg could be collected");
    Ok(())
}

/// An unfilled slot 0 must leave the collection empty.  The chain anchor is
/// the implicit ret-addr push at sp-4, which is not in the `[4, 8]` slot
/// table, and only slot 1 is filled, so the dense prefix is empty.
#[test]
fn missing_slot_zero_skips_collection() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;

    // rel = 8 from the anchor at sp-4 below, filling slot 1 of the [4, 8] table.
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

    // Chain anchor.  rel = 0 is not in the [4, 8] slot table, so the
    // `is_first_store` exception lets the walk continue.
    let sp_minus_4 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_minus_4)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_minus_4, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let before_inputs = fg.node_inputs(find_call(fg.graph())?).len();

    let mut pipeline = cf_rp_pipeline();
    // Ret addr at offset 0 from the anchor, args at +4 and +8.  Slot 0 is
    // absent, so the dense prefix is empty.
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let after_inputs = fg.node_inputs(find_call(fg.graph())?).len();
    assert_eq!(
        before_inputs, after_inputs,
        "no args should have been collected when slot 0 is missing"
    );
    Ok(())
}

#[test]
fn call_with_no_stack_stores_unchanged() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let _sp_val = b.read_variable(&sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let before_inputs = fg.node_inputs(find_call(fg.graph())?).len();

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let after_inputs = fg.node_inputs(find_call(fg.graph())?).len();
    assert_eq!(
        before_inputs, after_inputs,
        "no args should have been collected"
    );
    Ok(())
}

/// A disjoint in-frame SP-relative store between two arg pushes is collected
/// as another argument, not treated as a terminator: it is indistinguishable
/// from a real push.  The trash at `sp+0` occupies slot 2 (`sp-8` and `sp-4`
/// being slots 0 and 1), so the contiguous window is 0,1,2.
#[test]
fn disjoint_in_window_store_is_collected_not_a_terminator() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

    // Trash store at sp+0, inside the cdecl arg-offset range and addressing
    // the same memory class as an arg slot.
    let trash = b.build_int_const(0xAAAAu64, ValueType::I32)?;
    b.build_store(sp_v0, trash, rsleigh::VnSpace::RAM)?;

    let sp_v2 = b.build_sub_as_add_neg(sp_v1, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + 3 collected args at slots 0,1,2.
    let collected: Vec<u128> = inputs[4..]
        .iter()
        .map(|&v| const_val(&fg, v, "trash_in_arg_window"))
        .collect();
    // The in-window "trash" at slot 2 is indistinguishable from a real arg.
    assert_eq!(
        collected,
        vec![11, 22, 0xAAAA],
        "every reaching SP-relative store in the contiguous window is collected"
    );
    Ok(())
}

/// Soundness floor under `Strict`: a non-SP-rooted Store between the pushes
/// and the Call terminates the walk, so only the push closest to the Call is
/// collected.  Models the `volatile int g = ...;` barrier `gcc -O2` interleaves
/// with stack-arg pushes.  `StackGlobalDisjoint` recovers the upstream args.
#[test]
fn strict_walker_terminates_at_non_aliasing_global_store() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

    // Volatile global write, cross-class against the stack-arg slots.
    let global_addr = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
    let global_data = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(global_addr, global_data, rsleigh::VnSpace::RAM)?;

    let sp_v2 = b.build_sub_as_add_neg(sp_v1, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // Pin Strict explicitly: the default is `StackGlobalDisjoint`, which would
    // step through the global write instead.
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::test_support::octx_strict())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + memory + target + sp + arg0 = 5.
    assert_eq!(
        inputs.len(),
        5,
        "strict walker collects only the most-recent push before the global \
         terminator; got inputs={inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 11),
        "arg0 should be 11, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    Ok(())
}

/// Multi-store stress on the same floor: the first non-SP store terminates the
/// walk, so no stack args reach the Call at all.
#[test]
fn strict_walker_collects_no_args_when_first_chain_node_is_global_store() -> Result<()> {
    let sp = stack_vn();
    let arg_vals: [u64; 4] = [11, 22, 33, 44];
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_initial = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;

    let mut sp_cur = sp_initial;
    let global_data = b.build_int_const(0x1234u64, ValueType::I32)?;
    // A global write after each push means the chain backward from the Call
    // starts on the final global write, terminating before any arg.
    for (i, base_global_addr) in [0xCAFE0000u64, 0xCAFE0010, 0xCAFE0020, 0xCAFE0030]
        .into_iter()
        .enumerate()
    {
        let arg_idx = 3 - i;
        sp_cur = b.build_sub_as_add_neg(sp_cur, four, ValueType::I32)?;
        b.write_variable(&sp, sp_cur)?;
        let arg = b.build_int_const(arg_vals[arg_idx], ValueType::I32)?;
        b.build_store(sp_cur, arg, rsleigh::VnSpace::RAM)?;

        let g_addr = b.build_int_const(base_global_addr, ValueType::I32)?;
        b.build_store(g_addr, global_data, rsleigh::VnSpace::RAM)?;
    }

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // Pin Strict explicitly; the default is `StackGlobalDisjoint`.
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::test_support::octx_strict())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // The trailing global write is the most-recent chain node, so the walk
    // stops immediately: ctrl + memory + target + sp = 4.
    assert_eq!(
        inputs.len(),
        4,
        "strict walker terminates at the leading global write; no stack args \
         collected; got inputs={inputs:?}"
    );
    Ok(())
}

// Chain order is not slot order: on i386 cdecl, gcc/clang -O2 routinely emits
// pushes in source order while the memory chain reflects program order, so the
// LAST arg stored is the most recent on the chain.  The original walker
// required successive stores at `anchor + stack_arg_offsets[args.len()]`, which
// only matches when the compiler happens to push in slot-descending order.
// Collection must succeed for any chain order, as long as every store's offset
// belongs to the convention's stack-arg-offset set.

/// i386 `free(arg0, arg1)`, the original repro from `exec_free_args` in the
/// FreeBSD i386 10.0 kernel.  Args go to `(%esp)` and `0x4(%esp)` in program
/// order, then `call` pushes the return address at sp-4, so the chain backward
/// is ret-addr, arg1, arg0.  Both args must be collected even though arg1 is
/// the most-recent stack store.
#[test]
fn cdecl_args_pushed_in_program_order_collected() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // In cdecl the outgoing-args region sits at the bottom of the frame and is
    // written without first decrementing SP.
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

    let sp_after_call_push = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_after_call_push)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // x86 cdecl: ret addr at offset 0, args at +4, +8, +12, ...
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+sp+2 stack args; got {inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 11),
        "arg0 should be 11, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    assert!(
        is_const(&fg, inputs[5], 22),
        "arg1 should be 22, got {:?}",
        fg.kind_of_value(inputs[5])
    );
    Ok(())
}

/// i386 `kmap_free_wakeup(arg0, arg1, arg2)`, the second repro from the same
/// function.  The compiler stores arg1, arg0, arg2 in that order, so no two
/// successive chain stores are at adjacent slots and the original in-order
/// walker bailed with `args = []`.  All three must land in the right slots.
#[test]
fn cdecl_three_args_in_arbitrary_order_collected() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let eight = b.build_int_const(8u64, ValueType::I32)?;

    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    let sp_plus_8 = b.build_int_binary_operation(sp_v0, eight, IntBinaryOp::Add, ValueType::I32)?;
    let arg2 = b.build_int_const(33u64, ValueType::I32)?;
    b.build_store(sp_plus_8, arg2, rsleigh::VnSpace::RAM)?;

    let sp_after_call_push = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_after_call_push)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        7,
        "expected ctrl+mem+target+sp+3 stack args; got {inputs:?}"
    );
    for (slot_idx, expected) in [11u64, 22, 33].iter().enumerate() {
        assert!(
            is_const(&fg, inputs[4 + slot_idx], *expected),
            "arg{slot_idx} should be {expected}, got {:?}",
            fg.kind_of_value(inputs[4 + slot_idx])
        );
    }
    Ok(())
}

/// When one slot is written twice, the callee sees the MOST RECENT value.
/// Walking backward, the first sighting of a slot is that most recent write;
/// later sightings are stale and must be ignored.
#[test]
fn most_recent_value_wins_for_repeated_slot() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;

    // Stale arg0, the older write.
    let stale = b.build_int_const(0xBADu64, ValueType::I32)?;
    b.build_store(sp_v0, stale, rsleigh::VnSpace::RAM)?;

    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

    // Overwrites the stale value.
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    let sp_after_call_push = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_after_call_push)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+sp+2 stack args; got {inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 11),
        "arg0 must be the most-recent write (11), not the stale 0xBAD; got {:?}",
        fg.kind_of_value(inputs[4])
    );
    Ok(())
}

/// A store outside the convention's stack-arg window must terminate the walk,
/// which is the safety property the original in-order rule provided, now
/// expressed as set membership.
///
/// Chain order, latest first: `ret-addr@-12, arg0@-8, arg1@-4, local@-16`.
/// The local at -16 is at relative offset -4 from the anchor and not in the
/// slot table, so aborting there keeps its value from leaking into an arg slot
/// should the convention table ever grow.
#[test]
fn out_of_window_stack_store_terminates_walk() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sixteen = b.build_int_const(16u64, ValueType::I32)?;

    // Above the outgoing-args region, so not in the stack-arg slot set.
    let sp_minus_16 = b.build_sub_as_add_neg(sp_v0, sixteen, ValueType::I32)?;
    let local = b.build_int_const(0xDEADu64, ValueType::I32)?;
    b.build_store(sp_minus_16, local, rsleigh::VnSpace::RAM)?;

    let sp_minus_4 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_minus_4, arg1, rsleigh::VnSpace::RAM)?;

    let eight = b.build_int_const(8u64, ValueType::I32)?;
    let sp_minus_8 = b.build_sub_as_add_neg(sp_v0, eight, ValueType::I32)?;
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_minus_8, arg0, rsleigh::VnSpace::RAM)?;

    let twelve = b.build_int_const(12u64, ValueType::I32)?;
    let sp_minus_12 = b.build_sub_as_add_neg(sp_v0, twelve, ValueType::I32)?;
    b.write_variable(&sp, sp_minus_12)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_minus_12, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // 2-slot cdecl table: ret-addr anchor at +0, arg0 at +4, arg1 at +8.
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    let collected: Vec<u128> = inputs[3..]
        .iter()
        .filter_map(|&out| {
            if matches!(fg.kind_of_value(out), NodeKind::IntConst(_)) {
                fg.int_const_u128(out)
            } else {
                None
            }
        })
        .collect();
    assert!(
        !collected.contains(&0xDEAD_u128),
        "OOW local 0xDEAD must not be collected as an arg; got {collected:?}"
    );
    // Termination only bounds the upstream walk, not what is already collected.
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+sp+2 stack args; got {inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 11),
        "arg0 should be 11, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    assert!(
        is_const(&fg, inputs[5], 22),
        "arg1 should be 22, got {:?}",
        fg.kind_of_value(inputs[5])
    );
    Ok(())
}

/// With no per-call override the function-default table is used.
#[test]
fn call_stack_arg_collect_uses_default_when_no_override() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg0 = b.build_int_const(77u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg0, rsleigh::VnSpace::RAM)?;

    // Ret-addr-push anchor.
    let anchor = b.build_int_const(0xABCDu64, ValueType::I32)?;
    b.build_store(sp_v0, anchor, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x2000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // No side-table entry, so the default offsets [4, 8] apply.
    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + arg0 = 5 inputs.
    assert_eq!(
        inputs.len(),
        5,
        "default-CC arg at offset +4 must be collected; got inputs={inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 77),
        "arg0 should be IntConst(77), got {:?}",
        fg.kind_of_value(inputs[4])
    );
    Ok(())
}

/// A per-call override wins over the function-default table.  The default is
/// `[4, 8]` and the override `[0, 4]`, so the store at `sp + 0` is slot 0
/// under the override but outside the default and would otherwise be missed.
#[test]
fn call_stack_arg_collect_uses_override_when_present() -> Result<()> {
    #![allow(clippy::unwrap_used)]

    let sp = stack_vn();

    // Slot 0 under the override table, absent from the default one.
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let arg0 = b.build_int_const(66u64, ValueType::I32)?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x5000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // The offsets override is derived from this stored CC.
    let override_cc = strider_target::BuiltCallingConvention::try_new(
        strider_target::BuiltCallingConventionParts {
            arg_passing_regs: vec![],
            callee_saved_regs: vec![],
            ret_val_regs: vec![],
            ret_val_regs_float: vec![],
            stack_vn: sp,
            stack_args: Some(strider_target::StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
            link_register_vn: None,
            preserves_memory: false,
        },
    )
    .unwrap();
    let call_id = fg
        .walk_kind(|k| matches!(k, NodeKind::Call))
        .next()
        .expect("Call node must exist");
    fg.side_tables_mut().set_call_cc(call_id, override_cc);

    // Run with the default table [4, 8]; the pass must read the override.
    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let call_id_post = fg
        .walk_kind(|k| matches!(k, NodeKind::Call))
        .next()
        .expect("Call node must still exist");
    let inputs: Vec<ValueId> = fg.node_inputs(call_id_post).into_iter().collect();
    // ctrl + mem + target + sp + the arg at +0 = 5 inputs.
    assert_eq!(
        inputs.len(),
        5,
        "override CC [0,4] must collect arg at offset +0; got {inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 66),
        "arg0 should be IntConst(66) from override table, got {:?}",
        fg.kind_of_value(inputs[4])
    );
    Ok(())
}

/// The store's offset must come from `Function::stack_offsets` when an entry
/// exists, not from re-deriving it.  Here the arg0 store's address is replaced
/// with an opaque constant so decomposition returns `None`, and the side-table
/// is populated by hand as `StackOffsetDetect` would have.  Reading the
/// side-table collects the arg; re-deriving passes through and collects none.
#[test]
fn call_stack_arg_collect_reads_offset_from_side_table_not_decompose() -> Result<()> {
    #![allow(clippy::unwrap_used)]

    let sp = stack_vn();
    // store arg0=77 at sp+4, anchor at sp+0, then call.
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg0 = b.build_int_const(77u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg0, rsleigh::VnSpace::RAM)?;

    // Anchor store at sp+0 (ret-addr-push role).
    let anchor = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_v0, anchor, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x2000u64, ValueType::I32)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Canonicalize so decomposition would work if it were used.
    {
        let prep = cf_rp_pipeline();
        prep.run(&mut fg, &mut crate::OptCtx::new(None))?;
    }

    // The arg0 store is the one whose data input is IntConst(77).
    let arg0_store = fg
        .walk()
        .find(|&n| {
            if !matches!(fg.node_kind(n), NodeKind::Store(_)) {
                return false;
            }
            let inputs = fg.node_inputs(n);
            inputs.len() == 3 && is_const(&fg, inputs[2], 77)
        })
        .expect("arg0 Store(IntConst(77)) must exist");

    // Slot 1 is the address.  Making it opaque means a walker without the
    // side-table would pass through this store and miss arg0.
    let opaque_addr = {
        let mut ef = strider_ir::EditFunction::new(&mut fg);
        ef.build_int_const(0xDEAD_BEEFu64, ValueType::I32).unwrap()
    };
    let addr_input_id = fg.node_input_id_at(arg0_store, 1).unwrap();
    fg.graph_mut().update_input(addr_input_id, opaque_addr);

    // The sentinel fingerprint keeps IR validation from rejecting the
    // manually-wired graph.
    let opaque_producer = fg.producer(opaque_addr);
    fg.side_tables_mut().extend_asm_fingerprint(
        opaque_producer,
        &[strider_ir_test_utils::SENTINEL_LIFT_ADDR],
    );

    // The base must equal what the anchor store at sp+0 resolves to, so both
    // stores agree on one SP root and pass the base-consistency check.
    let sp_base = fg
        .walk_kind(|k| matches!(*k, NodeKind::InitialVar(id) if fg.initial_vn(id) == sp))
        .next()
        .map(|n| fg.node_outputs(n).iter().copied().next().unwrap())
        .expect("InitialVar(sp) node must exist");
    let arg0_store_addr = fg.store_addr(arg0_store);
    fg.side_tables_mut()
        .set_stack_slot(arg0_store_addr, sp_base, 4);

    // Slot table [4, 8], with offset 0 as the anchor.
    let pass = CallStackArgCollect;
    crate::pipeline::run_post(&pass, &mut fg, &mut crate::OptCtx::new(None))?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + arg0 = 5 inputs.
    assert_eq!(
        inputs.len(),
        5,
        "side-table offset must be used to collect arg0 even with opaque address; got inputs={inputs:?}"
    );
    assert!(
        is_const(&fg, inputs[4], 77),
        "arg0 should be IntConst(77), got {:?}",
        fg.kind_of_value(inputs[4])
    );
    Ok(())
}
