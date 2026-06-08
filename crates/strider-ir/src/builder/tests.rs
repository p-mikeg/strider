use super::*;
use crate::IRViewer;
use anyhow::anyhow;

use crate::error::Result;
use crate::node::{IntPayload, NodeKind, ValueKind, ValueType};
use crate::node::{ExtendOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

/// Local mock-construction helper mirroring the convention-from-parts
/// shape these tests want.  strider-ir's own unit tests cannot use
/// `strider_ir_test_utils::builder` — under `cargo test` the dev-dep
/// links a *separate* compilation of strider-ir, so a helper returning
/// `strider_ir::FunctionBuilder` would mismatch the unit-test crate's own
/// `FunctionBuilder`.  So we synthesise the convention and call the local
/// [`FunctionBuilder::new`] directly.  Unlike the test-utils helper, it does
/// NOT stamp the sentinel lift address (it mirrors the old `new_raw`); tests
/// that build fingerprint-bearing nodes set the lift address themselves.
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
    };
    // Note: unlike the test-utils `RegisterSet`, this local helper does NOT
    // stamp the sentinel lift address — it mirrors the old `new_raw`, which
    // left `lift_addr` as `None`.  Tests that build fingerprint-bearing
    // nodes set the lift address themselves.
    FunctionBuilder::new(tracked, &cc, endianness)
}

/// Build a minimal builder with no variables so tests that do not need
/// SSA variables remain simple.
fn empty_builder() -> Result<FunctionBuilder> {
    raw_builder(vec![], &[], &[], &[], None, 0, strider_target::Endianness::Little)
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
    assert_eq!(kind, NodeKind::IntConst(IntPayload::Small(bits)));
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
        crate::node::IntBinaryOp::Add,
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
    let opaque = b.build_int_unary_operation(raw, crate::node::IntUnaryOp::Neg, ValueType::I64)?;
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
    let i1 = b.build_int_unary_operation(c1, crate::node::IntUnaryOp::Neg, ValueType::I32)?;
    let i2 = b.build_int_unary_operation(c2, crate::node::IntUnaryOp::Neg, ValueType::I32)?;
    // Both inputs are I32 — the caller reinterprets each as F32 via
    // IntBitsToFloat (`cast_to_float_if_needed`) before the strict build.
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

// ── CallOther / SegmentOp / CPoolRef / New ──────────────────────────────

/// Helper: an empty CallOther footprint (no implicit reads/writes, no
/// memory clobber) for the trap-class / no-footprint builder tests.
fn empty_call_other_abi() -> strider_target::BuiltCallOtherAbi {
    strider_target::BuiltCallOtherAbi {
        implicit_reads: Vec::new(),
        implicit_writes: Vec::new(),
        clobbers_memory: false,
    }
}

/// Helper: build a single-region builder with an active region set.
fn builder_with_region() -> Result<FunctionBuilder> {
    let mut b = empty_builder()?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    Ok(b)
}

/// Helper: build a single-region builder that tracks `vns` (so a CallOther
/// `output` / implicit-write register has a tracked container for the
/// builder's `write_reg_vn` result writeback).
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

    let (node, result) =
        b.build_call_other(7, "NEON_rev64", None, &[], &empty_call_other_abi(), None, false)?;
    assert!(result.is_none(), "no output vn -> no ret-val output");

    // Ctrl advances; memory does NOT (empty footprint, clobbers_memory=false).
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
    let out_vn = reg_vn(0x10, 4); // 4-byte reg → I32 output
    // Track `out_vn` so the builder's `write_reg_vn` result writeback has a
    // container.
    let mut b = builder_with_region_tracking(vec![out_vn])?;
    let arg = b.build_int_const(0x42u64, ValueType::I64)?;
    let (node, result) = b.build_call_other(
        3,
        "cpuid",
        None,
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
    // C2 (strider): pin Graph::memory_output_of as the named accessor
    // for what handle_call_other previously read as `node_outputs[1]`.
    let out_vn = reg_vn(0x20, 4); // 4-byte reg → I32 output
    let mut b = builder_with_region_tracking(vec![out_vn])?;
    let (node, _) = b.build_call_other(
        4,
        "cpuid",
        None,
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
    let res = b.build_call_other(0, "cpuid", None, &[mem], &empty_call_other_abi(), None, false);
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

/// Regression: high-offset register varnodes (e.g. ppc64 / aarch64be
/// condition-register slices) can have `addr_off + size` overflow `u64`.
/// The overlap-dedup helper must use saturating arithmetic to match the
/// convention `build_largest_container_map` already documents — a plain `+`
/// panics in debug and wrap-misclassifies containment in release.
#[test]
fn dedup_overlapping_largest_is_overflow_safe_on_high_offset_varnodes() {
    let wide = reg_vn(u64::MAX - 1, 8);
    let narrow = reg_vn(u64::MAX - 1, 4);
    // Must not panic; the wider varnode subsumes the narrower one.
    let kept = dedup_overlapping_largest(&[wide, narrow]);
    assert_eq!(kept, vec![wide], "wider high-offset varnode wins, no overflow");
}

/// A wide-typed (`I128`) constant whose value fits in `u64` is stored
/// inline as `IntPayload::Small` — the width lives in the output
/// `ValueKind`, so the wide interner is used only when the value exceeds
/// `u64`.  Both forms read back at the declared width, and the two
/// construction paths canonicalize to one node (dedup).
#[test]
fn small_valued_wide_const_uses_small_payload() -> Result<()> {
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

    // Small I128 value → inline Small payload, read back as I128.
    let small = b.build_int_const(5u64, ValueType::I128)?;
    let small_node = b.function().producer(small);
    assert!(
        matches!(
            b.function().node_kind(small_node),
            NodeKind::IntConst(IntPayload::Small(5))
        ),
        "small-valued I128 const must use the inline Small payload",
    );
    assert_eq!(b.function().int_const_u128(small), Some(5u128));
    assert_eq!(b.function().value_type(small)?, ValueType::I128);

    // Value exceeding u64 → interned Wide payload.
    let big_val: u128 = 1u128 << 100;
    let big = b.build_int_const(big_val, ValueType::I128)?;
    let big_node = b.function().producer(big);
    assert!(
        matches!(
            b.function().node_kind(big_node),
            NodeKind::IntConst(IntPayload::Wide(_))
        ),
        "I128 value > u64::MAX must intern as Wide",
    );
    assert_eq!(b.function().int_const_u128(big), Some(big_val));

    // Canonical: build_int_const and build_int_const_wide agree for the same
    // small value → the same node.
    let small2 =
        b.build_int_const_wide(crate::wide_const::WideConstStorage::I128(5), ValueType::I128)?;
    assert_eq!(b.function().producer(small2), small_node);
    Ok(())
}

/// `FunctionBuilder::new` is the SSoT for vn ordering: the tracked
/// `all_vns` set must come out sorted by (space, offset, size)
/// regardless of the order the vns were handed in, so `VarId`
/// assignment (and every derived clobber-slot index) is deterministic.
#[test]
fn function_builder_sorts_all_vns_deterministically() -> Result<()> {
    // Three disjoint registers handed in OUT of sorted order.
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
    assert_eq!(got, expected, "all_vns must be sorted by (space, off, size)");
    Ok(())
}

/// `Function::container_of` resolves a sub-register query to its tracked
/// largest container, so a calling convention that names `eax` (4 bytes)
/// while the function tracks `rax` (8 bytes) maps correctly.  A vn that
/// is its own container maps to itself; a vn with no tracked container
/// maps to itself.
#[test]
fn container_of_resolves_subregister_to_tracked_container() -> Result<()> {
    let rax = reg_vn(0x0, 8);
    let eax = reg_vn(0x0, 4);
    let sp = reg_vn(0x7000, 8);
    // Hand in BOTH rax and eax; dedup keeps rax (container), map records eax -> rax.
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
    assert_eq!(f.container_of(&eax), rax, "eax must resolve to its rax container");
    assert_eq!(f.container_of(&rax), rax, "rax is its own container");
    let r9 = reg_vn(0x90, 8);
    assert_eq!(f.container_of(&r9), r9, "untracked, uncontained -> self");
    Ok(())
}

/// A calling convention whose ret-val register is a SUB-register (`eax`)
/// of a tracked container (`rax`) must still classify the container as the
/// return value — not silently drop it (call_ret_vals_for) nor mis-file it
/// as a clobber (call_clobbered_for). Pins the container_of routing.
#[test]
fn cc_subregister_ret_reg_resolves_to_tracked_container() -> Result<()> {
    use strider_target::BuiltCallingConvention;
    let rax = reg_vn(0x0, 8);
    let eax = reg_vn(0x0, 4);
    let sp = reg_vn(0x7000, 8);
    let cc = BuiltCallingConvention::try_new(
        vec![],    // arg_passing_regs
        vec![],    // callee_saved_regs
        vec![eax], // ret_val_regs (sub-register!)
        vec![],    // ret_val_regs_float
        sp,        // stack_vn
        None,      // stack_args
        0,         // ret_stack_pop
        None,      // link_register_vn
        false,     // preserves_memory
    )?;
    // Build a function that tracks rax (+ sp). all_vns() then contains the
    // rax container, and container_of(eax) resolves to rax.
    let b = raw_builder(
        vec![rax],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let f = b.function();
    let ret_vals = f.call_ret_vals_for(&cc);
    assert_eq!(ret_vals, vec![rax], "eax ret reg resolves to its rax container");
    let clobbers = f.call_clobbered_for(&cc);
    assert!(
        !clobbers.contains(&rax),
        "the rax return register must not also appear as a clobber",
    );
    Ok(())
}

#[test]
fn set_stack_args_round_trips_on_default_cc() -> Result<()> {
    use strider_target::StackArgs;
    let sp = reg_vn(0x7000, 8);
    let mut b = raw_builder(vec![], &[], &[], &[], Some(sp), 0, strider_target::Endianness::Little)?;
    b.set_stack_args(Some(StackArgs { base_offset: 8, increment: 8 }));
    assert_eq!(
        b.function().default_cc().stack_args,
        Some(StackArgs { base_offset: 8, increment: 8 }),
    );
    Ok(())
}

/// Reading a sub-register when only the wider container is tracked routes
/// through `Function::container_of` (the persisted map), shifting/masking
/// out of the container. Pins that the read path no longer depends on the
/// deleted builder-lifetime `largest_container` cache.
#[test]
fn read_subregister_routes_through_container_map() -> Result<()> {
    let rax = reg_vn(0x0, 8);
    let eax = reg_vn(0x0, 4);
    let sp = reg_vn(0x7000, 8);
    let mut b = raw_builder(
        vec![rax, eax],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let v = b.read_reg_vn(&eax)?;
    assert_eq!(
        b.function().value_type(v).unwrap(),
        ValueType::I32,
        "eax read yields I32 sliced from the rax container",
    );
    Ok(())
}

/// The footprint-resolving `build_call_other` reads each
/// `abi.implicit_reads` register itself (via `read_reg_vn`), appends those
/// reads after the explicit args, emits the result + per-implicit-write
/// clobber outputs, writes each clobber back to its register, and records
/// the `CallDescriptor::CallOther(abi)` on the node — reproducing the IR
/// the lifter used to assemble by hand.
#[test]
fn build_call_other_from_abi_resolves_footprint() -> Result<()> {
    use strider_target::Endianness;

    // Track RCX (implicit read) + RAX, RDX (implicit writes), all full
    // 8-byte containers so read_reg_vn/write_reg_vn map straight to the
    // tracked variable.
    let rcx = reg_vn(0x10, 8);
    let rax = reg_vn(0x00, 8);
    let rdx = reg_vn(0x20, 8);
    let out_vn = reg_vn(0x40, 4); // distinct 4-byte reg → I32 result
    // Track `out_vn` too so the builder's `write_reg_vn` result writeback
    // resolves to its container.
    let mut b = raw_builder(
        vec![rcx, rax, rdx, out_vn],
        &[],
        &[],
        &[],
        None,
        0,
        Endianness::Little,
    )?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let explicit = b.build_int_const(0x42u64, ValueType::I64)?;

    let abi = strider_target::BuiltCallOtherAbi {
        implicit_reads: vec![rcx],
        implicit_writes: vec![rax, rdx],
        clobbers_memory: true,
    };

    let mem_before = b.cur_region_memory()?;
    let (node, result) =
        b.build_call_other(5, "syscall", None, &[explicit], &abi, Some(out_vn), false)?;

    // Inputs: [ctrl, mem] ++ explicit_args ++ [read(RCX)].  No target.
    let inputs: Vec<ValueId> = b.function().node_inputs(node).into_iter().collect();
    assert_eq!(inputs.len(), 4, "ctrl + mem + explicit + 1 implicit read");
    assert!(matches!(b.function().value_kind(inputs[0]), ValueKind::Control));
    assert!(matches!(b.function().value_kind(inputs[1]), ValueKind::Memory));
    assert_eq!(inputs[2], explicit, "explicit arg precedes the implicit read");
    // inputs[3] is the read of RCX: the builder reads it via read_reg_vn,
    // so it equals the current SSA value of the RCX variable (an I64-typed
    // value edge — RCX is an 8-byte container).
    assert_eq!(
        inputs[3],
        b.read_variable(&rcx)?,
        "last input must be the implicit read of register RCX",
    );
    assert_eq!(
        b.function().value_kind(inputs[3]),
        ValueKind::Typed(ValueType::I64),
        "RCX read is I64-typed (8-byte container)",
    );

    // Outputs: [ctrl, mem, result(tagged out_vn), RAX, RDX].
    let outs: Vec<ValueId> = b.function().node_outputs(node).to_vec();
    assert_eq!(outs.len(), 5, "ctrl + mem + result + 2 clobbers");
    assert!(matches!(b.function().value_kind(outs[0]), ValueKind::Control));
    assert!(matches!(b.function().value_kind(outs[1]), ValueKind::Memory));
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
    assert_eq!(b.function().get_vn_for_value(outs[3]), Some(rax), "clobber slot tags RAX");
    assert_eq!(b.function().get_vn_for_value(outs[4]), Some(rdx), "clobber slot tags RDX");

    // Implicit-write registers were written back: a later read of RAX
    // returns the clobber output (outs[3]).
    let rax_after = b.read_variable(&rax)?;
    assert_eq!(rax_after, outs[3], "RAX rebound to its clobber output");

    // Memory advanced (clobbers_memory = true).
    let mem_after = b.cur_region_memory()?;
    assert_ne!(mem_before, mem_after, "clobbers_memory → memory advances");

    // The vn-resolved ABI footprint is recorded on the node.
    match b.function().call_descriptor(node) {
        Some(crate::CallDescriptor::CallOther(recorded)) => {
            assert_eq!(*recorded, abi, "recorded footprint must equal the input abi");
        }
        other => panic!("expected CallDescriptor::CallOther, got {other:?}"),
    }
    assert_eq!(b.function().call_other_name(node), Some("syscall"));
    Ok(())
}

/// An implicit-write register that has no tracked container cannot be
/// written back: `build_call_other` surfaces the `write_reg_vn` error
/// rather than silently dropping the clobber.
#[test]
fn build_call_other_rejects_untracked_implicit_write() -> Result<()> {
    let mut b = builder_with_region()?;
    // No tracked variables, so this register has no enclosing container.
    let untracked = reg_vn(0, 4);
    let abi = strider_target::BuiltCallOtherAbi {
        implicit_reads: Vec::new(),
        implicit_writes: vec![untracked],
        clobbers_memory: false,
    };
    let res = b.build_call_other(11, "bogus", None, &[], &abi, None, false);
    assert!(res.is_err(), "untracked implicit-write register must error");
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
    let (node, result) = b.build_call_other(0, "ud2", None, &[], &empty_call_other_abi(), None, true)?;
    assert!(result.is_none(), "no output vn -> no ret-val output");
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
    b.build_call_other(0, "ud2", None, &[], &empty_call_other_abi(), None, true)?;
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
    b.build_call_other(0, "cpuid", None, &[], &empty_call_other_abi(), None, false)?;
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
    let mut b = raw_builder(vec![sp], &[], &[], &[], Some(sp), 8, strider_target::Endianness::Little)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let pre_sp = b.read_variable(&sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::I64)?;
    b.build_call(target, None)?;

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
        NodeKind::IntConst(IntPayload::Small(8)),
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
    let mut b = raw_builder(vec![sp], &[], &[], &[], Some(sp), 0, strider_target::Endianness::Little)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let pre_sp = b.read_variable(&sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::I64)?;
    b.build_call(target, None)?;

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
// The fix in `FunctionBuilder::new` extends the same overlap-filter that
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
    let b = raw_builder(vec![outer, inner], &[], &[], &[], None, 0, strider_target::Endianness::Little)?;
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
    let b = raw_builder(vec![outer, inner], &[], &[], &[], None, 0, strider_target::Endianness::Little)?;
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
    let b = raw_builder(vec![a, b_vn], &[], &[], &[], None, 0, strider_target::Endianness::Little)?;
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
        &NodeKind::IntConst(IntPayload::Small(1)),
        "constant I1 → I8 must fold to IntConst(1) typed I8"
    );
    Ok(())
}

// ── ret-val regs are the raw declared list ──────────────────────────────
//
// `ret_val_vars()` now returns the CC's declared return registers
// verbatim (int then float), with no tracked-container projection.  The
// Return / Call read paths resolve each declared register to its tracked
// container via `read_reg_vn`, so the raw list is the right shape — a
// wider register is read at its full declared width rather than narrowed
// to a tracked sub-register.

/// `ret_val_vars()` returns the declared ret reg verbatim, even when a
/// wider view is the tracked one.
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
    // Both varnodes referenced: the overlap filter keeps only the 8-byte
    // view, but `ret_val_vars` reports the declared 4-byte ret reg as-is.
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
        b.ret_val_vars(),
        &[f0_4byte],
        "ret_val_vars returns the declared ret reg verbatim (no projection)"
    );
    Ok(())
}

// ── Derived register lists + function return ──────────────────────────
//
// These tests pin the register-list projections (`ret_val_regs`,
// `call_clobbered`, `call_other_clobbered`) and the
// `build_function_return` lowering.  The projections are no longer stored
// fields — they are derived on demand from `Function::all_vns` +
// `Function::default_cc` (the raw declared lists + the clobber filter), so
// these tests confirm the derivations reproduce the same shapes the
// formerly build-time-computed lists held.

/// The built function's `ret_val_regs()` / `call_clobbered_regs()`
/// accessors surface exactly the projected lists `new` computed —
/// the representative ABI shape with a sub-register ret upgrade and a
/// caller-clobbered split (ret-prefix then the rest).
#[test]
fn projected_cc_lists_match_built_function_fields() -> Result<()> {
    let r0 = reg_vn(0x10, 8); // ret + arg + clobbered
    let r1 = reg_vn(0x20, 8); // plain caller-clobbered
    let r2 = reg_vn(0x30, 8); // callee-saved (excluded from clobber)
    let sp = reg_vn(0x40, 8); // stack pointer (excluded from clobber)

    let mut b = raw_builder(
        vec![r0, r1, r2, sp],
        &[r0],       // arg_passing
        &[r2],       // callee_saved
        &[r0],       // ret_vars
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;

    // ret_val_regs: r0 is tracked, no upgrade needed.
    assert_eq!(
        b.function().ret_val_regs(),
        &[r0],
        "ret_val_regs projects the ABI ret list"
    );

    // call_ret_val_regs: r0 is a tracked, clobbered ret reg.
    assert_eq!(
        b.function().call_ret_val_regs(),
        vec![r0],
        "call_ret_val_regs returns only the ret-val registers (r0)"
    );
    // call_clobbered_regs: only the non-ret caller-clobbered regs (r1);
    // r0 has moved to the ret-val group; r2 (callee-saved) and sp excluded.
    assert_eq!(
        b.function().call_clobbered_regs(),
        vec![r1],
        "call_clobbered_regs returns only the non-ret caller-clobbered regs"
    );
    // The full combined set (ret-vals ++ clobbers) reproduces the old list.
    let combined: Vec<_> = b.function().call_ret_val_regs().into_iter()
        .chain(b.function().call_clobbered_regs())
        .collect();
    assert_eq!(
        combined,
        vec![r0, r1],
        "combined ret-val + clobbers equals the old full clobber list"
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

    let b = raw_builder(
        vec![r0, r1, r2, sp],
        &[r0],  // arg_passing
        &[r2],  // callee_saved (default)
        &[r0],  // ret_vars
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let f = b.function();

    // Default: ret-val group = [r0]; clobber group = [r1].
    // (r2 callee-saved, sp excluded from both.)
    assert_eq!(
        f.call_ret_vals_for(f.default_cc()),
        vec![r0],
        "call_ret_vals_for default-CC returns the ret-val register r0"
    );
    assert_eq!(
        f.call_clobbered_for(f.default_cc()),
        vec![r1],
        "call_clobbered_for default-CC returns only the non-ret clobbered reg r1"
    );
    assert_eq!(f.call_clobbered_for(f.default_cc()), f.call_clobbered_regs());
    // Combined (ret-vals ++ clobbers) reproduces the old single list [r0, r1].
    let full_default: Vec<_> = f.call_ret_vals_for(f.default_cc()).into_iter()
        .chain(f.call_clobbered_for(f.default_cc()))
        .collect();
    assert_eq!(full_default, vec![r0, r1]);

    // Override CC: mark BOTH r1 and r2 callee-saved, no ret regs.  The
    // override has no ret-val group (no ret_val_regs) so the entire
    // combined set is the clobber group.  Only r0 survives the
    // callee-saved filter — strictly smaller than the default.
    let override_cc = strider_target::BuiltCallingConvention {
        arg_passing_regs: vec![],
        callee_saved_regs: vec![r1, r2],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    };
    assert_eq!(
        f.call_ret_vals_for(&override_cc),
        vec![],
        "override CC with no ret regs has an empty ret-val group"
    );
    assert_eq!(
        f.call_clobbered_for(&override_cc),
        vec![r0],
        "override CC marking r1+r2 callee-saved leaves only r0 in clobbers"
    );
    let full_override: Vec<_> = f.call_ret_vals_for(&override_cc).into_iter()
        .chain(f.call_clobbered_for(&override_cc))
        .collect();
    assert!(
        full_override.len() < full_default.len(),
        "override combined set must be strictly smaller than the default"
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
    let mut b = raw_builder(
        vec![r0, r1],
        &[],
        &[],
        &[r0, r1], // two ABI ret regs
        None,
        0,
        strider_target::Endianness::Little,
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
    let ret = crate::walk::walk_graph(f.graph(), entry)
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
    let mut b = raw_builder(vec![flag], &[], &[], &[], None, 0, strider_target::Endianness::Little)?;
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

/// Reading a 1-byte sub-register (`AL`) out of a tracked 8-byte container
/// (`RAX`) under little-endian goes through the builder's register-aliasing
/// path: the container read is `Truncate`d to the sub-register width.  For a
/// sub-register at offset 0 the shift is 0, so the read is a direct
/// `Truncate` of the container read with no `ShiftRight` in between.
#[test]
fn read_reg_vn_truncates_subregister_of_tracked_container() -> Result<()> {
    use strider_target::Endianness;

    let rax = reg_vn(0x100, 8);
    let al = reg_vn(0x100, 1); // low byte, same offset → shift 0
    let mut b = raw_builder(
        vec![rax],
        &[],
        &[],
        &[],
        None,
        0,
        Endianness::Little,
    )?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // Read the low-byte sub-register through the aliasing path.  The
    // container's value is the opaque initial read of the tracked RAX (a
    // non-constant), so the truncate is materialised as a real `Truncate`
    // node rather than constant-folded away.
    let read = b.read_reg_vn(&al)?;

    // The result is an I8-typed Truncate (shift 0 → no ShiftRight).
    assert_eq!(
        b.function().value_kind(read),
        ValueKind::Typed(ValueType::I8),
        "AL read must be I8-typed"
    );
    let read_node = b.function().producer(read);
    assert!(
        matches!(b.function().node_kind(read_node), NodeKind::Truncate),
        "AL (offset 0, shift 0) read must be a direct Truncate of the container read, got {:?}",
        b.function().node_kind(read_node)
    );
    Ok(())
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
    let count_before = b.function().graph().all_node_ids().count();
    // Mutate via graph_mut() — create an IntConst node directly.
    let node_id = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(IntPayload::Small(42_u64)),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    // Read back via the immutable view; the new node must be visible.
    let count_after = b.function().graph().all_node_ids().count();
    assert_eq!(count_after, count_before + 1, "graph_mut() write must be visible via graph()");
    assert!(matches!(
        b.function().node_kind(node_id),
        NodeKind::IntConst(IntPayload::Small(42))
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
        .expect("entry is always set after new()");
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
        NodeKind::IntConst(IntPayload::Small(99_u64)),
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
        NodeKind::IntConst(IntPayload::Small(1_u64)),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    // Second mutation: create constant B.  The second call sees the first
    // mutation (the underlying graph counter advanced) — node ids must differ.
    let b_id = b.function_mut().graph_mut().create_node(
        NodeKind::IntConst(IntPayload::Small(2_u64)),
        std::iter::empty(),
        [ValueKind::Typed(ValueType::I64)],
    );
    assert_ne!(a, b_id, "consecutive create_node calls must produce distinct ids");
    // Both nodes are in the arena.
    assert!(matches!(
        b.function().node_kind(a),
        NodeKind::IntConst(IntPayload::Small(1))
    ));
    assert!(matches!(
        b.function().node_kind(b_id),
        NodeKind::IntConst(IntPayload::Small(2))
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
    let NodeKind::IntConst(IntPayload::Wide(id)) = b.function().node_kind(node) else {
        panic!("expected IntConst(Wide), got {:?}", b.function().node_kind(node));
    };
    assert_eq!(b.function().wide_const(*id), &v);
    Ok(())
}

#[test]
fn build_int_const_wide_u512_round_trips_through_graph() -> Result<()> {
    let mut b = builder_with_region()?;
    let v = crate::wide_const::WideConstStorage::I512([1, 2, 3, 4, 5, 6, 7, 8]);
    let value = b.build_int_const_wide(v.clone(), ValueType::I512)?;
    let node = b.function().producer(value);
    let NodeKind::IntConst(IntPayload::Wide(id)) = b.function().node_kind(node) else {
        panic!();
    };
    assert_eq!(b.function().wide_const(*id), &v);
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

/// Regression: `build_int_const` must reject `I512` (and `I256`) because
/// `IntConst` stores the value in `u128`.  Without the guard, the resulting
/// `IntConst` would claim a width its storage cannot represent — silent type
/// confusion.
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
fn build_int_const_wide_rejects_non_wide_output_type() -> Result<()> {
    // I64 is not a valid wide-const output type; I80/I128/I256/I512 are.
    let mut b = builder_with_region()?;
    let v = crate::wide_const::WideConstStorage::I256([0; 4]);
    let err = b
        .build_int_const_wide(v, ValueType::I64)
        .expect_err("I64 must be rejected by build_int_const_wide");
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
    b.function_mut().extend_asm_fingerprint(ret, &[SENTINEL_LIFT_ADDR]);
    let entry_id = b.entry();
    let function = b.function();
    validate(function, entry_id).expect("IntConst(Wide(...)) built via intern_wide_const must validate clean");
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
    b.function_mut().extend_asm_fingerprint(ret, &[SENTINEL_LIFT_ADDR]);

    let pre = b.function().wide_const_interner.len();
    assert_eq!(pre, 2, "before compact, both wide consts are in the side-table");

    let mut bfg = b.build()?;
    bfg.compact()?;

    let post = bfg.wide_const_interner.len();
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

    /// The SystemV integer arg-passing registers (RDI, RSI, RDX, RCX, R8,
    /// R9).  Every CC arg register is read at the Call site via
    /// `read_reg_vn`, which requires a tracked container — so a Call-
    /// building fixture must track the full arg set, not just RDI.
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
        // Track the full SystemV arg-register set: every CC arg register is
        // read via `read_reg_vn`, which errors on an untracked register.
        let mut tracked = vec![rax, rsp];
        tracked.extend(x86_64_arg_regs(&regs));
        let mut b = FunctionBuilder::new(tracked, &cc, strider_target::Endianness::Little).unwrap();
        let _ = rdi;
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let addr = b
            .build_int_const(0xdead_beef_u64, ValueType::I64)
            .unwrap();
        b.build_call(addr, None).unwrap();
        // The Call output kinds match `build_call(addr, None)` exactly: Control,
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
        // FunctionBuilder::new auto-adds the cc.ret_val_regs (rax, rdx),
        // ret_val_regs_float (xmm0, xmm1), the arg-passing regs (rdi, rsi,
        // rdx, rcx, r8, r9), and the stack pointer into the tracked set even
        // when the caller's `all_used_variables` doesn't list them.  An
        // "all-preserving" override must mark every one of those
        // callee-saved or they'll appear as clobber outputs.
        let rdx = regs.name_to_vn("RDX").unwrap();
        let xmm0 = regs.name_to_vn("XMM0").unwrap();
        let xmm1 = regs.name_to_vn("XMM1").unwrap();
        let _ = rdi;
        let mut b = FunctionBuilder::new(vec![rax, rsp], &cc, strider_target::Endianness::Little).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);

        // Override CC: every tracked variable is callee-saved → 0 clobbers.
        let mut callee_saved = vec![rax, rdx, xmm0, xmm1];
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
        };

        let addr = b
            .build_int_const(0xdead_beef_u64, ValueType::I64)
            .unwrap();
        b.build_call(addr, Some(&override_cc)).unwrap();
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
        // Inputs: control + memory + target + sp.  No arg slots.
        assert_eq!(inputs.len(), 4, "fentry-style Call takes no args (ctrl, mem, target, sp)");
        assert!(
            function.call_cc(call_node).is_some(),
            "override CC is recorded even when it clobbers nothing"
        );
        // No clobber outputs → no value_vn clobber tags on this Call.
        assert!(
            function
                .node_outputs(call_node)
                .iter()
                .all(|&v| function.get_vn_for_value(v).is_none()),
            "fentry-style Call has no clobber outputs, so none are tagged"
        );
    }

    /// A built Call's inputs must be `[ctrl, mem, target, sp, ...args]`:
    /// the stack-pointer value is wired at slot `[3]` (ahead of the
    /// arguments) and the first arg follows at slot `[4]`.
    #[test]
    fn call_sp_input_precedes_args() {
        let cc = x86_64_built_cc();
        let regs = x86_64_regs();
        let rax = regs.name_to_vn("RAX").unwrap();
        let rdi = regs.name_to_vn("RDI").unwrap();
        let rsp = regs.name_to_vn("RSP").unwrap();
        // Track RSP and the full SystemV arg-register set: every CC arg
        // register is read via `read_reg_vn`, which errors on an untracked
        // register.  RDI is still slot [4] (the first arg).
        let mut tracked = vec![rax, rsp];
        tracked.extend(x86_64_arg_regs(&regs));
        let mut b = FunctionBuilder::new(
            tracked,
            &cc,
            strider_target::Endianness::Little,
        )
        .unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);

        // The current SP value at the call site — the value wired into
        // input slot [3].
        let sp_value = b.read_variable(&rsp).unwrap();
        // The current RDI value — the lone arg, wired into slot [4].
        let arg0_value = b.read_variable(&rdi).unwrap();

        let addr = b.build_int_const(0xdead_beef_u64, ValueType::I64).unwrap();
        b.build_call(addr, None).unwrap();

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

    // ── FunctionBuilder extended-use round-trip ────────────────────────

    /// Drive the builder through several rounds of in-place mutation
    /// (mimicking an iterative analysis loop) without consuming it
    /// via `build()`.  At each step `entry()` must stay stable and
    /// `graph_mut()` must keep producing fresh node ids.
    #[test]
    fn analysis_loop_without_build_round_trips() {
        let mut b = empty_builder().unwrap();
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
            NodeKind::IntConst(IntPayload::Small(1_u64)),
            std::iter::empty(),
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(b.entry(), entry, "entry() stable after first mutation");

        // Second mutation: another synthesis; the first node must persist.
        let r2 = b.function_mut().graph_mut().create_node(
            NodeKind::IntConst(IntPayload::Small(2_u64)),
            std::iter::empty(),
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(b.entry(), entry, "entry() stable after second mutation");
        assert_ne!(r1, r2, "consecutive create_node calls produce distinct ids");

        // Both synthesized nodes are live in the arena.
        assert!(matches!(b.function().node_kind(r1), NodeKind::IntConst(IntPayload::Small(1))));
        assert!(matches!(b.function().node_kind(r2), NodeKind::IntConst(IntPayload::Small(2))));
    }

    /// After driving the builder through several rounds of in-place
    /// mutation, calling `build()` must still produce a valid graph
    /// (passes `validate`).  Pins the "build still works after
    /// extended use" contract that every imperative opt pass relies
    /// on.
    #[test]
    fn final_build_after_extended_use_yields_valid_built() {
        let mut b = empty_builder().unwrap();
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
        for k in 1u64..=5 {
            b.function_mut().graph_mut().create_node(
                NodeKind::IntConst(IntPayload::Small(k)),
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

// ── call_ret_val — Call emits ret-val outputs before clobbers ─────────────
//
// Verifies that after the ret-val / clobber split:
//   - `call_ret_vals_for(cc)` returns exactly the ret-val registers.
//   - `call_clobbered_for(cc)` returns only the non-ret caller-saved regs.
//   - A built Call emits `[Control, Memory, <ret-val outputs...>, <clobbers...>]`
//     in that exact order, with each ret-val output's `value_vn` tagged.
#[test]
fn call_ret_val_split_outputs_and_accessor() -> Result<()> {
    // rax: both ret-val and would-be caller-clobbered
    let rax = reg_vn(0x00, 8);
    // rcx: plain caller-clobbered (not a ret reg)
    let rcx = reg_vn(0x08, 8);
    // rbx: callee-saved (excluded from clobbers)
    let rbx = reg_vn(0x10, 8);
    // rsp: stack pointer (excluded from clobbers)
    let sp  = reg_vn(0x18, 8);

    let mut b = raw_builder(
        vec![rax, rcx, rbx, sp],
        &[],       // arg_passing
        &[rbx],    // callee_saved
        &[rax],    // ret_val_regs
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;

    let cc = b.function().default_cc().clone();

    // (a) call_ret_vals_for returns only rax.
    let ret_vals = b.function().call_ret_vals_for(&cc);
    assert_eq!(
        ret_vals,
        vec![rax],
        "call_ret_vals_for must return exactly the ret-val registers"
    );

    // (b) call_clobbered_for must NOT contain rax (it moved to the ret-val group).
    let clobbered = b.function().call_clobbered_for(&cc);
    assert!(
        !clobbered.contains(&rax),
        "call_clobbered_for must not contain the ret-val register rax; got {clobbered:?}"
    );
    // rcx is caller-clobbered and not a ret reg, so it stays in clobbered.
    assert!(
        clobbered.contains(&rcx),
        "call_clobbered_for must still contain the plain caller-clobbered reg rcx"
    );

    // (c) Build a Call and verify output order:
    //   [Control, Memory, <rax-ret-val>, <rcx-clobber>]
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1234_u64, ValueType::I64)?;
    b.build_call(addr, None)?;
    b.set_lift_addr(None);

    let f = b.function();
    let call_node = f
        .graph()
        .all_node_ids()
        .find(|n| matches!(f.node_kind(*n), NodeKind::Call))
        .expect("exactly one Call node must be present");
    let outs = f.node_outputs(call_node);

    // Outputs: [Control, Memory] + ret_vals + clobbers
    assert_eq!(
        outs.len(),
        2 + ret_vals.len() + clobbered.len(),
        "Call output count must be Control + Memory + ret_vals + clobbers"
    );

    // Slot 2 is the ret-val (rax).
    let rax_out = outs[2];
    assert_eq!(
        f.get_vn_for_value(rax_out),
        Some(rax),
        "ret-val output at slot 2 must carry value_vn = rax"
    );

    // Slot 3 is the clobber (rcx).
    let rcx_out = outs[3];
    assert_eq!(
        f.get_vn_for_value(rcx_out),
        Some(rcx),
        "clobber output at slot 3 must carry value_vn = rcx"
    );

    Ok(())
}

/// Every arg-passing register's InitialVar is registered as its positional
/// arg carrier at builder-entry time, before any optimization runs.
#[test]
fn register_args_recorded_at_builder_entry() -> Result<()> {
    let rdi = rsleigh::Vn { size: 8, addr_off: 0x38, addr_space: rsleigh::VnSpace::REGISTER };
    let rsi = rsleigh::Vn { size: 8, addr_off: 0x30, addr_space: rsleigh::VnSpace::REGISTER };
    let sp = rsleigh::Vn { size: 8, addr_off: 0x20, addr_space: rsleigh::VnSpace::REGISTER };
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
    };
    let mut b = FunctionBuilder::new(vec![rdi, rsi, sp], &cc, strider_target::Endianness::Little)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let arg0 = b.function().arg_index_to_values(0);
    let arg1 = b.function().arg_index_to_values(1);
    assert_eq!(arg0.len(), 1, "arg 0 carrier registered at entry");
    assert_eq!(arg1.len(), 1, "arg 1 carrier registered at entry");
    assert!(matches!(b.function().node_kind(b.function().producer(arg0[0])),
        NodeKind::InitialVar(v) if *v == rdi));
    assert!(matches!(b.function().node_kind(b.function().producer(arg1[0])),
        NodeKind::InitialVar(v) if *v == rsi));
    Ok(())
}
