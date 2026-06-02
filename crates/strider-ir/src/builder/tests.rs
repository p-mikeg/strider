use super::*;
use anyhow::anyhow;

use crate::error::Result;
use crate::node::{NodeKind, ValueKind, ValueType};
use crate::ops::{ExtendOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

/// Build a minimal builder with no variables so tests that do not need
/// SSA variables remain simple.
fn empty_builder() -> Result<FunctionBuilder> {
    FunctionBuilder::empty()
}

// ── get_as_unsigned_int ──────────────────────────────────────────────────

/// A I8 constant built from a wider raw value must be masked to `u8::MAX`.
#[test]
fn get_unsigned_int_truncates_to_declared_width() -> Result<()> {
    let mut b = empty_builder()?;
    // Store u8::MAX + 1 — only the low byte is in-range for I8
    let value = b.build_int_const(u8::MAX as u64 + 1, ValueType::I8)?;
    // The node was created with kind IntConst(256) but the type is I8,
    // so get_as_unsigned_int must mask it.
    let val = b.get_as_unsigned_int(value)?;
    assert_eq!(val, Some(0)); // 256 & 0xFF == 0
    Ok(())
}

#[test]
fn get_as_int_accepts_bool_const() -> Result<()> {
    let mut b = empty_builder()?;
    let bt = b.build_boolean_const(true);
    let bf = b.build_boolean_const(false);
    // Booleans are 1-bit integers: the single bit is the sign bit, so a
    // `true` (bit 1) sign-extends to -1, while its unsigned view is 1.
    assert_eq!(b.get_as_int(bt)?, Some((1u64, -1i64)));
    assert_eq!(b.get_as_int(bf)?, Some((0u64, 0i64)));
    Ok(())
}

/// `get_as_unsigned_int` on a non-const node must return `None`.
#[test]
fn get_unsigned_int_is_none_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, ValueType::I64)?;
    let rhs = b.build_int_const(2u64, ValueType::I64)?;
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I64)?;
    assert_eq!(b.get_as_unsigned_int(add)?, None);
    Ok(())
}

// ── get_as_signed_int ────────────────────────────────────────────────────

/// A I8 value with MSB set (`u8::MAX`) must sign-extend to -1 as i64.
#[test]
fn get_signed_int_sign_extends_negative_u8() -> Result<()> {
    let mut b = empty_builder()?;
    let value = b.build_int_const(u8::MAX as u64, ValueType::I8)?;
    assert_eq!(b.get_as_signed_int(value)?, Some(-1i64));
    Ok(())
}

/// A I8 value below the sign bit (`i8::MAX`) must stay positive.
#[test]
fn get_signed_int_positive_u8_stays_positive() -> Result<()> {
    let mut b = empty_builder()?;
    let value = b.build_int_const(i8::MAX as u64, ValueType::I8)?;
    assert_eq!(b.get_as_signed_int(value)?, Some(i8::MAX as i64));
    Ok(())
}

// ── truncate_if_needed ───────────────────────────────────────────────────

/// Truncating a constant folds into a new constant of the target type,
/// not a Truncate node.
#[test]
fn truncate_const_folds_to_const() -> Result<()> {
    let mut b = empty_builder()?;
    let value = b.build_int_const(0xABCDu64, ValueType::I16)?;
    let truncated = b.truncate_if_needed(value, ValueType::I8)?;
    // Must fold to a constant
    let val = b.get_as_unsigned_int(truncated)?;
    assert_eq!(val, Some(0xCD), "low byte of 0xABCD is 0xCD");
    // No Truncate node should have been emitted
    let node = b.function().producer(truncated);
    assert!(matches!(b.function().node_kind(node), NodeKind::IntConst(_)));
    Ok(())
}

/// For a **non-const** value already at the target width (or narrower),
/// `truncate_if_needed` must return the same output id unchanged.
/// (Const values are always folded into a new constant node regardless of
/// direction, so the no-op path only applies to non-const values.)
#[test]
fn truncate_noop_when_already_narrow_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    // Build a non-const I8 expression: add(1u8, 2u8)
    let lhs = b.build_int_const(1u64, ValueType::I8)?;
    let rhs = b.build_int_const(2u64, ValueType::I8)?;
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I8)?;
    // "Truncating" to a wider type must return the same node unchanged
    let result = b.truncate_if_needed(add, ValueType::I16)?;
    assert_eq!(
        result, add,
        "non-const I8 value must not be touched when target is I16"
    );
    Ok(())
}

/// A non-constant I32 truncated to I8 must emit a Truncate node.
#[test]
fn truncate_emits_truncate_node_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, ValueType::I32)?;
    let rhs = b.build_int_const(2u64, ValueType::I32)?;
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I32)?;

    let truncated = b.truncate_if_needed(add, ValueType::I8)?;
    let node = b.function().producer(truncated);
    assert!(
        matches!(b.function().node_kind(node), NodeKind::Truncate),
        "expected Truncate node, got {:?}",
        b.function().node_kind(node)
    );
    Ok(())
}

// ── extend_if_needed ─────────────────────────────────────────────────────

/// Zero-extending a constant must fold: the result is a wider constant
/// with high bits cleared.
#[test]
fn zero_extend_const_folds_to_wider_const() -> Result<()> {
    let mut b = empty_builder()?;
    let value = b.build_int_const(u8::MAX as u64, ValueType::I8)?;
    let extended = b.extend_if_needed(value, ValueType::I32, ExtendOp::ZeroExtend)?;
    assert_eq!(b.get_as_unsigned_int(extended)?, Some(u8::MAX as u64));
    let node = b.function().producer(extended);
    assert!(matches!(b.function().node_kind(node), NodeKind::IntConst(_)));
    Ok(())
}

/// Sign-extending a negative I8 constant (`u8::MAX` = -1 as i8) must fold
/// to `u32::MAX` (all bits set) as a wider constant.
#[test]
fn sign_extend_const_folds_negative_value() -> Result<()> {
    let mut b = empty_builder()?;
    let value = b.build_int_const(u8::MAX as u64, ValueType::I8)?;
    let extended = b.extend_if_needed(value, ValueType::I32, ExtendOp::SignExtend)?;
    assert_eq!(b.get_as_unsigned_int(extended)?, Some(u32::MAX as u64));
    Ok(())
}

/// Extending a non-constant must emit an Extend node.
#[test]
fn extend_emits_extend_node_for_non_const() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, ValueType::I8)?;
    let rhs = b.build_int_const(2u64, ValueType::I8)?;
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I8)?;

    let extended = b.extend_if_needed(add, ValueType::I64, ExtendOp::ZeroExtend)?;
    let node = b.function().producer(extended);
    assert!(
        matches!(b.function().node_kind(node), NodeKind::Extend(_)),
        "expected Extend node"
    );
    Ok(())
}

/// If the value is already the target width, `extend_if_needed` must
/// return it unchanged.
#[test]
fn extend_noop_when_already_wide_enough() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, ValueType::I64)?;
    let rhs = b.build_int_const(2u64, ValueType::I64)?;
    let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I64)?;

    let result = b.extend_if_needed(add, ValueType::I64, ExtendOp::ZeroExtend)?;
    assert_eq!(result, add);
    Ok(())
}

// ── build_int_binary_operation ────────────────────────────────────────────

/// Building an Add on two constants of the same type must produce an
/// `IntBinaryOp(Add)` node (no constant folding at this layer).
#[test]
fn build_int_binary_op_produces_binary_op_node() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(3u64, ValueType::I64)?;
    let rhs = b.build_int_const(4u64, ValueType::I64)?;
    let result =
        b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I64)?;
    let node = b.function().producer(result);
    assert_eq!(
        b.function().node_kind(node),
        &NodeKind::IntBinaryOp(IntBinaryOp::Add)
    );
    Ok(())
}

/// The builder is strict: when an operand is narrower than the target
/// type, the *caller* inserts the coercion (`convert_to_int_if_needed`)
/// so both operands reach the target type before the build.
#[test]
fn build_int_binary_op_coerces_narrower_operand() -> Result<()> {
    let mut b = empty_builder()?;
    let lhs = b.build_int_const(1u64, ValueType::I8)?;
    let rhs = b.build_int_const(2u64, ValueType::I64)?;
    let lhs = b.convert_to_int_if_needed(lhs, ValueType::I64)?;
    let result =
        b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I64)?;
    // The result must be typed as I64
    let kind = b.function().value_kind(result);
    assert_eq!(kind, ValueKind::Typed(ValueType::I64));
    Ok(())
}

// ── build_int_cmp_operation ───────────────────────────────────────────────

/// A comparison must always produce a `Bool` output regardless of the
/// operand type.
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

// ── boolean (I1) bitwise operation ──────────────────────────────────────────

/// Boolean AND of two I1 constants is modelled as a bitwise
/// `IntBinaryOp(And)` typed `I1` (booleans are 1-bit integers).
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

// ── deduplication across build helpers ────────────────────────────────────

/// Two identical constants must alias to the same output id (graph-level
/// deduplication).
#[test]
fn identical_constants_are_deduplicated() -> Result<()> {
    let mut b = empty_builder()?;
    let a = b.build_int_const(77u64, ValueType::I32)?;
    let c = b.build_int_const(77u64, ValueType::I32)?;
    assert_eq!(a, c, "same constant must reuse the same node");
    Ok(())
}

/// Two constants with different values must NOT alias.
#[test]
fn different_constants_are_distinct() -> Result<()> {
    let mut b = empty_builder()?;
    let a = b.build_int_const(1u64, ValueType::I32)?;
    let c = b.build_int_const(2u64, ValueType::I32)?;
    assert_ne!(a, c);
    Ok(())
}

// ── Float builder methods ────────────────────────────────────────────────

#[test]
fn build_float_const_f32_has_correct_bits() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f32.to_bits() as u64;
    let value = b.build_float_const(bits, ValueType::F32);
    let kind = *b.function().kind_of_value(value);
    assert_eq!(kind, NodeKind::FloatConst(bits));
    assert_eq!(
        b.function().value_kind(value),
        ValueKind::Typed(ValueType::F32)
    );
    Ok(())
}

#[test]
fn build_float_const_f64_has_correct_bits() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f64.to_bits();
    let value = b.build_float_const(bits, ValueType::F64);
    let kind = *b.function().kind_of_value(value);
    assert_eq!(kind, NodeKind::FloatConst(bits));
    assert_eq!(
        b.function().value_kind(value),
        ValueKind::Typed(ValueType::F64)
    );
    Ok(())
}

#[test]
fn get_as_float_bits_returns_bits_for_float_const() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 2.5f64.to_bits();
    let value = b.build_float_const(bits, ValueType::F64);
    assert_eq!(b.get_as_float_bits(value)?, Some(bits));
    Ok(())
}

#[test]
fn get_as_float_bits_returns_none_for_int_const() -> Result<()> {
    let mut b = empty_builder()?;
    let value = b.build_int_const(42u64, ValueType::I64)?;
    assert_eq!(b.get_as_float_bits(value)?, None);
    Ok(())
}

#[test]
fn int_bits_to_float_folds_int_const_immediately() -> Result<()> {
    let mut b = empty_builder()?;
    let bits = 1.0f32.to_bits() as u64;
    let int_value = b.build_int_const(bits, ValueType::I32)?;
    let float_value = b.build_int_bits_to_float(int_value, ValueType::F32)?;
    // Should be a FloatConst, not an IntBitsToFloat node
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
    // Should be an IntConst, not a FloatBitsToInt node
    let kind = *b.function().kind_of_value(int_value);
    assert_eq!(kind, NodeKind::IntConst(u128::from(bits)));
    Ok(())
}

#[test]
fn int_bits_to_float_rejects_width_mismatch() -> Result<()> {
    let mut b = empty_builder()?;
    // A bit-reinterpret must be same-width: I64 (8 bytes) -> F32 (4 bytes)
    // is nonsensical and must error rather than silently truncate.
    let int_value = b.build_int_const(0u64, ValueType::I64)?;
    assert!(b
        .build_int_bits_to_float(int_value, ValueType::F32)
        .is_err());
    Ok(())
}

#[test]
fn float_bits_to_int_rejects_width_mismatch() -> Result<()> {
    let mut b = empty_builder()?;
    // F64 (8 bytes) -> I32 (4 bytes) reinterpret is a width mismatch.
    let float_value = b.build_float_const(0u64, ValueType::F64);
    assert!(b
        .build_float_bits_to_int(float_value, ValueType::I32)
        .is_err());
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
    // Build an Add(x, 0) so the result is not an IntConst node.
    let non_const = b.build_int_binary_operation(
        int_val,
        zero,
        crate::ops::IntBinaryOp::Add,
        ValueType::I32,
    )?;
    let float_value = b.build_int_bits_to_float(non_const, ValueType::F32)?;
    let kind = *b.function().kind_of_value(float_value);
    assert_eq!(kind, NodeKind::IntBitsToFloat);
    Ok(())
}

// ── int→float bitcast tests ────────────────────────────────────────────────

#[test]
fn cast_to_float_of_int_is_int_bits_to_float() -> Result<()> {
    let mut b = empty_builder()?;
    // A non-const int so the immediate IntConst→FloatConst fold doesn't apply.
    let raw = b.build_int_const(42u64, ValueType::I64)?;
    let opaque = b.build_int_unary_operation(raw, crate::ops::IntUnaryOp::Neg, ValueType::I64)?;
    let cast = b.cast_to_float_if_needed(opaque, ValueType::F64)?;
    // No CastToFloat node exists: a same-width int→float is a bitcast.
    assert_eq!(*b.function().kind_of_value(cast), NodeKind::IntBitsToFloat);
    assert_eq!(b.value_type(cast)?, ValueType::F64);
    Ok(())
}

#[test]
fn cast_to_float_if_needed_is_identity_for_same_type() -> Result<()> {
    let mut b = empty_builder()?;
    let float_val = b.build_float_const(1.0f32.to_bits() as u64, ValueType::F32);
    let result = b.cast_to_float_if_needed(float_val, ValueType::F32)?;
    // Should be the same output — no new node inserted.
    assert_eq!(result, float_val);
    Ok(())
}

#[test]
fn build_float_binary_op_with_int_inputs_bitcasts() -> Result<()> {
    let mut b = empty_builder()?;
    // Non-const I32 operands (a const would immediately fold IntBitsToFloat
    // into a FloatConst, hiding the bitcast node).
    let c1 = b.build_int_const(0x3F800000u64, ValueType::I32)?;
    let c2 = b.build_int_const(0x40000000u64, ValueType::I32)?;
    let i1 = b.build_int_unary_operation(c1, crate::ops::IntUnaryOp::Neg, ValueType::I32)?;
    let i2 = b.build_int_unary_operation(c2, crate::ops::IntUnaryOp::Neg, ValueType::I32)?;
    // Both inputs are I32 — the caller reinterprets each as F32 via
    // IntBitsToFloat (`cast_to_float_if_needed`) before the strict build.
    let i1 = b.cast_to_float_if_needed(i1, ValueType::F32)?;
    let i2 = b.cast_to_float_if_needed(i2, ValueType::F32)?;
    let result = b.build_float_binary_op(i1, i2, FloatBinaryOp::Add, ValueType::F32)?;
    let kind = *b.function().kind_of_value(result);
    assert_eq!(kind, NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
    let [lhs, rhs] = b
        .function().graph()
        .node_inputs_exact::<2>(b.function().producer(result))?;
    let lhs_node = b.function().producer(lhs);
    let rhs_node = b.function().producer(rhs);
    assert_eq!(*b.function().node_kind(lhs_node), NodeKind::IntBitsToFloat);
    assert_eq!(*b.function().node_kind(rhs_node), NodeKind::IntBitsToFloat);
    Ok(())
}

// ── CallOther / SegmentOp / CPoolRef / New ──────────────────────────────

/// Helper: build a single-region builder with an active region set.
fn builder_with_region() -> Result<FunctionBuilder> {
    let mut b = FunctionBuilder::empty()?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    Ok(b)
}

#[test]
fn build_call_other_without_result_advances_ctrl_only() -> Result<()> {
    let mut b = builder_with_region()?;
    let ctrl_before = b.cur_region_control()?;
    let mem_before = b.cur_region_memory()?;

    let (node, value, clobber_outs) =
        b.build_call_other(7, "NEON_rev64", None, &[], &[], &[], None, false)?;
    assert!(value.is_none(), "no result_ty -> no value output");
    assert!(clobber_outs.is_empty(), "no clobbers -> no clobber slots");

    // Ctrl advances; memory does NOT (caller decides via memory_edge).
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
    let mut b = builder_with_region()?;
    let arg = b.build_int_const(0x42u64, ValueType::I64)?;
    let (node, value, _) = b.build_call_other(
        3,
        "cpuid",
        None,
        &[arg],
        &[],
        &[],
        Some(ValueType::I32),
        false,
    )?;
    let val = value.ok_or_else(|| anyhow!("result_ty = Some -> value output"))?;
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
    // C2 (strider): pin Graph::memory_output_of as the named accessor
    // for what handle_call_other previously read as `node_outputs[1]`.
    let mut b = builder_with_region()?;
    let (node, _, _) = b.build_call_other(
        4,
        "cpuid",
        None,
        &[],
        &[],
        &[],
        Some(ValueType::I32),
        false,
    )?;
    let mem_value = b.function().graph().memory_output_of(node)?;
    assert_eq!(b.function().value_kind(mem_value), ValueKind::Memory);
    Ok(())
}

#[test]
fn memory_output_of_errors_on_node_with_no_memory_output() -> Result<()> {
    let mut b = builder_with_region()?;
    let c = b.build_int_const(7u64, ValueType::I32)?;
    let int_node = b.function().producer(c);
    let err = b
        .function().graph()
        .memory_output_of(int_node)
        .expect_err("IntConst has no Memory output");
    assert!(
        err.to_string().contains("no Memory output"),
        "got: {err}"
    );
    Ok(())
}

#[test]
fn build_call_other_rejects_non_value_arg() -> Result<()> {
    let mut b = builder_with_region()?;
    let mem = b.cur_region_memory()?;
    let res = b.build_call_other(0, "cpuid", None, &[mem], &[], &[], None, false);
    let err = res.expect_err("expected ExpectedValue error");
    assert!(
        err.to_string().contains("is not a value edge"),
        "got: {err}"
    );
    Ok(())
}

/// Helper: construct a register-space Vn for CallOther implicit-write
/// tests.  CallOther's per-node clobber-override side-table records
/// these Vns verbatim; they don't need to correspond to any
/// tracked-variable entry.
fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

/// After entry-region setup, the builder's build-time `VarId ↔ Vn`
/// table and the stored `all_vns` SSoT must agree on the
/// tracked-variable set: every tracked var appears exactly once in
/// `all_vns` and has exactly one corresponding `InitialVar` node.
/// `tracked_vns()` (which reads `all_vns`) must surface exactly the
/// tracked set.
#[test]
fn all_vns_tracks_each_initial_var_varnode() -> Result<()> {
    let r0 = reg_vn(0x10, 8);
    let r1 = reg_vn(0x20, 8);
    let mut b = FunctionBuilder::new_raw(vec![r0, r1], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // (a) tracked_vns() surfaces exactly {r0, r1}.
    let tracked: std::collections::HashSet<rsleigh::Vn> =
        b.function().tracked_vns().collect();
    let expected: std::collections::HashSet<rsleigh::Vn> =
        [r0, r1].into_iter().collect();
    assert_eq!(tracked, expected, "tracked_vns must be exactly the tracked set");

    // (b) Each tracked var appears once in `all_vns` and corresponds to
    // exactly one `InitialVar` node.  Cross-check via the builder's
    // build-time VarId table (vn_of_var).
    let all_vns: Vec<rsleigh::Vn> = b.function().tracked_vns().collect();
    assert_eq!(all_vns.len(), 2, "one all_vns entry per tracked var");
    for var_id in b.var_table.keys() {
        let vn = b.vn_of_var(var_id).expect("tracked var has a Vn");
        assert_eq!(
            all_vns.iter().filter(|&&v| v == vn).count(),
            1,
            "exactly one all_vns entry for {vn:?}",
        );
        // The function carries exactly one InitialVar node for this varnode.
        let initial_vars = b
            .function()
            .graph()
            .all_node_ids()
            .filter(|&n| {
                matches!(b.function().node_kind(n), NodeKind::InitialVar(v) if *v == vn)
            })
            .count();
        assert_eq!(
            initial_vars, 1,
            "exactly one InitialVar({vn:?}) node",
        );
    }
    Ok(())
}

/// Regression: high-offset register varnodes (e.g. ppc64 / aarch64be
/// condition-register slices) can have `addr_off + size` overflow `u64`.
/// The overlap-dedup helper must use saturating arithmetic to match the
/// convention `largest_container_for` already documents — a plain `+`
/// panics in debug and wrap-misclassifies containment in release.
#[test]
fn dedup_overlapping_largest_is_overflow_safe_on_high_offset_varnodes() {
    let wide = reg_vn(u64::MAX - 1, 8);
    let narrow = reg_vn(u64::MAX - 1, 4);
    // Must not panic; the wider varnode subsumes the narrower one.
    let kept = dedup_overlapping_largest(&[wide, narrow]);
    assert_eq!(kept, vec![wide], "wider high-offset varnode wins, no overflow");
}

#[test]
fn build_call_other_with_value_emits_value_then_clobbers_in_order() -> Result<()> {
    // Two synthetic clobber kinds; their corresponding Vns are
    // recorded only on the per-CallOther clobber-override side-table.
    // No tracked-variable lookup happens in build_call_other.
    let mut b = builder_with_region()?;
    let r0 = reg_vn(0, 4);
    let r1 = reg_vn(4, 4);

    let (node, value, clobber_outs) = b.build_call_other(
        8,
        "cpuid",
        None,
        &[],
        &[r0, r1],
        &[
            ValueKind::Typed(ValueType::I32),
            ValueKind::Typed(ValueType::I32),
        ],
        Some(ValueType::I32),
        false,
    )?;
    assert!(value.is_some(), "result_ty -> value slot");
    assert_eq!(
        clobber_outs.len(),
        2,
        "two clobbers -> two clobber slots"
    );
    let n_outs = b.function().node_outputs(node).len();
    assert_eq!(n_outs, 5, "ctrl + mem + value + 2 clobbers");
    assert_eq!(b.function().call_other_name(node), Some("cpuid"));
    Ok(())
}

#[test]
fn build_call_other_rejects_non_value_clobber_kind() -> Result<()> {
    let mut b = builder_with_region()?;
    let r0 = reg_vn(0, 4);
    let res = b.build_call_other(
        11,
        "bogus",
        None,
        &[],
        &[r0],
        &[ValueKind::Control],
        None,
        false,
    );
    let err = res.expect_err("non-value clobber kind should be rejected");
    assert!(
        err.to_string().contains("not a value kind"),
        "got: {err}"
    );
    Ok(())
}

#[test]
fn build_call_other_rejects_arity_mismatch_between_clobber_vns_and_kinds() -> Result<()> {
    let mut b = builder_with_region()?;
    let r0 = reg_vn(0, 4);
    let res = b.build_call_other(12, "bogus", None, &[], &[r0], &[], None, false);
    let err = res.expect_err("arity mismatch should be rejected");
    assert!(
        err.to_string().contains("clobber_vns.len()"),
        "got: {err}"
    );
    Ok(())
}

#[test]
fn create_node_attributed_unions_contributor_fingerprints() -> Result<()> {
    // Pin the contract: create_node_attributed unions every contributor's
    // asm-fingerprint into the resulting node, so opt-pass synthesised
    // nodes carry a superset of their contributors' attribution.
    let mut b = builder_with_region()?;
    // Seed two IntConsts under different lift_addrs.
    b.set_lift_addr(Some(0x100));
    let l = b.build_int_const(5u64, ValueType::I8)?;
    let l_node = b.function().producer(l);
    b.set_lift_addr(Some(0x104));
    let r = b.build_int_const(7u64, ValueType::I8)?;
    let r_node = b.function().producer(r);
    // Synthesise a fresh Or node attributing both.  Use the IR graph's
    // create_node_attributed directly (rather than going through the
    // builder) to test the helper in isolation.
    b.set_lift_addr(None);
    let or_node = b.function_mut().create_node_attributed(
        NodeKind::IntBinaryOp(IntBinaryOp::Or),
        [l, r],
        [crate::node::ValueKind::Typed(ValueType::I8)],
        &[l_node, r_node],
    );
    let fp = b.function().asm_fingerprint(or_node);
    assert!(
        fp.contains(&0x100) && fp.contains(&0x104),
        "create_node_attributed must union both contributors' fingerprints; got {fp:?}"
    );
    Ok(())
}

#[test]
fn create_node_cache_hit_unions_lift_addr_into_fingerprint() -> Result<()> {
    // Pin the asm-fingerprint contract: when create_node hits the
    // dedup cache (returning a previously-built equivalent NodeId),
    // the wrapping FunctionBuilder must STILL union the current
    // lift_addr into the cached node's fingerprint.  Without this
    // union the second lift's contributing address would be silently
    // lost — patterns matching the cached node would not see the
    // second lift's attribution.
    let mut b = builder_with_region()?;

    // First build of IntConst(42, I64) under lift_addr 0x100.
    b.set_lift_addr(Some(0x100));
    let c1 = b.build_int_const(42u64, ValueType::I64)?;
    let c1_node = b.function().producer(c1);

    // Second build under lift_addr 0x104.  Same kind+type+inputs, so
    // create_node returns the cached NodeId.
    b.set_lift_addr(Some(0x104));
    let c2 = b.build_int_const(42u64, ValueType::I64)?;
    let c2_node = b.function().producer(c2);

    assert_eq!(c1_node, c2_node, "cache must return the same NodeId");
    let fp = b.function().asm_fingerprint(c1_node);
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
    // Pin the trap (NoReturn-class) CallOther's output shape: with no
    // args / clobbers / result, exactly two outputs, both structural
    // (Control + Memory).  terminate=true closes the region as part of
    // the no-return classification.
    let mut b = builder_with_region()?;
    let (node, value, clobbers) = b.build_call_other(0, "ud2", None, &[], &[], &[], None, true)?;
    assert!(value.is_none(), "no result_ty -> no value output");
    assert!(clobbers.is_empty(), "no clobbers -> no clobber slots");
    let outs: Vec<_> = b.function().node_outputs(node).to_vec();
    assert_eq!(outs.len(), 2, "trap CallOther has exactly [Control, Memory]");
    let kinds: Vec<_> = outs.iter().map(|o| b.function().value_kind(*o)).collect();
    assert!(matches!(kinds[0], ValueKind::Control), "slot 0 must be Control");
    assert!(matches!(kinds[1], ValueKind::Memory), "slot 1 must be Memory");
    Ok(())
}

#[test]
fn build_return_self_terminates() -> Result<()> {
    // build_return owns its own region termination — no external
    // mark_cur_region_terminated call is needed.  After build_return
    // returns, the region is already closed and cur_region_control() errors.
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
    // build_call_other with terminate=true (the NoReturn class) must
    // close the region on its own — no external mark_cur_region_terminated.
    let mut b = builder_with_region()?;
    b.build_call_other(0, "ud2", None, &[], &[], &[], None, true)?;
    let ctrl = b.cur_region_control();
    assert!(
        ctrl.is_err(),
        "cur_region_control must fail after build_call_other(terminate=true); got: {ctrl:?}"
    );
    Ok(())
}

#[test]
fn build_call_other_terminate_false_keeps_region_open() -> Result<()> {
    // build_call_other with terminate=false (the modeled Call class) must
    // leave the region open — control advances to the CallOther's Control
    // output, but the region is still live.
    let mut b = builder_with_region()?;
    b.build_call_other(0, "cpuid", None, &[], &[], &[], None, false)?;
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
    assert_eq!(b.function().node_kind(node), &NodeKind::SegmentOp { op_id: 1 });
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

// ── extend_if_needed with an I1 (boolean) input ───────────────────────────

/// `extend_if_needed` widening an I1 (boolean) value to a wider integer
/// must emit a real `ZeroExtend` — I1 is now an ordinary 1-bit integer, so
/// no separate bool→int cast is needed.
///
/// Concretely: MIPS/ARM comparison instructions emit an I1 result that may
/// then be zero-extended into a wider register.
#[test]
fn extend_if_needed_with_bool_input_inserts_cast_to_int() -> Result<()> {
    let mut b = empty_builder()?;

    // Build an I1 value: an integer comparison 1 < 2 (always true, but
    // not folded at this layer — the builder does not constant-fold cmps).
    let lhs = b.build_int_const(1u64, ValueType::I32)?;
    let rhs = b.build_int_const(2u64, ValueType::I32)?;
    let bool_val = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, ValueType::I32)?;

    // Sanity: the comparison result is I1-typed.
    assert_eq!(
        b.function().value_kind(bool_val),
        ValueKind::Typed(ValueType::I1),
        "comparison must produce I1"
    );

    // Extend the I1 into a I32.
    let extended = b.extend_if_needed(bool_val, ValueType::I32, ExtendOp::ZeroExtend)?;

    // The result must be I32-typed.
    assert_eq!(
        b.function().value_kind(extended),
        ValueKind::Typed(ValueType::I32),
        "extend_if_needed must produce I32 when requested"
    );

    // The widening produces a ZeroExtend node consuming the I1 directly.
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

// ── F80 / I80 bit-conversion: skip the immediate-fold ────────────────────
//
// `build_int_bits_to_float(IntConst, F32/F64)` and
// `build_float_bits_to_int(FloatConst, I8..I128)` immediate-fold to the
// other constant kind because F32/F64 fit in `FloatConst`'s u64 payload.
// F80 is 80 bits — doesn't fit — so the immediate-fold must be skipped
// and a real bit-conversion node emitted.  This pins that behavior so a
// future contributor doesn't accidentally truncate F80 by re-enabling
// the fold for all widths.

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
    // Non-F80 path still folds for safety regression: F64 IntBitsToFloat
    // collapses to FloatConst.
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
    // Non-F80 path still folds: F64 FloatBitsToInt collapses to IntConst.
    let float_const64 = b.build_float_const(0u64, ValueType::F64);
    let result_u64 = b.build_float_bits_to_int(float_const64, ValueType::I64)?;
    let node_u64 = b.function().producer(result_u64);
    assert!(
        matches!(b.function().node_kind(node_u64), NodeKind::IntConst(_)),
        "F64 path must still fold to IntConst (regression check)"
    );
    Ok(())
}

// ── post-call SP adjust ─────────────────────────────────────────────────

use strider_ir_test_utils::stack_vn_x86_64 as sp_vn_u64;

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
    let target = b.build_int_const(0x1000u64, ValueType::I64)?;
    b.build_call(target)?;

    let post_sp = b.read_variable(&sp)?;
    assert_ne!(
        pre_sp, post_sp,
        "SP must be rebound after Call when ret_stack_pop != 0"
    );

    // The new SP must be an Add node.
    let add_node = b.function().producer(post_sp);
    assert_eq!(
        b.function().node_kind(add_node),
        &NodeKind::IntBinaryOp(IntBinaryOp::Add)
    );

    let inputs: Vec<ValueId> = b.function().node_inputs(add_node).into_iter().collect();
    assert_eq!(inputs.len(), 2, "Add has two inputs");

    // One input is the pre-call SP; the other is an IntConst(8).
    let (lhs, rhs) = (inputs[0], inputs[1]);
    assert_eq!(lhs, pre_sp, "Add consumes the pre-call SP output");
    let rhs_kind = *b.function().kind_of_value(rhs);
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
    let target = b.build_int_const(0x1000u64, ValueType::I64)?;
    b.build_call(target)?;

    let post_sp = b.read_variable(&sp)?;
    // No Add node was emitted — SP is unchanged.
    assert_eq!(
        pre_sp, post_sp,
        "ret_stack_pop = 0 must not introduce a new Add node"
    );
    Ok(())
}

// ── UNIQUE-space overlapping varnode filtering ─────────
//
// Sleigh occasionally writes a wider UNIQUE varnode and reads a narrow slice
// of it (e.g. on MIPS, MULT writes a 64-bit unique then a Copy reads a 4-byte
// slice into $v0).  Without filtering, the 4-byte and 8-byte unique varnodes
// were treated as independent SSA variables — the narrow read returned an
// undefined `InitialVar` and the multiplication never materialised in IR.
//
// The fix in `FunctionBuilder::new_raw` extends the same overlap-filter that
// REGISTER space uses to UNIQUE space: when both an outer and an inner
// varnode are touched, the outer wins, and the pcode-lift register-aliasing
// logic rebuilds the inner via shift/truncate when needed.

fn unique_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: off,
        addr_space: rsleigh::VnSpace::UNIQUE,
        size,
    }
}

/// When two UNIQUE-space varnodes overlap (a narrow one fully contained in
/// a wider one), only the wider one must be tracked as an SSA variable.
/// This is the regression check: without this filter, MIPS MULT's
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

// ── Bool-to-flag-register write must coerce to int ─────
//
// ARM/AArch64 status flags (N, Z, V, C) are 1-byte register varnodes.  The
// Sleigh lifter for `cmp` writes Bool-producing ops (`IntCmpOp::Sless`,
// `IntCmpOp::Sborrow`, ...) into those flag registers.  If the write side
// stores the Bool node directly into the variable, downstream phi-reductions
// can collapse a chain like `phi(I8) ← Sless@Bool, Sless@Bool` into a direct
// Sless@Bool feed of a consumer that expects AnyInt — failing the IR
// validator after the optimizer pipeline.
//
// The mitigation that lives at the IR layer is `convert_to_int_if_needed`:
// when called on a Bool with an integer target type, it must produce a
// CastToInt-wrapped value of the integer type.  The pcode-lift `write_reg_vn`
// invokes this helper at every variable write; this test pins the helper's
// contract so future refactors don't silently regress the bool-into-int cycle.

fn flag_reg_byte() -> rsleigh::Vn {
    // Generic 1-byte REGISTER varnode shaped like ARM N/Z/V/C flags.
    rsleigh::Vn {
        addr_off: 0x60,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 1,
    }
}

/// `convert_to_int_if_needed` on an I1 (boolean) with an I8 target produces
/// an I8-typed output.  Because the boolean here is a *constant* (true), the
/// width change folds directly into an `IntConst(1)` typed I8.
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
    assert_eq!(
        b.function().node_kind(coerced_node),
        &NodeKind::IntConst(1),
        "constant I1 → I8 must fold to IntConst(1) typed I8"
    );
    Ok(())
}

// ── ret-val regs that the overlap filter dropped must
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
        addr_off: 0x1000,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let f0_f1_8byte = rsleigh::Vn {
        addr_off: 0x1000,
        addr_space: rsleigh::VnSpace::REGISTER,
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
        addr_off: 0x1000,
        addr_space: rsleigh::VnSpace::REGISTER,
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
        addr_off: 0x1000,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let unrelated = rsleigh::Vn {
        addr_off: 0x2000,
        addr_space: rsleigh::VnSpace::REGISTER,
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

// ── Derived register lists + function return ──────────────────────────
//
// These tests pin the register-list projections (`ret_val_regs`,
// `call_clobbered`, `call_other_clobbered`) and the
// `build_function_return` lowering.  The projections are no longer stored
// fields — they are derived on demand from `Function::all_vns` +
// `Function::default_cc` (via `upgrade_vn` + the clobber filter), so these
// tests confirm the derivations reproduce the same shapes the formerly
// build-time-computed lists held.

/// The built function's `ret_val_regs()` / `call_clobbered_regs()`
/// accessors surface exactly the projected lists `new_raw` computed —
/// the representative ABI shape with a sub-register ret upgrade and a
/// caller-clobbered split (ret-prefix then the rest).
#[test]
fn projected_cc_lists_match_built_function_fields() -> Result<()> {
    let r0 = reg_vn(0x10, 8); // ret + arg + clobbered
    let r1 = reg_vn(0x20, 8); // plain caller-clobbered
    let r2 = reg_vn(0x30, 8); // callee-saved (excluded from clobber)
    let sp = reg_vn(0x40, 8); // stack pointer (excluded from clobber)

    let mut b = FunctionBuilder::new_raw(
        vec![r0, r1, r2, sp],
        &[r0],       // arg_passing
        &[r2],       // callee_saved
        &[r0],       // ret_vars
        Some(sp),
        0,
    )?;

    // ret_val_regs: r0 is tracked, no upgrade needed.
    assert_eq!(
        b.function().ret_val_regs(),
        &[r0],
        "ret_val_regs projects the ABI ret list"
    );

    // call_clobbered: ret-prefix (r0) then the rest (r1); r2 (callee-saved)
    // and sp (stack pointer) excluded; ordering is ret-first.
    assert_eq!(
        b.function().call_clobbered_regs(),
        &[r0, r1],
        "call_clobbered front-loads ret regs then appends the remaining \
         caller-clobbered regs, excluding callee-saved and SP"
    );

    // call_other_clobbered is populated by `build()`: complete the
    // function with a minimal terminated region first.
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    b.set_lift_addr(Some(0x1000));
    b.build_function_return()?;
    b.set_lift_addr(None);
    let f = b.build()?;

    // call_other_clobbered: every tracked var except SP.
    let mut coc: Vec<_> = f.call_other_clobbered_regs().to_vec();
    coc.sort_by_key(|v| v.addr_off);
    assert_eq!(
        coc,
        vec![r0, r1, r2],
        "call_other_clobbered is every tracked var except the stack pointer"
    );

    Ok(())
}

/// `call_clobbered_for(cc)` derives a per-call clobber set against an
/// arbitrary CC over the same `all_vns`.  An override CC that marks an
/// extra register callee-saved must yield a strictly smaller clobber set
/// than the function-default — proving the derivation honours the
/// effective CC rather than a baked-in default list.
#[test]
fn call_clobbered_for_override_cc_differs_from_default() -> Result<()> {
    let r0 = reg_vn(0x10, 8); // ret + clobbered under default
    let r1 = reg_vn(0x20, 8); // plain caller-clobbered under default
    let r2 = reg_vn(0x30, 8); // callee-saved under default
    let sp = reg_vn(0x40, 8); // stack pointer

    let b = FunctionBuilder::new_raw(
        vec![r0, r1, r2, sp],
        &[r0],  // arg_passing
        &[r2],  // callee_saved (default)
        &[r0],  // ret_vars
        Some(sp),
        0,
    )?;
    let f = b.function();

    // Default: ret-prefix r0, then r1.  (r2 callee-saved, sp excluded.)
    assert_eq!(
        f.call_clobbered_for(f.default_cc()),
        vec![r0, r1],
        "default-CC derivation matches the function default"
    );
    assert_eq!(f.call_clobbered_for(f.default_cc()), f.call_clobbered_regs());

    // Override CC: mark BOTH r1 and r2 callee-saved, no ret regs.  The
    // override clobber set must shrink to just r0 (the only tracked,
    // non-callee-saved, non-SP var) — strictly smaller than the default.
    let override_cc = strider_target::BuiltCallingConvention {
        arg_passing_regs: vec![],
        callee_saved_regs: vec![r1, r2],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    };
    assert_eq!(
        f.call_clobbered_for(&override_cc),
        vec![r0],
        "override CC marking r1+r2 callee-saved shrinks the clobber set to r0"
    );
    assert!(
        f.call_clobbered_for(&override_cc).len()
            < f.call_clobbered_for(f.default_cc()).len(),
        "override clobber set must be strictly smaller than the default"
    );
    Ok(())
}

/// `build_function_return` wires exactly the function's resolved CC
/// return registers (in `ret_val_regs()` order) as the Return node's
/// value inputs — no caller threads the list anymore.
#[test]
fn build_function_return_wires_exactly_the_cc_ret_regs() -> Result<()> {
    let r0 = reg_vn(0x10, 8);
    let r1 = reg_vn(0x18, 8);
    let mut b = FunctionBuilder::new_raw(
        vec![r0, r1],
        &[],
        &[],
        &[r0, r1], // two ABI ret regs
        None,
        0,
    )?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // The current value of each ret var is its InitialVar value.
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

    // Find the Return node and inspect its value inputs (skip ctrl + mem).
    let entry = f.entry().expect("built function has an entry");
    let ret = f
        .graph()
        .walk_from(entry)
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("function-return path emits a Return node");
    let inputs: Vec<ValueId> = f.node_inputs(ret).into_iter().collect();
    // inputs[0] = control, inputs[1] = memory, the rest are ret values.
    let ret_values: Vec<ValueId> = inputs[2..].to_vec();
    assert_eq!(
        ret_values, expected,
        "build_function_return wires exactly the CC ret regs' current \
         values, in ret_val_regs() order"
    );
    Ok(())
}

/// End-to-end: write a Bool to a 1-byte register variable through the
/// coerce-then-write sequence pcode-lift's `write_reg_vn` uses.  Reading
/// the variable back must return an integer-typed output, never the raw
/// Bool — that was the root state that fed Bool into AnyInt-expecting
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
    let lhs = b.build_int_const(1u64, ValueType::I32)?;
    let rhs = b.build_int_const(2u64, ValueType::I32)?;
    let bool_val = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, ValueType::I32)?;

    // Mirror pcode-lift's write_reg_vn coercion: convert to reg's
    // declared int type (I8 for a 1-byte flag), then write.
    let reg_ty = ValueType::int_for_byte_size(flag.size)?;
    let coerced = b.convert_to_int_if_needed(bool_val, reg_ty)?;
    b.write_variable(&flag, coerced)?;

    // Read back — must be I8-typed, never Bool.
    let read_back = b.read_variable(&flag)?;
    assert_eq!(
        b.function().value_kind(read_back),
        ValueKind::Typed(ValueType::I8),
        "1-byte flag variable must read back as I8 after a coerced Bool write"
    );
    Ok(())
}

// ── upgrade_to_tracked sub-register fallback ─
//
// On x86_64 SysV, `arg_passing_regs[0] = RDI` (8-byte).  For
// `int forward_1(int a) { sink1(a); return a; }`, the function only ever
// reads `EDI` (4-byte sub-register).  The IR builder's overlap filter
// keeps `EDI` (since `RDI` is never touched), but the calling
// convention asks `upgrade_to_tracked` to map `RDI` to a tracked variable.
// The original implementation only searched for a tracked variable that
// COVERS `vn` (wider-or-equal byte range fully containing it).  No such
// tracked variable existed in this case, so the lookup returned `None`
// and `arg_passing_vars` excluded `RDI` entirely — the `Call` node ended
// up with no slot for arg index 0, breaking pattern queries like
// `call().arg(0, function_arg(0))`.
//
// The fix adds a sub-register fallback: when no covering tracked
// variable exists, return the LARGEST tracked variable CONTAINED IN
// `vn`'s byte range.  The function only reads that sub-register, so
// the bytes outside its range are unused — using the sub-register as
// the arg-passing-var loses no information.

/// Build a `VarTable` tracking exactly `vns`.  `upgrade_to_tracked_for`
/// only reads the tracked varnodes, so the concrete `VarId`s don't matter
/// here — insertion order is irrelevant to the cover / sub-register choice.
fn var_table_of(vns: impl IntoIterator<Item = rsleigh::Vn>) -> crate::graph::VarTable {
    let mut table = crate::graph::VarTable::default();
    for vn in vns {
        table.intern(vn);
    }
    table
}

/// A `Vn` already tracked must return itself unchanged.
#[test]
fn upgrade_to_tracked_returns_exact_match_when_vn_is_tracked() {
    let rdi = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let map = var_table_of([rdi]);
    assert_eq!(upgrade_to_tracked_for(&map, rdi), Some(rdi));
}

/// When `vn` is not tracked but a wider tracked variable covers it, the
/// covering variable must be returned (existing behaviour).
#[test]
fn upgrade_to_tracked_returns_smallest_covering_tracked_when_vn_is_not_tracked() {
    // RDI 8-byte tracked; we ask for EDI (4-byte sub-register at the same
    // offset) — must return RDI.
    let rdi = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let edi = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let map = var_table_of([rdi]);
    assert_eq!(upgrade_to_tracked_for(&map, edi), Some(rdi));
}

/// when no covering tracked variable exists but a
/// sub-register is tracked, return the largest contained-in tracked
/// variable.  This is the case for `int forward_1(int a)` on x86_64
/// SysV where the function only reads `EDI` and the convention asks
/// to upgrade `RDI`.
#[test]
fn upgrade_to_tracked_returns_largest_contained_sub_when_no_cover_exists() {
    let rdi = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let edi = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    // Only EDI is tracked; ask for RDI.
    let map = var_table_of([edi]);
    assert_eq!(
        upgrade_to_tracked_for(&map, rdi),
        Some(edi),
        "RDI not tracked but EDI is contained-in RDI's range — fallback \
         must return EDI so the Call node still has an arg slot"
    );
}

/// Sanity check: an unrelated tracked variable (different offset, no
/// overlap) yields no match.
#[test]
fn upgrade_to_tracked_returns_none_when_no_overlap() {
    let rdi = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let unrelated = rsleigh::Vn {
        addr_off: 0x200,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let map = var_table_of([unrelated]);
    assert_eq!(upgrade_to_tracked_for(&map, rdi), None);
}

/// When multiple covering tracked variables exist, the SMALLEST one
/// wins (tightest container).
#[test]
fn upgrade_to_tracked_chooses_smallest_cover_when_multiple_covers_exist() {
    // Asking for a 1-byte vn at off 56.  Both 4-byte and 8-byte covers
    // are tracked — the 4-byte one is tighter.
    let target = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 1,
    };
    let cover_4 = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let cover_8 = rsleigh::Vn {
        addr_off: 56,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let map = var_table_of([cover_4, cover_8]);
    assert_eq!(upgrade_to_tracked_for(&map, target), Some(cover_4));
}

/// when multiple sub-register tracked variables exist
/// (e.g. RCX covers both CL at off 0 size 1 and ECX at off 0 size 4),
/// the LARGEST sub-register wins because it preserves the most
/// information about the value the function actually computed.
#[test]
fn upgrade_to_tracked_chooses_largest_sub_when_multiple_subs_exist() {
    // RCX 8-byte: not tracked.
    let rcx = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let ecx = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let cl = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 1,
    };
    let map = var_table_of([ecx, cl]);
    assert_eq!(
        upgrade_to_tracked_for(&map, rcx),
        Some(ecx),
        "ECX (4 bytes) wins over CL (1 byte) — bigger sub-register \
         preserves more of what the function actually computed"
    );
}

// ── graph_mut / entry / non-consuming use of the builder ─────────────────
//
// These tests pin the contract: the builder exposes `graph_mut()` and
// `entry()` so callers can mutate the underlying `Graph` in place (e.g.
// run optimizer passes) without consuming the builder via `build()`.

/// `graph_mut()` must return a mutable reference to the same `Graph` that
/// `graph()` exposes immutably — so a write through `graph_mut()` is visible
/// through the immutable view.
#[test]
fn graph_mut_returns_mutable_reference_to_inner_graph() -> Result<()> {
    let mut b = empty_builder()?;
    // Capture the node count via the immutable view first.
    let count_before = b.function().graph().nodes.len();
    // Mutate via graph_mut() — create an IntConst node directly.
    let node_id = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(42u128),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    // Read back via the immutable view; the new node must be visible.
    let count_after = b.function().graph().nodes.len();
    assert_eq!(count_after, count_before + 1, "graph_mut() write must be visible via graph()");
    assert!(matches!(
        b.function().node_kind(node_id),
        NodeKind::IntConst(42)
    ));
    Ok(())
}

/// `entry()` must return the same `NodeId` that `build()` would record on the
/// produced `Graph`.  This is the contract opt passes rely on
/// when they take `(graph, entry)` from a builder that hasn't been consumed.
#[test]
fn entry_returns_recorded_entry_node_id() -> Result<()> {
    let b = empty_builder()?;
    let entry_via_accessor = b.entry();
    // The builder delegates entry() to the underlying Function's entry,
    // which is set atomically in build_entry().
    let entry_via_function = b
        .function()
        .entry()
        .expect("entry is always set after new_raw()");
    assert_eq!(
        entry_via_accessor, entry_via_function,
        "FunctionBuilder::entry() must match Function::entry()"
    );
    Ok(())
}

/// Calling `build()` after mutating via `graph_mut()` must still succeed
/// and the resulting `Graph` must be consistent with the
/// in-place mutations.
#[test]
fn build_after_inplace_optimization_still_succeeds() -> Result<()> {
    let mut b = empty_builder()?;
    // Set up a one-region function so build() has something to validate.
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let val = b.build_int_const(7u64, ValueType::I64)?;
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    // Mutate via graph_mut() in the same way an opt pass would.
    let extra = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(99u128),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    // The extra IntConst is detached / not reachable from the entry —
    // validation skips unreachable nodes via the reachability gate.  No
    // fingerprint stamp needed on `extra`.
    // After the mutation, build() must still succeed.
    let built = b.build()?;
    // The extra node is in the arena (graph keeps every node it ever
    // creates; reachability is independent of presence in the map).
    assert!(
        built.graph().all_node_ids().any(|n| n == extra),
        "build() after graph_mut() mutation must preserve the new node"
    );
    Ok(())
}

/// Two consecutive in-place mutations via `graph_mut()` must both be visible
/// in the final state — i.e. the second mutation sees the first's effect.
#[test]
fn consecutive_inplace_optimizations_compose() -> Result<()> {
    let mut b = empty_builder()?;
    // First mutation: create constant A.
    let a = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(1u128),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    // Second mutation: create constant B.  The second call sees the first
    // mutation (the underlying graph counter advanced) — node ids must differ.
    let b_id = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(2u128),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    assert_ne!(a, b_id, "consecutive create_node calls must produce distinct ids");
    // Both nodes are in the arena.
    assert!(matches!(
        b.function().node_kind(a),
        NodeKind::IntConst(1)
    ));
    assert!(matches!(
        b.function().node_kind(b_id),
        NodeKind::IntConst(2)
    ));
    Ok(())
}

#[test]
fn set_lift_addr_pair_scopes_attribution_and_restores_on_exit() -> Result<()> {
    // The bare set_lift_addr(Some(addr)) … set_lift_addr(None) pattern
    // is what every production site uses.  This pins that the
    // attribution scope behaves as expected: the inner value applies
    // while set, and after clearing the lift_addr returns to whatever
    // it was before.
    let mut b = builder_with_region()?;
    assert_eq!(b.lift_addr(), None);
    b.set_lift_addr(Some(0x100));
    assert_eq!(b.lift_addr(), Some(0x100));
    b.set_lift_addr(None);
    assert_eq!(b.lift_addr(), None, "manual restore returns to prior addr");

    // Nested: outer 0x200, transiently override to 0xA then 0xB then
    // back up to 0xA, finally back to 0x200.
    b.set_lift_addr(Some(0x200));
    b.set_lift_addr(Some(0xA));
    b.set_lift_addr(Some(0xB));
    assert_eq!(b.lift_addr(), Some(0xB));
    b.set_lift_addr(Some(0xA));
    assert_eq!(b.lift_addr(), Some(0xA));
    b.set_lift_addr(Some(0x200));
    assert_eq!(b.lift_addr(), Some(0x200));
    Ok(())
}

#[test]
fn set_lift_addr_attributes_node_to_current_addr() -> Result<()> {
    // A node created while lift_addr is set picks up that addr in its
    // asm-fingerprint side-table entry.
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

    assert_eq!(b.function().asm_fingerprint(pre_node), &[0x10]);
    assert_eq!(b.function().asm_fingerprint(in_node), &[0xC0DE]);
    assert_eq!(b.function().asm_fingerprint(post_node), &[0x10]);
    Ok(())
}

#[test]
fn build_int_const_wide_u256_round_trips_through_graph() -> Result<()> {
    let mut b = builder_with_region()?;
    let v = crate::wide_const::WideConstStorage::I256([0x1234, 0xabcd, 0, 0]);
    let value = b.build_int_const_wide(v.clone(), ValueType::I256)?;
    let node = b.function().producer(value);
    let NodeKind::IntConstWide(id) = b.function().node_kind(node) else {
        panic!("expected IntConstWide, got {:?}", b.function().node_kind(node));
    };
    assert_eq!(b.function().graph().wide_const(*id), &v);
    Ok(())
}

#[test]
fn build_int_const_wide_u512_round_trips_through_graph() -> Result<()> {
    let mut b = builder_with_region()?;
    let v = crate::wide_const::WideConstStorage::I512([1, 2, 3, 4, 5, 6, 7, 8]);
    let value = b.build_int_const_wide(v.clone(), ValueType::I512)?;
    let node = b.function().producer(value);
    let NodeKind::IntConstWide(id) = b.function().node_kind(node) else {
        panic!();
    };
    assert_eq!(b.function().graph().wide_const(*id), &v);
    Ok(())
}

#[test]
fn build_int_const_wide_dedups_repeated_values() -> Result<()> {
    let mut b = builder_with_region()?;
    let v = crate::wide_const::WideConstStorage::I256([42, 0, 0, 0]);
    let o1 = b.build_int_const_wide(v.clone(), ValueType::I256)?;
    let o2 = b.build_int_const_wide(v, ValueType::I256)?;
    let n1 = b.function().producer(o1);
    let n2 = b.function().producer(o2);
    assert_eq!(n1, n2, "structural dedup must reuse the same NodeId");
    Ok(())
}

/// Regression: `build_int_const` and `make_int_const`
/// must reject `I512` (and `I256`) because both store the value in `u128`.
/// Without the guard, the resulting `IntConst` would claim a width its
/// storage cannot represent — silent type confusion.
#[test]
fn build_int_const_rejects_u256_and_u512() -> Result<()> {
    let mut b = builder_with_region()?;
    let err256 = b
        .build_int_const(0u64, ValueType::I256)
        .expect_err("I256 must be rejected — use build_int_const_wide");
    assert!(err256.to_string().contains("I256"), "got: {err256}");
    let err512 = b
        .build_int_const(0u64, ValueType::I512)
        .expect_err("I512 must be rejected — use build_int_const_wide");
    assert!(err512.to_string().contains("I512"), "got: {err512}");
    Ok(())
}

#[test]
fn make_int_const_rejects_u256_and_u512() {
    use crate::graph::Graph;
    let mut g = Graph::new();
    let err256 = g
        .make_int_const(0u64, ValueType::I256)
        .expect_err("I256 rejected");
    assert!(err256.to_string().contains("I256"), "got: {err256}");
    let err512 = g
        .make_int_const(0u64, ValueType::I512)
        .expect_err("I512 rejected");
    assert!(err512.to_string().contains("I512"), "got: {err512}");
}

#[test]
fn build_int_const_wide_rejects_non_wide_output_type() -> Result<()> {
    let mut b = builder_with_region()?;
    let v = crate::wide_const::WideConstStorage::I256([0; 4]);
    let err = b
        .build_int_const_wide(v, ValueType::I128)
        .expect_err("I128 must be rejected — use build_int_const");
    assert!(err.to_string().contains("non-wide output type"), "got: {err}");
    Ok(())
}

#[test]
fn build_int_const_wide_rejects_storage_byte_size_mismatch() -> Result<()> {
    let mut b = builder_with_region()?;
    let v_256 = crate::wide_const::WideConstStorage::I256([0; 4]);
    let err = b
        .build_int_const_wide(v_256, ValueType::I512)
        .expect_err("I256 storage with I512 output must be rejected");
    assert!(err.to_string().contains("byte_size"), "got: {err}");
    Ok(())
}

#[test]
fn int_const_wide_validates_clean_when_built_via_intern() -> Result<()> {
    use crate::validate::validate;
    let mut b = builder_with_region()?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let v = crate::wide_const::WideConstStorage::I256([0x1234_5678, 0, 0, 0]);
    let value = b.build_int_const_wide(v, ValueType::I256)?;
    b.set_lift_addr(None);
    // Wire the wide const into the reachable spine via Return[ctrl, mem, value].
    let entry_ctrl = b.function().node_outputs(b.entry()).iter().copied().next().unwrap();
    // Build a minimal Return — needs Memory input; pull it from InitialMemory.
    let mem_node = b
        .function().graph()
        .all_node_ids()
        .find(|n| matches!(b.function().node_kind(*n), NodeKind::InitialMemory))
        .unwrap();
    let mem_value = b.function().node_outputs(mem_node).iter().copied().next().unwrap();
    let ret = b
        .function_mut().graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, mem_value, value], []);
    b.function_mut().set_asm_fingerprint(ret, vec![SENTINEL_LIFT_ADDR]);
    let entry_id = b.entry();
    let function = b.function();
    validate(function, entry_id).expect("IntConstWide built via intern_wide_const must validate clean");
    Ok(())
}

#[test]
fn compact_gcs_unreferenced_wide_consts() -> Result<()> {
    use crate::wide_const::WideConstStorage;
    let mut b = builder_with_region()?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let _live = b.build_int_const_wide(WideConstStorage::I256([1; 4]), ValueType::I256)?;
    // Build an additional wide const that we'll never wire into the
    // reachable graph — `compact()` should drop it.
    let _zombie =
        b.build_int_const_wide(WideConstStorage::I256([2; 4]), ValueType::I256)?;
    b.set_lift_addr(None);
    // Zombie isn't referenced by `_live` and the only Return walk-spine
    // visits `_live` (we wire it through Return to keep it reachable).
    let mem_node = b
        .function().graph()
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
    let entry_ctrl = b
        .function()
        .node_outputs(b.entry())
        .iter()
        .copied()
        .next()
        .unwrap();
    let ret = b
        .function_mut().graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, mem_value, _live], []);
    b.function_mut().set_asm_fingerprint(ret, vec![SENTINEL_LIFT_ADDR]);

    let pre = b.function().graph().wide_const_interner.len();
    assert_eq!(pre, 2, "before compact, both wide consts are in the side-table");

    let mut bfg = b.build()?;
    bfg.compact()?;

    let post = bfg.graph().wide_const_interner.len();
    assert_eq!(
        post, 1,
        "compact must drop the unreferenced zombie wide const; got {post} entries"
    );
    Ok(())
}

// ── build_call_with_cc — per-Call CC override ───────────────────────────

mod build_call_with_cc {
    use super::*;
    use strider_target::{BuiltCallingConvention, CallingConvention, SleighArch};

    fn x86_64_regs() -> rsleigh::SleighRegs {
        SleighArch::x86_64().probe_regs().unwrap()
    }

    fn x86_64_built_cc() -> BuiltCallingConvention {
        CallingConvention::x86_64_systemv()
            .unwrap()
            .build(&x86_64_regs())
            .unwrap()
    }

    #[test]
    fn build_call_with_cc_none_matches_build_call() {
        let cc = x86_64_built_cc();
        let regs = x86_64_regs();
        let rax = regs.name_to_vn("RAX").unwrap();
        let rdi = regs.name_to_vn("RDI").unwrap();
        let rsp = regs.name_to_vn("RSP").unwrap();
        let mut b = FunctionBuilder::new(vec![rax, rdi, rsp], &cc).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let addr = b
            .build_int_const(0xdead_beef_u64, ValueType::I64)
            .unwrap();
        b.build_call_with_cc(addr, None).unwrap();
        // The Call output kinds match `build_call(addr)` exactly: Control,
        // Memory, then one slot per `call_clobbered_variables` entry.
        let function = b.function();
        let call_node = function.graph()
            .all_node_ids()
            .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
            .unwrap();
        assert!(
            function.node_outputs(call_node).len() >= 2,
            "Control + Memory at minimum"
        );
        assert!(
            function.call_cc(call_node).is_none(),
            "no override means no recorded call_cc"
        );
    }

    #[test]
    fn build_call_with_cc_all_preserving_clobbers_nothing() {
        let cc = x86_64_built_cc();
        let regs = x86_64_regs();
        let rax = regs.name_to_vn("RAX").unwrap();
        let rdi = regs.name_to_vn("RDI").unwrap();
        let rsp = regs.name_to_vn("RSP").unwrap();
        // FunctionBuilder::new auto-adds the cc.ret_val_regs (rax, rdx) and
        // ret_val_regs_float (xmm0, xmm1) into the tracked set even if the
        // caller's `all_used_variables` doesn't list them.  An "all-preserving"
        // override needs to mark those callee-saved too or they'll appear as
        // clobber outputs.
        let rdx = regs.name_to_vn("RDX").unwrap();
        let xmm0 = regs.name_to_vn("XMM0").unwrap();
        let xmm1 = regs.name_to_vn("XMM1").unwrap();
        let mut b = FunctionBuilder::new(vec![rax, rdi, rsp], &cc).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);

        // Override CC: every tracked variable is callee-saved → 0 clobbers.
        let override_cc = BuiltCallingConvention {
            arg_passing_regs: vec![],
            callee_saved_regs: vec![rax, rdi, rdx, xmm0, xmm1],
            ret_val_regs: vec![],
            ret_val_regs_float: vec![],
            stack_vn: rsp,
            stack_arg_offsets: vec![],
            ret_stack_pop: 0,
            link_register_vn: None,
            preserves_memory: false,
        };

        let addr = b
            .build_int_const(0xdead_beef_u64, ValueType::I64)
            .unwrap();
        b.build_call_with_cc(addr, Some(&override_cc)).unwrap();
        let function = b.function();
        let call_node = function.graph()
            .all_node_ids()
            .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
            .unwrap();
        let outs = function.node_outputs(call_node);
        // Outputs: Control + Memory + 0 clobbered slots.
        assert_eq!(
            outs.len(),
            2,
            "fentry-style Call has 0 clobbered output slots"
        );
        let inputs: Vec<_> = function.node_inputs(call_node).into_iter().collect();
        // Inputs: control + memory + target.  No arg slots.
        assert_eq!(inputs.len(), 3, "fentry-style Call takes no args");
        assert!(
            function.call_cc(call_node).is_some(),
            "override CC is recorded even when it clobbers nothing"
        );
        // No clobber outputs → no value_vn clobber tags on this Call.
        assert!(
            function
                .node_outputs(call_node)
                .iter()
                .all(|&v| function.clobbered_vn(v).is_none()),
            "fentry-style Call has no clobber outputs, so none are tagged"
        );
    }

    // ── FunctionBuilder extended-use round-trip ────────────────────────

    /// Drive the builder through several rounds of in-place mutation
    /// (mimicking an iterative analysis loop) without consuming it
    /// via `build()`.  At each step `entry()` must stay stable and
    /// `graph_mut()` must keep producing fresh node ids.
    #[test]
    fn analysis_loop_without_build_round_trips() {
        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(0u64, ValueType::I64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);

        let entry = b.entry();

        // First mutation: synthesize a fresh IntConst via graph_mut().
        let r1 = b.function_mut().graph_mut().create_node(
            NodeKind::IntConst(1u128),
            std::iter::empty(),
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(b.entry(), entry, "entry() stable after first mutation");

        // Second mutation: another synthesis; the first node must persist.
        let r2 = b.function_mut().graph_mut().create_node(
            NodeKind::IntConst(2u128),
            std::iter::empty(),
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(b.entry(), entry, "entry() stable after second mutation");
        assert_ne!(r1, r2, "consecutive create_node calls produce distinct ids");

        // Both synthesized nodes are live in the arena.
        assert!(matches!(b.function().node_kind(r1), NodeKind::IntConst(1)));
        assert!(matches!(b.function().node_kind(r2), NodeKind::IntConst(2)));
    }

    /// After driving the builder through several rounds of in-place
    /// mutation, calling `build()` must still produce a valid graph
    /// (passes `validate`).  Pins the "build still works after
    /// extended use" contract that every imperative opt pass relies
    /// on.
    #[test]
    fn final_build_after_extended_use_yields_valid_built() {
        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(7u64, ValueType::I64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);

        // N rounds of in-place mutation via graph_mut() — synthesize
        // a fresh node, leave it detached.  The validator skips
        // unreachable nodes, so detached extras are still valid.
        for k in 1u128..=5 {
            b.function_mut().graph_mut().create_node(
                NodeKind::IntConst(k),
                std::iter::empty(),
                [ValueKind::Typed(ValueType::I64)],
            );
        }

        let function = b.build().unwrap();
        let entry = function.entry().unwrap();
        crate::validate::validate(&function, entry)
            .expect("build() after extended use must yield a valid graph");
    }
}
