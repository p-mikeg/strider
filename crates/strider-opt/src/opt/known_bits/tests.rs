use super::*;
use crate::test_support::{assert_returns_const, make_fn, return_kind, run_to_fixed_point};
use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{ExtendOp, FunctionBuilder, IntBinaryOp};
use strider_ir_test_utils::RegisterSet;

/// `(x | 7) & 4`: bits 0-2 of the Or are known 1, so the And is fully
/// determined.
#[test]
fn known_bits_or_then_and() -> Result<()> {
    let mut fg2 = make_fn(|b| {
        let x_seed = b.build_int_const(0u64, ValueType::I64).unwrap();
        let c7 = b.build_int_const(7u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let ored = b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, ValueType::I64)?;
        b.build_int_binary_operation(ored, c4, IntBinaryOp::And, ValueType::I64)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg2)?;
    assert_returns_const(&fg2, 4);
    Ok(())
}

/// `(x & 0xF0) & 0x0F`: the masks are disjoint, so the result is 0.
#[test]
fn known_bits_and_mask_then_and() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let f0 = b.build_int_const(0xF0u64, ValueType::I8).unwrap();
        let f = b.build_int_const(0x0Fu64, ValueType::I8).unwrap();
        let inner = b.build_int_binary_operation(x, f0, IntBinaryOp::And, ValueType::I8)?;
        b.build_int_binary_operation(inner, f, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0);
    Ok(())
}

/// An `IntConst` is already fully known; the pass must not report a change.
#[test]
fn known_bits_const_no_change() -> Result<()> {
    let mut fg = make_fn(|b| Ok(b.build_int_const(42u64, ValueType::I64).unwrap()))?;
    assert!(
        !crate::pipeline::run_one(&KnownBits, &mut fg, &mut crate::OptCtx::new(None))?.changed()
    );
    Ok(())
}

/// `popcount(I8)` maxes at 8, so bits 4..7 are known zero and `& 0xF0` is 0.
#[test]
fn known_bits_popcount_range() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let pc = b.build_popcount(x, ValueType::I8)?;
        let mask = b.build_int_const(0xF0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(pc, mask, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0);
    Ok(())
}

/// Popcount range at the I64 edge with a genuinely opaque input: the I8 case
/// above has a constant input, so it can't distinguish exact-value from range
/// and never reaches the 7-bit boundary.
#[test]
fn known_bits_popcount_range_i64_opaque() -> Result<()> {
    let v = rsleigh::Vn {
        addr_off: 0x80,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = RegisterSet::new().tracked(v).build_fn_single_region()?;
    let x = b.read_variable(&v)?;
    let pc = b.build_popcount(x, ValueType::I64)?;
    // Bits 7..63: everything above the 7 bits that can hold 0..=64.
    let mask = b
        .build_int_const(0xFFFF_FFFF_FFFF_FF80u64, ValueType::I64)
        .unwrap();
    let and = b.build_int_binary_operation(pc, mask, IntBinaryOp::And, ValueType::I64)?;
    b.build_return(Some(and), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0);
    Ok(())
}

/// `x >> 4` at I8 leaves the upper 4 bits zero, so `& 0xF0` is 0.
#[test]
fn known_bits_shift_right_upper_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x55u64, ValueType::I8).unwrap();
        let four = b.build_int_const(4u64, ValueType::I8).unwrap();
        let shr = b.build_int_binary_operation(x, four, IntBinaryOp::ShiftRight, ValueType::I8)?;
        let mask_high = b.build_int_const(0xF0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(shr, mask_high, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0);
    Ok(())
}

/// `x << 5` at I8 leaves the lower 5 bits zero, so `& 0x1F` is 0.
#[test]
fn known_bits_shift_left_lower_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let five = b.build_int_const(5u64, ValueType::I8).unwrap();
        let shl = b.build_int_binary_operation(x, five, IntBinaryOp::ShiftLeft, ValueType::I8)?;
        let mask_low = b.build_int_const(0x1Fu64, ValueType::I8).unwrap();
        b.build_int_binary_operation(shl, mask_low, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0);
    Ok(())
}

/// The worklist must carry known-1 bits through a long Or chain.
#[test]
fn known_bits_long_or_and_chain() -> Result<()> {
    let mut fg = make_fn(|b| {
        let mut acc = b.build_int_const(0u64, ValueType::I64).unwrap();
        for i in 0..8u64 {
            let bit = b.build_int_const(1u64 << i, ValueType::I64).unwrap();
            acc = b.build_int_binary_operation(acc, bit, IntBinaryOp::Or, ValueType::I64)?;
        }
        let mask = b.build_int_const(0xFFu64, ValueType::I64).unwrap();
        b.build_int_binary_operation(acc, mask, IntBinaryOp::And, ValueType::I64)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0xFF);
    Ok(())
}

/// `lzcount(I8)` maxes at 8, so bits 4..7 are known zero and `& 0xF0` is 0.
#[test]
fn known_bits_lzcount_range() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x01u64, ValueType::I8).unwrap();
        let lz = b.build_lzcount(x, ValueType::I8)?;
        let mask = b.build_int_const(0xF0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(lz, mask, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0);
    Ok(())
}

/// Xor of two operands with identical known bits: every bit agrees, so every
/// bit is known 0.
#[test]
fn known_bits_xor_identical_or_known_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x55u64, ValueType::I8).unwrap();
        let ff = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, ValueType::I8)?;
        b.build_int_binary_operation(or_, or_, IntBinaryOp::Xor, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0);
    Ok(())
}

/// Complement round-trip: `~~x == x`.  Complement is `Xor(x, all_ones)`, so
/// this exercises the Xor arm's ones/zeros swap twice.
#[test]
fn known_bits_neg_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xAAu64, ValueType::I8).unwrap();
        let ff = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, ValueType::I8)?;
        let all_ones = b.build_int_const(u128::MAX, ValueType::I8)?;
        let n1 = b.build_int_binary_operation(or_, all_ones, IntBinaryOp::Xor, ValueType::I8)?;
        b.build_int_binary_operation(n1, all_ones, IntBinaryOp::Xor, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0xFF);
    Ok(())
}

#[test]
fn known_bits_truncate_preserves_low_bits() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0xABCDu64, ValueType::I16).unwrap();
        b.truncate_if_needed(v, ValueType::I8)
    })?;
    // The builder may already have folded this at construction; either way the
    // end state must match.
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    let semantic = fg.int_const_u128(val);
    assert_eq!(semantic, Some(0xCD), "truncate must preserve low byte");
    Ok(())
}

use crate::test_support::return_value;

/// `analyze` has no contradiction error path: facts are recomputed and
/// overwritten each visit, never unioned, so the only fallible arm is
/// malformed IR.
#[test]
fn analyze_returns_populated_map_no_merge_error() -> Result<()> {
    let fg = make_fn(|b| {
        let c = b.build_int_const(7u64, ValueType::I64).unwrap();
        let mask = b.build_int_const(4u64, ValueType::I64).unwrap();
        b.build_int_binary_operation(c, mask, IntBinaryOp::And, ValueType::I64)
    })?;
    let ctx = &fg;
    let known = super::analyze(ctx)?;
    let return_val = return_value(fg.graph())?;
    let any_known_four = known.iter().any(|(out, &kb)| {
        let Some(ty) = fg.value_type_opt(out) else {
            return false;
        };
        let Some(mask) = super::type_mask_u128(ty) else {
            return false;
        };
        kb.all_known(mask) && kb.ones == 4
    });
    assert!(
        any_known_four,
        "analyze must record the And(7,4) output as known = 4"
    );
    let _ = return_val;
    Ok(())
}

/// Folding driven by known bits alone, on a non-constant operand.
#[test]
fn known_bits_and_with_zero_folds_via_map() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let zero = b.build_int_const(0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(
        fg.int_const_u128(val),
        Some(0),
        "And(x, 0) is known-zero and must fold to IntConst(0) via the map rewrite",
    );
    Ok(())
}

/// Booleans are `I1`, so the mask handling must cover `bit_width(I1) == 1`.
#[test]
fn known_bits_i1_folds_via_map() -> Result<()> {
    let mut fg = make_fn(|b| {
        let zero = b.build_int_const(0u64, ValueType::I1).unwrap();
        let one = b.build_int_const(1u64, ValueType::I1).unwrap();
        b.build_int_binary_operation(zero, one, IntBinaryOp::Or, ValueType::I1)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 1);
    Ok(())
}

/// The lattice is 128-bit wide: bit 100 must survive an I128 fold.
#[test]
fn known_bits_i128_high_bit_or_folds() -> Result<()> {
    let hi: u128 = 1u128 << 100;
    let mut fg = make_fn(|b| {
        let a = b.build_int_const(hi, ValueType::I128).unwrap();
        let zero = b.build_int_const(0u64, ValueType::I128).unwrap();
        b.build_int_binary_operation(a, zero, IntBinaryOp::Or, ValueType::I128)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(
        fg.int_const_u128(val),
        Some(hi),
        "KnownBits must track the full 128-bit Or and fold to IntConst(1<<100)",
    );
    Ok(())
}

/// A fully-known output reachable via two consumers must fold exactly once:
/// the map holds one entry per output regardless of consumer count.
#[test]
fn known_bits_shared_output_folds_once() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0u64, ValueType::I8).unwrap();
        let eight = b.build_int_const(8u64, ValueType::I8).unwrap();
        // Fully known = 8, but not an IntConst node, and consumed twice below.
        let shared = b.build_int_binary_operation(c, eight, IntBinaryOp::Or, ValueType::I8)?;
        let m8 = b.build_int_const(8u64, ValueType::I8).unwrap();
        let m4 = b.build_int_const(4u64, ValueType::I8).unwrap();
        let a = b.build_int_binary_operation(shared, m8, IntBinaryOp::And, ValueType::I8)?;
        let d = b.build_int_binary_operation(shared, m4, IntBinaryOp::And, ValueType::I8)?;
        // 8 ^ 0 = 8.
        b.build_int_binary_operation(a, d, IntBinaryOp::Xor, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(
        fg.int_const_u128(val),
        Some(8),
        "shared fully-known output must fold cleanly with no double-processing",
    );
    Ok(())
}

/// `make_fn` plus one tracked 1-byte variable, so a test can `read_variable`
/// for a genuinely unknown value.
fn make_fn_with_var<F>(f: F) -> Result<strider_ir::Function>
where
    F: FnOnce(&mut FunctionBuilder, rsleigh::Vn) -> Result<strider_ir::Value>,
{
    let v = rsleigh::Vn {
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 1,
    };
    let mut b = RegisterSet::new().tracked(v).build_fn_single_region()?;
    let val = f(&mut b, v)?;
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// `ShiftRight(x | 2, 1) & 1` is 1 for every `x`: the literal `2` pins bit 1
/// known-1, the shift moves it to bit 0, the mask keeps only bit 0.  A shift
/// arm that cleared the lhs bits would lose the propagated known-1.
#[test]
fn known_bits_shift_right_propagates_lhs_ones() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let two = b.build_int_const(2u64, ValueType::I8).unwrap();
        let one = b.build_int_const(1u64, ValueType::I8).unwrap();
        let ored = b.build_int_binary_operation(x, two, IntBinaryOp::Or, ValueType::I8)?;
        let shifted =
            b.build_int_binary_operation(ored, one, IntBinaryOp::ShiftRight, ValueType::I8)?;
        b.build_int_binary_operation(shifted, one, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(fg.int_const_u128(val), Some(1));
    Ok(())
}

/// Mirror of the ShiftRight case: `ShiftLeft(x | 1, 7) & 0x80` is 0x80 for
/// every `x`.
#[test]
fn known_bits_shift_left_propagates_lhs_ones() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let one = b.build_int_const(1u64, ValueType::I8).unwrap();
        let seven = b.build_int_const(7u64, ValueType::I8).unwrap();
        let mask80 = b.build_int_const(0x80u64, ValueType::I8).unwrap();
        let ored = b.build_int_binary_operation(x, one, IntBinaryOp::Or, ValueType::I8)?;
        let shifted =
            b.build_int_binary_operation(ored, seven, IntBinaryOp::ShiftLeft, ValueType::I8)?;
        b.build_int_binary_operation(shifted, mask80, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(fg.int_const_u128(val), Some(0x80));
    Ok(())
}

/// A fully-known LHS is not enough: an unknown shift amount must yield no
/// known bits, or ConstantFold could later collapse the shift to a bogus
/// constant.
#[test]
fn known_bits_shift_by_unknown_amount_does_not_fold() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let known_lhs = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let var_shift = b.read_variable(&var)?;
        b.build_int_binary_operation(known_lhs, var_shift, IntBinaryOp::ShiftLeft, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let kind = return_kind(fg.graph())?;
    assert_eq!(
        kind,
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
        "a shift by an unknown amount must stay a ShiftLeft node, not fold to a constant"
    );
    Ok(())
}

// Sleigh returns 0 for any shift amount >= bit_width.  Masking the amount to
// the low log2(bit_width) bits instead would wrap it back into range and give
// wrong known bits, e.g. `0xFFu8 << 8` computed as `0xFF << 0`.

/// Per Sleigh, `1u8 << 8` is 0, not 1.
#[test]
fn known_bits_shl_at_bit_width_folds_to_zero_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, ValueType::I8).unwrap();
        let eight = b.build_int_const(8u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(one, eight, IntBinaryOp::ShiftLeft, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(
        fg.int_const_u128(val),
        Some(0),
        "Sleigh: 1u8 << 8 = 0 (shift >= bit_width returns 0).  Pre-fix \
         KnownBits computed `1u8 << (8 & 7) = 1` and left the value \
         unresolved or folded to 1."
    );
    Ok(())
}

/// Right-shift counterpart: per Sleigh, `0xFFu32 >> 32` is 0.
#[test]
fn known_bits_shr_at_bit_width_folds_to_zero_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0xFFu64, ValueType::I32).unwrap();
        let thirty_two = b.build_int_const(32u64, ValueType::I32).unwrap();
        b.build_int_binary_operation(v, thirty_two, IntBinaryOp::ShiftRight, ValueType::I32)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(
        fg.int_const_u128(val),
        Some(0),
        "Sleigh: 0xFFu32 >> 32 = 0.  Pre-fix KnownBits computed \
         `0xFF >> (32 & 31) = 0xFF` and the chain fell through to non-zero."
    );
    Ok(())
}

/// PPC CR0-byte extraction chain: `((cr0 & 1) | 2) >> 1) & 1` is 1 for every
/// `cr0`, since the literal `2` unconditionally sets bit 1 of the Or.  Pins
/// propagation across Or, ShiftRight, and And in sequence.
#[test]
fn known_bits_ppc_cr0_extract_chain() -> Result<()> {
    let mut fg = make_fn_with_var(|b, cr0_var| {
        let cr0 = b.read_variable(&cr0_var)?;
        let one = b.build_int_const(1u64, ValueType::I8).unwrap();
        let two = b.build_int_const(2u64, ValueType::I8).unwrap();
        let masked = b.build_int_binary_operation(cr0, one, IntBinaryOp::And, ValueType::I8)?;
        let ored = b.build_int_binary_operation(two, masked, IntBinaryOp::Or, ValueType::I8)?;
        let shifted =
            b.build_int_binary_operation(ored, one, IntBinaryOp::ShiftRight, ValueType::I8)?;
        b.build_int_binary_operation(shifted, one, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    let semantic = fg.int_const_u128(val);
    assert_eq!(
        semantic,
        Some(1),
        "((cr0 & 1) | 2) >> 1 & 1 must fold to 1 for every cr0; \
         got non-constant return value (KnownBits propagation failure)"
    );
    Ok(())
}

// `extend_if_needed` folds an `IntConst` input at builder level, so the
// SignExtend tests below feed it an Or-of-constants instead: fully known, but
// not an `IntConst` node.  The fixed-point loop folds the Or first, then the
// SignExtend arm fires on the re-run.

/// Sign bit known 0: the upper 56 bits of the extension are zero.
#[test]
fn known_bits_sign_extend_msb_zero_folds_to_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let zero = b.build_int_const(0u64, ValueType::I8).unwrap();
        let c = b.build_int_const(0x7Fu64, ValueType::I8).unwrap();
        let or_ = b.build_int_binary_operation(zero, c, IntBinaryOp::Or, ValueType::I8)?;
        b.extend_if_needed(or_, ValueType::I64, ExtendOp::SignExtend)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0x7F_u64);
    Ok(())
}

/// Sign bit known 1: the upper 56 bits of the extension are one.
#[test]
fn known_bits_sign_extend_msb_one_folds_to_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let zero = b.build_int_const(0u64, ValueType::I8).unwrap();
        let c = b.build_int_const(0x80u64, ValueType::I8).unwrap();
        let or_ = b.build_int_binary_operation(zero, c, IntBinaryOp::Or, ValueType::I8)?;
        b.extend_if_needed(or_, ValueType::I64, ExtendOp::SignExtend)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0xFFFF_FFFF_FFFF_FF80_u64);
    Ok(())
}

/// A ZeroExtend's upper bits are known zero regardless of the input, so a mask
/// touching only those bits folds to 0.
#[test]
fn known_bits_zero_extend_upper_known_zero_enables_mask_drop() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let widened = b.extend_if_needed(x, ValueType::I64, ExtendOp::ZeroExtend)?;
        let high_mask = b
            .build_int_const(0xFFFF_FFFF_FFFF_FF00u64, ValueType::I64)
            .unwrap();
        b.build_int_binary_operation(widened, high_mask, IntBinaryOp::And, ValueType::I64)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(
        fg.int_const_u128(val),
        Some(0),
        "And(ZeroExtend(I8→I64), 0xFF..00) must fold to 0 — upper 56 bits known zero",
    );
    Ok(())
}

/// The SignExtend counterpart of the ZeroExtend case: an unknown sign bit
/// gives no upper-bit facts, so nothing folds.
#[test]
fn known_bits_sign_extend_unknown_msb_does_not_fold() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let widened = b.extend_if_needed(x, ValueType::I64, ExtendOp::SignExtend)?;
        let high_mask = b
            .build_int_const(0xFFFF_FFFF_FFFF_FF00u64, ValueType::I64)
            .unwrap();
        b.build_int_binary_operation(widened, high_mask, IntBinaryOp::And, ValueType::I64)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_eq!(
        return_kind(fg.graph())?,
        NodeKind::IntBinaryOp(IntBinaryOp::And),
        "SignExtend of an unknown-sign value gives no upper-bit facts — no fold",
    );
    Ok(())
}

/// There is no `SShiftRight` arm, so an arithmetic shift stays opaque even
/// when the sign bit is provably zero and the result is mathematically 0.
/// Flip this to assert the fold if such an arm is ever added.
#[test]
fn known_bits_sshift_right_of_known_sign_zero_is_opaque() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let low_mask = b.build_int_const(0x7Fu64, ValueType::I8).unwrap();
        let nonneg = b.build_int_binary_operation(x, low_mask, IntBinaryOp::And, ValueType::I8)?;
        let four = b.build_int_const(4u64, ValueType::I8).unwrap();
        let shifted =
            b.build_int_binary_operation(nonneg, four, IntBinaryOp::SShiftRight, ValueType::I8)?;
        let high_mask = b.build_int_const(0xF8u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(shifted, high_mask, IntBinaryOp::And, ValueType::I8)
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_eq!(
        return_kind(fg.graph())?,
        NodeKind::IntBinaryOp(IntBinaryOp::And),
        "SShiftRight is not modelled by KnownBits — the chain must survive unfolded",
    );
    Ok(())
}

// A fold cascade-culls the operand cones that justified it, so their
// asm-fingerprints must be absorbed into the new constant first.
// Over-tainting is fine; the contract is superset-only.

/// The operand `7` carries a distinct addr and differs in value from the
/// folded result `4`, so the new constant cannot be a dedup hit on the `7`
/// node: an absorbed OPERAND_ADDR can only have come from the cone walk.
#[test]
fn known_bits_fold_absorbs_contributing_operand_fingerprint() -> Result<()> {
    use strider_ir::IRViewer;
    const OPERAND_ADDR: u64 = 0xC0DE_0002;

    let mut fg = make_fn(|b| {
        let x_seed = b.build_int_const(0u64, ValueType::I64).unwrap();
        // Every other node carries the sentinel.
        b.set_lift_addr(Some(OPERAND_ADDR));
        let c7 = b.build_int_const(7u64, ValueType::I64).unwrap();
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let ored = b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, ValueType::I64)?;
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        b.build_int_binary_operation(ored, c4, IntBinaryOp::And, ValueType::I64)
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    assert_returns_const(&fg, 4);
    let folded = fg.producer(return_value(fg.graph())?);
    assert!(
        fg.side_tables()
            .asm_fingerprint(folded)
            .contains(&OPERAND_ADDR),
        "KnownBits must absorb the contributing operand's asm-fingerprint into \
         the folded constant (proof of why the value is constant); got {:?}",
        fg.side_tables().asm_fingerprint(folded)
    );
    Ok(())
}

/// The fixpoint-propagation hole: `((x & 1) | 2) & 0` folds to 0, but the
/// inner `x & 1` never folds (it depends on `x`), so its fingerprint can never
/// ride a later fold upward.  It sits two levels down, so a one-hop input
/// absorb would lose it when the cone is culled.
#[test]
fn known_bits_fold_absorbs_cone_through_nonfolding_intermediate() -> Result<()> {
    use strider_ir::IRViewer;
    const INNER_ADDR: u64 = 0xC0DE_0003;

    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        b.set_lift_addr(Some(INNER_ADDR));
        let one = b.build_int_const(1u64, ValueType::I8).unwrap();
        let x_and_1 = b.build_int_binary_operation(x, one, IntBinaryOp::And, ValueType::I8)?;
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let two = b.build_int_const(2u64, ValueType::I8).unwrap();
        let ored = b.build_int_binary_operation(x_and_1, two, IntBinaryOp::Or, ValueType::I8)?;
        let zero = b.build_int_const(0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(ored, zero, IntBinaryOp::And, ValueType::I8)
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    assert_returns_const(&fg, 0);
    let folded = fg.producer(return_value(fg.graph())?);
    assert!(
        fg.side_tables()
            .asm_fingerprint(folded)
            .contains(&INNER_ADDR),
        "fold must absorb the full backward cone — including the non-folding \
         inner `x & 1` two levels down, whose addr the fixpoint can never \
         propagate; got {:?}",
        fg.side_tables().asm_fingerprint(folded)
    );
    Ok(())
}

/// The other direction: `Load[addr] & 0` folds because of the `& 0`, so the
/// Load's address cone contributed nothing and the walk must stop at the Load
/// rather than tainting it.
#[test]
fn known_bits_fold_does_not_taint_opaque_load_address_cone() -> Result<()> {
    use strider_ir::IRViewer;
    const ADDR_ADDR: u64 = 0xC0DE_0099;

    let mut fg = make_fn_with_var(|b, _var| {
        // An unrestricted cone walk would descend Load -> address and absorb
        // this.
        b.set_lift_addr(Some(ADDR_ADDR));
        let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
        let zero = b.build_int_const(0u64, ValueType::I64).unwrap();
        b.build_int_binary_operation(loaded, zero, IntBinaryOp::And, ValueType::I64)
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    assert_returns_const(&fg, 0);
    let folded = fg.producer(return_value(fg.graph())?);
    assert!(
        !fg.side_tables()
            .asm_fingerprint(folded)
            .contains(&ADDR_ADDR),
        "fold must NOT taint the opaque Load's address cone — the address \
         did not contribute to the known bits; got {:?}",
        fg.side_tables().asm_fingerprint(folded)
    );
    Ok(())
}

/// Two folds sharing an upstream cone must EACH absorb it.  A `seen` set
/// shared across folds would skip nodes the first fold already visited and
/// under-attribute the second; the memo returns the full set every time.
///
/// A shared `(0 | 7)` with a distinct addr feeds `& 4` and `& 1`.  Both
/// results differ from each other and from 7, so neither folded constant can
/// pick up the addr by deduping with the shared node.
#[test]
fn known_bits_shared_cone_both_folds_absorb_fingerprint() -> Result<()> {
    use strider_ir::IRViewer;
    const SHARED_ADDR: u64 = 0xC0DE_5EED;

    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let x_seed = b.build_int_const(0u64, ValueType::I8).unwrap();
        b.set_lift_addr(Some(SHARED_ADDR));
        let c7 = b.build_int_const(7u64, ValueType::I8).unwrap();
        let shared = b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, ValueType::I8)?;
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let c4 = b.build_int_const(4u64, ValueType::I8).unwrap();
        let fold_a = b.build_int_binary_operation(shared, c4, IntBinaryOp::And, ValueType::I8)?;
        let c1 = b.build_int_const(1u64, ValueType::I8).unwrap();
        let fold_b = b.build_int_binary_operation(shared, c1, IntBinaryOp::And, ValueType::I8)?;
        // Or each fold with the unknown `x` so both the `4` and the `1`
        // constants stay live after the pass.
        let live_a = b.build_int_binary_operation(fold_a, x, IntBinaryOp::Or, ValueType::I8)?;
        let live_b = b.build_int_binary_operation(fold_b, x, IntBinaryOp::Or, ValueType::I8)?;
        b.build_int_binary_operation(live_a, live_b, IntBinaryOp::Or, ValueType::I8)
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    let top = fg.producer(return_value(fg.graph())?);
    // The top Or folds too, so locate the two fold results by value among all
    // surviving IntConst nodes.
    let mut found_4 = false;
    let mut found_1 = false;
    let int_consts: Vec<NodeId> = fg
        .walk_kind(|k| matches!(k, NodeKind::IntConst(_)))
        .collect();
    for node in int_consts {
        let out = fg.node_outputs(node)[0];
        let Some(v) = fg.int_const_u128(out) else {
            continue;
        };
        if v == 4 {
            found_4 = true;
            assert!(
                fg.side_tables()
                    .asm_fingerprint(node)
                    .contains(&SHARED_ADDR),
                "the `& 4` fold must absorb the shared cone's addr; got {:?}",
                fg.side_tables().asm_fingerprint(node)
            );
        } else if v == 1 {
            found_1 = true;
            assert!(
                fg.side_tables()
                    .asm_fingerprint(node)
                    .contains(&SHARED_ADDR),
                "the `& 1` fold must ALSO absorb the shared cone's addr — a \
                 shared `seen` set would have lost it on the second fold; got {:?}",
                fg.side_tables().asm_fingerprint(node)
            );
        }
    }
    assert!(
        found_4 && found_1,
        "both folded constants (4 and 1) must be present in the graph (top={top:?})"
    );
    Ok(())
}

#[test]
fn kb_default_is_all_unknown() {
    let kb = super::KnownBitsFacts::default();
    assert_eq!(kb.ones, 0);
    assert_eq!(kb.zeros, 0);
}

#[test]
fn kb_struct_literal_disjoint_ones_zeros() {
    let kb = super::KnownBitsFacts {
        ones: 0b01,
        zeros: 0b10,
    };
    assert_eq!(kb.ones, 0b01);
    assert_eq!(kb.zeros, 0b10);
}

/// The default must stay fully unknown, not all-zeros or all-ones: the
/// `Truncate` arm reads `known[input]` directly, so a drifted default would
/// synthesise spurious known bits for any input the analysis gated out.
#[test]
fn kb_default_is_fully_unknown_not_all_zero_or_all_one() {
    let kb = super::KnownBitsFacts::default();
    assert_eq!(
        kb.ones, 0,
        "KnownBitsFacts::default().ones must be 0 (no bit known to be 1)"
    );
    assert_eq!(
        kb.zeros, 0,
        "KnownBitsFacts::default().zeros must be 0 (no bit known to be 0)"
    );
}
