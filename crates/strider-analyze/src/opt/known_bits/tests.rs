use crate::opt::pipeline::Optimizer;
use crate::opt::test_support::{make_fn, return_kind};
use super::*;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir_test_utils::RegisterSet;
use strider_ir::{ExtendOp, FunctionBuilder, IntBinaryOp};

// ── Original tests ────────────────────────────────────────────────────────────

/// `(x | 7) & 4` — bits 0-2 of `Or` are known 1; after And with 4 every
/// bit is determined → should fold to `IntConst(4)`.
#[test]
fn known_bits_or_then_and() -> Result<()> {
    let mut fg2 = make_fn(|b| {
        let x_seed = b.build_int_const(0u64, ValueType::I64).unwrap();
        let c7 = b.build_int_const(7u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let ored = b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, ValueType::I64)?;
        b.build_int_binary_operation(ored, c4, IntBinaryOp::And, ValueType::I64)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg2, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg2)?, NodeKind::IntConst(4));
    Ok(())
}

/// `(x & 0xF0) & 0x0F` — the two masks have no overlap, so the result is 0.
#[test]
fn known_bits_and_mask_then_and() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let f0 = b.build_int_const(0xF0u64, ValueType::I8).unwrap();
        let f = b.build_int_const(0x0Fu64, ValueType::I8).unwrap();
        let inner = b.build_int_binary_operation(x, f0, IntBinaryOp::And, ValueType::I8)?;
        b.build_int_binary_operation(inner, f, IntBinaryOp::And, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// A plain `IntConst` already has all bits known — the optimizer must not
/// loop or report spurious changes.
#[test]
fn known_bits_const_no_change() -> Result<()> {
    let mut fg = make_fn(|b| Ok(b.build_int_const(42u64, ValueType::I64).unwrap()))?;
    assert!(!KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed());
    Ok(())
}

/// `popcount(I8)` fits in 4 bits (max = 8), so bits 4..7 are known zero.
/// `and(popcount(x), 0xF0)` should fold to 0.
#[test]
fn known_bits_popcount_range() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let pc = b.build_popcount(x, ValueType::I8)?;
        let mask = b.build_int_const(0xF0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(pc, mask, IntBinaryOp::And, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

// ── Comprehensive tests ───────────────────────────────────────────────────────

/// After `x >> 4` for I8, the upper 4 bits are statically zero. ANDing
/// with `0xF0` (which targets only the upper bits) must fold to 0 — KnownBits
/// proves the result has no set bits.
#[test]
fn known_bits_shift_right_upper_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x55u64, ValueType::I8).unwrap(); // any value
        let four = b.build_int_const(4u64, ValueType::I8).unwrap();
        let shr = b.build_int_binary_operation(x, four, IntBinaryOp::ShiftRight, ValueType::I8)?;
        let mask_high = b.build_int_const(0xF0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(shr, mask_high, IntBinaryOp::And, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// After `x << 5` for I8, the lower 5 bits are statically zero. ANDing with
/// `0x1F` (lower 5 bits) must fold to 0.
#[test]
fn known_bits_shift_left_lower_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let five = b.build_int_const(5u64, ValueType::I8).unwrap();
        let shl = b.build_int_binary_operation(x, five, IntBinaryOp::ShiftLeft, ValueType::I8)?;
        let mask_low = b.build_int_const(0x1Fu64, ValueType::I8).unwrap();
        b.build_int_binary_operation(shl, mask_low, IntBinaryOp::And, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// A long OR chain of single-bit constants then ANDed with 0xFF — the worklist
/// must propagate the OR's known-1 bits through the chain.
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
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFF));
    Ok(())
}

/// `lzcount(I8)` fits in 4 bits (max value 8). `and(lzcount(x), 0xF0)` must
/// fold to 0 — the upper 4 bits of an lzcount(I8) result are statically known
/// to be zero.
#[test]
fn known_bits_lzcount_range() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x01u64, ValueType::I8).unwrap();
        let lz = b.build_lzcount(x, ValueType::I8)?;
        let mask = b.build_int_const(0xF0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(lz, mask, IntBinaryOp::And, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// XOR of identical-bits inputs: bit known if both agree → must be known 0.
/// `(x | 0xFF) ^ (x | 0xFF)` for I8 — KnownBits should prove this is 0.
#[test]
fn known_bits_xor_identical_or_known_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x55u64, ValueType::I8).unwrap();
        let ff = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, ValueType::I8)?;
        // (x|0xFF) is statically all-ones; xoring with itself folds to 0.
        // (Note: this also exercises ConstantFold's `x ^ x → 0` identity, but
        // KnownBits-only would prove the result by the both-known-1 case.)
        b.build_int_binary_operation(or_, or_, IntBinaryOp::Xor, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// Bitwise NOT swaps known-ones and known-zeros.  `(x | 0xFF) NOT NOT`
/// for I8 returns 0xFF — testing that bitwise-NOT propagation is correct
/// round-trip.  Bitwise complement is `Xor(x, IntConst(all_ones))` since
/// the former BitNot unary-op was removed; KnownBits' Xor arm handles the
/// known-bits flip when one operand is a fully-known all-ones constant.
#[test]
fn known_bits_neg_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xAAu64, ValueType::I8).unwrap();
        let ff = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, ValueType::I8)?;
        // ~~(x|0xFF) — bitwise-NOT round-trip = identity, encoded as two
        // chained `Xor(_, 0xFF)` at I8.
        let all_ones = b.build_all_ones_const(ValueType::I8)?;
        let n1 = b.build_int_binary_operation(or_, all_ones, IntBinaryOp::Xor, ValueType::I8)?;
        b.build_int_binary_operation(n1, all_ones, IntBinaryOp::Xor, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFF));
    Ok(())
}

/// `truncate(0xABCD I16) → I8` — the truncate preserves lower bits, so the
/// result has all bits known to 0xCD. KnownBits must propagate through
/// Truncate.
#[test]
fn known_bits_truncate_preserves_low_bits() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0xABCDu64, ValueType::I16).unwrap();
        b.truncate_if_needed(v, ValueType::I8)
    })?;
    // The builder likely already folded this at construction; just verify
    // the final state matches.
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    let val = return_value(&fg)?;
    let semantic = fg.int_const_val(val);
    assert_eq!(semantic, Some(0xCD), "truncate must preserve low byte");
    Ok(())
}

use crate::opt::test_support::return_value;

// ── analyze is infallible-by-shape: no merge / contradiction path ────────────
//
// `KnownBitsFacts::merge` (and its `Result`/contradiction check) was removed:
// each output's facts are recomputed from scratch and overwritten every visit,
// so there is no union-with-previous that could contradict.  These tests pin
// that the analysis no longer surfaces a merge error — they are structural
// (the `analyze` signature still returns `Result<KnownBitsMap>` only for the
// malformed-IR arm of `node_known_bits`, never for a merge), so they simply
// compile + run to a populated map and confirm the expected folds happen.

/// `analyze()` over a well-formed graph returns `Ok` and populates the map
/// for a fully-known output.  Pins that the analysis loop no longer has a
/// merge/contradiction error path.
#[test]
fn analyze_returns_populated_map_no_merge_error() -> Result<()> {
    let fg = make_fn(|b| {
        let c = b.build_int_const(7u64, ValueType::I64).unwrap();
        let mask = b.build_int_const(4u64, ValueType::I64).unwrap();
        b.build_int_binary_operation(c, mask, IntBinaryOp::And, ValueType::I64)
    })?;
    let ctx = strider_pattern::RewriteCtxView::from_built(&fg)?;
    // No `?`-on-merge here: the only fallible arm is malformed IR, which a
    // well-formed graph never hits.  The call compiling + returning Ok is the
    // structural confirmation that the merge/Result was dropped.
    let known = super::analyze(ctx)?;
    // The `And(7, 4)` output must be recorded as fully known = 4.
    let return_val = return_value(&fg)?;
    // Walk to the And node's output via the returned map: at least one output
    // must be fully known to 4.
    let any_known_four = known.iter().any(|(out, &kb)| {
        let Some(ty) = fg.value_kind(out).as_value() else {
            return false;
        };
        let Some(mask) = super::u64_type_mask(ty) else {
            return false;
        };
        kb.all_known(mask) && kb.ones == 4
    });
    assert!(any_known_four, "analyze must record the And(7,4) output as known = 4");
    // Sanity: the function still has a return value (we didn't break the graph).
    let _ = return_val;
    Ok(())
}

/// `And(x, 0)` is known-zero regardless of `x` — the map-iteration rewrite
/// must fold the And output to `IntConst(0)`.  Exercises the new flat
/// "iterate the finished map" rewrite path on a non-constant operand.
#[test]
fn known_bits_and_with_zero_folds_via_map() -> Result<()> {
    let mut fg = make_fn_with_var(|b, var| {
        let x = b.read_variable(&var)?;
        let zero = b.build_int_const(0u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::And, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    let val = return_value(&fg)?;
    assert_eq!(
        fg.int_const_val(val),
        Some(0),
        "And(x, 0) is known-zero and must fold to IntConst(0) via the map rewrite",
    );
    Ok(())
}

/// A fully-known I1 (boolean) output must fold uniformly with wider ints:
/// `Xor(c, c)` for two equal I1 constants is known-0, and the map-iteration
/// rewrite must emit `IntConst(0):I1`.  Pins that the `ty`/mask handling
/// covers `bit_width(I1) == 1`.
#[test]
fn known_bits_i1_folds_via_map() -> Result<()> {
    let mut fg = make_fn(|b| {
        // `Or(0, 1) : I1` is fully known to 1; the rewrite must fold it.
        let zero = b.build_int_const(0u64, ValueType::I1).unwrap();
        let one = b.build_int_const(1u64, ValueType::I1).unwrap();
        b.build_int_binary_operation(zero, one, IntBinaryOp::Or, ValueType::I1)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(1),
        "fully-known I1 output must fold to IntConst(1):I1 via the map rewrite",
    );
    Ok(())
}

/// A fully-known output reachable via two consumers must fold exactly once.
/// The old rewrite walked the graph and re-derived facts per node, which
/// could revisit a shared output; the map holds one entry per output, so the
/// flat map-iteration rewrite visits each output once regardless of how many
/// consumers reach it.  `(c | 8)` is fed to two separate ANDs; the shared
/// `Or` output folds, and so do both ANDs, with no double-processing.
#[test]
fn known_bits_shared_output_folds_once() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0u64, ValueType::I8).unwrap();
        let eight = b.build_int_const(8u64, ValueType::I8).unwrap();
        // `Or(0, 8)` is fully known = 8; it is a non-IntConst node whose
        // output is consumed twice below.
        let shared = b.build_int_binary_operation(c, eight, IntBinaryOp::Or, ValueType::I8)?;
        let m8 = b.build_int_const(8u64, ValueType::I8).unwrap();
        let m4 = b.build_int_const(4u64, ValueType::I8).unwrap();
        let a = b.build_int_binary_operation(shared, m8, IntBinaryOp::And, ValueType::I8)?;
        let d = b.build_int_binary_operation(shared, m4, IntBinaryOp::And, ValueType::I8)?;
        // `(shared & 8)` = 8, `(shared & 4)` = 0 → XOR = 8.
        b.build_int_binary_operation(a, d, IntBinaryOp::Xor, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    let val = return_value(&fg)?;
    assert_eq!(
        fg.int_const_val(val),
        Some(8),
        "shared fully-known output must fold cleanly with no double-processing",
    );
    Ok(())
}

// ── shifts must propagate the lhs's known bits ───────────────────────────────

/// Variant of `make_fn` that tracks a single 1-byte variable so the closure
/// can read it via `read_variable` to obtain a non-constant `InitialVar`
/// output — used by tests that want to model an unknown source value (such
/// as a freshly-entered architectural register).
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

/// `ShiftRight(x | 2, 1) & 1` for I8 must fold to `IntConst(1)`: the literal
/// `2` has bit 1 known-1, OR with anything keeps bit 1 set, the shift moves
/// it to bit 0, and the final mask keeps only bit 0 — so the answer is `1`
/// regardless of `x`.  The previous KnownBits implementation cleared the
/// lhs bits entirely on shift, losing the propagated known-1.
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
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    let val = return_value(&fg)?;
    assert_eq!(fg.int_const_val(val), Some(1));
    Ok(())
}

/// `ShiftLeft(x | 1, 7) & 0x80` for I8 must fold to `IntConst(0x80)`: bit 0
/// of `x|1` is known 1; shifting by 7 moves it to bit 7 (known 1) while bits
/// 0-6 become known 0.  ANDing with `0x80` keeps only bit 7 — so the result
/// is `0x80` regardless of `x`.
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
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    let val = return_value(&fg)?;
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
// result.  E.g. `IntConst(0xFF, I8) << 8` should be 0 (Sleigh) but
// the pre-fix arm computed `0xFF << (8 & 7) = 0xFF << 0 = 0xFF`.
//
// The visible bug: `(value << bit_width) & 1` should fold to 0 (because
// any value shifted by bit_width is 0), but the masked-shift form
// folds it as `(value << 0) & 1` and leaves it unresolved.

/// `IntConst(1, I8) << IntConst(8, I8)` is 0 per Sleigh — KnownBits
/// must fold the chain to a constant 0, not 1.
#[test]
fn known_bits_shl_at_bit_width_folds_to_zero_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, ValueType::I8).unwrap();
        let eight = b.build_int_const(8u64, ValueType::I8).unwrap();
        b.build_int_binary_operation(one, eight, IntBinaryOp::ShiftLeft, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    let val = return_value(&fg)?;
    assert_eq!(
        fg.int_const_val(val),
        Some(0),
        "Sleigh: 1u8 << 8 = 0 (shift >= bit_width returns 0).  Pre-fix \
         KnownBits computed `1u8 << (8 & 7) = 1` and left the value \
         unresolved or folded to 1."
    );
    Ok(())
}

/// `IntConst(0xFF, I32) >> IntConst(32, I32)` is 0 per Sleigh — KnownBits
/// must report all bits known zero.
#[test]
fn known_bits_shr_at_bit_width_folds_to_zero_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0xFFu64, ValueType::I32).unwrap();
        let thirty_two = b.build_int_const(32u64, ValueType::I32).unwrap();
        b.build_int_binary_operation(v, thirty_two, IntBinaryOp::ShiftRight, ValueType::I32)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    let val = return_value(&fg)?;
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
        let one = b.build_int_const(1u64, ValueType::I8).unwrap();
        let two = b.build_int_const(2u64, ValueType::I8).unwrap();
        let masked = b.build_int_binary_operation(cr0, one, IntBinaryOp::And, ValueType::I8)?;
        let ored = b.build_int_binary_operation(two, masked, IntBinaryOp::Or, ValueType::I8)?;
        let shifted =
            b.build_int_binary_operation(ored, one, IntBinaryOp::ShiftRight, ValueType::I8)?;
        b.build_int_binary_operation(shifted, one, IntBinaryOp::And, ValueType::I8)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    let val = return_value(&fg)?;
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
// KnownBits's rewrite pass first folds the Or to IntConst; the surrounding
// `while changed` loop then re-runs `analyze`, and only then does the
// SignExtend node's arm fire (or fail to fire, before the fix).

/// `SignExtend((0u8 | 0x7Fu8) : I8 → I64)` — MSB of the inner Or is known 0,
/// so the upper 56 bits of the SignExtend result must be zero.  Without the
/// SignExtend arm in `node_known_bits`, the SignExtend stays as a node;
/// with it, the entire chain folds to `IntConst(0x7F)`.
#[test]
fn known_bits_sign_extend_msb_zero_folds_to_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let zero = b.build_int_const(0u64, ValueType::I8).unwrap();
        let c = b.build_int_const(0x7Fu64, ValueType::I8).unwrap();
        let or_ = b.build_int_binary_operation(zero, c, IntBinaryOp::Or, ValueType::I8)?;
        b.extend_if_needed(or_, ValueType::I64, ExtendOp::SignExtend)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0x7Fu128),
        "SignExtend of (0|0x7F) (MSB=0) must fold to IntConst(0x7F) once \
         the SignExtend arm propagates known bits"
    );
    Ok(())
}

/// `SignExtend((0u8 | 0x80u8) : I8 → I64)` — MSB of the inner Or is known 1,
/// so the upper 56 bits of the SignExtend result must be one.  Result must
/// fold to `IntConst(0xFFFF_FFFF_FFFF_FF80)`.
#[test]
fn known_bits_sign_extend_msb_one_folds_to_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let zero = b.build_int_const(0u64, ValueType::I8).unwrap();
        let c = b.build_int_const(0x80u64, ValueType::I8).unwrap();
        let or_ = b.build_int_binary_operation(zero, c, IntBinaryOp::Or, ValueType::I8)?;
        b.extend_if_needed(or_, ValueType::I64, ExtendOp::SignExtend)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg, &crate::opt::OptCtx::empty())?.changed();
    }
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0xFFFF_FFFF_FFFF_FF80u128),
        "SignExtend of (0|0x80) (MSB=1) must fold to all-ones upper bits \
         once the SignExtend arm propagates known bits"
    );
    Ok(())
}

// ── KnownBitsFacts constructor invariant ────────────────────────────────────────────────

#[test]
fn kb_default_is_all_unknown() {
    let kb = super::KnownBitsFacts::default();
    assert_eq!(kb.ones, 0);
    assert_eq!(kb.zeros, 0);
}

#[test]
fn kb_struct_literal_disjoint_ones_zeros() {
    let kb = super::KnownBitsFacts { ones: 0b01, zeros: 0b10 };
    assert_eq!(kb.ones, 0b01);
    assert_eq!(kb.zeros, 0b10);
}

/// Pin the invariant that `KnownBitsMap` returns `KnownBitsFacts::default()` =
/// "fully unknown" (both `ones` and `zeros` zero) for an untracked
/// `ValueId`.  The `Truncate` arm of `node_known_bits` reads
/// `known[input]` directly and propagates the result through
/// `& type_mask`; if `KnownBitsFacts::default()` ever drifted to "all ones" or
/// "all zeros" the Truncate would synthesise spurious known bits on
/// any input whose KB analysis returned `None` (e.g. I80 / I128 /
/// I256 chains where `u64_type_mask` gates out).
#[test]
fn kb_default_is_fully_unknown_not_all_zero_or_all_one() {
    let kb = super::KnownBitsFacts::default();
    assert_eq!(kb.ones, 0, "KnownBitsFacts::default().ones must be 0 (no bit known to be 1)");
    assert_eq!(kb.zeros, 0, "KnownBitsFacts::default().zeros must be 0 (no bit known to be 0)");
    // Propagation invariant: AND-ing default with any mask produces
    // default (i.e. propagating an unknown through Truncate keeps it
    // unknown).
    let masked = super::KnownBitsFacts {
        ones: kb.ones & 0xFFu64,
        zeros: kb.zeros & 0xFFu64,
    };
    assert_eq!(masked.ones, 0);
    assert_eq!(masked.zeros, 0);
}
