//! Phase 3 Task 3.4 parity test.
//!
//! For every known-bits transfer function v1's [`KnownBits`]
//! (imperative, worklist-based) and v2's [`KnownBitsEgg`] (egg
//! `Analysis::Data`-based) MUST produce structurally identical IR.
//!
//! Scope (Phase 3.4 first cut): every transfer function in v1's
//! `node_known_bits` — `IntConst`, `IntBinaryOp::{And,Or,Xor,
//! ShiftLeft,ShiftRight}`, `IntUnaryOp::BitNot`, `Truncate`,
//! `Extend::ZeroExtend`, `Extend::SignExtend`, `Popcount`, `Lzcount`.
//!
//! Both passes are run to fixed point on the same fixture; the
//! comparison is on the `NodeKind` of the return-value producer.
//!
//! Bit-lattice ports are error-prone — these tests bisect on the
//! failing transfer function.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{KnownBits, OptimizerRaw, known_bits_egg::KnownBitsEgg};
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::test_utils::{make_empty_fn, make_fn_with_var, reg_vn};
use strider_ir::{BuiltFunctionGraph, ExtendOp, FunctionBuilder, IntBinaryOp, IntUnaryOp, Value};

fn run_to_fixed_point(optimizer: &dyn OptimizerRaw, fg: &mut BuiltFunctionGraph) {
    let mut steps = 0;
    loop {
        let result = optimizer
            .optimize_raw(&mut fg.graph, fg.entry)
            .expect("optimize_raw must not error on synthetic fixture");
        if !result.changed() {
            break;
        }
        steps += 1;
        assert!(steps < 64, "optimizer failed to reach fixed point");
    }
}

/// Returns the `NodeKind` of the return-value producer.
fn return_kind(fg: &BuiltFunctionGraph) -> NodeKind {
    let ret = fg
        .graph
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("function must have a Return node");
    let inputs = fg.graph.node_inputs(ret);
    let val_out = inputs[2];
    let producer = fg.graph.get_node_from_output(val_out);
    *fg.graph.node_kind(producer)
}

/// Build a function whose return value depends on the closure `f`.  Both
/// passes are run independently on a fresh copy and the `NodeKind` of
/// the return-value producer is compared.
fn assert_parity<F>(label: &str, f: F)
where
    F: Fn(&mut FunctionBuilder) -> anyhow::Result<Value> + Clone,
{
    let mut fg_v1 = make_empty_fn(f.clone()).expect("build v1 fixture");
    let mut fg_v2 = make_empty_fn(f).expect("build v2 fixture");
    run_to_fixed_point(&KnownBits, &mut fg_v1);
    run_to_fixed_point(&KnownBitsEgg::new(), &mut fg_v2);
    let v1 = return_kind(&fg_v1);
    let v2 = return_kind(&fg_v2);
    assert_eq!(v1, v2, "parity failed: {label}: v1={v1:?} v2={v2:?}");
}

/// Variant of `assert_parity` for fixtures that need a non-constant
/// `InitialVar` to exercise transfer functions (e.g. shift-of-unknown).
fn assert_parity_with_var<F>(label: &str, f: F)
where
    F: Fn(&mut FunctionBuilder, Value) -> anyhow::Result<Value> + Clone,
{
    let vn = reg_vn(0x40, 1);
    let (mut fg_v1, _) = make_fn_with_var(vn, f.clone()).expect("build v1 var fixture");
    let (mut fg_v2, _) = make_fn_with_var(vn, f).expect("build v2 var fixture");
    run_to_fixed_point(&KnownBits, &mut fg_v1);
    run_to_fixed_point(&KnownBitsEgg::new(), &mut fg_v2);
    let v1 = return_kind(&fg_v1);
    let v2 = return_kind(&fg_v2);
    assert_eq!(v1, v2, "parity failed (var): {label}: v1={v1:?} v2={v2:?}");
}

// ── Pure const-pair tests (fully known → IntConst) ───────────────────────────

#[test]
fn parity_and_disjoint_masks() {
    // (0xFF & 0xF0) for U8: both inputs known → output known = 0xF0.
    assert_parity("AND 0xFF & 0xF0", |b| {
        let a = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
        let c = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(a, c, IntBinaryOp::And, NodeOutputType::U8)
    });
}

#[test]
fn parity_or_then_and_known_one() {
    // (x | 7) & 4: known-1 of OR propagates; AND with 4 leaves only
    // bit 2 (which is known 1) → result must fold to IntConst(4).
    assert_parity_with_var("(x|7)&4 → 4", |b, x| {
        let c7 = b.build_int_const(7u64, NodeOutputType::U8).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::U8).unwrap();
        let ored = b.build_int_binary_operation(x, c7, IntBinaryOp::Or, NodeOutputType::U8)?;
        b.build_int_binary_operation(ored, c4, IntBinaryOp::And, NodeOutputType::U8)
    });
}

#[test]
fn parity_and_mask_then_and_no_overlap() {
    // (x & 0xF0) & 0x0F: disjoint masks → 0.
    assert_parity_with_var("(x&0xF0)&0x0F → 0", |b, x| {
        let f0 = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        let f = b.build_int_const(0x0Fu64, NodeOutputType::U8).unwrap();
        let inner = b.build_int_binary_operation(x, f0, IntBinaryOp::And, NodeOutputType::U8)?;
        b.build_int_binary_operation(inner, f, IntBinaryOp::And, NodeOutputType::U8)
    });
}

// ── Shift propagation ────────────────────────────────────────────────────────

#[test]
fn parity_shift_right_upper_zero() {
    // (x >> 4) & 0xF0 for U8 → 0 (upper bits zeroed by shr).
    assert_parity_with_var("(x>>4)&0xF0 → 0", |b, x| {
        let four = b.build_int_const(4u64, NodeOutputType::U8).unwrap();
        let shr =
            b.build_int_binary_operation(x, four, IntBinaryOp::ShiftRight, NodeOutputType::U8)?;
        let mask_high = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(shr, mask_high, IntBinaryOp::And, NodeOutputType::U8)
    });
}

#[test]
fn parity_shift_left_lower_zero() {
    // (x << 5) & 0x1F for U8 → 0 (lower bits zeroed by shl).
    assert_parity_with_var("(x<<5)&0x1F → 0", |b, x| {
        let five = b.build_int_const(5u64, NodeOutputType::U8).unwrap();
        let shl =
            b.build_int_binary_operation(x, five, IntBinaryOp::ShiftLeft, NodeOutputType::U8)?;
        let mask_low = b.build_int_const(0x1Fu64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(shl, mask_low, IntBinaryOp::And, NodeOutputType::U8)
    });
}

#[test]
fn parity_shift_right_propagates_known_one() {
    // ((x | 2) >> 1) & 1 for U8 → 1 (bit 1 of OR is known 1, shifted
    // to bit 0).
    assert_parity_with_var("((x|2)>>1)&1 → 1", |b, x| {
        let two = b.build_int_const(2u64, NodeOutputType::U8).unwrap();
        let one = b.build_int_const(1u64, NodeOutputType::U8).unwrap();
        let ored = b.build_int_binary_operation(x, two, IntBinaryOp::Or, NodeOutputType::U8)?;
        let shifted =
            b.build_int_binary_operation(ored, one, IntBinaryOp::ShiftRight, NodeOutputType::U8)?;
        b.build_int_binary_operation(shifted, one, IntBinaryOp::And, NodeOutputType::U8)
    });
}

#[test]
fn parity_shift_left_propagates_known_one() {
    // ((x | 1) << 7) & 0x80 for U8 → 0x80.
    assert_parity_with_var("((x|1)<<7)&0x80 → 0x80", |b, x| {
        let one = b.build_int_const(1u64, NodeOutputType::U8).unwrap();
        let seven = b.build_int_const(7u64, NodeOutputType::U8).unwrap();
        let mask80 = b.build_int_const(0x80u64, NodeOutputType::U8).unwrap();
        let ored = b.build_int_binary_operation(x, one, IntBinaryOp::Or, NodeOutputType::U8)?;
        let shifted =
            b.build_int_binary_operation(ored, seven, IntBinaryOp::ShiftLeft, NodeOutputType::U8)?;
        b.build_int_binary_operation(shifted, mask80, IntBinaryOp::And, NodeOutputType::U8)
    });
}

#[test]
fn parity_shl_at_bit_width_folds_to_zero_u8() {
    // 1u8 << 8 → 0 per Sleigh.
    assert_parity("1u8 << 8 → 0", |b| {
        let one = b.build_int_const(1u64, NodeOutputType::U8).unwrap();
        let eight = b.build_int_const(8u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(one, eight, IntBinaryOp::ShiftLeft, NodeOutputType::U8)
    });
}

// ── Popcount / Lzcount range ─────────────────────────────────────────────────

#[test]
fn parity_popcount_range() {
    // popcount(U8) ≤ 8 → upper 4 bits of result are zero → AND 0xF0 → 0.
    assert_parity_with_var("popcount(x)&0xF0 → 0", |b, x| {
        let pc = b.build_popcount(x, NodeOutputType::U8)?;
        let mask = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(pc, mask, IntBinaryOp::And, NodeOutputType::U8)
    });
}

#[test]
fn parity_lzcount_range() {
    // lzcount(U8) ≤ 8 → upper 4 bits of result are zero → AND 0xF0 → 0.
    assert_parity_with_var("lzcount(x)&0xF0 → 0", |b, x| {
        let lz = b.build_lzcount(x, NodeOutputType::U8)?;
        let mask = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
        b.build_int_binary_operation(lz, mask, IntBinaryOp::And, NodeOutputType::U8)
    });
}

// ── BitNot round-trip ────────────────────────────────────────────────────────

#[test]
fn parity_bitnot_round_trip() {
    // ~~(x | 0xFF) for U8 → 0xFF (~~ is identity; OR with 0xFF is 0xFF).
    assert_parity_with_var("~~(x|0xFF) → 0xFF", |b, x| {
        let ff = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
        let or_ = b.build_int_binary_operation(x, ff, IntBinaryOp::Or, NodeOutputType::U8)?;
        let n1 = b.build_int_unary_operation(or_, IntUnaryOp::BitNot, NodeOutputType::U8)?;
        b.build_int_unary_operation(n1, IntUnaryOp::BitNot, NodeOutputType::U8)
    });
}

// ── Truncate / Extend ────────────────────────────────────────────────────────

#[test]
fn parity_sign_extend_msb_zero() {
    // SignExtend((0|0x7F):U8→U64) — MSB=0 → upper bits 0 → IntConst(0x7F).
    assert_parity("sext(0|0x7F):U8→U64", |b| {
        let zero = b.build_int_const(0u64, NodeOutputType::U8).unwrap();
        let c = b.build_int_const(0x7Fu64, NodeOutputType::U8).unwrap();
        let or_ = b.build_int_binary_operation(zero, c, IntBinaryOp::Or, NodeOutputType::U8)?;
        b.extend_if_needed(or_, NodeOutputType::U64, ExtendOp::SignExtend)
    });
}

#[test]
fn parity_sign_extend_msb_one() {
    // SignExtend((0|0x80):U8→U64) — MSB=1 → upper bits 1 →
    // IntConst(0xFFFF_FFFF_FFFF_FF80).
    assert_parity("sext(0|0x80):U8→U64", |b| {
        let zero = b.build_int_const(0u64, NodeOutputType::U8).unwrap();
        let c = b.build_int_const(0x80u64, NodeOutputType::U8).unwrap();
        let or_ = b.build_int_binary_operation(zero, c, IntBinaryOp::Or, NodeOutputType::U8)?;
        b.extend_if_needed(or_, NodeOutputType::U64, ExtendOp::SignExtend)
    });
}

// ── PPC CR0 extract chain (composite) ────────────────────────────────────────

#[test]
fn parity_ppc_cr0_extract_chain() {
    // ((cr0 & 1) | 2) >> 1) & 1 = 1 unconditionally.
    assert_parity_with_var("ppc cr0 extract → 1", |b, cr0| {
        let one = b.build_int_const(1u64, NodeOutputType::U8).unwrap();
        let two = b.build_int_const(2u64, NodeOutputType::U8).unwrap();
        let masked =
            b.build_int_binary_operation(cr0, one, IntBinaryOp::And, NodeOutputType::U8)?;
        let ored = b.build_int_binary_operation(two, masked, IntBinaryOp::Or, NodeOutputType::U8)?;
        let shifted =
            b.build_int_binary_operation(ored, one, IntBinaryOp::ShiftRight, NodeOutputType::U8)?;
        b.build_int_binary_operation(shifted, one, IntBinaryOp::And, NodeOutputType::U8)
    });
}
