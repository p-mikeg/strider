use crate::pipeline::Optimizer;
use crate::test_support::make_fn;
use super::*;
use anyhow::anyhow;
use ir::node::{NodeKind, NodeOutputType};
use ir::{ExtendOp, FunctionBuilder, IntBinaryOp};

fn return_kind(fg: &ir::BuiltFunctionGraph) -> Result<NodeKind> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    let val = fg.node_inputs(ret)[2];
    Ok(*fg.kind_of_output(val))
}

// ── Original tests ────────────────────────────────────────────────────────────

/// `(x | 7) & 4` — bits 0-2 of `Or` are known 1; after And with 4 every
/// bit is determined → should fold to `IntConst(4)`.
#[test]
fn known_bits_or_then_and() -> Result<()> {
    let mut fg2 = make_fn(|b| {
        let x_seed = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
        let c7 = b.build_int_const(7u64, NodeOutputType::U64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::U64).unwrap();
        let ored = b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, NodeOutputType::U64)?;
        b.build_int_binary_operation(ored, c4, IntBinaryOp::And, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg2.graph, fg2.entry)?.changed();
    }
    assert_eq!(return_kind(&fg2)?, NodeKind::IntConst(4));
    Ok(())
}

/// `(x & 0xF0) & 0x0F` — the two masks have no overlap, so the result is 0.
#[test]
fn known_bits_and_mask_then_and() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
        let f0 = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        let f = b.build_int_const(0x0Fu64, NodeOutputType::U8).unwrap();
        let inner = b.build_int_binary_operation(x, f0, IntBinaryOp::And, NodeOutputType::U8)?;
        b.build_int_binary_operation(inner, f, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0));
    Ok(())
}

/// A plain `IntConst` already has all bits known — the optimizer must not
/// loop or report spurious changes.
#[test]
fn known_bits_const_no_change() -> Result<()> {
    let mut fg = make_fn(|b| Ok(b.build_int_const(42u64, NodeOutputType::U64).unwrap()))?;
    assert!(!KnownBits.optimize(&mut fg.graph, fg.entry)?.changed());
    Ok(())
}

/// `popcount(U8)` fits in 4 bits (max = 8), so bits 4..7 are known zero.
/// `and(popcount(x), 0xF0)` should fold to 0.
#[test]
fn known_bits_popcount_range() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
        let pc = b.build_popcount(x, NodeOutputType::U8)?;
        let mask = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(pc, mask, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0));
    Ok(())
}

// ── Comprehensive tests ───────────────────────────────────────────────────────

/// After `x >> 4` for U8, the upper 4 bits are statically zero. ANDing
/// with `0xF0` (which targets only the upper bits) must fold to 0 — KnownBits
/// proves the result has no set bits.
#[test]
fn known_bits_shift_right_upper_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x55u64, NodeOutputType::U8).unwrap(); // any value
        let four = b.build_int_const(4u64, NodeOutputType::U8).unwrap();
        let shr = b.build_int_binary_operation(x, four, IntBinaryOp::ShiftRight, NodeOutputType::U8)?;
        let mask_high = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(shr, mask_high, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0));
    Ok(())
}

/// After `x << 5` for U8, the lower 5 bits are statically zero. ANDing with
/// `0x1F` (lower 5 bits) must fold to 0.
#[test]
fn known_bits_shift_left_lower_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
        let five = b.build_int_const(5u64, NodeOutputType::U8).unwrap();
        let shl = b.build_int_binary_operation(x, five, IntBinaryOp::ShiftLeft, NodeOutputType::U8)?;
        let mask_low = b.build_int_const(0x1Fu64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(shl, mask_low, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0));
    Ok(())
}

/// A long OR chain of single-bit constants then ANDed with 0xFF — the worklist
/// must propagate the OR's known-1 bits through the chain.
#[test]
fn known_bits_long_or_and_chain() -> Result<()> {
    let mut fg = make_fn(|b| {
        let mut acc = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
        for i in 0..8u64 {
            let bit = b.build_int_const(1u64 << i, NodeOutputType::U64).unwrap();
            acc = b.build_int_binary_operation(acc, bit, IntBinaryOp::Or, NodeOutputType::U64)?;
        }
        let mask = b.build_int_const(0xFFu64, NodeOutputType::U64).unwrap();
        b.build_int_binary_operation(acc, mask, IntBinaryOp::And, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0xFF));
    Ok(())
}

/// `lzcount(U8)` fits in 4 bits (max value 8). `and(lzcount(x), 0xF0)` must
/// fold to 0 — the upper 4 bits of an lzcount(U8) result are statically known
/// to be zero.
#[test]
fn known_bits_lzcount_range() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x01u64, NodeOutputType::U8).unwrap();
        let lz = b.build_lzcount(x, NodeOutputType::U8)?;
        let mask = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(lz, mask, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0));
    Ok(())
}

/// XOR of identical-bits inputs: bit known if both agree → must be known 0.
/// `(x | 0xFF) ^ (x | 0xFF)` for U8 — KnownBits should prove this is 0.
#[test]
fn known_bits_xor_identical_or_known_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x55u64, NodeOutputType::U8).unwrap();
        let ff = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, NodeOutputType::U8)?;
        // (x|0xFF) is statically all-ones; xoring with itself folds to 0.
        // (Note: this also exercises ConstantFold's `x ^ x → 0` identity, but
        // KnownBits-only would prove the result by the both-known-1 case.)
        b.build_int_binary_operation(or_, or_, IntBinaryOp::Xor, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0));
    Ok(())
}

/// Bitwise NOT swaps known-ones and known-zeros.  `(x | 0xFF) NOT NOT`
/// for U8 returns 0xFF — testing that bitwise-NOT propagation is correct
/// round-trip.
///
/// `IntUnaryOp::BitNot` is bitwise complement (`~x`); `IntUnaryOp::Neg` is
/// two's-complement negation (`-x`).  rsleigh's `IntNeg` opcode lifts to
/// `BitNot` (the Sleigh nomenclature predates the rename).
#[test]
fn known_bits_neg_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xAAu64, NodeOutputType::U8).unwrap();
        let ff = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, NodeOutputType::U8)?;
        // ~~(x|0xFF) — bitwise-NOT round-trip = identity.
        let n1 = b.build_int_unary_operation(or_, ir::IntUnaryOp::BitNot, NodeOutputType::U8)?;
        b.build_int_unary_operation(n1, ir::IntUnaryOp::BitNot, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind((&fg).into())?, NodeKind::IntConst(0xFF));
    Ok(())
}

/// `truncate(0xABCD U16) → U8` — the truncate preserves lower bits, so the
/// result has all bits known to 0xCD. KnownBits must propagate through
/// Truncate.
#[test]
fn known_bits_truncate_preserves_low_bits() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0xABCDu64, NodeOutputType::U16).unwrap();
        b.truncate_if_needed(v, NodeOutputType::U8)
    })?;
    // The builder likely already folded this at construction; just verify
    // the final state matches.
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    let val = return_value((&fg).into())?;
    let semantic = fg.int_const_val(val);
    assert_eq!(semantic, Some(0xCD), "truncate must preserve low byte");
    Ok(())
}

fn return_value(fg: &ir::BuiltFunctionGraph) -> Result<ir::Value> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(fg.node_inputs(ret)[2])
}

#[test]
fn merge_returns_err_on_contradiction() {
    // Bit 0 is provably 1 in `a`, provably 0 in `b`.  Merging a then b
    // must surface the contradiction as an Err — silently letting `ones`
    // win would mask a real soundness bug in either the analyzer or the
    // IR shape that produced the conflicting verdicts.
    let mut c = super::Kb::default();
    let a = super::Kb { ones: 0b1, zeros: 0 };
    let b = super::Kb { ones: 0, zeros: 0b1 };
    c.merge(a).expect("first merge clean");
    let err = c.merge(b);
    assert!(
        err.is_err(),
        "expected Err on contradicting merge; got {err:?}",
    );
}

// ── shifts must propagate the lhs's known bits ───────────────────────────────

/// Variant of `make_fn` that tracks a single 1-byte variable so the closure
/// can read it via `read_variable` to obtain a non-constant `InitialVar`
/// output — used by tests that want to model an unknown source value (such
/// as a freshly-entered architectural register).
fn make_fn_with_var<F>(f: F) -> Result<ir::BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder, rsleigh::Vn) -> Result<ir::Value>,
{
    let v = rsleigh::Vn {
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 1,
    };
    let mut b = FunctionBuilder::new_raw(vec![v], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let val = f(&mut b, v)?;
    b.build_return(Some(val), &[])?;
    b.build()
}

/// `ShiftRight(x | 2, 1) & 1` for U8 must fold to `IntConst(1)`: the literal
/// `2` has bit 1 known-1, OR with anything keeps bit 1 set, the shift moves
/// it to bit 0, and the final mask keeps only bit 0 — so the answer is `1`
/// regardless of `x`.  The previous KnownBits implementation cleared the
/// lhs bits entirely on shift, losing the propagated known-1.
#[test]
fn known_bits_shift_right_propagates_lhs_ones() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let two = b.build_int_const(2u64, NodeOutputType::U8).unwrap();
        let one = b.build_int_const(1u64, NodeOutputType::U8).unwrap();
        let ored = b.build_int_binary_operation(x, two, IntBinaryOp::Or, NodeOutputType::U8)?;
        let shifted =
            b.build_int_binary_operation(ored, one, IntBinaryOp::ShiftRight, NodeOutputType::U8)?;
        b.build_int_binary_operation(shifted, one, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    let val = return_value((&fg).into())?;
    assert_eq!(fg.int_const_val(val), Some(1));
    Ok(())
}

/// `ShiftLeft(x | 1, 7) & 0x80` for U8 must fold to `IntConst(0x80)`: bit 0
/// of `x|1` is known 1; shifting by 7 moves it to bit 7 (known 1) while bits
/// 0-6 become known 0.  ANDing with `0x80` keeps only bit 7 — so the result
/// is `0x80` regardless of `x`.
#[test]
fn known_bits_shift_left_propagates_lhs_ones() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let one = b.build_int_const(1u64, NodeOutputType::U8).unwrap();
        let seven = b.build_int_const(7u64, NodeOutputType::U8).unwrap();
        let mask80 = b.build_int_const(0x80u64, NodeOutputType::U8).unwrap();
        let ored = b.build_int_binary_operation(x, one, IntBinaryOp::Or, NodeOutputType::U8)?;
        let shifted =
            b.build_int_binary_operation(ored, seven, IntBinaryOp::ShiftLeft, NodeOutputType::U8)?;
        b.build_int_binary_operation(shifted, mask80, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    let val = return_value((&fg).into())?;
    assert_eq!(fg.int_const_val(val), Some(0x80));
    Ok(())
}

// ── Sleigh INT_LEFT/INT_RIGHT out-of-range shift semantics in KnownBits ──
//
// KnownBits's `IntBinaryOp::ShiftLeft` / `ShiftRight` arms compute a
// shift via `rhs_kb.ones & (ty.bit_width() - 1)` — i.e. mask the shift
// amount to the low log2(bit_width) bits.  Sleigh's
// `OpBehaviorIntLeft::evaluateBinary` (sleigh/src/opbehavior.cc:411)
// returns 0 when the shift is `>= bit_width`; the masked-shift form
// instead loops back to the low bits and produces a wrong known-bits
// result.  E.g. `IntConst(0xFF, U8) << 8` should be 0 (Sleigh) but
// the pre-fix arm computed `0xFF << (8 & 7) = 0xFF << 0 = 0xFF`.
//
// The visible bug: `(value << bit_width) & 1` should fold to 0 (because
// any value shifted by bit_width is 0), but the masked-shift form
// folds it as `(value << 0) & 1` and leaves it unresolved.

/// `IntConst(1, U8) << IntConst(8, U8)` is 0 per Sleigh — KnownBits
/// must fold the chain to a constant 0, not 1.
#[test]
fn known_bits_shl_at_bit_width_folds_to_zero_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, NodeOutputType::U8).unwrap();
        let eight = b.build_int_const(8u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(one, eight, IntBinaryOp::ShiftLeft, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    let val = return_value((&fg).into())?;
    assert_eq!(
        fg.int_const_val(val),
        Some(0),
        "Sleigh: 1u8 << 8 = 0 (shift >= bit_width returns 0).  Pre-fix \
         KnownBits computed `1u8 << (8 & 7) = 1` and left the value \
         unresolved or folded to 1."
    );
    Ok(())
}

/// `IntConst(0xFF, U32) >> IntConst(32, U32)` is 0 per Sleigh — KnownBits
/// must report all bits known zero.
#[test]
fn known_bits_shr_at_bit_width_folds_to_zero_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0xFFu64, NodeOutputType::U32).unwrap();
        let thirty_two = b.build_int_const(32u64, NodeOutputType::U32).unwrap();
        b.build_int_binary_operation(v, thirty_two, IntBinaryOp::ShiftRight, NodeOutputType::U32)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    let val = return_value((&fg).into())?;
    assert_eq!(
        fg.int_const_val(val),
        Some(0),
        "Sleigh: 0xFFu32 >> 32 = 0.  Pre-fix KnownBits computed \
         `0xFF >> (32 & 31) = 0xFF` and the chain fell through to non-zero."
    );
    Ok(())
}

/// PPC CR0-byte extraction chain: an unknown one-byte source value
/// (the cr0 register) is masked, ORed with a literal that pre-sets the EQ
/// bit, right-shifted to position the EQ bit at bit 0, and finally ANDed
/// with 1.  Mathematically, `((cr0 & 1) | 2) >> 1) & 1 == 1` for every
/// value of `cr0` because bit 1 of the OR is unconditionally set by the
/// literal `2`.  KnownBits must propagate the literal's known-1 bit through
/// `Or`, then `ShiftRight`, then `And`.
#[test]
fn known_bits_ppc_cr0_extract_chain() -> Result<()> {
    let mut fg = make_fn_with_var(|b, cr0_var| {
        let cr0 = b.read_variable(&cr0_var)?;
        let one = b.build_int_const(1u64, NodeOutputType::U8).unwrap();
        let two = b.build_int_const(2u64, NodeOutputType::U8).unwrap();
        let masked = b.build_int_binary_operation(cr0, one, IntBinaryOp::And, NodeOutputType::U8)?;
        let ored = b.build_int_binary_operation(two, masked, IntBinaryOp::Or, NodeOutputType::U8)?;
        let shifted =
            b.build_int_binary_operation(ored, one, IntBinaryOp::ShiftRight, NodeOutputType::U8)?;
        b.build_int_binary_operation(shifted, one, IntBinaryOp::And, NodeOutputType::U8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    let val = return_value((&fg).into())?;
    let semantic = fg.int_const_val(val);
    assert_eq!(
        semantic,
        Some(1),
        "((cr0 & 1) | 2) >> 1 & 1 must fold to 1 for every cr0; \
         got non-constant return value (KnownBits propagation failure)"
    );
    Ok(())
}


// ── SignExtend propagation ────────────────────────────────────────────────────
//
// `extend_if_needed` folds an `IntConst` input at builder level (coerce.rs:185),
// so to exercise the KnownBits SignExtend path we feed it a non-IntConst Or-of-
// constants whose result is fully known but whose node kind isn't IntConst.
// KnownBits Phase 2 first folds the Or to IntConst; the surrounding `while
// changed` loop then re-runs `analyze`, and only then does the SignExtend node's
// arm fire (or fail to fire, before the fix).

/// `SignExtend((0u8 | 0x7Fu8) : U8 → U64)` — MSB of the inner Or is known 0,
/// so the upper 56 bits of the SignExtend result must be zero.  Without the
/// SignExtend arm in `node_known_bits`, the SignExtend stays as a node;
/// with it, the entire chain folds to `IntConst(0x7F)`.
#[test]
fn known_bits_sign_extend_msb_zero_folds_to_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let zero = b.build_int_const(0u64, NodeOutputType::U8).unwrap();
        let c = b.build_int_const(0x7Fu64, NodeOutputType::U8).unwrap();
        let or_ = b.build_int_binary_operation(zero, c, IntBinaryOp::Or, NodeOutputType::U8)?;
        b.extend_if_needed(or_, NodeOutputType::U64, ExtendOp::SignExtend)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(
        return_kind((&fg).into())?,
        NodeKind::IntConst(0x7Fu128),
        "SignExtend of (0|0x7F) (MSB=0) must fold to IntConst(0x7F) once \
         the SignExtend arm propagates known bits"
    );
    Ok(())
}

/// `SignExtend((0u8 | 0x80u8) : U8 → U64)` — MSB of the inner Or is known 1,
/// so the upper 56 bits of the SignExtend result must be one.  Result must
/// fold to `IntConst(0xFFFF_FFFF_FFFF_FF80)`.
#[test]
fn known_bits_sign_extend_msb_one_folds_to_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let zero = b.build_int_const(0u64, NodeOutputType::U8).unwrap();
        let c = b.build_int_const(0x80u64, NodeOutputType::U8).unwrap();
        let or_ = b.build_int_binary_operation(zero, c, IntBinaryOp::Or, NodeOutputType::U8)?;
        b.extend_if_needed(or_, NodeOutputType::U64, ExtendOp::SignExtend)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(
        return_kind((&fg).into())?,
        NodeKind::IntConst(0xFFFF_FFFF_FFFF_FF80u128),
        "SignExtend of (0|0x80) (MSB=1) must fold to all-ones upper bits \
         once the SignExtend arm propagates known bits"
    );
    Ok(())
}
