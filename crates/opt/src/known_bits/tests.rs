use super::*;
use crate::error::ErrorKind;
use ir::node::{NodeKind, NodeOutputType};
use ir::{FunctionBuilder, IntBinaryOp};

fn make_fn<F>(f: F) -> Result<ir::BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<ir::Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let val = f(&mut b)?;
    b.build_return(Some(val), &[])?;
    Ok(b.build()?)
}

fn return_kind(fg: &ir::BuiltFunctionGraph) -> Result<NodeKind> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or(ErrorKind::NoReturnNode)?;
    let val = fg.graph.node_inputs(ret)[2];
    Ok(*fg.graph.node_kind(fg.graph.get_node_from_output(val)))
}

// ── Original tests ────────────────────────────────────────────────────────────

/// `(x | 7) & 4` — bits 0-2 of `Or` are known 1; after And with 4 every
/// bit is determined → should fold to `IntConst(4)`.
#[test]
fn known_bits_or_then_and() -> Result<()> {
    let mut fg2 = make_fn(|b| {
        let x_seed = b.build_int_const(0u64, NodeOutputType::U64);
        let c7 = b.build_int_const(7u64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let ored = b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(ored, c4, IntBinaryOp::And, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg2)?.changed();
    }
    assert_eq!(return_kind(&fg2)?, NodeKind::IntConst(4));
    Ok(())
}

/// `(x & 0xF0) & 0x0F` — the two masks have no overlap, so the result is 0.
#[test]
fn known_bits_and_mask_then_and() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::U8);
        let f0 = b.build_int_const(0xF0u64, NodeOutputType::U8);
        let f = b.build_int_const(0x0Fu64, NodeOutputType::U8);
        let inner = b.build_int_binary_operation(x, f0, IntBinaryOp::And, NodeOutputType::U8)?;
        Ok(b.build_int_binary_operation(inner, f, IntBinaryOp::And, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// A plain `IntConst` already has all bits known — the optimizer must not
/// loop or report spurious changes.
#[test]
fn known_bits_const_no_change() -> Result<()> {
    let mut fg = make_fn(|b| Ok(b.build_int_const(42u64, NodeOutputType::U64)))?;
    assert!(!KnownBits.optimize(&mut fg)?.changed());
    Ok(())
}

/// `popcount(U8)` fits in 4 bits (max = 8), so bits 4..7 are known zero.
/// `and(popcount(x), 0xF0)` should fold to 0.
#[test]
fn known_bits_popcount_range() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::U8);
        let pc = b.build_popcount(x, NodeOutputType::U8)?;
        let mask = b.build_int_const(0xF0u64, NodeOutputType::U8);
        Ok(b.build_int_binary_operation(pc, mask, IntBinaryOp::And, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

// ── Comprehensive tests ───────────────────────────────────────────────────────

/// After `x >> 4` for U8, the upper 4 bits are statically zero. ANDing
/// with `0xF0` (which targets only the upper bits) must fold to 0 — KnownBits
/// proves the result has no set bits.
#[test]
fn known_bits_shift_right_upper_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x55u64, NodeOutputType::U8); // any value
        let four = b.build_int_const(4u64, NodeOutputType::U8);
        let shr = b.build_int_binary_operation(x, four, IntBinaryOp::ShiftRight, NodeOutputType::U8)?;
        let mask_high = b.build_int_const(0xF0u64, NodeOutputType::U8);
        Ok(b.build_int_binary_operation(shr, mask_high, IntBinaryOp::And, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// After `x << 5` for U8, the lower 5 bits are statically zero. ANDing with
/// `0x1F` (lower 5 bits) must fold to 0.
#[test]
fn known_bits_shift_left_lower_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::U8);
        let five = b.build_int_const(5u64, NodeOutputType::U8);
        let shl = b.build_int_binary_operation(x, five, IntBinaryOp::ShiftLeft, NodeOutputType::U8)?;
        let mask_low = b.build_int_const(0x1Fu64, NodeOutputType::U8);
        Ok(b.build_int_binary_operation(shl, mask_low, IntBinaryOp::And, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// A long OR chain of single-bit constants then ANDed with 0xFF — the worklist
/// must propagate the OR's known-1 bits through the chain.
#[test]
fn known_bits_long_or_and_chain() -> Result<()> {
    let mut fg = make_fn(|b| {
        let mut acc = b.build_int_const(0u64, NodeOutputType::U64);
        for i in 0..8u64 {
            let bit = b.build_int_const(1u64 << i, NodeOutputType::U64);
            acc = b.build_int_binary_operation(acc, bit, IntBinaryOp::Or, NodeOutputType::U64)?;
        }
        let mask = b.build_int_const(0xFFu64, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(acc, mask, IntBinaryOp::And, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFF));
    Ok(())
}

/// `lzcount(U8)` fits in 4 bits (max value 8). `and(lzcount(x), 0xF0)` must
/// fold to 0 — the upper 4 bits of an lzcount(U8) result are statically known
/// to be zero.
#[test]
fn known_bits_lzcount_range() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x01u64, NodeOutputType::U8);
        let lz = b.build_lzcount(x, NodeOutputType::U8)?;
        let mask = b.build_int_const(0xF0u64, NodeOutputType::U8);
        Ok(b.build_int_binary_operation(lz, mask, IntBinaryOp::And, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// XOR of identical-bits inputs: bit known if both agree → must be known 0.
/// `(x | 0xFF) ^ (x | 0xFF)` for U8 — KnownBits should prove this is 0.
#[test]
fn known_bits_xor_identical_or_known_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x55u64, NodeOutputType::U8);
        let ff = b.build_int_const(0xFFu64, NodeOutputType::U8);
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, NodeOutputType::U8)?;
        // (x|0xFF) is statically all-ones; xoring with itself folds to 0.
        // (Note: this also exercises ConstantFold's `x ^ x → 0` identity, but
        // KnownBits-only would prove the result by the both-known-1 case.)
        Ok(b.build_int_binary_operation(or_, or_, IntBinaryOp::Xor, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// NOT swaps known-ones and known-zeros. `(x | 0xFF) NOT NOT` for U8 returns
/// 0xFF — testing that NOT propagation is correct round-trip.
#[test]
fn known_bits_not_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xAAu64, NodeOutputType::U8);
        let ff = b.build_int_const(0xFFu64, NodeOutputType::U8);
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, NodeOutputType::U8)?;
        // !!(x|0xFF) — NOT NOT = identity at the bit level.
        let n1 = b.build_int_unary_operation(or_, ir::IntUnaryOp::Not, NodeOutputType::U8)?;
        Ok(b.build_int_unary_operation(n1, ir::IntUnaryOp::Not, NodeOutputType::U8)?)
    })?;
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFF));
    Ok(())
}

/// `truncate(0xABCD U16) → U8` — the truncate preserves lower bits, so the
/// result has all bits known to 0xCD. KnownBits must propagate through
/// Truncate.
#[test]
fn known_bits_truncate_preserves_low_bits() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0xABCDu64, NodeOutputType::U16);
        Ok(b.truncate_if_needed(v, NodeOutputType::U8)?)
    })?;
    // The builder likely already folded this at construction; just verify
    // the final state matches.
    let mut changed = true;
    while changed {
        changed = KnownBits.optimize(&mut fg)?.changed();
    }
    let val = return_value(&fg)?;
    let semantic = fg.int_const_val(val);
    assert_eq!(semantic, Some(0xCD), "truncate must preserve low byte");
    Ok(())
}

fn return_value(fg: &ir::BuiltFunctionGraph) -> Result<ir::Value> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or(ErrorKind::NoReturnNode)?;
    Ok(fg.graph.node_inputs(ret)[2])
}

#[test]
fn merge_preserves_invariant_under_conflict() {
    // Bit 0 is ones in `a`, zeros in `b`. After merging both into `c`,
    // ones & zeros must be 0 — `ones` wins on conflict.
    let mut c = super::Kb::default();
    let a = super::Kb { ones: 0b1, zeros: 0 };
    let b = super::Kb { ones: 0, zeros: 0b1 };
    c.merge(a);
    c.merge(b);
    assert_eq!(
        c.ones & c.zeros,
        0,
        "ones & zeros must be 0; got ones={:#b} zeros={:#b}",
        c.ones,
        c.zeros
    );
}
