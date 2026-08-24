use super::*;
use crate::IRViewer;
use anyhow::anyhow;

use crate::error::Result;
use crate::node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp, NodeKind, ValueKind, ValueType,
};
use cranelift_entity::EntityRef;
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

/// Local stand-in for `strider_ir_test_utils::builder`, which cannot be used
/// here: under `cargo test` the dev-dep links a separate compilation of
/// strider-ir, so its `FunctionBuilder` is a different type.
///
/// Leaves `lift_addr` as `None`.
fn raw_builder(
    tracked: Vec<rsleigh::Vn>,
    arg_passing: &[rsleigh::Vn],
    callee_saved: &[rsleigh::Vn],
    ret_val: &[rsleigh::Vn],
    stack_vn: Option<rsleigh::Vn>,
    ret_stack_pop: i64,
    endianness: strider_target::Endianness,
) -> Result<FunctionBuilder> {
    let cc = strider_target::BuiltCallingConvention {
        arg_passing_regs: arg_passing.to_vec(),
        callee_saved_regs: callee_saved.to_vec(),
        ret_val_regs: ret_val.to_vec(),
        ret_val_regs_float: Vec::new(),
        stack_vn: stack_vn
            .unwrap_or_else(|| strider_target::BuiltCallingConvention::default().stack_vn),
        stack_args: None,
        ret_stack_pop,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    };
    // Mirror the lifter and track the stack vn: `build_call` reads SP from the
    // variable table.
    let mut tracked = tracked;
    if !tracked.contains(&cc.stack_vn) {
        tracked.push(cc.stack_vn);
    }
    FunctionBuilder::new(tracked, cc, endianness)
}

fn empty_builder() -> Result<FunctionBuilder> {
    raw_builder(
        vec![],
        &[],
        &[],
        &[],
        None,
        0,
        strider_target::Endianness::Little,
    )
}

fn producer_kind(b: &FunctionBuilder, value: ValueId) -> NodeKind {
    *b.function().kind_of_value(value)
}

#[track_caller]
fn assert_const_folded(b: &FunctionBuilder, value: ValueId, expected: u64) {
    assert_eq!(
        b.int_const_u128(value),
        Some(u128::from(expected)),
        "folded constant must read back as {expected:#x}"
    );
    assert!(
        matches!(producer_kind(b, value), NodeKind::IntConst(_)),
        "expected an IntConst producer, got {:?}",
        producer_kind(b, value)
    );
}

/// `Add(1, 2)` at width `ty`. The builder never constant-folds binary ops, so
/// the producer really is an `IntBinaryOp`.
fn non_const_add(b: &mut FunctionBuilder, ty: ValueType) -> Result<ValueId> {
    let lhs = b.build_int_const(1u64, ty)?;
    let rhs = b.build_int_const(2u64, ty)?;
    b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ty)
}

#[test]
fn get_unsigned_int_truncates_to_declared_width() -> Result<()> {
    let mut b = empty_builder()?;
    let value = b.build_int_const(u8::MAX as u64 + 1, ValueType::I8)?;
    let val = b.int_const_u128(value);
    assert_eq!(val, Some(0)); // 256 & 0xFF == 0
    Ok(())
}

#[test]
fn get_as_int_accepts_bool_const() -> Result<()> {
    let mut b = empty_builder()?;
    let bt = b.build_boolean_const(true);
    let bf = b.build_boolean_const(false);
    // In a 1-bit integer the single bit IS the sign bit, so `true` is -1
    // signed, 1 unsigned.
    assert_eq!(
        b.int_const_u128(bt).zip(b.int_const_i128(bt)),
        Some((1u128, -1i128))
    );
    assert_eq!(
        b.int_const_u128(bf).zip(b.int_const_i128(bf)),
        Some((0u128, 0i128))
    );
    Ok(())
}

#[test]
fn get_unsigned_int_is_none_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let add = non_const_add(&mut b, ValueType::I64)?;
    assert_eq!(b.int_const_u128(add), None);
    Ok(())
}

#[test]
fn get_signed_int_sign_extension_cases() -> Result<()> {
    let cases: [(&str, u64, i128); 2] = [
        (
            "get_signed_int_sign_extends_negative_u8",
            u8::MAX as u64,
            -1,
        ),
        (
            "get_signed_int_positive_u8_stays_positive",
            i8::MAX as u64,
            i8::MAX as i128,
        ),
    ];
    let mut b = empty_builder()?;
    for (label, raw, expected) in cases {
        let value = b.build_int_const(raw, ValueType::I8)?;
        assert_eq!(b.int_const_i128(value), Some(expected), "{label}");
    }
    Ok(())
}

#[test]
fn truncate_const_folds_to_const() -> Result<()> {
    let mut b = empty_builder()?;
    let value = b.build_int_const(0xABCDu64, ValueType::I16)?;
    let truncated = b.truncate_if_needed(value, ValueType::I8)?;
    assert_const_folded(&b, truncated, 0xCD);
    Ok(())
}

/// The no-op path applies only to non-const values: a constant always folds
/// into a fresh node whichever direction the width moves.
#[test]
fn truncate_noop_when_already_narrow_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let add = non_const_add(&mut b, ValueType::I8)?;
    let result = b.truncate_if_needed(add, ValueType::I16)?;
    assert_eq!(
        result, add,
        "non-const I8 value must not be touched when target is I16"
    );
    Ok(())
}

#[test]
fn truncate_emits_truncate_node_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let add = non_const_add(&mut b, ValueType::I32)?;
    let truncated = b.truncate_if_needed(add, ValueType::I8)?;
    assert!(
        matches!(producer_kind(&b, truncated), NodeKind::Truncate),
        "expected Truncate node, got {:?}",
        producer_kind(&b, truncated)
    );
    Ok(())
}

#[test]
fn extend_const_folds_to_wider_const() -> Result<()> {
    let cases: [(&str, ExtendOp, u128); 2] = [
        (
            "zero_extend_const_folds_to_wider_const",
            ExtendOp::ZeroExtend,
            u8::MAX as u128,
        ),
        (
            "sign_extend_const_folds_negative_value",
            ExtendOp::SignExtend,
            u32::MAX as u128,
        ),
    ];
    for (label, op, expected) in cases {
        let mut b = empty_builder()?;
        let value = b.build_int_const(u8::MAX as u64, ValueType::I8)?;
        let extended = b.extend_if_needed(value, ValueType::I32, op)?;
        assert_eq!(b.int_const_u128(extended), Some(expected), "{label}");
        assert!(
            matches!(producer_kind(&b, extended), NodeKind::IntConst(_)),
            "{label}: const extend must fold to IntConst"
        );
    }
    Ok(())
}

#[test]
fn extend_emits_extend_node_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let add = non_const_add(&mut b, ValueType::I8)?;
    let extended = b.extend_if_needed(add, ValueType::I64, ExtendOp::ZeroExtend)?;
    assert!(
        matches!(producer_kind(&b, extended), NodeKind::Extend(_)),
        "expected Extend node"
    );
    Ok(())
}

#[test]
fn extend_noop_when_already_wide_enough() -> Result<()> {
    let mut b = empty_builder()?;
    let add = non_const_add(&mut b, ValueType::I64)?;
    let result = b.extend_if_needed(add, ValueType::I64, ExtendOp::ZeroExtend)?;
    assert_eq!(result, add);
    Ok(())
}

#[test]
fn extend_noop_when_wider_than_target() -> Result<()> {
    let mut b = empty_builder()?;
    let wide_add = non_const_add(&mut b, ValueType::I64)?;
    assert_eq!(
        b.extend_if_needed(wide_add, ValueType::I32, ExtendOp::ZeroExtend)?,
        wide_add,
        "extend on a value wider than the target is a no-op"
    );
    let wide_const = b.build_int_const(0xDEAD_BEEFu64, ValueType::I64)?;
    assert_eq!(
        b.extend_if_needed(wide_const, ValueType::I32, ExtendOp::SignExtend)?,
        wide_const,
        "extend on a constant wider than the target is a no-op"
    );
    Ok(())
}

/// This layer does no constant folding, so two constants still get a node.
#[test]
fn build_int_binary_op_produces_binary_op_node() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(3u64, ValueType::I64)?;
    let rhs = b.build_int_const(4u64, ValueType::I64)?;
    let result = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I64)?;
    let node = b.function().producer(result);
    assert_eq!(
        b.function().node_kind(node),
        &NodeKind::IntBinaryOp(IntBinaryOp::Add)
    );
    Ok(())
}

#[test]
fn build_int_binary_op_coerces_narrower_operand() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, ValueType::I8)?;
    let rhs = b.build_int_const(2u64, ValueType::I64)?;
    let lhs = b.convert_to_int_if_needed(lhs, ValueType::I64)?;
    let result = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I64)?;
    let kind = b.function().value_kind(result);
    assert_eq!(kind, ValueKind::Typed(ValueType::I64));
    Ok(())
}

#[test]
fn build_int_cmp_produces_bool_output() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(10u64, ValueType::I32)?;
    let rhs = b.build_int_const(20u64, ValueType::I32)?;
    let result = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, ValueType::I32)?;
    let kind = b.function().value_kind(result);
    assert_eq!(kind, ValueKind::Typed(ValueType::I1));
    Ok(())
}

/// Boolean AND is a bitwise `IntBinaryOp(And)` at `I1`.
#[test]
fn build_boolean_operation_produces_bool_binary_node() -> Result<()> {
    let mut b = empty_builder()?;
    let t = b.build_boolean_const(true);
    let f = b.build_boolean_const(false);
    let result = b.build_int_binary_operation(t, f, IntBinaryOp::And, ValueType::I1)?;
    let node = b.function().producer(result);
    assert_eq!(
        b.function().node_kind(node),
        &NodeKind::IntBinaryOp(IntBinaryOp::And)
    );
    assert_eq!(
        b.function().value_kind(result),
        ValueKind::Typed(ValueType::I1)
    );
    Ok(())
}

#[test]
fn identical_constants_are_deduplicated() -> Result<()> {
    let mut b = empty_builder()?;
    let a = b.build_int_const(77u64, ValueType::I32)?;
    let c = b.build_int_const(77u64, ValueType::I32)?;
    assert_eq!(a, c, "same constant must reuse the same node");
    Ok(())
}

#[test]
fn different_constants_are_distinct() -> Result<()> {
    let mut b = empty_builder()?;
    let a = b.build_int_const(1u64, ValueType::I32)?;
    let c = b.build_int_const(2u64, ValueType::I32)?;
    assert_ne!(a, c);
    Ok(())
}

#[test]
fn build_float_const_has_correct_bits() -> Result<()> {
    let cases: [(&str, u64, ValueType); 2] = [
        (
            "build_float_const_f32_has_correct_bits",
            1.0f32.to_bits() as u64,
            ValueType::F32,
        ),
        (
            "build_float_const_f64_has_correct_bits",
            1.0f64.to_bits(),
            ValueType::F64,
        ),
    ];
    let mut b = empty_builder()?;
    for (label, bits, ty) in cases {
        let value = b.build_float_const(bits, ty);
        assert_eq!(
            producer_kind(&b, value),
            NodeKind::FloatConst(bits),
            "{label}"
        );
        assert_eq!(
            b.function().value_kind(value),
            ValueKind::Typed(ty),
            "{label}"
        );
    }
    Ok(())
}

#[test]
fn int_bits_to_float_folds_int_const_immediately() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f32.to_bits() as u64;
    let int_value = b.build_int_const(bits, ValueType::I32)?;
    let float_value = b.build_int_bits_to_float(int_value, ValueType::F32)?;
    let kind = *b.function().kind_of_value(float_value);
    assert_eq!(kind, NodeKind::FloatConst(bits));
    Ok(())
}

#[test]
fn float_bits_to_int_folds_float_const_immediately() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f64.to_bits();
    let float_value = b.build_float_const(bits, ValueType::F64);
    let int_value = b.build_float_bits_to_int(float_value, ValueType::I64)?;
    let kind = *b.function().kind_of_value(int_value);
    assert!(matches!(kind, NodeKind::IntConst(_)));
    assert_eq!(
        b.function().int_const_u128(int_value),
        Some(u128::from(bits))
    );
    Ok(())
}

#[test]
fn int_bits_to_float_rejects_width_mismatch() -> Result<()> {
    let mut b = empty_builder()?;
    let int_value = b.build_int_const(0u64, ValueType::I64)?;
    assert!(
        b.build_int_bits_to_float(int_value, ValueType::F32)
            .is_err()
    );
    Ok(())
}

#[test]
fn float_bits_to_int_rejects_width_mismatch() -> Result<()> {
    let mut b = empty_builder()?;
    let float_value = b.build_float_const(0u64, ValueType::F64);
    assert!(
        b.build_float_bits_to_int(float_value, ValueType::I32)
            .is_err()
    );
    Ok(())
}

#[test]
fn build_float_binary_op_produces_correct_node() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_float_const(1.0f32.to_bits() as u64, ValueType::F32);
    let rhs = b.build_float_const(2.0f32.to_bits() as u64, ValueType::F32);
    let value = b.build_float_binary_op(lhs, rhs, FloatBinaryOp::Add, ValueType::F32)?;
    let kind = *b.function().kind_of_value(value);
    assert_eq!(kind, NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
    Ok(())
}

#[test]
fn build_float_cmp_op_produces_bool_output() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_float_const(1.0f64.to_bits(), ValueType::F64);
    let rhs = b.build_float_const(2.0f64.to_bits(), ValueType::F64);
    let value = b.build_float_cmp_op(lhs, rhs, FloatCmpOp::Less)?;
    assert_eq!(
        b.function().value_kind(value),
        ValueKind::Typed(ValueType::I1)
    );
    Ok(())
}

#[test]
fn build_int_bits_to_float_inserts_node_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let int_val = b.build_int_const(0x3F800000u64, ValueType::I32)?;
    let zero = b.build_int_const(0u64, ValueType::I32)?;
    // Add(x, 0) keeps the result off the IntConst fold path.
    let non_const =
        b.build_int_binary_operation(int_val, zero, crate::node::IntBinaryOp::Add, ValueType::I32)?;
    let float_value = b.build_int_bits_to_float(non_const, ValueType::F32)?;
    let kind = *b.function().kind_of_value(float_value);
    assert_eq!(kind, NodeKind::IntBitsToFloat);
    Ok(())
}

#[test]
fn cast_to_float_of_int_is_int_bits_to_float() -> Result<()> {
    let mut b = empty_builder()?;
    // Non-const, so the immediate constant fold does not apply.
    let raw = b.build_int_const(42u64, ValueType::I64)?;
    let opaque = b.build_int_unary_operation(raw, crate::node::IntUnaryOp::Neg, ValueType::I64)?;
    let cast = b.cast_to_float_if_needed(opaque, ValueType::F64)?;
    // A same-width conversion is a bitcast.
    assert_eq!(*b.function().kind_of_value(cast), NodeKind::IntBitsToFloat);
    assert_eq!(b.value_type(cast)?, ValueType::F64);
    Ok(())
}

/// Mirrors the lifter's `build_cc_call`: float arguments are appended at their
/// ABI position, a container shared by two registers passes one slice each,
/// and the list stops at the first untracked position.
#[test]
fn build_call_cc_float_args_are_positional_slices() -> Result<()> {
    use strider_ir_test_utils::reg_vn;
    let sp = strider_ir_test_utils::stack_vn_x86_64();
    let q0 = reg_vn(0x100, 16);
    let (d0, d1) = (reg_vn(0x100, 8), reg_vn(0x108, 8));
    // Outside q0, and seeded by `FunctionBuilder::new` like every other ABI
    // argument register, so it is its own tracked container.
    let d2 = reg_vn(0x110, 8);
    let r0 = reg_vn(0x0, 8);

    let cc = strider_target::BuiltCallingConvention {
        arg_passing_regs: vec![r0],
        arg_passing_regs_float: vec![d0, d1, d2],
        callee_saved_regs: vec![q0, r0, sp],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
    };
    let mut b = FunctionBuilder::new(vec![q0, r0, sp], cc, strider_target::Endianness::Little)?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let addr = b.build_int_const(0xdead_beefu64, ValueType::I64)?;
    let call = b.build_call_cc(addr, None)?;

    let f = b.function();
    let inputs: Vec<_> = f.node_inputs(call).into_iter().collect();
    // [ctrl, mem, target, sp, r0, float 0, float 1, float 2].
    assert_eq!(inputs.len(), 8, "one argument per ABI float position");
    let (float0, float1) = (inputs[5], inputs[6]);
    assert_eq!(
        f.value_type(float0)?,
        ValueType::I64,
        "d0 passes its own 8-byte slice, not the 16-byte q0"
    );
    assert_eq!(f.value_type(float1)?, ValueType::I64);
    assert_ne!(float0, float1, "one argument is never two");
    Ok(())
}

/// A 9-byte varnode does not fit `u64`, so its constants take the wide path.
#[test]
fn i72_const_round_trips_through_the_interner() -> Result<()> {
    let mut b = empty_builder()?;
    assert!(ValueType::I72.is_wide_int());
    let v: u128 = (1u128 << 72) - 1;
    let c = b.build_int_const(v, ValueType::I72)?;
    assert_eq!(b.function().int_const_u128(c), Some(v));
    assert_eq!(b.function().int_const_i128(c), Some(-1));
    // Masking to the declared width is what makes the top carry byte readable.
    let over = b.build_int_const(v + 1, ValueType::I72)?;
    assert_eq!(b.function().int_const_u128(over), Some(0));
    Ok(())
}

/// The inferred type must be bitcastable from the integer it was inferred for.
#[test]
fn infer_float_type_matches_input_width() -> Result<()> {
    let mut b = empty_builder()?;
    for (int_ty, float_ty) in [
        (ValueType::I16, ValueType::F16),
        (ValueType::I32, ValueType::F32),
        (ValueType::I64, ValueType::F64),
        (ValueType::I80, ValueType::F80),
        (ValueType::I128, ValueType::F128),
    ] {
        let raw = b.build_int_const(1u64, int_ty)?;
        let opaque = b.build_int_unary_operation(raw, crate::node::IntUnaryOp::Neg, int_ty)?;
        assert_eq!(b.infer_float_type(opaque)?, float_ty, "for {int_ty}");
        let cast = b.cast_to_float_if_needed(opaque, float_ty)?;
        assert_eq!(b.value_type(cast)?, float_ty);
    }
    Ok(())
}

#[test]
fn cast_to_float_if_needed_is_identity_for_same_type() -> Result<()> {
    let mut b = empty_builder()?;
    let float_val = b.build_float_const(1.0f32.to_bits() as u64, ValueType::F32);
    let result = b.cast_to_float_if_needed(float_val, ValueType::F32)?;
    assert_eq!(result, float_val);
    Ok(())
}

#[test]
fn build_float_binary_op_with_int_inputs_bitcasts() -> Result<()> {
    let mut b = empty_builder()?;
    // Non-const operands: a constant would fold into a FloatConst.
    let c1 = b.build_int_const(0x3F800000u64, ValueType::I32)?;
    let c2 = b.build_int_const(0x40000000u64, ValueType::I32)?;
    let i1 = b.build_int_unary_operation(c1, crate::node::IntUnaryOp::Neg, ValueType::I32)?;
    let i2 = b.build_int_unary_operation(c2, crate::node::IntUnaryOp::Neg, ValueType::I32)?;
    let i1 = b.cast_to_float_if_needed(i1, ValueType::F32)?;
    let i2 = b.cast_to_float_if_needed(i2, ValueType::F32)?;
    let result = b.build_float_binary_op(i1, i2, FloatBinaryOp::Add, ValueType::F32)?;
    let kind = *b.function().kind_of_value(result);
    assert_eq!(kind, NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
    let [lhs, rhs] = b
        .function()
        .node_inputs_exact::<2>(b.function().producer(result))?;
    let lhs_node = b.function().producer(lhs);
    let rhs_node = b.function().producer(rhs);
    assert_eq!(*b.function().node_kind(lhs_node), NodeKind::IntBitsToFloat);
    assert_eq!(*b.function().node_kind(rhs_node), NodeKind::IntBitsToFloat);
    Ok(())
}

fn empty_call_other_abi() -> strider_target::BuiltCallOtherAbi {
    strider_target::BuiltCallOtherAbi {
        implicit_reads: Vec::new(),
        implicit_writes: Vec::new(),
        clobbers_memory: false,
        no_return: false,
    }
}

fn builder_with_region() -> Result<FunctionBuilder> {
    let mut b = empty_builder()?;
    let r = b.create_region_all()?;
    b.set_entry_region_all(r)?;
    b.set_region(r);
    Ok(b)
}

/// Tracks `vns` so a CallOther result or implicit-write register has a
/// container to write back into.
fn builder_with_region_tracking(vns: Vec<rsleigh::Vn>) -> Result<FunctionBuilder> {
    let mut b = raw_builder(
        vns,
        &[],
        &[],
        &[],
        None,
        0,
        strider_target::Endianness::Little,
    )?;
    let r = b.create_region_all()?;
    b.set_entry_region_all(r)?;
    b.set_region(r);
    Ok(b)
}

#[test]
fn build_call_other_without_result_advances_ctrl_only() -> Result<()> {
    let mut b = builder_with_region()?;
    let ctrl_before = b.cur_region_control()?;
    let mem_before = b.cur_region_memory()?;

    let (node, result) =
        b.build_call_other_abi(7, "NEON_rev64", &[], &empty_call_other_abi(), None, false)?;
    assert!(result.is_none(), "no output vn -> no ret-val output");

    let ctrl_after = b.cur_region_control()?;
    let mem_after = b.cur_region_memory()?;
    assert_ne!(ctrl_before, ctrl_after);
    assert_eq!(mem_before, mem_after, "memory must NOT advance");

    assert_eq!(
        b.function().node_kind(node),
        &NodeKind::CallOther { user_op_id: 7 }
    );
    Ok(())
}

#[test]
fn build_call_other_with_result_returns_typed_value() -> Result<()> {
    let out_vn = reg_vn(0x10, 4);
    let mut b = builder_with_region_tracking(vec![out_vn])?;
    let arg = b.build_int_const(0x42u64, ValueType::I64)?;
    let (node, result) = b.build_call_other_abi(
        3,
        "cpuid",
        &[arg],
        &empty_call_other_abi(),
        Some(out_vn),
        false,
    )?;
    let val = result.ok_or_else(|| anyhow!("output vn = Some → ret-val output"))?;
    assert_eq!(
        b.function().value_kind(val),
        ValueKind::Typed(ValueType::I32)
    );
    assert_eq!(
        b.function().node_kind(node),
        &NodeKind::CallOther { user_op_id: 3 }
    );
    Ok(())
}

#[test]
fn memory_output_of_finds_call_other_memory_slot() -> Result<()> {
    let out_vn = reg_vn(0x20, 4);
    let mut b = builder_with_region_tracking(vec![out_vn])?;
    let (node, _) = b.build_call_other_abi(
        4,
        "cpuid",
        &[],
        &empty_call_other_abi(),
        Some(out_vn),
        false,
    )?;
    let mem_value = b.function().memory_output_of(node)?;
    assert_eq!(b.function().value_kind(mem_value), ValueKind::Memory);
    Ok(())
}

#[test]
fn memory_output_of_errors_on_node_with_no_memory_output() -> Result<()> {
    let mut b = builder_with_region()?;
    let c = b.build_int_const(7u64, ValueType::I32)?;
    let int_node = b.function().producer(c);
    let err = b
        .function()
        .memory_output_of(int_node)
        .expect_err("IntConst has no Memory output");
    assert!(err.to_string().contains("no Memory output"), "got: {err}");
    Ok(())
}

#[test]
fn memory_input_of_resolves_token_slot_per_kind() -> Result<()> {
    // Pins the token-slot layout: slot 0 for Store/Load, slot 1 for Call,
    // none for MemPhi or the InitialMemory root.
    let mut b = builder_with_region()?;

    let mem_value = b.cur_region_memory()?;
    let mem_phi = b.function().producer(mem_value);
    assert!(matches!(b.function().node_kind(mem_phi), NodeKind::MemPhi));
    assert_eq!(
        b.function().memory_input_of(mem_phi),
        None,
        "MemPhi slot 0 is the phi-token, not a memory input"
    );

    let space = rsleigh::VnSpace::RAM;
    let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
    let data = b.build_int_const(7u64, ValueType::I32)?;
    let mem_before_store = b.cur_region_memory()?;
    b.build_store(addr, data, space)?;
    let store = b.function().producer(b.cur_region_memory()?);
    assert!(matches!(b.function().node_kind(store), NodeKind::Store(_)));
    assert_eq!(
        b.function().memory_input_of(store),
        Some(mem_before_store),
        "Store reads its memory token from input slot 0"
    );
    assert_eq!(
        b.function().memory_input_of(store),
        b.function().node_inputs(store).into_iter().next(),
    );

    let loaded = b.build_load(addr, space, ValueType::I32)?;
    let load = b.function().producer(loaded);
    assert!(matches!(b.function().node_kind(load), NodeKind::Load(_)));
    assert_eq!(
        b.function().memory_input_of(load),
        b.function().node_inputs(load).into_iter().next(),
        "Load reads its memory token from input slot 0"
    );

    let target = b.build_int_const(0x2000u64, ValueType::I64)?;
    let call = b.build_call_cc(target, None)?;
    assert_eq!(
        b.function().memory_input_of(call),
        b.function().node_inputs(call).into_iter().nth(1),
        "Call reads its memory token from input slot 1"
    );

    let int_node = b.function().producer(addr);
    assert_eq!(b.function().memory_input_of(int_node), None);

    Ok(())
}

#[test]
fn build_call_other_rejects_non_value_arg() -> Result<()> {
    let mut b = builder_with_region()?;
    let mem = b.cur_region_memory()?;
    let res = b.build_call_other_abi(0, "cpuid", &[mem], &empty_call_other_abi(), None, false);
    let err = res.expect_err("expected ExpectedValue error");
    assert!(
        err.to_string().contains("is not a value edge"),
        "got: {err}"
    );
    Ok(())
}

/// These need not correspond to any tracked-variable entry.
fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

/// Any value fitting `u128` interns as `Bits` whatever the declared width, so
/// `build_int_const` and `build_int_const_limbs` reach one interned value.
#[test]
fn fitting_values_intern_as_bits() -> Result<()> {
    use crate::node::const_value::ConstValue;
    let sp = reg_vn(0x7000, 8);
    let mut b = raw_builder(
        vec![],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;

    let small = b.build_int_const(5u64, ValueType::I128)?;
    let small_node = b.function().producer(small);
    let NodeKind::IntConst(small_id) = *b.function().node_kind(small_node) else {
        panic!("expected IntConst");
    };
    assert_eq!(b.function().const_value(small_id), &ConstValue::Bits(5));
    assert_eq!(b.function().int_const_u128(small), Some(5u128));
    assert_eq!(b.function().value_type(small)?, ValueType::I128);

    // Exceeds u64, still fits u128, so still `Bits`.
    let big_val: u128 = 1u128 << 100;
    let big = b.build_int_const(big_val, ValueType::I128)?;
    let big_node = b.function().producer(big);
    let NodeKind::IntConst(big_id) = *b.function().node_kind(big_node) else {
        panic!("expected IntConst");
    };
    assert_eq!(b.function().const_value(big_id), &ConstValue::Bits(big_val));
    assert_eq!(b.function().int_const_u128(big), Some(big_val));

    // Limbs that fit u128 collapse to `Bits` too, though the differing width
    // keeps this a distinct node.
    let small2 = b.build_int_const_limbs(&[5, 0, 0, 0], ValueType::I256)?;
    let small2_node = b.function().producer(small2);
    let NodeKind::IntConst(small2_id) = *b.function().node_kind(small2_node) else {
        panic!("expected IntConst");
    };
    assert_eq!(small2_id, small_id, "value 5 must share one ConstId");
    Ok(())
}

/// `all_vns` comes out sorted whatever order the caller passed.
#[test]
fn function_builder_sorts_all_vns_deterministically() -> Result<()> {
    let r_hi = reg_vn(0x40, 8);
    let r_lo = reg_vn(0x10, 8);
    let r_mid = reg_vn(0x20, 8);
    let sp = reg_vn(0x7000, 8);
    let b = raw_builder(
        vec![r_hi, r_mid, r_lo],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let got: Vec<rsleigh::Vn> = b.function().all_vns().to_vec();
    let mut expected = got.clone();
    expected.sort_by_key(|v| (v.addr_space.shortcut_raw(), v.addr_off, v.size));
    assert_eq!(
        got, expected,
        "all_vns must be sorted by (space, off, size)"
    );
    Ok(())
}

/// A vn that is its own container, or has none tracked, maps to itself.
#[test]
fn container_of_resolves_subregister_to_tracked_container() -> Result<()> {
    let rax = reg_vn(0x0, 8);
    let eax = reg_vn(0x0, 4);
    let sp = reg_vn(0x7000, 8);
    let b = raw_builder(
        vec![rax, eax],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let f = b.function();
    let container_of = |vn: &rsleigh::Vn| vn_container::largest_container_in(f.all_vns(), vn);
    assert_eq!(
        container_of(&eax),
        rax,
        "eax must resolve to its rax container"
    );
    assert_eq!(container_of(&rax), rax, "rax is its own container");
    let r9 = reg_vn(0x90, 8);
    assert_eq!(container_of(&r9), r9, "untracked, uncontained -> self");
    Ok(())
}

/// The whole footprint round trip: implicit reads become inputs, implicit
/// writes become clobber outputs written back to their registers.
#[test]
fn build_call_other_from_abi_resolves_footprint() -> Result<()> {
    use strider_target::Endianness;

    // All full 8-byte containers, so reads and writes map straight to the
    // tracked variable.
    let rcx = reg_vn(0x10, 8);
    let rax = reg_vn(0x00, 8);
    let rdx = reg_vn(0x20, 8);
    let out_vn = reg_vn(0x40, 4);
    let mut b = raw_builder(
        vec![rcx, rax, rdx, out_vn],
        &[],
        &[],
        &[],
        None,
        0,
        Endianness::Little,
    )?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);

    let explicit = b.build_int_const(0x42u64, ValueType::I64)?;

    let abi = strider_target::BuiltCallOtherAbi {
        implicit_reads: vec![rcx],
        implicit_writes: vec![rax, rdx],
        clobbers_memory: true,
        no_return: false,
    };

    let mem_before = b.cur_region_memory()?;
    let (node, result) =
        b.build_call_other_abi(5, "syscall", &[explicit], &abi, Some(out_vn), false)?;

    // Inputs are [ctrl, mem], implicit reads, explicit pcode operands.
    let inputs: Vec<ValueId> = b.function().node_inputs(node).into_iter().collect();
    assert_eq!(inputs.len(), 4, "ctrl + mem + 1 implicit read + explicit");
    assert!(matches!(
        b.function().value_kind(inputs[0]),
        ValueKind::Control
    ));
    assert!(matches!(
        b.function().value_kind(inputs[1]),
        ValueKind::Memory
    ));
    assert_eq!(
        inputs[2],
        b.read_variable(&rcx)?,
        "implicit read of register RCX precedes the explicit arg",
    );
    assert_eq!(
        b.function().value_kind(inputs[2]),
        ValueKind::Typed(ValueType::I64),
        "RCX read is I64-typed (8-byte container)",
    );
    assert_eq!(
        inputs[3], explicit,
        "explicit arg follows the implicit read"
    );

    // Outputs are [ctrl, mem, result, RAX, RDX].
    let outs: Vec<ValueId> = b.function().node_outputs(node).to_vec();
    assert_eq!(outs.len(), 5, "ctrl + mem + result + 2 clobbers");
    assert!(matches!(
        b.function().value_kind(outs[0]),
        ValueKind::Control
    ));
    assert!(matches!(
        b.function().value_kind(outs[1]),
        ValueKind::Memory
    ));
    let result_val = result.ok_or_else(|| anyhow!("output vn → a result value"))?;
    assert_eq!(outs[2], result_val, "slot 2 is the returned result value");
    assert_eq!(
        b.function().value_kind(result_val),
        ValueKind::Typed(ValueType::I32),
        "result is typed by the output vn's byte size",
    );
    assert_eq!(
        b.function().get_vn_for_value(result_val),
        Some(out_vn),
        "result output carries the output vn tag",
    );
    assert_eq!(
        b.function().get_vn_for_value(outs[3]),
        Some(rax),
        "clobber slot tags RAX"
    );
    assert_eq!(
        b.function().get_vn_for_value(outs[4]),
        Some(rdx),
        "clobber slot tags RDX"
    );

    let rax_after = b.read_variable(&rax)?;
    assert_eq!(rax_after, outs[3], "RAX rebound to its clobber output");

    let mem_after = b.cur_region_memory()?;
    assert_ne!(mem_before, mem_after, "clobbers_memory → memory advances");

    assert_eq!(
        b.function().side_tables().call_other_name(node),
        Some("syscall")
    );
    Ok(())
}

#[test]
fn build_call_other_rejects_untracked_implicit_write() -> Result<()> {
    let mut b = builder_with_region()?;
    // No tracked variables, so this has no enclosing container.
    let untracked = reg_vn(0, 4);
    let abi = strider_target::BuiltCallOtherAbi {
        implicit_reads: Vec::new(),
        implicit_writes: vec![untracked],
        clobbers_memory: false,
        no_return: false,
    };
    let res = b.build_call_other_abi(11, "bogus", &[], &abi, None, false);
    assert!(res.is_err(), "untracked implicit-write register must error");
    Ok(())
}

#[test]
fn create_node_attributed_unions_contributor_fingerprints() -> Result<()> {
    let mut b = builder_with_region()?;
    b.set_lift_addr(Some(0x100));
    let l = b.build_int_const(5u64, ValueType::I8)?;
    let l_node = b.function().producer(l);
    b.set_lift_addr(Some(0x104));
    let r = b.build_int_const(7u64, ValueType::I8)?;
    let r_node = b.function().producer(r);
    // Go through the graph directly to isolate the helper from the ambient
    // lift_addr stamp.
    b.set_lift_addr(None);
    let or_node = b.function_mut().create_node_attributed(
        NodeKind::IntBinaryOp(IntBinaryOp::Or),
        [l, r],
        [crate::node::ValueKind::Typed(ValueType::I8)],
        &[l_node, r_node],
    );
    let fp = b.function().side_tables().asm_fingerprint(or_node);
    assert!(
        fp.contains(&0x100) && fp.contains(&0x104),
        "create_node_attributed must union both contributors' fingerprints; got {fp:?}"
    );
    Ok(())
}

#[test]
fn create_node_cache_hit_unions_lift_addr_into_fingerprint() -> Result<()> {
    let mut b = builder_with_region()?;

    b.set_lift_addr(Some(0x100));
    let c1 = b.build_int_const(42u64, ValueType::I64)?;
    let c1_node = b.function().producer(c1);

    // Same kind, type and inputs, so this hits the cache.
    b.set_lift_addr(Some(0x104));
    let c2 = b.build_int_const(42u64, ValueType::I64)?;
    let c2_node = b.function().producer(c2);

    assert_eq!(c1_node, c2_node, "cache must return the same NodeId");
    let fp = b.function().side_tables().asm_fingerprint(c1_node);
    assert!(
        fp.contains(&0x100),
        "fingerprint must retain first lift's address (0x100); got {fp:?}"
    );
    assert!(
        fp.contains(&0x104),
        "fingerprint must union the cache-hit lift's address (0x104); got {fp:?}"
    );
    Ok(())
}

#[test]
fn build_call_other_no_args_emits_ctrl_mem_only() -> Result<()> {
    let mut b = builder_with_region()?;
    let (node, result) =
        b.build_call_other_abi(0, "ud2", &[], &empty_call_other_abi(), None, true)?;
    assert!(result.is_none(), "no output vn -> no ret-val output");
    let outs: Vec<_> = b.function().node_outputs(node).to_vec();
    assert_eq!(
        outs.len(),
        2,
        "trap CallOther has exactly [Control, Memory]"
    );
    let kinds: Vec<_> = outs.iter().map(|o| b.function().value_kind(*o)).collect();
    assert!(
        matches!(kinds[0], ValueKind::Control),
        "slot 0 must be Control"
    );
    assert!(
        matches!(kinds[1], ValueKind::Memory),
        "slot 1 must be Memory"
    );
    Ok(())
}

#[test]
fn build_return_self_terminates() -> Result<()> {
    let mut b = builder_with_region()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let val = b.build_int_const(0u64, ValueType::I64)?;
    b.build_return(Some(val), &[])?;
    let ctrl = b.cur_region_control();
    assert!(
        ctrl.is_err(),
        "cur_region_control must fail immediately after build_return (self-terminates); got: {ctrl:?}"
    );
    Ok(())
}

#[test]
fn build_call_other_terminate_true_closes_region() -> Result<()> {
    let mut b = builder_with_region()?;
    b.build_call_other_abi(0, "ud2", &[], &empty_call_other_abi(), None, true)?;
    let ctrl = b.cur_region_control();
    assert!(
        ctrl.is_err(),
        "cur_region_control must fail after build_call_other(terminate=true); got: {ctrl:?}"
    );
    Ok(())
}

#[test]
fn build_call_other_terminate_false_keeps_region_open() -> Result<()> {
    let mut b = builder_with_region()?;
    b.build_call_other_abi(0, "cpuid", &[], &empty_call_other_abi(), None, false)?;
    let ctrl = b.cur_region_control();
    assert!(
        ctrl.is_ok(),
        "region must stay open after build_call_other(terminate=false); got: {ctrl:?}"
    );
    Ok(())
}

#[test]
fn build_segment_op_produces_pure_node() -> Result<()> {
    let mut b = builder_with_region()?;
    let seg = b.build_int_const(0x10u64, ValueType::I16)?;
    let off = b.build_int_const(0x100u64, ValueType::I32)?;
    let value = b.build_segment_op(1, seg, off, ValueType::I64)?;
    let node = b.function().producer(value);
    assert_eq!(
        b.function().node_kind(node),
        &NodeKind::SegmentOp { op_id: 1 }
    );
    assert_eq!(
        b.function().value_kind(value),
        ValueKind::Typed(ValueType::I64)
    );
    Ok(())
}

#[test]
fn build_segment_op_is_cacheable_across_identical_calls() -> Result<()> {
    let mut b = builder_with_region()?;
    let seg = b.build_int_const(0x10u64, ValueType::I16)?;
    let off = b.build_int_const(0x100u64, ValueType::I32)?;
    let a = b.build_segment_op(1, seg, off, ValueType::I64)?;
    let c = b.build_segment_op(1, seg, off, ValueType::I64)?;
    assert_eq!(a, c, "SegmentOp is pure → identical calls must dedup");
    Ok(())
}

#[test]
fn build_cpool_ref_produces_typed_node() -> Result<()> {
    let mut b = builder_with_region()?;
    let r0 = b.build_int_const(0xAAu64, ValueType::I32)?;
    let r1 = b.build_int_const(0xBBu64, ValueType::I32)?;
    let value = b.build_cpool_ref(&[r0, r1], ValueType::I64)?;
    let node = b.function().producer(value);
    assert_eq!(b.function().node_kind(node), &NodeKind::CPoolRef);
    Ok(())
}

#[test]
fn build_cpool_ref_is_not_deduplicated() -> Result<()> {
    let mut b = builder_with_region()?;
    let r0 = b.build_int_const(0xAAu64, ValueType::I32)?;
    let a = b.build_cpool_ref(&[r0], ValueType::I64)?;
    let c = b.build_cpool_ref(&[r0], ValueType::I64)?;
    assert_ne!(
        a, c,
        "CPoolRef is non-cacheable → must yield distinct nodes"
    );
    Ok(())
}

#[test]
fn build_new_produces_typed_node() -> Result<()> {
    let mut b = builder_with_region()?;
    let size = b.build_int_const(32u64, ValueType::I64)?;
    let value = b.build_new(&[size], ValueType::I64)?;
    let node = b.function().producer(value);
    assert_eq!(b.function().node_kind(node), &NodeKind::New);
    Ok(())
}

#[test]
fn build_new_is_not_deduplicated() -> Result<()> {
    let mut b = builder_with_region()?;
    let size = b.build_int_const(32u64, ValueType::I64)?;
    let a = b.build_new(&[size], ValueType::I64)?;
    let c = b.build_new(&[size], ValueType::I64)?;
    assert_ne!(a, c, "each allocation must yield a distinct node");
    Ok(())
}

/// An I1 is an ordinary 1-bit integer, so widening it emits a real
/// `ZeroExtend`.
#[test]
fn extend_if_needed_with_bool_input_inserts_cast_to_int() -> Result<()> {
    let mut b = empty_builder()?;

    // Always true, but this layer does not constant-fold comparisons.
    let lhs = b.build_int_const(1u64, ValueType::I32)?;
    let rhs = b.build_int_const(2u64, ValueType::I32)?;
    let bool_val = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, ValueType::I32)?;

    assert_eq!(
        b.function().value_kind(bool_val),
        ValueKind::Typed(ValueType::I1),
        "comparison must produce I1"
    );

    let extended = b.extend_if_needed(bool_val, ValueType::I32, ExtendOp::ZeroExtend)?;

    assert_eq!(
        b.function().value_kind(extended),
        ValueKind::Typed(ValueType::I32),
        "extend_if_needed must produce I32 when requested"
    );

    let extended_node = b.function().producer(extended);
    assert_eq!(
        *b.function().node_kind(extended_node),
        NodeKind::Extend(ExtendOp::ZeroExtend),
        "I1 → wider int must be a ZeroExtend"
    );
    let first_value = b
        .function()
        .node_inputs(extended_node)
        .into_iter()
        .next()
        .expect("Extend has one input");
    assert_eq!(
        first_value, bool_val,
        "ZeroExtend must consume the I1 comparison result directly"
    );

    Ok(())
}

// F32/F64 fit `FloatConst`'s u64 payload and immediate-fold; F80 does not, so
// the fold must be skipped there.

#[test]
fn int_bits_to_float_f80_emits_node_not_const() -> Result<()> {
    let mut b = empty_builder()?;
    let int_const = b.build_int_const(0xDEAD_BEEF_CAFEu64, ValueType::I80)?;
    let result = b.build_int_bits_to_float(int_const, ValueType::F80)?;
    let node = b.function().producer(result);
    assert_eq!(
        b.function().node_kind(node),
        &NodeKind::IntBitsToFloat,
        "F80 path must emit IntBitsToFloat node, not fold to FloatConst"
    );
    let int_const64 = b.build_int_const(0u64, ValueType::I64)?;
    let result_f64 = b.build_int_bits_to_float(int_const64, ValueType::F64)?;
    let node_f64 = b.function().producer(result_f64);
    assert!(
        matches!(b.function().node_kind(node_f64), NodeKind::FloatConst(_)),
        "F64 path must still fold to FloatConst (regression check)"
    );
    Ok(())
}

#[test]
fn float_bits_to_int_f80_emits_node_not_const() -> Result<()> {
    let mut b = empty_builder()?;
    let float_const = b.build_float_const(0xBEEFu64, ValueType::F80);
    let result = b.build_float_bits_to_int(float_const, ValueType::I80)?;
    let node = b.function().producer(result);
    assert_eq!(
        b.function().node_kind(node),
        &NodeKind::FloatBitsToInt,
        "F80 input must emit FloatBitsToInt node, not fold to IntConst"
    );
    let float_const64 = b.build_float_const(0u64, ValueType::F64);
    let result_u64 = b.build_float_bits_to_int(float_const64, ValueType::I64)?;
    let node_u64 = b.function().producer(result_u64);
    assert!(
        matches!(b.function().node_kind(node_u64), NodeKind::IntConst(_)),
        "F64 path must still fold to IntConst (regression check)"
    );
    Ok(())
}

/// `FloatConst`'s u64 payload cannot carry a 128-bit pattern, so the fold must
/// be skipped for F128 as it is for F80. F16 fits and still folds.
#[test]
fn bits_casts_fold_by_payload_width() -> Result<()> {
    let mut b = empty_builder()?;

    let wide = b.build_int_const(u128::MAX, ValueType::I128)?;
    let f128 = b.build_int_bits_to_float(wide, ValueType::F128)?;
    assert_eq!(
        b.function().node_kind(b.function().producer(f128)),
        &NodeKind::IntBitsToFloat,
        "F128 must not fold through a u64 payload"
    );

    let f128_const = b.build_float_const(0xBEEF, ValueType::F128);
    let back = b.build_float_bits_to_int(f128_const, ValueType::I128)?;
    assert_eq!(
        b.function().node_kind(b.function().producer(back)),
        &NodeKind::FloatBitsToInt,
        "F128 input must not fold through a u64 payload"
    );

    let half = b.build_int_const(0x3C00u64, ValueType::I16)?;
    let f16 = b.build_int_bits_to_float(half, ValueType::F16)?;
    assert!(
        matches!(
            b.function().node_kind(b.function().producer(f16)),
            NodeKind::FloatConst(_)
        ),
        "F16 fits the payload and folds"
    );
    Ok(())
}

use strider_ir_test_utils::stack_vn_x86_64 as sp_vn_u64;

/// SP must come out rebound to `Add(pre_call_SP, IntConst(ret_stack_pop))`.
#[test]
fn build_call_emits_post_call_sp_adjust() -> Result<()> {
    let sp = sp_vn_u64();
    let mut b = raw_builder(
        vec![sp],
        &[],
        &[],
        &[],
        Some(sp),
        8,
        strider_target::Endianness::Little,
    )?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);

    let pre_sp = b.read_variable(&sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::I64)?;
    b.build_call_cc(target, None)?;

    let post_sp = b.read_variable(&sp)?;
    assert_ne!(
        pre_sp, post_sp,
        "SP must be rebound after Call when ret_stack_pop != 0"
    );

    let add_node = b.function().producer(post_sp);
    assert_eq!(
        b.function().node_kind(add_node),
        &NodeKind::IntBinaryOp(IntBinaryOp::Add)
    );

    let inputs: Vec<ValueId> = b.function().node_inputs(add_node).into_iter().collect();
    assert_eq!(inputs.len(), 2, "Add has two inputs");

    let (lhs, rhs) = (inputs[0], inputs[1]);
    assert_eq!(lhs, pre_sp, "Add consumes the pre-call SP output");
    let rhs_kind = *b.function().kind_of_value(rhs);
    assert!(matches!(rhs_kind, NodeKind::IntConst(_)));
    assert_eq!(
        b.function().int_const_u128(rhs),
        Some(8),
        "rhs must be IntConst(ret_stack_pop) = 8"
    );
    Ok(())
}

/// With `ret_stack_pop == 0`, SP flows through the `Call` unchanged and no
/// adjust node appears.
#[test]
fn build_call_no_sp_adjust_when_ret_stack_pop_zero() -> Result<()> {
    let sp = sp_vn_u64();
    let mut b = raw_builder(
        vec![sp],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);

    let pre_sp = b.read_variable(&sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::I64)?;
    b.build_call_cc(target, None)?;

    let post_sp = b.read_variable(&sp)?;
    assert_eq!(
        pre_sp, post_sp,
        "ret_stack_pop = 0 must not introduce a new Add node"
    );
    Ok(())
}

// Sleigh can write a wide UNIQUE varnode then read a narrow slice of it (on
// MIPS, MULT writes a 64-bit unique and a Copy reads 4 bytes of it into $v0),
// so the REGISTER-space overlap filter applies to UNIQUE space too.

fn unique_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: off,
        addr_space: rsleigh::VnSpace::UNIQUE,
        size,
    }
}

/// Only the container is tracked, whether the contained varnode starts at the
/// container's offset or mid-container, mirroring `ah` inside `ax`.
#[test]
fn new_raw_filters_contained_unique_varnodes() -> Result<()> {
    let cases: [(&str, rsleigh::Vn, rsleigh::Vn); 2] = [
        (
            "new_raw_filters_overlapping_unique_varnodes",
            unique_vn(0x100, 8),
            unique_vn(0x100, 4), // same offset, narrower
        ),
        (
            "new_raw_filters_mid_offset_unique_subvarnode",
            unique_vn(0x200, 8),
            unique_vn(0x204, 4), // upper 4 bytes of outer
        ),
    ];
    for (label, outer, inner) in cases {
        let b = raw_builder(
            vec![outer, inner],
            &[],
            &[],
            &[],
            None,
            0,
            strider_target::Endianness::Little,
        )?;
        let tracked: Vec<rsleigh::Vn> = b.function().all_vns().to_vec();
        assert!(
            tracked.contains(&outer),
            "{label}: wider UNIQUE varnode must remain tracked; got {tracked:?}"
        );
        assert!(
            !tracked.contains(&inner),
            "{label}: contained UNIQUE varnode must be filtered; got {tracked:?}"
        );
    }
    Ok(())
}

/// The filter must not over-reach: disjoint varnodes both stay tracked.
#[test]
fn new_raw_keeps_disjoint_unique_varnodes() -> Result<()> {
    let a = unique_vn(0x300, 4);
    let b_vn = unique_vn(0x400, 4); // different offset, disjoint
    let b = raw_builder(
        vec![a, b_vn],
        &[],
        &[],
        &[],
        None,
        0,
        strider_target::Endianness::Little,
    )?;
    let tracked: Vec<rsleigh::Vn> = b.function().all_vns().to_vec();
    assert!(tracked.contains(&a));
    assert!(tracked.contains(&b_vn));
    Ok(())
}

// ARM/AArch64 status flags are 1-byte registers that Sleigh's `cmp` writes
// I1-producing comparisons into. Storing the I1 straight into the variable
// lets a later phi feed an I1 to a consumer expecting a wider integer, which
// the validator rejects; `convert_to_int_if_needed` is the guard.

fn flag_reg_byte() -> rsleigh::Vn {
    // Shaped like an ARM N/Z/V/C flag.
    rsleigh::Vn {
        addr_off: 0x60,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 1,
    }
}

/// A constant boolean's width change folds straight into an `IntConst(1)`
/// typed I8.
#[test]
fn convert_to_int_if_needed_coerces_bool_to_int() -> Result<()> {
    let mut b = empty_builder()?;
    let bool_val = b.build_boolean_const(true);
    assert_eq!(
        b.function().value_kind(bool_val),
        ValueKind::Typed(ValueType::I1),
        "build_boolean_const is I1-typed"
    );
    let coerced = b.convert_to_int_if_needed(bool_val, ValueType::I8)?;
    assert_eq!(
        b.function().value_kind(coerced),
        ValueKind::Typed(ValueType::I8),
        "convert_to_int_if_needed must produce the requested int type"
    );
    let coerced_node = b.function().producer(coerced);
    assert!(matches!(
        b.function().node_kind(coerced_node),
        NodeKind::IntConst(_)
    ));
    assert_eq!(
        b.function().int_const_u128(coerced),
        Some(1),
        "constant I1 → I8 must fold to IntConst(1) typed I8"
    );
    Ok(())
}

/// The declared ret regs come back verbatim, with no container projection,
/// even when a wider view is the tracked one.
#[test]
fn ret_val_vars_returns_declared_reg_verbatim() -> Result<()> {
    let f0_4byte = rsleigh::Vn {
        addr_off: 0x1000,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let f0_f1_8byte = rsleigh::Vn {
        addr_off: 0x1000,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    // The overlap filter keeps only the 8-byte view as tracked.
    let b = raw_builder(
        vec![f0_4byte, f0_f1_8byte],
        &[],
        &[],
        &[f0_4byte],
        None,
        0,
        strider_target::Endianness::Little,
    )?;
    assert_eq!(
        b.function().ret_val_regs(),
        &[f0_4byte],
        "ret_val_regs returns the declared ret reg verbatim (no projection)"
    );
    Ok(())
}

/// Pins a representative ABI shape: a sub-register ret upgrade and a
/// caller-clobbered split of ret-prefix then the rest.
#[test]
fn projected_cc_lists_match_built_function_fields() -> Result<()> {
    let r0 = reg_vn(0x10, 8); // ret + arg + clobbered
    let r1 = reg_vn(0x20, 8); // plain caller-clobbered
    let r2 = reg_vn(0x30, 8); // callee-saved (excluded from clobber)
    let sp = reg_vn(0x40, 8); // stack pointer (excluded from clobber)

    let mut b = raw_builder(
        vec![r0, r1, r2, sp],
        &[r0], // arg_passing
        &[r2], // callee_saved
        &[r0], // ret_vars
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;

    assert_eq!(
        b.function().ret_val_regs(),
        &[r0],
        "ret_val_regs projects the ABI ret list"
    );

    let (ret_group, clobber_group) =
        crate::cc_ret_and_clobber_vns(b.function(), b.function().default_cc());
    assert_eq!(
        ret_group,
        vec![r0],
        "ret group is the ret-val register (r0)"
    );
    assert_eq!(
        clobber_group,
        vec![r1],
        "clobber group is only the non-ret caller-clobbered reg (r1)"
    );
    let combined: Vec<_> = ret_group.into_iter().chain(clobber_group).collect();
    assert_eq!(
        combined,
        vec![r0, r1],
        "combined ret-val + clobbers equals the old full clobber list"
    );

    // `build()` populates call_other_clobbered, so the function needs a
    // terminated region first.
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);
    b.set_lift_addr(Some(0x1000));
    b.build_function_return()?;
    b.set_lift_addr(None);
    let f = b.build()?;

    let sp_vn = f.stack_vn();
    let mut coc: Vec<_> = f
        .all_vns()
        .iter()
        .copied()
        .filter(|v| *v != sp_vn)
        .collect::<Vec<_>>();
    coc.sort_by_key(|v| v.addr_off);
    assert_eq!(
        coc,
        vec![r0, r1, r2],
        "call_other_clobbered is every tracked var except the stack pointer"
    );

    Ok(())
}

/// The Return's value inputs are exactly the CC return registers, in
/// `ret_val_regs()` order.
#[test]
fn build_function_return_wires_exactly_the_cc_ret_regs() -> Result<()> {
    let r0 = reg_vn(0x10, 8);
    let r1 = reg_vn(0x18, 8);
    let mut b = raw_builder(
        vec![r0, r1],
        &[],
        &[],
        &[r0, r1], // two ABI ret regs
        None,
        0,
        strider_target::Endianness::Little,
    )?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);

    let expected: Vec<ValueId> = b
        .function()
        .ret_val_regs()
        .iter()
        .map(|vn| b.read_variable(vn))
        .collect::<Result<_>>()?;
    assert_eq!(expected.len(), 2, "two ABI ret regs are tracked");

    b.set_lift_addr(Some(0x1000));
    b.build_function_return()?;
    b.set_lift_addr(None);
    let f = b.build()?;

    let entry = f.entry();
    let ret = crate::walk::walk_graph(f.graph(), entry)
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("function-return path emits a Return node");
    let inputs: Vec<ValueId> = f.node_inputs(ret).into_iter().collect();
    // Slots 0 and 1 are control and memory.
    let ret_values: Vec<ValueId> = inputs[2..].to_vec();
    assert_eq!(
        ret_values, expected,
        "build_function_return wires exactly the CC ret regs' current \
         values, in ret_val_regs() order"
    );
    Ok(())
}

/// End to end through the lifter's coerce-then-write sequence: the variable
/// must read back integer-typed, never as the raw I1.
#[test]
fn write_bool_to_byte_reg_var_coerces_to_int() -> Result<()> {
    let flag = flag_reg_byte();
    let mut b = raw_builder(
        vec![flag],
        &[],
        &[],
        &[],
        None,
        0,
        strider_target::Endianness::Little,
    )?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);

    let lhs = b.build_int_const(1u64, ValueType::I32)?;
    let rhs = b.build_int_const(2u64, ValueType::I32)?;
    let bool_val = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, ValueType::I32)?;

    // Mirror the lifter: coerce to the register's declared int type, write.
    let reg_ty = ValueType::int_for_byte_size(flag.size)?;
    let coerced = b.convert_to_int_if_needed(bool_val, reg_ty)?;
    b.write_variable(&flag, coerced)?;

    let read_back = b.read_variable(&flag)?;
    assert_eq!(
        b.function().value_kind(read_back),
        ValueKind::Typed(ValueType::I8),
        "1-byte flag variable must read back as I8 after a coerced Bool write"
    );
    Ok(())
}

/// A write through `graph_mut()` must be visible through the immutable view.
#[test]
fn graph_mut_returns_mutable_reference_to_inner_graph() -> Result<()> {
    let mut b = empty_builder()?;
    let count_before = b.function().graph().all_node_ids().count();
    let node_id = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(crate::node::const_value::ConstId::new((42_u64) as usize)),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    let count_after = b.function().graph().all_node_ids().count();
    assert_eq!(
        count_after,
        count_before + 1,
        "graph_mut() write must be visible via graph()"
    );
    assert!(matches!(
        b.function().node_kind(node_id),
        NodeKind::IntConst(_)
    ));
    Ok(())
}

#[test]
fn entry_returns_recorded_entry_node_id() -> Result<()> {
    let b = empty_builder()?;
    let entry_via_accessor = b.entry();
    let entry_via_function = b.function().entry();
    assert_eq!(
        entry_via_accessor, entry_via_function,
        "FunctionBuilder::entry() must match Function::entry()"
    );
    Ok(())
}

#[test]
fn build_after_inplace_optimization_still_succeeds() -> Result<()> {
    let mut b = empty_builder()?;
    // build() needs something to validate.
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let val = b.build_int_const(7u64, ValueType::I64)?;
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    let extra = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(crate::node::const_value::ConstId::new((99_u64) as usize)),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    // `extra` is unreachable from the entry, so it needs no fingerprint stamp.
    let built = b.build()?;
    assert!(
        built.graph().all_node_ids().any(|n| n == extra),
        "build() after graph_mut() mutation must preserve the new node"
    );
    Ok(())
}

/// The second mutation must see the first's effect.
#[test]
fn consecutive_inplace_optimizations_compose() -> Result<()> {
    let mut b = empty_builder()?;
    let a = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(crate::node::const_value::ConstId::new((1_u64) as usize)),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    let b_id = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(crate::node::const_value::ConstId::new((2_u64) as usize)),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    assert_ne!(
        a, b_id,
        "consecutive create_node calls must produce distinct ids"
    );
    assert!(matches!(b.function().node_kind(a), NodeKind::IntConst(_)));
    assert!(matches!(
        b.function().node_kind(b_id),
        NodeKind::IntConst(_)
    ));
    Ok(())
}

#[test]
fn set_lift_addr_pair_scopes_attribution_and_restores_on_exit() -> Result<()> {
    let mut b = builder_with_region()?;
    assert_eq!(b.lift_addr, None);
    b.set_lift_addr(Some(0x100));
    assert_eq!(b.lift_addr, Some(0x100));
    b.set_lift_addr(None);
    assert_eq!(b.lift_addr, None, "manual restore returns to prior addr");

    // Nested overrides and their unwind.
    b.set_lift_addr(Some(0x200));
    b.set_lift_addr(Some(0xA));
    b.set_lift_addr(Some(0xB));
    assert_eq!(b.lift_addr, Some(0xB));
    b.set_lift_addr(Some(0xA));
    assert_eq!(b.lift_addr, Some(0xA));
    b.set_lift_addr(Some(0x200));
    assert_eq!(b.lift_addr, Some(0x200));
    Ok(())
}

#[test]
fn set_lift_addr_attributes_node_to_current_addr() -> Result<()> {
    let mut b = builder_with_region()?;
    b.set_lift_addr(Some(0x10));
    let outside_pre = b.build_int_const(1u64, ValueType::I64)?;
    b.set_lift_addr(Some(0xC0DE));
    let inside = b.build_int_const(2u64, ValueType::I64)?;
    b.set_lift_addr(Some(0x10));
    let outside_post = b.build_int_const(3u64, ValueType::I64)?;

    let pre_node = b.function().producer(outside_pre);
    let in_node = b.function().producer(inside);
    let post_node = b.function().producer(outside_post);

    assert_eq!(
        b.function().side_tables().asm_fingerprint(pre_node),
        rustc_hash::FxHashSet::from_iter([0x10])
    );
    assert_eq!(
        b.function().side_tables().asm_fingerprint(in_node),
        rustc_hash::FxHashSet::from_iter([0xC0DE])
    );
    assert_eq!(
        b.function().side_tables().asm_fingerprint(post_node),
        rustc_hash::FxHashSet::from_iter([0x10])
    );
    Ok(())
}

/// A value genuinely past `u128` interns as `Wide` and reads back as the
/// original limbs.
#[test]
fn build_int_const_limbs_round_trips_through_graph() -> Result<()> {
    use crate::node::const_value::ConstValue;
    let cases: [(&str, Vec<u64>, ValueType); 2] = [
        (
            "u256_round_trips_through_graph",
            vec![0x1234, 0xabcd, 0, 0x8000_0000_0000_0000],
            ValueType::I256,
        ),
        (
            "u512_round_trips_through_graph",
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            ValueType::I512,
        ),
    ];
    let mut b = builder_with_region()?;
    for (label, limbs, ty) in cases {
        let value = b.build_int_const_limbs(&limbs, ty)?;
        let node = b.function().producer(value);
        let NodeKind::IntConst(id) = *b.function().node_kind(node) else {
            panic!(
                "{label}: expected IntConst, got {:?}",
                b.function().node_kind(node)
            );
        };
        assert_eq!(
            b.function().const_value(id),
            &ConstValue::Wide(limbs.into_boxed_slice()),
            "{label}"
        );
    }
    Ok(())
}

/// `int_const_i128` reads a canonical `IntConst` sign-extended from its
/// declared width; it does NOT peel `Neg`/`Truncate`/`Extend` wrappers, and
/// returns `None` for a non-constant value.
#[test]
fn int_const_i128_sign_extends_and_rejects_non_const() -> Result<()> {
    let mut b = builder_with_region()?;
    // 0xFFFF_FFFC at I32 reads as -4 (sign-extended from its declared width).
    let neg = b.build_int_const(0xFFFF_FFFCu64, ValueType::I32)?;
    assert_eq!(b.function().int_const_i128(neg), Some(-4));
    let pos = b.build_int_const(7u64, ValueType::I32)?;
    assert_eq!(b.function().int_const_i128(pos), Some(7));
    let sum = b.build_int_binary_operation(neg, pos, IntBinaryOp::Add, ValueType::I32)?;
    assert_eq!(b.function().int_const_i128(sum), None);
    Ok(())
}

#[test]
fn build_int_const_limbs_dedups_repeated_values() -> Result<()> {
    let mut b = builder_with_region()?;
    let limbs = [42u64, 0, 0, 0x8000_0000_0000_0000];
    let o1 = b.build_int_const_limbs(&limbs, ValueType::I256)?;
    let o2 = b.build_int_const_limbs(&limbs, ValueType::I256)?;
    let n1 = b.function().producer(o1);
    let n2 = b.function().producer(o2);
    assert_eq!(n1, n2, "structural dedup must reuse the same NodeId");
    Ok(())
}

/// A value fitting `u128` is accepted at the widest types without the limb
/// path.
#[test]
fn build_int_const_accepts_u256_and_u512() -> Result<()> {
    let mut b = builder_with_region()?;
    let v256 = b.build_int_const(0u64, ValueType::I256)?;
    assert_eq!(b.function().value_type(v256)?, ValueType::I256);
    let v512 = b.build_int_const(7u64, ValueType::I512)?;
    assert_eq!(b.function().value_type(v512)?, ValueType::I512);
    assert_eq!(b.function().int_const_u128(v512), Some(7));
    Ok(())
}

#[test]
fn build_int_const_limbs_rejects_non_wide_output_type() -> Result<()> {
    let mut b = builder_with_region()?;
    let err = b
        .build_int_const_limbs(&[0; 4], ValueType::I64)
        .expect_err("I64 must be rejected by build_int_const_limbs");
    assert!(
        err.to_string().contains("non-wide output type"),
        "got: {err}"
    );
    Ok(())
}

#[test]
fn int_const_wide_validates_clean_when_built_via_intern() -> Result<()> {
    use crate::validate::validate;
    let mut b = builder_with_region()?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let value =
        b.build_int_const_limbs(&[0x1234_5678, 0, 0, 0x8000_0000_0000_0000], ValueType::I256)?;
    b.set_lift_addr(None);
    // Chain control off the entry Region, not Entry: Entry's Control already
    // feeds the region, and a second consumer breaks the single-successor
    // invariant.
    let region_node = b
        .function()
        .graph()
        .all_node_ids()
        .find(|n| matches!(b.function().node_kind(*n), NodeKind::Region))
        .unwrap();
    let entry_ctrl = b
        .function()
        .node_outputs(region_node)
        .iter()
        .copied()
        .next()
        .unwrap();
    let mem_node = b
        .function()
        .graph()
        .all_node_ids()
        .find(|n| matches!(b.function().node_kind(*n), NodeKind::InitialMemory))
        .unwrap();
    let mem_value = b
        .function()
        .node_outputs(mem_node)
        .iter()
        .copied()
        .next()
        .unwrap();
    let ret = b.function_mut().graph_mut().create_node(
        NodeKind::Return,
        [entry_ctrl, mem_value, value],
        [],
    );
    b.function_mut()
        .side_tables_mut()
        .extend_asm_fingerprint(ret, &[SENTINEL_LIFT_ADDR]);
    let function = b.function();
    validate(function).expect("genuinely-wide IntConst must validate clean");
    Ok(())
}

#[test]
fn compact_gcs_unreferenced_wide_consts() -> Result<()> {
    let mut b = builder_with_region()?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let _live = b.build_int_const_limbs(&[1, 1, 1, 1], ValueType::I256)?;
    // Never wired into the reachable graph, so `compact()` should drop it.
    let _zombie = b.build_int_const_limbs(&[2, 2, 2, 2], ValueType::I256)?;
    b.set_lift_addr(None);
    let mem_node = b
        .function()
        .graph()
        .all_node_ids()
        .find(|n| matches!(b.function().node_kind(*n), NodeKind::InitialMemory))
        .unwrap();
    let mem_value = b
        .function()
        .node_outputs(mem_node)
        .iter()
        .copied()
        .next()
        .unwrap();
    // Chain control off the entry Region, not Entry: Entry's Control already
    // feeds the region, and a second consumer breaks the single-successor
    // invariant.
    let region_node = b
        .function()
        .graph()
        .all_node_ids()
        .find(|n| matches!(b.function().node_kind(*n), NodeKind::Region))
        .unwrap();
    let entry_ctrl = b
        .function()
        .node_outputs(region_node)
        .iter()
        .copied()
        .next()
        .unwrap();
    let ret = b.function_mut().graph_mut().create_node(
        NodeKind::Return,
        [entry_ctrl, mem_value, _live],
        [],
    );
    b.function_mut()
        .side_tables_mut()
        .extend_asm_fingerprint(ret, &[SENTINEL_LIFT_ADDR]);

    let pre = b.function().const_interner.len();
    assert_eq!(
        pre, 2,
        "before compact, both wide consts are in the interner"
    );

    let mut bfg = b.build()?;
    bfg.compact()?;

    let post = bfg.const_interner.len();
    assert_eq!(
        post, 1,
        "compact must drop the unreferenced zombie wide const; got {post} entries"
    );
    Ok(())
}

mod build_call_with_cc {
    use super::*;
    use strider_target::{BuiltCallingConvention, CallingConvention, SleighArch};

    fn x86_64_regs() -> rsleigh::SleighRegs {
        SleighArch::x86_64().probe_regs().unwrap()
    }

    fn x86_64_built_cc() -> BuiltCallingConvention {
        CallingConvention::x86_64_systemv()
            .build(&x86_64_regs())
            .unwrap()
    }

    /// A Call reads every CC arg register, so a Call-building fixture must
    /// track the whole set.
    fn x86_64_arg_regs(regs: &rsleigh::SleighRegs) -> Vec<rsleigh::Vn> {
        ["RDI", "RSI", "RDX", "RCX", "R8", "R9"]
            .iter()
            .map(|n| regs.name_to_vn(n).unwrap())
            .collect()
    }

    #[test]
    fn build_call_with_cc_none_matches_build_call() {
        let cc = x86_64_built_cc();
        let regs = x86_64_regs();
        let rax = regs.name_to_vn("RAX").unwrap();
        let rdi = regs.name_to_vn("RDI").unwrap();
        let rsp = regs.name_to_vn("RSP").unwrap();
        // Reading an untracked arg register errors, so track them all.
        let mut tracked = vec![rax, rsp];
        tracked.extend(x86_64_arg_regs(&regs));
        let mut b = FunctionBuilder::new(tracked, cc, strider_target::Endianness::Little).unwrap();
        let _ = rdi;
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        let addr = b.build_int_const(0xdead_beef_u64, ValueType::I64).unwrap();
        b.build_call_cc(addr, None).unwrap();
        let function = b.function();
        let call_node = function
            .graph()
            .all_node_ids()
            .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
            .unwrap();
        assert!(
            function.node_outputs(call_node).len() >= 2,
            "Control + Memory at minimum"
        );
        assert_eq!(
            function.get_cc(call_node),
            function.default_cc(),
            "no override → effective CC is the function default"
        );
    }

    /// Float argument registers are APPENDED to the integer ones, so an
    /// existing `call().arg(N)` query over integer arguments keeps its index.
    #[test]
    fn float_args_follow_integer_args_in_call_inputs() {
        let regs = x86_64_regs();
        let rdi = regs.name_to_vn("RDI").unwrap();
        let rsi = regs.name_to_vn("RSI").unwrap();
        let xmm0 = regs.name_to_vn("XMM0").unwrap();
        let xmm1 = regs.name_to_vn("XMM1").unwrap();
        let rsp = regs.name_to_vn("RSP").unwrap();
        let cc = BuiltCallingConvention {
            arg_passing_regs: vec![rdi, rsi],
            arg_passing_regs_float: vec![xmm0, xmm1],
            stack_vn: rsp,
            ..Default::default()
        };
        let mut b = FunctionBuilder::new(
            vec![rdi, rsi, xmm0, xmm1, rsp],
            cc,
            strider_target::Endianness::Little,
        )
        .unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        let addr = b.build_int_const(0xdead_beef_u64, ValueType::I64).unwrap();
        // Snapshot the pre-call value of each argument register, so the
        // assertion names registers rather than value ids.
        let expected: Vec<ValueId> = [rdi, rsi, xmm0, xmm1]
            .iter()
            .map(|vn| b.read_variable(vn).unwrap())
            .collect();
        let call = b.build_call_cc(addr, None).unwrap();

        let function = b.function();
        let inputs: Vec<_> = function.node_inputs(call).into_iter().collect();
        assert_eq!(
            inputs.len(),
            8,
            "ctrl, mem, target, sp, 2 int args, 2 float args"
        );
        assert_eq!(
            &inputs[4..],
            expected.as_slice(),
            "argument slots must read RDI, RSI, XMM0, XMM1 in that order",
        );
    }

    /// Registers sharing one container each pass their own slice, so one
    /// argument is never two, and a register the caller never listed is seeded
    /// rather than dropped.
    #[test]
    fn float_args_slice_shared_containers_and_cover_every_abi_position() {
        let regs = x86_64_regs();
        let rsp = regs.name_to_vn("RSP").unwrap();
        let xmm0 = regs.name_to_vn("XMM0").unwrap();
        let xmm1 = regs.name_to_vn("XMM1").unwrap();
        // A narrow view of XMM0: the low 8 bytes, as a `movsd` argument sees it.
        let xmm0_lo = rsleigh::Vn {
            addr_space: xmm0.addr_space,
            addr_off: xmm0.addr_off,
            size: 8,
        };
        let cc = BuiltCallingConvention {
            arg_passing_regs_float: vec![xmm0, xmm0_lo, xmm1],
            stack_vn: rsp,
            ..Default::default()
        };
        let mut b =
            FunctionBuilder::new(vec![xmm0, rsp], cc, strider_target::Endianness::Little).unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        let addr = b.build_int_const(0xdead_beef_u64, ValueType::I64).unwrap();
        let expected_xmm0 = b.read_variable(&xmm0).unwrap();
        let call = b.build_call_cc(addr, None).unwrap();

        let function = b.function();
        let inputs: Vec<_> = function.node_inputs(call).into_iter().collect();
        assert_eq!(
            inputs.len(),
            7,
            "ctrl, mem, target, sp, then float ABI positions 0, 1 and 2"
        );
        assert_eq!(inputs[4], expected_xmm0, "position 0 is the whole XMM0");
        assert_eq!(
            function.value_type(inputs[5]).unwrap(),
            ValueType::I64,
            "position 1 is XMM0's low half, sliced out of the container"
        );
    }

    /// An override convention is not the one the function was built against, so
    /// its float argument registers may be untracked. Emitting the prefix before
    /// the first gap keeps index `j` meaning ABI position `j`.
    #[test]
    fn override_cc_float_args_stop_at_an_untracked_position() {
        let regs = x86_64_regs();
        let rsp = regs.name_to_vn("RSP").unwrap();
        let xmm0 = regs.name_to_vn("XMM0").unwrap();
        let xmm1 = regs.name_to_vn("XMM1").unwrap();
        let cc = BuiltCallingConvention {
            stack_vn: rsp,
            ..Default::default()
        };
        let mut b =
            FunctionBuilder::new(vec![xmm0, rsp], cc, strider_target::Endianness::Little).unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        let addr = b.build_int_const(0xdead_beef_u64, ValueType::I64).unwrap();
        // XMM1 was never seeded, so ABI float position 1 has no SSA slot.
        let override_cc = BuiltCallingConvention {
            arg_passing_regs_float: vec![xmm0, xmm1, xmm0],
            stack_vn: rsp,
            ..Default::default()
        };
        let call = b.build_call_cc(addr, Some(&override_cc)).unwrap();

        let function = b.function();
        let inputs: Vec<_> = function.node_inputs(call).into_iter().collect();
        assert_eq!(
            inputs.len(),
            5,
            "ctrl, mem, target, sp, then float ABI position 0 alone"
        );
    }

    #[test]
    fn build_call_with_cc_all_preserving_clobbers_nothing() {
        let cc = x86_64_built_cc();
        let regs = x86_64_regs();
        let rax = regs.name_to_vn("RAX").unwrap();
        let rdi = regs.name_to_vn("RDI").unwrap();
        let rsp = regs.name_to_vn("RSP").unwrap();
        // `FunctionBuilder::new` seeds the CC arg-passing and return-value
        // registers into the tracked set even when the caller does not list
        // them, so an all-preserving override must mark each one callee-saved
        // or it shows up as a clobber output.
        let _ = rdi;
        // Every tracked variable callee-saved means zero clobbers.
        let mut callee_saved = vec![rax];
        callee_saved.extend(
            cc.ret_val_regs
                .iter()
                .chain(cc.ret_val_regs_float.iter())
                .chain(cc.arg_passing_regs_float.iter()),
        );
        let mut b =
            FunctionBuilder::new(vec![rax, rsp], cc, strider_target::Endianness::Little).unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);

        callee_saved.extend(x86_64_arg_regs(&regs));
        let override_cc = BuiltCallingConvention {
            arg_passing_regs: vec![],
            callee_saved_regs: callee_saved,
            ret_val_regs: vec![],
            ret_val_regs_float: vec![],
            stack_vn: rsp,
            stack_args: None,
            ret_stack_pop: 0,
            link_register_vn: None,
            preserves_memory: false,
            preserves_all_registers: false,
            no_return: false,
            ..Default::default()
        };

        let addr = b.build_int_const(0xdead_beef_u64, ValueType::I64).unwrap();
        b.build_call_cc(addr, Some(&override_cc)).unwrap();
        let function = b.function();
        let call_node = function
            .graph()
            .all_node_ids()
            .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
            .unwrap();
        let outs = function.node_outputs(call_node);
        assert_eq!(
            outs.len(),
            2,
            "fentry-style Call has 0 clobbered output slots"
        );
        let inputs: Vec<_> = function.node_inputs(call_node).into_iter().collect();
        assert_eq!(
            inputs.len(),
            4,
            "fentry-style Call takes no args (ctrl, mem, target, sp)"
        );
        assert_eq!(
            function.get_cc(call_node),
            &override_cc,
            "override CC is the effective CC even when it clobbers nothing"
        );
        assert!(
            function
                .node_outputs(call_node)
                .iter()
                .all(|&v| function.get_vn_for_value(v).is_none()),
            "fentry-style Call has no clobber outputs, so none are tagged"
        );
    }

    /// Inputs are `[ctrl, mem, target, sp, ...args]`: SP sits ahead of the
    /// arguments, so the first arg lands at slot 4.
    #[test]
    fn call_sp_input_precedes_args() {
        let cc = x86_64_built_cc();
        let regs = x86_64_regs();
        let rax = regs.name_to_vn("RAX").unwrap();
        let rdi = regs.name_to_vn("RDI").unwrap();
        let rsp = regs.name_to_vn("RSP").unwrap();
        // Reading an untracked arg register errors, so track them all.
        let mut tracked = vec![rax, rsp];
        tracked.extend(x86_64_arg_regs(&regs));
        let mut b = FunctionBuilder::new(tracked, cc, strider_target::Endianness::Little).unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);

        let sp_value = b.read_variable(&rsp).unwrap();
        let arg0_value = b.read_variable(&rdi).unwrap();

        let addr = b.build_int_const(0xdead_beef_u64, ValueType::I64).unwrap();
        b.build_call_cc(addr, None).unwrap();

        let function = b.function();
        let call_node = function
            .graph()
            .all_node_ids()
            .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
            .unwrap();
        let inputs: Vec<_> = function.node_inputs(call_node).into_iter().collect();

        assert!(
            inputs.len() >= 5,
            "Call inputs must be [ctrl, mem, target, sp, arg0]; got {} inputs",
            inputs.len()
        );
        assert_eq!(inputs[2], addr, "slot [2] is the call target");
        assert_eq!(inputs[3], sp_value, "slot [3] is the stack-pointer value");
        assert_eq!(inputs[4], arg0_value, "slot [4] is the first arg (RDI)");
    }

    /// Across several rounds of in-place mutation, `entry()` stays stable and
    /// `graph_mut()` keeps minting fresh ids.
    #[test]
    fn analysis_loop_without_build_round_trips() {
        let mut b = empty_builder().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(0u64, ValueType::I64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);

        let entry = b.entry();

        let r1 = b.function_mut().graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new((1_u64) as usize)),
            std::iter::empty(),
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(b.entry(), entry, "entry() stable after first mutation");

        let r2 = b.function_mut().graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new((2_u64) as usize)),
            std::iter::empty(),
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(b.entry(), entry, "entry() stable after second mutation");
        assert_ne!(r1, r2, "consecutive create_node calls produce distinct ids");

        assert!(matches!(b.function().node_kind(r1), NodeKind::IntConst(_)));
        assert!(matches!(b.function().node_kind(r2), NodeKind::IntConst(_)));
    }

    /// `build()` must still validate after extended in-place use.
    #[test]
    fn final_build_after_extended_use_yields_valid_built() {
        let mut b = empty_builder().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(7u64, ValueType::I64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);

        // Each round leaves a detached node behind; the validator skips
        // unreachable nodes.
        for k in 1u64..=5 {
            b.function_mut().graph_mut().create_node(
                NodeKind::IntConst(crate::node::const_value::ConstId::new((k) as usize)),
                std::iter::empty(),
                [ValueKind::Typed(ValueType::I64)],
            );
        }

        let function = b.build().unwrap();
        crate::validate::validate(&function)
            .expect("build() after extended use must yield a valid graph");
    }
}

/// The ret group holds exactly the ret-val registers, the clobber group only
/// the non-ret caller-saved ones, and a built Call emits
/// `[Control, Memory, ret-vals..., clobbers...]` with every output tagged by
/// its varnode.
#[test]
fn call_ret_val_split_outputs_and_accessor() -> Result<()> {
    // Both ret-val and would-be caller-clobbered.
    let rax = reg_vn(0x00, 8);
    // Plain caller-clobbered.
    let rcx = reg_vn(0x08, 8);
    // Callee-saved, so excluded from clobbers.
    let rbx = reg_vn(0x10, 8);
    // Stack pointer, likewise excluded.
    let sp = reg_vn(0x18, 8);

    let mut b = raw_builder(
        vec![rax, rcx, rbx, sp],
        &[],    // arg_passing
        &[rbx], // callee_saved
        &[rax], // ret_val_regs
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;

    let cc = b.function().default_cc().clone();

    let (ret_vals, clobbered) = crate::cc_ret_and_clobber_vns(b.function(), &cc);
    assert_eq!(
        ret_vals,
        vec![rax],
        "ret group must be exactly the ret-val registers"
    );
    assert!(
        !clobbered.contains(&rax),
        "call_clobbered_for must not contain the ret-val register rax; got {clobbered:?}"
    );
    assert!(
        clobbered.contains(&rcx),
        "call_clobbered_for must still contain the plain caller-clobbered reg rcx"
    );

    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.set_region(region);

    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1234_u64, ValueType::I64)?;
    b.build_call_cc(addr, None)?;
    b.set_lift_addr(None);

    let f = b.function();
    let call_node = f
        .graph()
        .all_node_ids()
        .find(|n| matches!(f.node_kind(*n), NodeKind::Call))
        .expect("exactly one Call node must be present");
    let outs = f.node_outputs(call_node);

    assert_eq!(
        outs.len(),
        2 + ret_vals.len() + clobbered.len(),
        "Call output count must be Control + Memory + ret_vals + clobbers"
    );

    let rax_out = outs[2];
    assert_eq!(
        f.get_vn_for_value(rax_out),
        Some(rax),
        "ret-val output at slot 2 must carry value_vn = rax"
    );

    let rcx_out = outs[3];
    assert_eq!(
        f.get_vn_for_value(rcx_out),
        Some(rcx),
        "clobber output at slot 3 must carry value_vn = rcx"
    );

    Ok(())
}

/// Every fitting value is `Bits`, I80 included, and reads back at the declared
/// width.
#[test]
fn small_valued_i80_const_interns_as_bits() -> Result<()> {
    use crate::node::const_value::ConstValue;
    let mut b = empty_builder()?;
    let via_small = b.build_int_const(5u64, ValueType::I80)?;
    let node = b.function().producer(via_small);
    let NodeKind::IntConst(id) = *b.function().node_kind(node) else {
        panic!("expected IntConst");
    };
    assert_eq!(b.function().const_value(id), &ConstValue::Bits(5));
    assert_eq!(b.function().value_type(via_small)?, ValueType::I80);
    assert_eq!(b.function().int_const_u128(via_small), Some(5u128));

    let again = b.build_int_const(5u64, ValueType::I80)?;
    assert_eq!(
        b.function().producer(again),
        node,
        "equal I80 value must dedup to one node"
    );
    Ok(())
}

/// Limbs that fit `u128` canonicalise to `Bits`, so the limb path and the
/// scalar path share one `ConstId` for the same value.
#[test]
fn small_valued_i256_limbs_canonicalise_to_bits() -> Result<()> {
    use crate::node::const_value::ConstValue;
    let mut b = empty_builder()?;
    let via_limbs = b.build_int_const_limbs(&[5, 0, 0, 0], ValueType::I256)?;
    let node = b.function().producer(via_limbs);
    let NodeKind::IntConst(id) = *b.function().node_kind(node) else {
        panic!("expected IntConst");
    };
    assert_eq!(
        b.function().const_value(id),
        &ConstValue::Bits(5),
        "small-valued I256 limbs must canonicalise to Bits, got {:?}",
        b.function().const_value(id)
    );
    assert_eq!(b.function().value_type(via_limbs)?, ValueType::I256);

    let via_i64 = b.build_int_const(5u64, ValueType::I64)?;
    let i64_node = b.function().producer(via_i64);
    let NodeKind::IntConst(i64_id) = *b.function().node_kind(i64_node) else {
        panic!("expected IntConst");
    };
    assert_eq!(i64_id, id, "value 5 must share one ConstId");
    assert_ne!(i64_node, node, "different widths must be distinct nodes");
    Ok(())
}

/// Interning masks to the declared width, so 3 at I1 becomes 1.
#[test]
fn i1_const_payload_masks_to_one_bit() -> Result<()> {
    use crate::node::const_value::ConstValue;
    let mut b = empty_builder()?;
    let v = b.build_int_const(3u64, ValueType::I1)?;
    let node = b.function().producer(v);
    let NodeKind::IntConst(id) = *b.function().node_kind(node) else {
        panic!("expected IntConst");
    };
    assert_eq!(
        b.function().const_value(id),
        &ConstValue::Bits(1),
        "value 3 at I1 must mask to 1, got {:?}",
        b.function().const_value(id)
    );
    let t = b.build_boolean_const(true);
    assert_eq!(
        b.function().producer(t),
        node,
        "masked I1 const dedups with boolean true"
    );
    Ok(())
}

/// Masking at exactly the carrier's width is the identity.
#[test]
fn i64_const_at_exactly_64_bits_keeps_all_bits() -> Result<()> {
    use crate::node::const_value::ConstValue;
    let mut b = empty_builder()?;
    let v = b.build_int_const(u64::MAX, ValueType::I64)?;
    let node = b.function().producer(v);
    let NodeKind::IntConst(id) = *b.function().node_kind(node) else {
        panic!("expected IntConst");
    };
    assert_eq!(
        b.function().const_value(id),
        &ConstValue::Bits(u128::from(u64::MAX)),
        "all 64 bits must survive the width mask, got {:?}",
        b.function().const_value(id)
    );
    assert_eq!(b.int_const_u128(v), Some(u128::from(u64::MAX)));
    Ok(())
}

/// A callee-saved register (never seeded into the tracked set) and a
/// never-seen varnode both resolve to themselves.
#[test]
fn container_of_untracked_callee_saved_and_adhoc_vns_resolve_to_self() -> Result<()> {
    let r_cs = reg_vn(0x200, 8);
    let b = raw_builder(
        vec![],
        &[],
        &[r_cs], // callee-saved, so not seeded into the tracked set
        &[],
        None,
        0,
        strider_target::Endianness::Little,
    )?;
    let f = b.function();
    let container_of = |vn: &rsleigh::Vn| vn_container::largest_container_in(f.all_vns(), vn);
    assert!(
        !f.all_vns().contains(&r_cs),
        "callee-saved CC regs are not seeded into the tracked set"
    );
    assert_eq!(
        container_of(&r_cs),
        r_cs,
        "untracked callee-saved reg resolves to itself"
    );

    let adhoc = unique_vn(0x999, 4);
    assert_eq!(
        container_of(&adhoc),
        adhoc,
        "never-seen UNIQUE vn resolves to itself"
    );
    Ok(())
}

/// Every arg-passing register's InitialVar is registered as its positional
/// arg carrier at builder-entry time.
#[test]
fn register_args_recorded_at_builder_entry() -> Result<()> {
    let rdi = rsleigh::Vn {
        size: 8,
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
    };
    let rsi = rsleigh::Vn {
        size: 8,
        addr_off: 0x30,
        addr_space: rsleigh::VnSpace::REGISTER,
    };
    let sp = rsleigh::Vn {
        size: 8,
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
    };
    let cc = strider_target::BuiltCallingConvention {
        arg_passing_regs: vec![rdi, rsi],
        callee_saved_regs: vec![],
        ret_val_regs: vec![rdi],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    };
    let mut b = FunctionBuilder::new(vec![rdi, rsi, sp], cc, strider_target::Endianness::Little)?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    // In prod the lifter does this right after `set_entry_region`.
    b.record_register_arg_carriers();
    b.set_region(region);

    let arg0 = b.function().side_tables().arg_index_to_values(0);
    let arg1 = b.function().side_tables().arg_index_to_values(1);
    assert_eq!(arg0.len(), 1, "arg 0 carrier registered at entry");
    assert_eq!(arg1.len(), 1, "arg 1 carrier registered at entry");
    assert!(
        matches!(b.function().node_kind(b.function().producer(arg0[0])),
        NodeKind::InitialVar(v) if b.function().initial_vn(*v) == rdi)
    );
    assert!(
        matches!(b.function().node_kind(b.function().producer(arg1[0])),
        NodeKind::InitialVar(v) if b.function().initial_vn(*v) == rsi)
    );
    Ok(())
}

/// An arg register that is a sub-register of a tracked container is recorded
/// by resolving to its container before the var-table lookup.
#[test]
fn register_arg_subregister_recorded_by_tracked_container() -> Result<()> {
    let rdi = reg_vn(0x38, 8);
    let edi = reg_vn(0x38, 4);
    let sp = reg_vn(0x20, 8);
    let cc = strider_target::BuiltCallingConvention {
        arg_passing_regs: vec![edi], // passed in the narrow alias
        callee_saved_regs: vec![],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    };
    // `FunctionBuilder::new` seeds edi then drops it as enclosed by rdi, so
    // the var table is keyed by rdi.
    let mut b = FunctionBuilder::new(vec![rdi, sp], cc, strider_target::Endianness::Little)?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    // In prod the lifter does this right after `set_entry_region`.
    b.record_register_arg_carriers();
    b.set_region(region);

    let arg0 = b.function().side_tables().arg_index_to_values(0);
    assert_eq!(
        arg0.len(),
        1,
        "sub-register arg 0 must be recorded by its tracked container"
    );
    assert!(
        matches!(b.function().node_kind(b.function().producer(arg0[0])),
        NodeKind::InitialVar(v) if b.function().initial_vn(*v) == rdi)
    );
    Ok(())
}

#[test]
fn build_switch_makes_n_control_outputs_and_records_targets() -> Result<()> {
    // Must be the local `empty_builder()`: the test-utils one is built
    // against the separate dev-dependency compilation of strider-ir, whose
    // types do not implement this crate's traits.
    let mut b = empty_builder()?;
    let entry = b.create_region_all()?;
    let a = b.create_region_all()?;
    let c = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
    b.build_switch(addr, &[(a, 0x1000), (c, 0x1020)])?;
    // The arms need terminating for the function to validate.
    b.set_region(a);
    b.build_return(None, &[])?;
    b.set_region(c);
    b.build_return(None, &[])?;
    let f = b.build()?;
    let sw = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Switch))
        .expect("switch node exists");
    assert_eq!(f.node_inputs(sw).len(), 2, "[ctrl, address]");
    assert_eq!(f.node_outputs(sw).len(), 2, "one control output per arm");
    assert_eq!(f.side_tables().switch_targets(sw), &[0x1000, 0x1020]);
    Ok(())
}

/// x86-64 declares `GDTR`/`IDTR` at 12 bytes and `LDTR`/`TR` at 14, and
/// `wire_entry_and_build_initial_vars` maps EVERY tracked varnode, so one
/// unmappable width fails the whole function.
#[test]
fn twelve_and_fourteen_byte_tracked_varnodes_build_initial_vars() -> Result<()> {
    for size in [12u32, 14] {
        let vn = rsleigh::Vn {
            addr_off: 0x2220,
            addr_space: rsleigh::VnSpace::REGISTER,
            size,
        };
        let mut b = raw_builder(
            vec![vn],
            &[],
            &[],
            &[],
            None,
            0,
            strider_target::Endianness::Little,
        )?;
        let region = b.create_region_all()?;
        b.set_entry_region_all(region)?;
        assert!(
            b.function().initial_var_value(&vn).is_some(),
            "a {size}-byte tracked varnode must get an InitialVar"
        );
    }
    Ok(())
}
