use super::*;
use crate::error::{ErrorKind, Result};
use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};
use crate::ops::{BoolBinaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};

/// Build a minimal builder with no variables so tests that do not need
/// SSA variables remain simple.
fn empty_builder() -> Result<FunctionBuilder> {
    FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
}

// ── get_as_unsigned_int ──────────────────────────────────────────────────

/// A U8 constant built from a wider raw value must be masked to `u8::MAX`.
#[test]
fn get_unsigned_int_truncates_to_declared_width() -> Result<()> {
    let mut b = empty_builder()?;
    // Store u8::MAX + 1 — only the low byte is in-range for U8
    let out = b.build_int_const(u8::MAX as u64 + 1, NodeOutputType::U8);
    // The node was created with kind IntConst(256) but the type is U8,
    // so get_as_unsigned_int must mask it.
    let val = b.get_as_unsigned_int(out)?;
    assert_eq!(val, Some(0)); // 256 & 0xFF == 0
    Ok(())
}

#[test]
fn get_as_int_accepts_bool_const() -> Result<()> {
    let mut b = empty_builder()?;
    let bt = b.build_boolean_const(true);
    let bf = b.build_boolean_const(false);
    assert_eq!(b.get_as_int(bt)?, Some((1u64, 1i64)));
    assert_eq!(b.get_as_int(bf)?, Some((0u64, 0i64)));
    Ok(())
}

/// `get_as_unsigned_int` on a non-const node must return `None`.
#[test]
fn get_unsigned_int_is_none_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, NodeOutputType::U64);
    let rhs = b.build_int_const(2u64, NodeOutputType::U64);
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;
    assert_eq!(b.get_as_unsigned_int(add)?, None);
    Ok(())
}

// ── get_as_signed_int ────────────────────────────────────────────────────

/// A U8 value with MSB set (`u8::MAX`) must sign-extend to -1 as i64.
#[test]
fn get_signed_int_sign_extends_negative_u8() -> Result<()> {
    let mut b = empty_builder()?;
    let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
    assert_eq!(b.get_as_signed_int(out)?, Some(-1i64));
    Ok(())
}

/// A U8 value below the sign bit (`i8::MAX`) must stay positive.
#[test]
fn get_signed_int_positive_u8_stays_positive() -> Result<()> {
    let mut b = empty_builder()?;
    let out = b.build_int_const(i8::MAX as u64, NodeOutputType::U8);
    assert_eq!(b.get_as_signed_int(out)?, Some(i8::MAX as i64));
    Ok(())
}

// ── truncate_if_needed ───────────────────────────────────────────────────

/// Truncating a constant folds into a new constant of the target type,
/// not a Truncate node.
#[test]
fn truncate_const_folds_to_const() -> Result<()> {
    let mut b = empty_builder()?;
    let out = b.build_int_const(0xABCDu64, NodeOutputType::U16);
    let truncated = b.truncate_if_needed(out, NodeOutputType::U8)?;
    // Must fold to a constant
    let val = b.get_as_unsigned_int(truncated)?;
    assert_eq!(val, Some(0xCD), "low byte of 0xABCD is 0xCD");
    // No Truncate node should have been emitted
    let node = b.graph().get_node_from_output(truncated);
    assert!(matches!(b.graph().node_kind(node), NodeKind::IntConst(_)));
    Ok(())
}

/// For a **non-const** value already at the target width (or narrower),
/// `truncate_if_needed` must return the same output id unchanged.
/// (Const values are always folded into a new constant node regardless of
/// direction, so the no-op path only applies to non-const values.)
#[test]
fn truncate_noop_when_already_narrow_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    // Build a non-const U8 expression: add(1u8, 2u8)
    let lhs = b.build_int_const(1u64, NodeOutputType::U8);
    let rhs = b.build_int_const(2u64, NodeOutputType::U8);
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U8)?;
    // "Truncating" to a wider type must return the same node unchanged
    let result = b.truncate_if_needed(add, NodeOutputType::U16)?;
    assert_eq!(
        result, add,
        "non-const U8 value must not be touched when target is U16"
    );
    Ok(())
}

/// A non-constant U32 truncated to U8 must emit a Truncate node.
#[test]
fn truncate_emits_truncate_node_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, NodeOutputType::U32);
    let rhs = b.build_int_const(2u64, NodeOutputType::U32);
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U32)?;

    let truncated = b.truncate_if_needed(add, NodeOutputType::U8)?;
    let node = b.graph().get_node_from_output(truncated);
    assert!(
        matches!(b.graph().node_kind(node), NodeKind::Truncate),
        "expected Truncate node, got {:?}",
        b.graph().node_kind(node)
    );
    Ok(())
}

// ── extend_if_needed ─────────────────────────────────────────────────────

/// Zero-extending a constant must fold: the result is a wider constant
/// with high bits cleared.
#[test]
fn zero_extend_const_folds_to_wider_const() -> Result<()> {
    let mut b = empty_builder()?;
    let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
    let extended = b.extend_if_needed(out, NodeOutputType::U32, ExtendOp::ZeroExtend)?;
    assert_eq!(b.get_as_unsigned_int(extended)?, Some(u8::MAX as u64));
    let node = b.graph().get_node_from_output(extended);
    assert!(matches!(b.graph().node_kind(node), NodeKind::IntConst(_)));
    Ok(())
}

/// Sign-extending a negative U8 constant (`u8::MAX` = -1 as i8) must fold
/// to `u32::MAX` (all bits set) as a wider constant.
#[test]
fn sign_extend_const_folds_negative_value() -> Result<()> {
    let mut b = empty_builder()?;
    let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
    let extended = b.extend_if_needed(out, NodeOutputType::U32, ExtendOp::SignExtend)?;
    assert_eq!(b.get_as_unsigned_int(extended)?, Some(u32::MAX as u64));
    Ok(())
}

/// Extending a non-constant must emit an Extend node.
#[test]
fn extend_emits_extend_node_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, NodeOutputType::U8);
    let rhs = b.build_int_const(2u64, NodeOutputType::U8);
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U8)?;

    let extended = b.extend_if_needed(add, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
    let node = b.graph().get_node_from_output(extended);
    assert!(
        matches!(b.graph().node_kind(node), NodeKind::Extend(_)),
        "expected Extend node"
    );
    Ok(())
}

/// If the value is already the target width, `extend_if_needed` must
/// return it unchanged.
#[test]
fn extend_noop_when_already_wide_enough() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, NodeOutputType::U64);
    let rhs = b.build_int_const(2u64, NodeOutputType::U64);
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;

    let result = b.extend_if_needed(add, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
    assert_eq!(result, add);
    Ok(())
}

// ── convert_to_bool_if_needed ─────────────────────────────────────────────

/// A known zero integer must fold to `BoolConst(false)`.
#[test]
fn convert_zero_int_to_bool_folds_to_false() -> Result<()> {
    let mut b = empty_builder()?;
    let zero = b.build_int_const(0u64, NodeOutputType::U32);
    let result = b.convert_to_bool_if_needed(zero)?;
    let node = b.graph().get_node_from_output(result);
    assert_eq!(b.graph().node_kind(node), &NodeKind::BoolConst(false));
    Ok(())
}

/// A known non-zero integer must fold to `BoolConst(true)`.
#[test]
fn convert_nonzero_int_to_bool_folds_to_true() -> Result<()> {
    let mut b = empty_builder()?;
    let nonzero = b.build_int_const(99u64, NodeOutputType::U32);
    let result = b.convert_to_bool_if_needed(nonzero)?;
    let node = b.graph().get_node_from_output(result);
    assert_eq!(b.graph().node_kind(node), &NodeKind::BoolConst(true));
    Ok(())
}

/// A value already of `Bool` type must be returned unchanged.
#[test]
fn convert_bool_to_bool_is_identity() -> Result<()> {
    let mut b = empty_builder()?;
    let bval = b.build_boolean_const(true);
    let result = b.convert_to_bool_if_needed(bval)?;
    assert_eq!(result, bval);
    Ok(())
}

/// A non-constant integer must produce a `CastToBool` node.
#[test]
fn convert_non_const_int_emits_cast_to_bool_node() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, NodeOutputType::U32);
    let rhs = b.build_int_const(2u64, NodeOutputType::U32);
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U32)?;

    let result = b.convert_to_bool_if_needed(add)?;
    let node = b.graph().get_node_from_output(result);
    assert!(
        matches!(b.graph().node_kind(node), NodeKind::CastToBool),
        "expected CastToBool node"
    );
    Ok(())
}

// ── build_int_binary_operation ────────────────────────────────────────────

/// Building an Add on two constants of the same type must produce an
/// `IntBinaryOp(Add)` node (no constant folding at this layer).
#[test]
fn build_int_binary_op_produces_binary_op_node() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(3u64, NodeOutputType::U64);
    let rhs = b.build_int_const(4u64, NodeOutputType::U64);
    let result =
        b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;
    let node = b.graph().get_node_from_output(result);
    assert_eq!(
        b.graph().node_kind(node),
        &NodeKind::IntBinaryOp(IntBinaryOp::Add)
    );
    Ok(())
}

/// When the operands differ in width, `build_int_binary_operation` must
/// insert a coercion node so both reach the target type.
#[test]
fn build_int_binary_op_coerces_narrower_operand() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, NodeOutputType::U8);
    let rhs = b.build_int_const(2u64, NodeOutputType::U64);
    let result =
        b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;
    // The result must be typed as U64
    let kind = b.graph().output_kind(result);
    assert_eq!(kind, NodeOutputKind::OutputType(NodeOutputType::U64));
    Ok(())
}

// ── build_int_cmp_operation ───────────────────────────────────────────────

/// A comparison must always produce a `Bool` output regardless of the
/// operand type.
#[test]
fn build_int_cmp_produces_bool_output() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(10u64, NodeOutputType::U32);
    let rhs = b.build_int_const(20u64, NodeOutputType::U32);
    let result = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, NodeOutputType::U32)?;
    let kind = b.graph().output_kind(result);
    assert_eq!(kind, NodeOutputKind::OutputType(NodeOutputType::Bool));
    Ok(())
}

// ── build_boolean_operation ────────────────────────────────────────────────

/// Boolean AND of two bool constants must produce a `BoolBinaryOp(And)`
/// node.
#[test]
fn build_boolean_operation_produces_bool_binary_node() -> Result<()> {
    let mut b = empty_builder()?;
    let t = b.build_boolean_const(true);
    let f = b.build_boolean_const(false);
    let result = b.build_boolean_operation(t, f, BoolBinaryOp::And)?;
    let node = b.graph().get_node_from_output(result);
    assert_eq!(
        b.graph().node_kind(node),
        &NodeKind::BoolBinaryOp(BoolBinaryOp::And)
    );
    assert_eq!(
        b.graph().output_kind(result),
        NodeOutputKind::OutputType(NodeOutputType::Bool)
    );
    Ok(())
}

// ── deduplication across build helpers ────────────────────────────────────

/// Two identical constants must alias to the same output id (graph-level
/// deduplication).
#[test]
fn identical_constants_are_deduplicated() -> Result<()> {
    let mut b = empty_builder()?;
    let a = b.build_int_const(77u64, NodeOutputType::U32);
    let c = b.build_int_const(77u64, NodeOutputType::U32);
    assert_eq!(a, c, "same constant must reuse the same node");
    Ok(())
}

/// Two constants with different values must NOT alias.
#[test]
fn different_constants_are_distinct() -> Result<()> {
    let mut b = empty_builder()?;
    let a = b.build_int_const(1u64, NodeOutputType::U32);
    let c = b.build_int_const(2u64, NodeOutputType::U32);
    assert_ne!(a, c);
    Ok(())
}

// ── Float builder methods ────────────────────────────────────────────────

#[test]
fn build_float_const_f32_has_correct_bits() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f32.to_bits() as u64;
    let out = b.build_float_const(bits, NodeOutputType::F32);
    let kind = *b.graph().node_kind(b.graph().get_node_from_output(out));
    assert_eq!(kind, NodeKind::FloatConst(bits));
    assert_eq!(
        b.graph().output_kind(out),
        NodeOutputKind::OutputType(NodeOutputType::F32)
    );
    Ok(())
}

#[test]
fn build_float_const_f64_has_correct_bits() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f64.to_bits();
    let out = b.build_float_const(bits, NodeOutputType::F64);
    let kind = *b.graph().node_kind(b.graph().get_node_from_output(out));
    assert_eq!(kind, NodeKind::FloatConst(bits));
    assert_eq!(
        b.graph().output_kind(out),
        NodeOutputKind::OutputType(NodeOutputType::F64)
    );
    Ok(())
}

#[test]
fn get_as_float_bits_returns_bits_for_float_const() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 2.5f64.to_bits();
    let out = b.build_float_const(bits, NodeOutputType::F64);
    assert_eq!(b.get_as_float_bits(out)?, Some(bits));
    Ok(())
}

#[test]
fn get_as_float_bits_returns_none_for_int_const() -> Result<()> {
    let mut b = empty_builder()?;
    let out = b.build_int_const(42u64, NodeOutputType::U64);
    assert_eq!(b.get_as_float_bits(out)?, None);
    Ok(())
}

#[test]
fn int_bits_to_float_folds_int_const_immediately() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f32.to_bits() as u64;
    let int_out = b.build_int_const(bits, NodeOutputType::U32);
    let float_out = b.build_int_bits_to_float(int_out, NodeOutputType::F32)?;
    // Should be a FloatConst, not an IntBitsToFloat node
    let kind = *b
        .graph()
        .node_kind(b.graph().get_node_from_output(float_out));
    assert_eq!(kind, NodeKind::FloatConst(bits));
    Ok(())
}

#[test]
fn float_bits_to_int_folds_float_const_immediately() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f64.to_bits();
    let float_out = b.build_float_const(bits, NodeOutputType::F64);
    let int_out = b.build_float_bits_to_int(float_out, NodeOutputType::U64)?;
    // Should be an IntConst, not a FloatBitsToInt node
    let kind = *b.graph().node_kind(b.graph().get_node_from_output(int_out));
    assert_eq!(kind, NodeKind::IntConst(u128::from(bits)));
    Ok(())
}

#[test]
fn build_float_binary_op_produces_correct_node() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
    let rhs = b.build_float_const(2.0f32.to_bits() as u64, NodeOutputType::F32);
    let out = b.build_float_binary_op(lhs, rhs, FloatBinaryOp::Add, NodeOutputType::F32)?;
    let kind = *b.graph().node_kind(b.graph().get_node_from_output(out));
    assert_eq!(kind, NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
    Ok(())
}

#[test]
fn build_float_cmp_op_produces_bool_output() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let rhs = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let out = b.build_float_cmp_op(lhs, rhs, FloatCmpOp::Less)?;
    assert_eq!(
        b.graph().output_kind(out),
        NodeOutputKind::OutputType(NodeOutputType::Bool)
    );
    Ok(())
}

#[test]
fn build_int_bits_to_float_inserts_node_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let int_val = b.build_int_const(0x3F800000u64, NodeOutputType::U32);
    let zero = b.build_int_const(0u64, NodeOutputType::U32);
    // Build an Add(x, 0) so the result is not an IntConst node.
    let non_const = b.build_int_binary_operation(
        int_val,
        zero,
        crate::ops::IntBinaryOp::Add,
        NodeOutputType::U32,
    )?;
    let float_out = b.build_int_bits_to_float(non_const, NodeOutputType::F32)?;
    let kind = *b
        .graph()
        .node_kind(b.graph().get_node_from_output(float_out));
    assert_eq!(kind, NodeKind::IntBitsToFloat);
    Ok(())
}

// ── CastToFloat tests ─────────────────────────────────────────────────────

#[test]
fn build_cast_to_float_creates_cast_node() -> Result<()> {
    let mut b = empty_builder()?;
    let int_val = b.build_int_const(42u64, NodeOutputType::U64);
    let cast = b.build_cast_to_float(int_val, NodeOutputType::F64);
    let kind = *b.graph().node_kind(b.graph().get_node_from_output(cast));
    assert_eq!(kind, NodeKind::CastToFloat);
    assert_eq!(b.get_output_type(cast)?, NodeOutputType::F64);
    Ok(())
}

#[test]
fn cast_to_float_if_needed_is_identity_for_same_type() -> Result<()> {
    let mut b = empty_builder()?;
    let float_val = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
    let result = b.cast_to_float_if_needed(float_val, NodeOutputType::F32)?;
    // Should be the same output — no new node inserted.
    assert_eq!(result, float_val);
    Ok(())
}

#[test]
fn build_float_binary_op_with_int_inputs_auto_casts() -> Result<()> {
    let mut b = empty_builder()?;
    let i1 = b.build_int_const(0x3F800000u64, NodeOutputType::U32);
    let i2 = b.build_int_const(0x40000000u64, NodeOutputType::U32);
    // Both inputs are U32 — builder should auto-insert CastToFloat.
    let result = b.build_float_binary_op(i1, i2, FloatBinaryOp::Add, NodeOutputType::F32)?;
    let kind = *b.graph().node_kind(b.graph().get_node_from_output(result));
    assert_eq!(kind, NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
    // Verify inputs are CastToFloat nodes.
    let [lhs, rhs] = b
        .graph()
        .node_inputs_exact::<2>(b.graph().get_node_from_output(result))?;
    let lhs_node = b.graph().get_node_from_output(lhs);
    let rhs_node = b.graph().get_node_from_output(rhs);
    assert_eq!(*b.graph().node_kind(lhs_node), NodeKind::CastToFloat);
    assert_eq!(*b.graph().node_kind(rhs_node), NodeKind::CastToFloat);
    Ok(())
}

// ── CallOther / SegmentOp / CPoolRef / New ──────────────────────────────

/// Helper: build a single-region builder with an active region set.
fn builder_with_region() -> Result<FunctionBuilder> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    Ok(b)
}

#[test]
fn build_call_other_without_output_advances_ctrl_and_memory() -> Result<()> {
    let mut b = builder_with_region()?;
    let ctrl_before = b.cur_region_control()?;
    let mem_before = b.cur_region_memory()?;

    let result = b.build_call_other(7, &[], None)?;
    assert!(result.is_none(), "no output varnode → no value output");

    // Ctrl and memory tokens must advance (be different outputs).
    let ctrl_after = b.cur_region_control()?;
    let mem_after = b.cur_region_memory()?;
    assert_ne!(ctrl_before, ctrl_after);
    assert_ne!(mem_before, mem_after);

    // The node must be a CallOther with the given id.
    let node = b.graph().get_node_from_output(ctrl_after);
    assert_eq!(
        b.graph().node_kind(node),
        &NodeKind::CallOther { user_op_id: 7 }
    );
    Ok(())
}

#[test]
fn build_call_other_with_output_returns_typed_value() -> Result<()> {
    let mut b = builder_with_region()?;
    let arg = b.build_int_const(0x42u64, NodeOutputType::U64);
    let out = b
        .build_call_other(3, &[arg], Some(NodeOutputType::U32))?
        .ok_or_else(|| ErrorKind::AssertionFailed("output_ty = Some → value output".into()))?;
    assert_eq!(
        b.graph().output_kind(out),
        NodeOutputKind::OutputType(NodeOutputType::U32)
    );
    let node = b.graph().get_node_from_output(out);
    assert_eq!(
        b.graph().node_kind(node),
        &NodeKind::CallOther { user_op_id: 3 }
    );
    Ok(())
}

#[test]
fn build_call_other_rejects_non_value_arg() -> Result<()> {
    let mut b = builder_with_region()?;
    let mem = b.cur_region_memory()?;
    let res = b.build_call_other(0, &[mem], None);
    assert!(matches!(
        res.as_ref().map_err(|e| e.kind()),
        Err(ErrorKind::ExpectedValue(_, _))
    ));
    Ok(())
}

#[test]
fn build_segment_op_produces_pure_node() -> Result<()> {
    let mut b = builder_with_region()?;
    let seg = b.build_int_const(0x10u64, NodeOutputType::U16);
    let off = b.build_int_const(0x100u64, NodeOutputType::U32);
    let out = b.build_segment_op(1, seg, off, NodeOutputType::U64)?;
    let node = b.graph().get_node_from_output(out);
    assert_eq!(b.graph().node_kind(node), &NodeKind::SegmentOp { op_id: 1 });
    assert_eq!(
        b.graph().output_kind(out),
        NodeOutputKind::OutputType(NodeOutputType::U64)
    );
    Ok(())
}

#[test]
fn build_segment_op_is_cacheable_across_identical_calls() -> Result<()> {
    let mut b = builder_with_region()?;
    let seg = b.build_int_const(0x10u64, NodeOutputType::U16);
    let off = b.build_int_const(0x100u64, NodeOutputType::U32);
    let a = b.build_segment_op(1, seg, off, NodeOutputType::U64)?;
    let c = b.build_segment_op(1, seg, off, NodeOutputType::U64)?;
    assert_eq!(a, c, "SegmentOp is pure → identical calls must dedup");
    Ok(())
}

#[test]
fn build_cpool_ref_produces_typed_node() -> Result<()> {
    let mut b = builder_with_region()?;
    let r0 = b.build_int_const(0xAAu64, NodeOutputType::U32);
    let r1 = b.build_int_const(0xBBu64, NodeOutputType::U32);
    let out = b.build_cpool_ref(&[r0, r1], NodeOutputType::U64)?;
    let node = b.graph().get_node_from_output(out);
    assert_eq!(b.graph().node_kind(node), &NodeKind::CPoolRef);
    Ok(())
}

#[test]
fn build_cpool_ref_is_not_deduplicated() -> Result<()> {
    let mut b = builder_with_region()?;
    let r0 = b.build_int_const(0xAAu64, NodeOutputType::U32);
    let a = b.build_cpool_ref(&[r0], NodeOutputType::U64)?;
    let c = b.build_cpool_ref(&[r0], NodeOutputType::U64)?;
    assert_ne!(
        a, c,
        "CPoolRef is non-cacheable → must yield distinct nodes"
    );
    Ok(())
}

#[test]
fn build_new_produces_typed_node() -> Result<()> {
    let mut b = builder_with_region()?;
    let size = b.build_int_const(32u64, NodeOutputType::U64);
    let out = b.build_new(&[size], NodeOutputType::U64)?;
    let node = b.graph().get_node_from_output(out);
    assert_eq!(b.graph().node_kind(node), &NodeKind::New);
    Ok(())
}

#[test]
fn build_new_is_not_deduplicated() -> Result<()> {
    let mut b = builder_with_region()?;
    let size = b.build_int_const(32u64, NodeOutputType::U64);
    let a = b.build_new(&[size], NodeOutputType::U64)?;
    let c = b.build_new(&[size], NodeOutputType::U64)?;
    assert_ne!(a, c, "each allocation must yield a distinct node");
    Ok(())
}

/// The analyzer lowers `Piece(hi, lo)` to
/// `Or(ShiftLeft(ZeroExtend(hi), lo_bits), ZeroExtend(lo))`.  When `hi` is
/// a float, the first `convert_to_int_if_needed` call must insert a
/// `CastToInt` so the subsequent integer operations are well-typed.  This
/// test replicates that lowering manually and verifies the `CastToInt`
/// appears on the path from the float input.
#[test]
fn piece_composition_auto_casts_float_input() -> Result<()> {
    let mut b = empty_builder()?;
    let float_val = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
    let int_lo = b.build_int_const(0u64, NodeOutputType::U32);

    // Replicate the analyzer's Piece composition.
    let out_ty = NodeOutputType::U64;
    let hi_ty = b.get_output_type(float_val)?.to_natural_int_type();
    let hi_int = b.convert_to_int_if_needed(float_val, hi_ty)?;
    let lo_ty = b.get_output_type(int_lo)?.to_natural_int_type();
    let lo_int = b.convert_to_int_if_needed(int_lo, lo_ty)?;
    let lo_bits = lo_ty.bit_width() as u64;
    let hi_wide = b.convert_to_int_if_needed(hi_int, out_ty)?;
    let lo_wide = b.convert_to_int_if_needed(lo_int, out_ty)?;
    let shift_amt = b.build_int_const(lo_bits, out_ty);
    let hi_shifted = b.build_int_binary_operation(
        hi_wide,
        shift_amt,
        IntBinaryOp::ShiftLeft,
        out_ty,
    )?;
    let result = b.build_int_binary_operation(
        hi_shifted,
        lo_wide,
        IntBinaryOp::Or,
        out_ty,
    )?;

    // The root must be the Or.
    let root_kind = *b.graph().node_kind(b.graph().get_node_from_output(result));
    assert_eq!(root_kind, NodeKind::IntBinaryOp(IntBinaryOp::Or));

    // `hi_int` consumes the float, so it must be a CastToInt node.
    let hi_int_node = b.graph().get_node_from_output(hi_int);
    assert_eq!(*b.graph().node_kind(hi_int_node), NodeKind::CastToInt);
    Ok(())
}

// ── extend_if_needed with non-integer input ───────────────────────────────

/// Regression for BUG-3/10: `extend_if_needed` with a Bool input must
/// insert a `CastToInt` coercion so the resulting value is typed as an
/// integer.  Before the fix the `Extend` node's signature (`AnyInt` input)
/// was violated and the validator rejected the graph with
/// "OutputType(Bool), expected AnyInt".
///
/// Concretely: MIPS/ARM comparison instructions emit a Bool result that
/// may then be zero-extended into a wider register.  The coerce path must
/// be: BoolOp → CastToInt → (no Extend needed if sizes already match, or
/// → Extend if narrower).
#[test]
fn extend_if_needed_with_bool_input_inserts_cast_to_int() -> Result<()> {
    let mut b = empty_builder()?;

    // Build a Bool value: an integer comparison 1 < 2 (always true, but
    // not folded at this layer — the builder does not constant-fold cmps).
    let lhs = b.build_int_const(1u64, NodeOutputType::U32);
    let rhs = b.build_int_const(2u64, NodeOutputType::U32);
    let bool_val = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, NodeOutputType::U32)?;

    // Sanity: the comparison result is Bool-typed.
    assert_eq!(
        b.graph().output_kind(bool_val),
        NodeOutputKind::OutputType(NodeOutputType::Bool),
        "comparison must produce Bool"
    );

    // Extend the Bool into a U32 — this is the path that broke before the fix.
    let extended = b.extend_if_needed(bool_val, NodeOutputType::U32, ExtendOp::ZeroExtend)?;

    // The result must be U32-typed.
    assert_eq!(
        b.graph().output_kind(extended),
        NodeOutputKind::OutputType(NodeOutputType::U32),
        "extend_if_needed must produce U32 when requested"
    );

    // No Extend node must have a Bool-typed input — that was the invalid state.
    // Walk every Extend node in the graph and verify its first input is AnyInt.
    for n in b.graph().nodes.keys() {
        if matches!(b.graph().node_kind(n), NodeKind::Extend(_)) {
            let inputs = b.graph().node_inputs(n);
            let first_input = inputs.into_iter().next().expect("Extend has one input");
            let input_kind = b.graph().output_kind(first_input);
            assert_ne!(
                input_kind,
                NodeOutputKind::OutputType(NodeOutputType::Bool),
                "Extend node must never receive a Bool input; found one at {n:?}"
            );
        }
    }

    // The fix routes through CastToInt.  Assert the output traces back through
    // a CastToInt node (possibly with a further Extend on top of it).
    fn find_cast_to_int_ancestor(g: &crate::graph::Graph, output: NodeOutputId) -> bool {
        let node = g.get_node_from_output(output);
        if matches!(g.node_kind(node), NodeKind::CastToInt) {
            return true;
        }
        g.node_inputs(node)
            .into_iter()
            .any(|inp| find_cast_to_int_ancestor(g, inp))
    }
    assert!(
        find_cast_to_int_ancestor(b.graph(), extended),
        "a CastToInt node must appear on the path from the Bool value to the extended result"
    );

    Ok(())
}

// ── F80 / U80 bit-conversion: skip the immediate-fold ────────────────────
//
// `build_int_bits_to_float(IntConst, F32/F64)` and
// `build_float_bits_to_int(FloatConst, U8..U128)` immediate-fold to the
// other constant kind because F32/F64 fit in `FloatConst`'s u64 payload.
// F80 is 80 bits — doesn't fit — so the immediate-fold must be skipped
// and a real bit-conversion node emitted.  This pins that behavior so a
// future contributor doesn't accidentally truncate F80 by re-enabling
// the fold for all widths.

#[test]
fn int_bits_to_float_f80_emits_node_not_const() -> Result<()> {
    let mut b = empty_builder()?;
    let int_const = b.build_int_const(0xDEAD_BEEF_CAFEu64, NodeOutputType::U80);
    let result = b.build_int_bits_to_float(int_const, NodeOutputType::F80)?;
    let node = b.graph().get_node_from_output(result);
    assert_eq!(
        b.graph().node_kind(node),
        &NodeKind::IntBitsToFloat,
        "F80 path must emit IntBitsToFloat node, not fold to FloatConst"
    );
    // Non-F80 path still folds for safety regression: F64 IntBitsToFloat
    // collapses to FloatConst.
    let int_const64 = b.build_int_const(0u64, NodeOutputType::U64);
    let result_f64 = b.build_int_bits_to_float(int_const64, NodeOutputType::F64)?;
    let node_f64 = b.graph().get_node_from_output(result_f64);
    assert!(
        matches!(b.graph().node_kind(node_f64), NodeKind::FloatConst(_)),
        "F64 path must still fold to FloatConst (regression check)"
    );
    Ok(())
}

#[test]
fn float_bits_to_int_f80_emits_node_not_const() -> Result<()> {
    let mut b = empty_builder()?;
    let float_const = b.build_float_const(0xBEEFu64, NodeOutputType::F80);
    let result = b.build_float_bits_to_int(float_const, NodeOutputType::U80)?;
    let node = b.graph().get_node_from_output(result);
    assert_eq!(
        b.graph().node_kind(node),
        &NodeKind::FloatBitsToInt,
        "F80 input must emit FloatBitsToInt node, not fold to IntConst"
    );
    // Non-F80 path still folds: F64 FloatBitsToInt collapses to IntConst.
    let float_const64 = b.build_float_const(0u64, NodeOutputType::F64);
    let result_u64 = b.build_float_bits_to_int(float_const64, NodeOutputType::U64)?;
    let node_u64 = b.graph().get_node_from_output(result_u64);
    assert!(
        matches!(b.graph().node_kind(node_u64), NodeKind::IntConst(_)),
        "F64 path must still fold to IntConst (regression check)"
    );
    Ok(())
}

// ── post-call SP adjust ─────────────────────────────────────────────────

/// Fake 8-byte stack pointer varnode in the REGISTER space.
fn sp_vn_u64() -> rsleigh::Vn {
    rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 8,
    }
}

/// After `build_call` returns, SP must be rebound to
/// `Add(pre_call_SP, IntConst(ret_stack_pop))` — the caller-visible effect
/// of the callee's `ret` on stack-push ISAs.
#[test]
fn build_call_emits_post_call_sp_adjust() -> Result<()> {
    let sp = sp_vn_u64();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[], &[], Some(sp), 8)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let pre_sp = b.read_variable(&sp)?;
    let target = b.build_int_const(0x1000u64, NodeOutputType::U64);
    b.build_call(target)?;

    let post_sp = b.read_variable(&sp)?;
    assert_ne!(
        pre_sp, post_sp,
        "SP must be rebound after Call when ret_stack_pop != 0"
    );

    // The new SP must be an Add node.
    let add_node = b.graph().get_node_from_output(post_sp);
    assert_eq!(
        b.graph().node_kind(add_node),
        &NodeKind::IntBinaryOp(IntBinaryOp::Add)
    );

    let inputs: Vec<NodeOutputId> = b.graph().node_inputs(add_node).into_iter().collect();
    assert_eq!(inputs.len(), 2, "Add has two inputs");

    // One input is the pre-call SP; the other is an IntConst(8).
    let (lhs, rhs) = (inputs[0], inputs[1]);
    assert_eq!(lhs, pre_sp, "Add consumes the pre-call SP output");
    let rhs_kind = *b.graph().node_kind(b.graph().get_node_from_output(rhs));
    assert_eq!(
        rhs_kind,
        NodeKind::IntConst(8),
        "rhs must be IntConst(ret_stack_pop) = 8"
    );
    Ok(())
}

/// When `ret_stack_pop == 0` (link-register ISAs) no SP-adjust node is
/// emitted — SP flows through the `Call` unchanged (or, when SP is
/// excluded from the clobbered set but ret_stack_pop is 0, remains
/// bound to its pre-call value).
#[test]
fn build_call_no_sp_adjust_when_ret_stack_pop_zero() -> Result<()> {
    let sp = sp_vn_u64();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[], &[], Some(sp), 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let pre_sp = b.read_variable(&sp)?;
    let target = b.build_int_const(0x1000u64, NodeOutputType::U64);
    b.build_call(target)?;

    let post_sp = b.read_variable(&sp)?;
    // No Add node was emitted — SP is unchanged.
    assert_eq!(
        pre_sp, post_sp,
        "ret_stack_pop = 0 must not introduce a new Add node"
    );
    Ok(())
}

// ── BUG-1 regression: UNIQUE-space overlapping varnode filtering ─────────
//
// Sleigh occasionally writes a wider UNIQUE varnode and reads a narrow slice
// of it (e.g. on MIPS, MULT writes a 64-bit unique then a Copy reads a 4-byte
// slice into $v0).  Without filtering, the 4-byte and 8-byte unique varnodes
// were treated as independent SSA variables — the narrow read returned an
// undefined `InitialVar` and the multiplication never materialised in IR.
//
// The fix in `FunctionBuilder::new_raw` extends the same overlap-filter that
// REGISTER space uses to UNIQUE space: when both an outer and an inner
// varnode are touched, the outer wins, and the analyzer's register-aliasing
// logic rebuilds the inner via shift/truncate when needed.

fn unique_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        addr: rsleigh::VnAddr { space: rsleigh::VnSpace::UNIQUE, off },
        size,
    }
}

/// When two UNIQUE-space varnodes overlap (a narrow one fully contained in
/// a wider one), only the wider one must be tracked as an SSA variable.
/// This is the BUG-1 root-cause check: without this filter, MIPS MULT's
/// 64-bit result and the 32-bit Copy slice are kept as two independent
/// variables and the multiplication is dropped.
#[test]
fn new_raw_filters_overlapping_unique_varnodes() -> Result<()> {
    let outer = unique_vn(0x100, 8);
    let inner = unique_vn(0x100, 4); // same offset, narrower
    let b = FunctionBuilder::new_raw(vec![outer, inner], &[], &[], &[], None, 0)?;
    let tracked: Vec<rsleigh::Vn> = b.variables().copied().collect();
    assert!(
        tracked.contains(&outer),
        "wider UNIQUE varnode must remain tracked; got {tracked:?}"
    );
    assert!(
        !tracked.contains(&inner),
        "narrower UNIQUE varnode contained in `outer` must be filtered; got {tracked:?}"
    );
    Ok(())
}

/// Mid-slice (non-zero offset within the container) UNIQUE sub-varnodes
/// must also be filtered.  Mirrors REGISTER-space sub-register handling
/// (e.g. `ah` at offset 1 inside `ax`).
#[test]
fn new_raw_filters_mid_offset_unique_subvarnode() -> Result<()> {
    let outer = unique_vn(0x200, 8);
    let inner = unique_vn(0x204, 4); // upper 4 bytes of outer
    let b = FunctionBuilder::new_raw(vec![outer, inner], &[], &[], &[], None, 0)?;
    let tracked: Vec<rsleigh::Vn> = b.variables().copied().collect();
    assert!(tracked.contains(&outer));
    assert!(!tracked.contains(&inner));
    Ok(())
}

/// Non-overlapping UNIQUE varnodes (different offsets, no containment)
/// must both remain tracked.  Sanity check that the filter does not over-
/// reach.
#[test]
fn new_raw_keeps_disjoint_unique_varnodes() -> Result<()> {
    let a = unique_vn(0x300, 4);
    let b_vn = unique_vn(0x400, 4); // different offset, disjoint
    let b = FunctionBuilder::new_raw(vec![a, b_vn], &[], &[], &[], None, 0)?;
    let tracked: Vec<rsleigh::Vn> = b.variables().copied().collect();
    assert!(tracked.contains(&a));
    assert!(tracked.contains(&b_vn));
    Ok(())
}

// ── BUG-3 regression: Bool-to-flag-register write must coerce to int ─────
//
// ARM/AArch64 status flags (N, Z, V, C) are 1-byte register varnodes.  The
// Sleigh lifter for `cmp` writes Bool-producing ops (`IntCmpOp::Sless`,
// `IntCmpOp::Sborrow`, ...) into those flag registers.  If the write side
// stores the Bool node directly into the variable, downstream phi-reductions
// can collapse a chain like `phi(U8) ← Sless@Bool, Sless@Bool` into a direct
// Sless@Bool feed of a consumer that expects AnyInt — failing the IR
// validator after the optimizer pipeline.
//
// The mitigation that lives at the IR layer is `convert_to_int_if_needed`:
// when called on a Bool with an integer target type, it must produce a
// CastToInt-wrapped value of the integer type.  The analyzer's `write_reg_vn`
// invokes this helper at every variable write; this test pins the helper's
// contract so future refactors don't silently regress the BUG-3 cycle.

fn flag_reg_byte() -> rsleigh::Vn {
    // Generic 1-byte REGISTER varnode shaped like ARM N/Z/V/C flags.
    rsleigh::Vn {
        addr: rsleigh::VnAddr { off: 0x60, space: rsleigh::VnSpace::REGISTER },
        size: 1,
    }
}

/// `convert_to_int_if_needed` on a Bool with U8 target produces a U8-typed
/// output, and a CastToInt node sits between the Bool and the result.
#[test]
fn convert_to_int_if_needed_coerces_bool_to_int() -> Result<()> {
    let mut b = empty_builder()?;
    let bool_val = b.build_boolean_const(true);
    assert_eq!(
        b.graph().output_kind(bool_val),
        NodeOutputKind::OutputType(NodeOutputType::Bool),
        "BoolConst is Bool-typed"
    );
    let coerced = b.convert_to_int_if_needed(bool_val, NodeOutputType::U8)?;
    assert_eq!(
        b.graph().output_kind(coerced),
        NodeOutputKind::OutputType(NodeOutputType::U8),
        "convert_to_int_if_needed must produce the requested int type"
    );
    let coerced_node = b.graph().get_node_from_output(coerced);
    assert_eq!(
        b.graph().node_kind(coerced_node),
        &NodeKind::CastToInt,
        "Bool → int must go through a CastToInt node (BUG-3 root mitigation)"
    );
    Ok(())
}

// ── BUG-8 regression: ret-val regs that the overlap filter dropped must
// upgrade to their tracked container ────────────────────────────────────
//
// MIPS-O32 lists `f0` (4-byte) as the float return register, but a
// double-returning function only writes through the 8-byte combined
// f0/f1 view.  The overlap-filter drops 4-byte f0 in favour of the
// wider 8-byte view.  Pre-fix code then dropped `f0` from
// `ret_val_vars` because it was no longer in `variable_to_id`, and
// the Return node never read the float chain — `f64_arith` returned
// junk from the integer ret-regs.
//
// The fix in `FunctionBuilder::new_raw` upgrades a filtered-out ret
// reg to the smallest tracked variable that fully contains it.

/// 4-byte ret-reg overlapped by an 8-byte tracked view: `ret_val_vars`
/// must contain the 8-byte container, not be empty.
#[test]
fn ret_val_vars_upgrade_to_tracked_container() -> Result<()> {
    let f0_4byte = rsleigh::Vn {
        addr: rsleigh::VnAddr { off: 0x1000, space: rsleigh::VnSpace::REGISTER },
        size: 4,
    };
    let f0_f1_8byte = rsleigh::Vn {
        addr: rsleigh::VnAddr { off: 0x1000, space: rsleigh::VnSpace::REGISTER },
        size: 8,
    };
    // Both varnodes referenced (mimicking the f64-using function): the
    // overlap filter will keep only the 8-byte view.
    let b = FunctionBuilder::new_raw(
        vec![f0_4byte, f0_f1_8byte],
        &[],
        &[],
        &[f0_4byte],
        None,
        0,
    )?;
    assert_eq!(
        b.ret_val_vars(),
        &[f0_f1_8byte],
        "ret_val_vars must upgrade the filtered-out 4-byte f0 to its \
         8-byte tracked container so the Return node still reads the \
         float result chain"
    );
    Ok(())
}

/// Single-precision case: when only the 4-byte view is referenced, the
/// filter doesn't drop it, so no upgrade happens — the ret slot stays at
/// reg's declared 4-byte width.
#[test]
fn ret_val_vars_no_upgrade_when_reg_already_tracked() -> Result<()> {
    let f0_4byte = rsleigh::Vn {
        addr: rsleigh::VnAddr { off: 0x1000, space: rsleigh::VnSpace::REGISTER },
        size: 4,
    };
    let b = FunctionBuilder::new_raw(
        vec![f0_4byte],
        &[],
        &[],
        &[f0_4byte],
        None,
        0,
    )?;
    assert_eq!(
        b.ret_val_vars(),
        &[f0_4byte],
        "ret_val_vars must keep the original 4-byte reg when it's tracked"
    );
    Ok(())
}

/// Ret reg whose container isn't tracked AND no wider view is tracked
/// stays dropped — the upgrade falls back to None when no container
/// exists in the variable set.
#[test]
fn ret_val_vars_drops_when_no_container_tracked() -> Result<()> {
    let f0_4byte = rsleigh::Vn {
        addr: rsleigh::VnAddr { off: 0x1000, space: rsleigh::VnSpace::REGISTER },
        size: 4,
    };
    let unrelated = rsleigh::Vn {
        addr: rsleigh::VnAddr { off: 0x2000, space: rsleigh::VnSpace::REGISTER },
        size: 4,
    };
    // f0_4byte is not in the input set at all.  ret_val_vars upgrade has
    // no candidate, so `f0` is simply dropped from the ret list.
    let b = FunctionBuilder::new_raw(
        vec![unrelated],
        &[],
        &[],
        &[f0_4byte],
        None,
        0,
    )?;
    assert!(
        b.ret_val_vars().is_empty(),
        "ret_val_vars must drop ret regs with no tracked container; got {:?}",
        b.ret_val_vars()
    );
    Ok(())
}

/// End-to-end: write a Bool to a 1-byte register variable through the
/// coerce-then-write sequence the analyzer's `write_reg_vn` uses.  Reading
/// the variable back must return an integer-typed output, never the raw
/// Bool — that was the BUG-3 root state that fed Bool into AnyInt-expecting
/// phi consumers post-optimization.
#[test]
fn write_bool_to_byte_reg_var_coerces_to_int() -> Result<()> {
    let flag = flag_reg_byte();
    let mut b = FunctionBuilder::new_raw(vec![flag], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // Synthesise a Bool-producing op (compare) — the same shape as
    // IntCmpOp::Sless that lifts from `cmp r0, #100`.
    let lhs = b.build_int_const(1u64, NodeOutputType::U32);
    let rhs = b.build_int_const(2u64, NodeOutputType::U32);
    let bool_val = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, NodeOutputType::U32)?;

    // Mirror the analyzer's write_reg_vn coercion: convert to reg's
    // declared int type (U8 for a 1-byte flag), then write.
    let reg_ty: NodeOutputType = flag.size.try_into()?;
    let coerced = b.convert_to_int_if_needed(bool_val, reg_ty)?;
    b.write_variable(&flag, coerced)?;

    // Read back — must be U8-typed, never Bool.
    let read_back = b.read_variable(&flag)?;
    assert_eq!(
        b.graph().output_kind(read_back),
        NodeOutputKind::OutputType(NodeOutputType::U8),
        "1-byte flag variable must read back as U8 after a coerced Bool write"
    );
    Ok(())
}
