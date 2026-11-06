use super::*;
use crate::Result;
use crate::test_support::{
    assert_near_linear, assert_returns_const, make_fn, make_fn_with_var, return_kind, return_value,
    run_pass, run_to_fixed_point, steps,
};
use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{ExtendOp, FunctionBuilder, IntBinaryOp, UBig};
use strider_ir_test_utils::{IrWalkerEx, RegisterSet, reg_vn};

/// A chain whose every bit known bits pin folds to its value. `x` is an
/// opaque `I8` register, which the constant-only rows leave unread.
#[test]
fn known_bits_chains_fold_to_their_value() -> Result<()> {
    type Chain = fn(&mut FunctionBuilder, ValueId) -> Result<ValueId>;
    use IntBinaryOp::{And, Or, ShiftLeft, ShiftRight, Xor};
    #[rustfmt::skip]
    let cases: [(&str, Chain, u128); 20] = [
        // `(0xFF & 0xF0) & 0x0F`: disjoint masks.
        ("and_mask_then_and", |b, _| {
            let x = b.build_int_const(0xFFu64, ValueType::int(8))?;
            let inner = b.build_const_binop(0xF0u64, x, And, ValueType::int(8))?;
            b.build_const_binop(0x0Fu64, inner, And, ValueType::int(8))
        }, 0),
        // `popcount(I8)` maxes at 8, so bits 4..7 are known zero.
        ("popcount_range", |b, _| {
            let x = b.build_int_const(0xFFu64, ValueType::int(8))?;
            let pc = b.build_popcount(x, ValueType::int(8))?;
            b.build_const_binop(0xF0u64, pc, And, ValueType::int(8))
        }, 0),
        // `lzcount(I8)` maxes at 8 likewise.
        ("lzcount_range", |b, _| {
            let x = b.build_int_const(0x01u64, ValueType::int(8))?;
            let lz = b.build_lzcount(x, ValueType::int(8))?;
            b.build_const_binop(0xF0u64, lz, And, ValueType::int(8))
        }, 0),
        // `>> 4` leaves the upper 4 bits zero.
        ("shift_right_upper_zero", |b, _| {
            let x = b.build_int_const(0x55u64, ValueType::int(8))?;
            let shr = b.build_const_binop(4u64, x, ShiftRight, ValueType::int(8))?;
            b.build_const_binop(0xF0u64, shr, And, ValueType::int(8))
        }, 0),
        // `<< 5` leaves the lower 5 bits zero.
        ("shift_left_lower_zero", |b, _| {
            let x = b.build_int_const(0xFFu64, ValueType::int(8))?;
            let shl = b.build_const_binop(5u64, x, ShiftLeft, ValueType::int(8))?;
            b.build_const_binop(0x1Fu64, shl, And, ValueType::int(8))
        }, 0),
        // Per Sleigh a shift at the width is 0: masking the count instead
        // gives `1 << 0` and `0xFF >> 0`.
        ("shl_at_bit_width_u8", |b, _| {
            let one = b.build_int_const(1u64, ValueType::int(8))?;
            b.build_const_binop(8u64, one, ShiftLeft, ValueType::int(8))
        }, 0),
        ("shr_at_bit_width_u32", |b, _| {
            let v = b.build_int_const(0xFFu64, ValueType::int(32))?;
            b.build_const_binop(32u64, v, ShiftRight, ValueType::int(32))
        }, 0),
        // The worklist carries known-1 bits through a long Or chain.
        ("long_or_and_chain", |b, _| {
            let ty = ValueType::int(64);
            let mut acc = b.build_int_const(0u64, ty)?;
            for i in 0..8u64 {
                acc = b.build_const_binop(1u64 << i, acc, Or, ty)?;
            }
            b.build_const_binop(0xFFu64, acc, And, ty)
        }, 0xFF),
        // Operands with identical known bits Xor to known 0 everywhere.
        ("xor_identical", |b, _| {
            let x = b.build_int_const(0x55u64, ValueType::int(8))?;
            let or_ = b.build_const_binop(0xFFu64, x, Or, ValueType::int(8))?;
            b.build_int_binary_operation(or_, or_, Xor, ValueType::int(8))
        }, 0),
        // `~~x`: the Xor arm's ones/zeros swap, twice.
        ("complement_round_trip", |b, _| {
            let x = b.build_int_const(0xAAu64, ValueType::int(8))?;
            let or_ = b.build_const_binop(0xFFu64, x, Or, ValueType::int(8))?;
            let n1 = b.build_const_binop(u128::MAX, or_, Xor, ValueType::int(8))?;
            b.build_const_binop(u128::MAX, n1, Xor, ValueType::int(8))
        }, 0xFF),
        // The mask handling covers `bit_width(I1) == 1`.
        ("i1", |b, _| {
            let zero = b.build_int_const(0u64, ValueType::I1)?;
            b.build_const_binop(1u64, zero, Or, ValueType::I1)
        }, 1),
        // The lattice is 128 bits wide: bit 100 survives.
        ("i128_high_bit", |b, _| {
            let a = b.build_int_const(1u128 << 100, ValueType::int(128))?;
            b.build_const_binop(0u64, a, Or, ValueType::int(128))
        }, 1 << 100),
        // A fully known output read twice folds once: `8 & 8 ^ 8 & 4`.
        ("shared_output", |b, _| {
            let c = b.build_int_const(0u64, ValueType::int(8))?;
            let shared = b.build_const_binop(8u64, c, Or, ValueType::int(8))?;
            let a = b.build_const_binop(8u64, shared, And, ValueType::int(8))?;
            let d = b.build_const_binop(4u64, shared, And, ValueType::int(8))?;
            b.build_int_binary_operation(a, d, Xor, ValueType::int(8))
        }, 8),
        // `extend_if_needed` folds a constant itself, so these extend an Or
        // of constants: the sign bit known 0, then known 1.
        ("sign_extend_msb_zero", |b, _| {
            let zero = b.build_int_const(0u64, ValueType::int(8))?;
            let or_ = b.build_const_binop(0x7Fu64, zero, Or, ValueType::int(8))?;
            b.extend_if_needed(or_, ValueType::int(64), ExtendOp::SignExtend)
        }, 0x7F),
        ("sign_extend_msb_one", |b, _| {
            let zero = b.build_int_const(0u64, ValueType::int(8))?;
            let or_ = b.build_const_binop(0x80u64, zero, Or, ValueType::int(8))?;
            b.extend_if_needed(or_, ValueType::int(64), ExtendOp::SignExtend)
        }, 0xFFFF_FFFF_FFFF_FF80),
        // Known bits alone, over an opaque operand.
        ("and_zero", |b, x| b.build_const_binop(0u64, x, And, ValueType::int(8)), 0),
        // `((x | 2) >> 1) & 1`: the propagated known-1 survives the shift.
        ("shift_right_keeps_lhs_ones", |b, x| {
            let ored = b.build_const_binop(2u64, x, Or, ValueType::int(8))?;
            let shifted = b.build_const_binop(1u64, ored, ShiftRight, ValueType::int(8))?;
            b.build_const_binop(1u64, shifted, And, ValueType::int(8))
        }, 1),
        // `((x | 1) << 7) & 0x80`.
        ("shift_left_keeps_lhs_ones", |b, x| {
            let ored = b.build_const_binop(1u64, x, Or, ValueType::int(8))?;
            let shifted = b.build_const_binop(7u64, ored, ShiftLeft, ValueType::int(8))?;
            b.build_const_binop(0x80u64, shifted, And, ValueType::int(8))
        }, 0x80),
        // The PPC CR0-byte extraction `(((x & 1) | 2) >> 1) & 1`.
        ("ppc_cr0_extract", |b, x| {
            let masked = b.build_const_binop(1u64, x, And, ValueType::int(8))?;
            let two = b.build_int_const(2u64, ValueType::int(8))?;
            let ored = b.build_int_binary_operation(two, masked, Or, ValueType::int(8))?;
            let shifted = b.build_const_binop(1u64, ored, ShiftRight, ValueType::int(8))?;
            b.build_const_binop(1u64, shifted, And, ValueType::int(8))
        }, 1),
        // A ZeroExtend's upper bits are known zero whatever its input.
        ("zero_extend_upper_zero", |b, x| {
            let widened = b.extend_if_needed(x, ValueType::int(64), ExtendOp::ZeroExtend)?;
            b.build_const_binop(0xFFFF_FFFF_FFFF_FF00u64, widened, And, ValueType::int(64))
        }, 0),
    ];
    for (case, chain, want) in cases {
        let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), chain)?;
        run_to_fixed_point(&KnownBits, &mut fg)?;
        assert_eq!(fg.int_const_u128(fg.returned()), Some(want), "{case}");
    }
    Ok(())
}

/// Idempotence: a function whose only value is an `IntConst` reports no
/// change.
#[test]
fn known_bits_const_no_change() -> Result<()> {
    let mut fg = make_fn(|b| b.build_int_const(42u64, ValueType::int(64)))?;
    assert!(!run_pass(&KnownBits, &mut fg)?);
    Ok(())
}

/// Popcount range at the I64 edge with a genuinely opaque input: the I8 case
/// above has a constant input, so it can't distinguish exact-value from range
/// and never reaches the 7-bit boundary.
#[test]
fn known_bits_popcount_range_i64_opaque() -> Result<()> {
    let v = reg_vn(0x80, 8);
    let mut b = RegisterSet::new().tracked(v).build_fn_single_region()?;
    let x = b.read_variable(&v)?;
    let pc = b.build_popcount(x, ValueType::int(64))?;
    // Bits 7..63: everything above the 7 bits that can hold 0..=64.
    let mask = b.build_int_const(0xFFFF_FFFF_FFFF_FF80u64, ValueType::int(64))?;
    let and = b.build_int_binary_operation(pc, mask, IntBinaryOp::And, ValueType::int(64))?;
    b.build_return(Some(and), &[])?;
    let mut fg = b.build()?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_returns_const(&fg, 0);
    Ok(())
}

/// Facts are recomputed and overwritten at each visit, never unioned, so the
/// only fallible arm is malformed IR.
#[test]
fn analyze_returns_populated_map_no_merge_error() -> Result<()> {
    let fg = make_fn(|b| {
        let c = b.build_int_const(7u64, ValueType::int(64))?;
        let mask = b.build_int_const(4u64, ValueType::int(64))?;
        b.build_int_binary_operation(c, mask, IntBinaryOp::And, ValueType::int(64))
    })?;
    let known = super::analyze(&fg);
    // The operand `4` is itself known = 4, so the `And` is read by name.
    let kb = known[fg.returned()];
    assert_eq!(
        (kb.ones, kb.zeros),
        (4, u128::from(u64::MAX) ^ 4),
        "analyze must record the And(7,4) output as known = 4"
    );
    Ok(())
}

/// A fully-known LHS is not enough: an unknown shift amount must yield no
/// known bits, or ConstantFold could later collapse the shift to a bogus
/// constant.
#[test]
fn known_bits_shift_by_unknown_amount_does_not_fold() -> Result<()> {
    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, var_shift| {
        let known_lhs = b.build_int_const(0xFFu64, ValueType::int(8))?;
        b.build_int_binary_operation(
            known_lhs,
            var_shift,
            IntBinaryOp::ShiftLeft,
            ValueType::int(8),
        )
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

/// The SignExtend counterpart of the ZeroExtend case: an unknown sign bit
/// gives no upper-bit facts, so nothing folds.
#[test]
fn known_bits_sign_extend_unknown_msb_does_not_fold() -> Result<()> {
    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        let widened = b.extend_if_needed(x, ValueType::int(64), ExtendOp::SignExtend)?;
        let high_mask = b.build_int_const(0xFFFF_FFFF_FFFF_FF00u64, ValueType::int(64))?;
        b.build_int_binary_operation(widened, high_mask, IntBinaryOp::And, ValueType::int(64))
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_eq!(
        return_kind(fg.graph())?,
        NodeKind::IntBinaryOp(IntBinaryOp::And),
        "SignExtend of an unknown-sign value gives no upper-bit facts, so no fold",
    );
    Ok(())
}

/// `SShiftRight` has no transfer, so a chain that is mathematically 0
/// (`& 0x7F`, `s>> 4`, `& 0xF8`) still survives unfolded.
#[test]
fn known_bits_sshift_right_of_known_sign_zero_is_opaque() -> Result<()> {
    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        let low_mask = b.build_int_const(0x7Fu64, ValueType::int(8))?;
        let nonneg =
            b.build_int_binary_operation(x, low_mask, IntBinaryOp::And, ValueType::int(8))?;
        let four = b.build_int_const(4u64, ValueType::int(8))?;
        let shifted = b.build_int_binary_operation(
            nonneg,
            four,
            IntBinaryOp::SShiftRight,
            ValueType::int(8),
        )?;
        let high_mask = b.build_int_const(0xF8u64, ValueType::int(8))?;
        b.build_int_binary_operation(shifted, high_mask, IntBinaryOp::And, ValueType::int(8))
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    assert_eq!(
        return_kind(fg.graph())?,
        NodeKind::IntBinaryOp(IntBinaryOp::And),
        "SShiftRight is not modelled by KnownBits, so the chain must survive unfolded",
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
    const OPERAND_ADDR: u64 = 0xC0DE_0002;

    let mut fg = make_fn(|b| {
        let x_seed = b.build_int_const(0u64, ValueType::int(64))?;
        // Every other node carries the sentinel.
        b.set_lift_addr(Some(OPERAND_ADDR));
        let c7 = b.build_int_const(7u64, ValueType::int(64))?;
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let ored = b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, ValueType::int(64))?;
        let c4 = b.build_int_const(4u64, ValueType::int(64))?;
        b.build_int_binary_operation(ored, c4, IntBinaryOp::And, ValueType::int(64))
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    assert_returns_const(&fg, 4);
    let folded = fg.producer(fg.returned());
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
    const INNER_ADDR: u64 = 0xC0DE_0003;

    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        b.set_lift_addr(Some(INNER_ADDR));
        let one = b.build_int_const(1u64, ValueType::int(8))?;
        let x_and_1 = b.build_int_binary_operation(x, one, IntBinaryOp::And, ValueType::int(8))?;
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let two = b.build_int_const(2u64, ValueType::int(8))?;
        let ored =
            b.build_int_binary_operation(x_and_1, two, IntBinaryOp::Or, ValueType::int(8))?;
        let zero = b.build_int_const(0u64, ValueType::int(8))?;
        b.build_int_binary_operation(ored, zero, IntBinaryOp::And, ValueType::int(8))
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    assert_returns_const(&fg, 0);
    let folded = fg.producer(fg.returned());
    assert!(
        fg.side_tables()
            .asm_fingerprint(folded)
            .contains(&INNER_ADDR),
        "fold must absorb the full backward cone, including the non-folding \
         inner `x & 1` two levels down, whose addr the fixpoint can never \
         propagate; got {:?}",
        fg.side_tables().asm_fingerprint(folded)
    );
    Ok(())
}

/// The same cone with a fingerprint-EMPTY intermediate, the transient state a
/// pass leaves between minting a node and absorbing its match into it.  A
/// union LINKS `src`'s root under `dst` and is a no-op while `src` has no root,
/// so linking a cone node to an input that has not yet absorbed its own subtree
/// would sever that subtree for good.
#[test]
fn known_bits_fold_absorbs_cone_through_unstamped_intermediate() -> Result<()> {
    use strider_ir::node::ValueKind;
    use strider_ir::{IRBuilderExt, IRViewer};
    const INNER_ADDR: u64 = 0xC0DE_0004;

    // `(x & 1) & 4`, every node stamped, so the built function validates.
    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        b.set_lift_addr(Some(INNER_ADDR));
        let one = b.build_int_const(1u64, ValueType::int(8))?;
        let x_and_1 = b.build_int_binary_operation(x, one, IntBinaryOp::And, ValueType::int(8))?;
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let four = b.build_int_const(4u64, ValueType::int(8))?;
        b.build_int_binary_operation(x_and_1, four, IntBinaryOp::And, ValueType::int(8))
    })?;

    // Splice an unstamped `| 2` in between: `((x & 1) | 2) & 4`.
    {
        let mut edit = crate::EditFunction::new(&mut fg);
        edit.cull_dead();
        let root_value = return_value(edit.function().graph())?;
        let root = edit.producer(root_value);
        let [x_and_1, four] = edit.producer_inputs_exact::<2>(root_value)?;
        let two = edit.build_int_const(2u64, ValueType::int(8))?;
        let ored = edit.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Or),
            [x_and_1, two],
            [ValueKind::Typed(ValueType::int(8))],
        );
        assert!(
            edit.function().side_tables().asm_fingerprint_is_empty(ored),
            "the spliced intermediate must start with no fingerprint"
        );
        let [ored_value] = edit.node_outputs_exact::<1>(ored)?;
        let new_root = edit.create_node_attributed(
            NodeKind::IntBinaryOp(IntBinaryOp::And),
            [ored_value, four],
            [ValueKind::Typed(ValueType::int(8))],
            &[root],
        );
        let [new_root_value] = edit.node_outputs_exact::<1>(new_root)?;
        edit.replace_value(root_value, new_root_value);
    }

    run_to_fixed_point(&KnownBits, &mut fg)?;

    assert_returns_const(&fg, 0);
    let folded = fg.producer(fg.returned());
    assert!(
        fg.side_tables()
            .asm_fingerprint(folded)
            .contains(&INNER_ADDR),
        "an empty intermediate must not sever the subtree below it; got {:?}",
        fg.side_tables().asm_fingerprint(folded)
    );
    Ok(())
}

/// The other direction: `Load[addr] & 0` folds because of the `& 0`, so the
/// Load's address cone contributed nothing and the walk must stop at the Load
/// rather than tainting it.
#[test]
fn known_bits_fold_does_not_taint_opaque_load_address_cone() -> Result<()> {
    const ADDR_ADDR: u64 = 0xC0DE_0099;

    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, _| {
        // An unrestricted cone walk would descend Load -> address and absorb
        // this.
        b.set_lift_addr(Some(ADDR_ADDR));
        let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
        let zero = b.build_int_const(0u64, ValueType::int(64))?;
        b.build_int_binary_operation(loaded, zero, IntBinaryOp::And, ValueType::int(64))
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    assert_returns_const(&fg, 0);
    let folded = fg.producer(fg.returned());
    assert!(
        !fg.side_tables()
            .asm_fingerprint(folded)
            .contains(&ADDR_ADDR),
        "fold must NOT taint the opaque Load's address cone: the address \
         did not contribute to the known bits; got {:?}",
        fg.side_tables().asm_fingerprint(folded)
    );
    Ok(())
}

/// Two folds sharing an upstream cone must EACH absorb it.  The shared
/// `linked` set walks every edge once; correctness rests on the union LINKING
/// rather than copying.
///
/// A shared `(0 | 7)` with a distinct addr feeds `& 4` and `& 1`.  Both
/// results differ from each other and from 7, so neither folded constant can
/// pick up the addr by deduping with the shared node.
#[test]
fn known_bits_shared_cone_both_folds_absorb_fingerprint() -> Result<()> {
    const SHARED_ADDR: u64 = 0xC0DE_5EED;

    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        let x_seed = b.build_int_const(0u64, ValueType::int(8))?;
        b.set_lift_addr(Some(SHARED_ADDR));
        let c7 = b.build_int_const(7u64, ValueType::int(8))?;
        let shared =
            b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, ValueType::int(8))?;
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let c4 = b.build_int_const(4u64, ValueType::int(8))?;
        let fold_a =
            b.build_int_binary_operation(shared, c4, IntBinaryOp::And, ValueType::int(8))?;
        let c1 = b.build_int_const(1u64, ValueType::int(8))?;
        let fold_b =
            b.build_int_binary_operation(shared, c1, IntBinaryOp::And, ValueType::int(8))?;
        // Or each fold with the unknown `x` so both the `4` and the `1`
        // constants stay live after the pass.
        let live_a = b.build_int_binary_operation(fold_a, x, IntBinaryOp::Or, ValueType::int(8))?;
        let live_b = b.build_int_binary_operation(fold_b, x, IntBinaryOp::Or, ValueType::int(8))?;
        b.build_int_binary_operation(live_a, live_b, IntBinaryOp::Or, ValueType::int(8))
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    let top = fg.producer(fg.returned());
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
                "the `& 1` fold must ALSO absorb the shared cone's addr; a \
                 shared `seen` set loses it on the second fold; got {:?}",
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

/// An opaque `x` xored with `depth` distinct constants, topped by one `& 0`
/// that folds.  Exactly one fold, at the tip of a `depth`-deep propagating
/// spine, so the cone transfer is the only cost that can scale with `depth`.
fn cone_steps_for_tip_fold(depth: u64) -> Result<u64> {
    let v = reg_vn(0x80, 8);
    let mut b = RegisterSet::new().tracked(v).build_fn_single_region()?;
    let mut acc = b.read_variable(&v)?;
    for i in 0..depth {
        let c = b.build_int_const(i + 1, ValueType::int(64))?;
        acc = b.build_int_binary_operation(acc, c, IntBinaryOp::Xor, ValueType::int(64))?;
    }
    let zero = b.build_int_const(0u64, ValueType::int(64))?;
    let tip = b.build_int_binary_operation(acc, zero, IntBinaryOp::And, ValueType::int(64))?;
    b.build_return(Some(tip), &[])?;
    let mut fg = b.build()?;
    steps(&[&super::CONE_STEPS], || {
        assert!(run_pass(&KnownBits, &mut fg)?, "the `& 0` tip must fold");
        Ok(())
    })
}

/// `t0 = x & 0` then `t(i) = t(i-1) | C(i)`: every level folds, and each cone
/// contains all the cones below it.
fn cone_steps_for_nested_folds(depth: u64) -> Result<u64> {
    let v = reg_vn(0x40, 8);
    let mut b = RegisterSet::new().tracked(v).build_fn_single_region()?;
    let x = b.read_variable(&v)?;
    let zero = b.build_int_const(0u64, ValueType::int(64))?;
    let mut acc = b.build_int_binary_operation(x, zero, IntBinaryOp::And, ValueType::int(64))?;
    for i in 0..depth {
        let c = b.build_int_const(1u64 << (i % 60), ValueType::int(64))?;
        acc = b.build_int_binary_operation(acc, c, IntBinaryOp::Or, ValueType::int(64))?;
    }
    b.build_return(Some(acc), &[])?;
    let mut fg = b.build()?;
    steps(&[&super::CONE_STEPS], || {
        assert!(
            run_pass(&KnownBits, &mut fg)?,
            "every level is fully known, so the chain folds"
        );
        Ok(())
    })
}

/// [`cone_steps_for_nested_folds`] with the ids reversed: each level is
/// created before the one it reads, so a fold's consumer has the lower id.
fn cone_steps_for_reversed_nested_folds(depth: u64) -> Result<u64> {
    let v = reg_vn(0x40, 8);
    let mut b = RegisterSet::new().tracked(v).build_fn_single_region()?;
    let x = b.read_variable(&v)?;
    let mut levels = Vec::new();
    for i in 0..depth {
        let c = b.build_int_const(1u64 << (i % 60), ValueType::int(64))?;
        levels.push(b.build_int_binary_operation(x, c, IntBinaryOp::Or, ValueType::int(64))?);
    }
    let zero = b.build_int_const(0u64, ValueType::int(64))?;
    let base = b.build_int_binary_operation(x, zero, IntBinaryOp::And, ValueType::int(64))?;
    b.build_return(Some(levels[0]), &[])?;
    let mut fg = b.build()?;
    let below = levels.iter().skip(1).copied().chain([base]);
    for (&level, below) in levels.iter().zip(below) {
        let node = fg.producer(level);
        let lhs = fg.graph().node_input_id_at(node, 0)?;
        fg.graph_mut().update_input(lhs, below);
    }
    fg.graph_mut().rebuild_cache();
    steps(&[&super::CONE_STEPS], || {
        assert!(
            run_pass(&KnownBits, &mut fg)?,
            "every level is fully known, so the chain folds"
        );
        Ok(())
    })
}

#[test]
fn reversed_nested_fold_cone_transfer_is_not_quadratic_in_fold_count() -> Result<()> {
    assert_near_linear(
        "reversed nested folds",
        200,
        |n| cone_steps_for_reversed_nested_folds(n as u64),
        300,
    )
}

#[test]
fn nested_fold_cone_transfer_is_not_quadratic_in_fold_count() -> Result<()> {
    assert_near_linear(
        "nested folds",
        200,
        |n| cone_steps_for_nested_folds(n as u64),
        300,
    )
}

#[test]
fn tip_fold_cone_transfer_is_not_quadratic_in_spine_depth() -> Result<()> {
    assert_near_linear("tip fold", 200, |n| cone_steps_for_tip_fold(n as u64), 300)
}

/// Sorted asm-fingerprint of `node`.
fn fingerprint_of(fg: &strider_ir::Function, node: NodeId) -> Vec<u64> {
    let mut v: Vec<u64> = fg.side_tables().asm_fingerprint(node).into_iter().collect();
    v.sort_unstable();
    v
}

/// The `((x & 1) | 2) & 0` shape the cone walk exists for, every node stamped
/// with its own addr.  Pins the exact absorbed set, not just membership.
#[test]
fn fold_absorbs_exactly_the_contributor_cone_addrs() -> Result<()> {
    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        b.set_lift_addr(Some(0xA1));
        let one = b.build_int_const(1u64, ValueType::int(8))?;
        b.set_lift_addr(Some(0xA2));
        let x_and_1 = b.build_int_binary_operation(x, one, IntBinaryOp::And, ValueType::int(8))?;
        b.set_lift_addr(Some(0xA3));
        let two = b.build_int_const(2u64, ValueType::int(8))?;
        b.set_lift_addr(Some(0xA4));
        let ored =
            b.build_int_binary_operation(x_and_1, two, IntBinaryOp::Or, ValueType::int(8))?;
        b.set_lift_addr(Some(0xA5));
        let zero = b.build_int_const(0u64, ValueType::int(8))?;
        b.set_lift_addr(Some(0xA6));
        b.build_int_binary_operation(ored, zero, IntBinaryOp::And, ValueType::int(8))
    })?;

    run_pass(&KnownBits, &mut fg)?;
    let folded = fg.producer(fg.returned());
    // The sentinel is the `InitialVar` producing `x`: a cone member, not
    // descended through.
    assert_eq!(
        fingerprint_of(&fg, folded),
        vec![
            0xA1,
            0xA2,
            0xA3,
            0xA4,
            0xA5,
            0xA6,
            strider_ir_test_utils::SENTINEL_LIFT_ADDR
        ]
    );
    Ok(())
}

/// A diamond: one shared cone feeding two folds.  Each fold's absorbed set is
/// pinned exactly, so a lost link shows up as a shrink.
#[test]
fn diamond_folds_absorb_exactly_their_shared_cone_addrs() -> Result<()> {
    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        b.set_lift_addr(Some(0xB1));
        let seed = b.build_int_const(0u64, ValueType::int(8))?;
        b.set_lift_addr(Some(0xB2));
        let c7 = b.build_int_const(7u64, ValueType::int(8))?;
        b.set_lift_addr(Some(0xB3));
        let shared = b.build_int_binary_operation(seed, c7, IntBinaryOp::Or, ValueType::int(8))?;
        b.set_lift_addr(Some(0xB4));
        let c4 = b.build_int_const(4u64, ValueType::int(8))?;
        b.set_lift_addr(Some(0xB5));
        let fold_a =
            b.build_int_binary_operation(shared, c4, IntBinaryOp::And, ValueType::int(8))?;
        b.set_lift_addr(Some(0xB6));
        let c1 = b.build_int_const(1u64, ValueType::int(8))?;
        b.set_lift_addr(Some(0xB7));
        let fold_b =
            b.build_int_binary_operation(shared, c1, IntBinaryOp::And, ValueType::int(8))?;
        b.set_lift_addr(Some(0xB8));
        let live_a = b.build_int_binary_operation(fold_a, x, IntBinaryOp::Or, ValueType::int(8))?;
        b.set_lift_addr(Some(0xB9));
        let live_b = b.build_int_binary_operation(fold_b, x, IntBinaryOp::Or, ValueType::int(8))?;
        b.set_lift_addr(Some(0xBA));
        b.build_int_binary_operation(live_a, live_b, IntBinaryOp::Or, ValueType::int(8))
    })?;

    run_pass(&KnownBits, &mut fg)?;

    let mut by_value: Vec<(u128, Vec<u64>)> = fg
        .walk_kind(|k| matches!(k, NodeKind::IntConst(_)))
        .filter_map(|n| {
            let v = fg.int_const_u128(fg.node_outputs(n)[0])?;
            Some((v, fingerprint_of(&fg, n)))
        })
        .collect();
    by_value.sort_unstable();
    assert_eq!(
        by_value,
        vec![
            (1u128, vec![0xB1, 0xB2, 0xB3, 0xB6, 0xB7]),
            (4u128, vec![0xB1, 0xB2, 0xB3, 0xB4, 0xB5]),
        ]
    );
    Ok(())
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

/// Nested folds: `t0 = x & 0`, then `t(i) = t(i-1) | C(i)`, so EVERY `t(i)` is
/// fully known and each one's cone contains all the cones below it.
///
/// The constant replacing `t(n)` must carry the fingerprint of every
/// instruction that proved it constant, several levels down.  Losing one
/// under-taints silently: the folded VALUE is correct either way.
#[test]
fn known_bits_nested_folds_absorb_every_level_of_the_cone() -> Result<()> {
    const DEPTH: usize = 6;
    let addr_of = |i: usize| 0xC0DE_1000u64 + i as u64;

    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        let zero = b.build_int_const(0u64, ValueType::int(8))?;
        let mut acc = b.build_int_binary_operation(x, zero, IntBinaryOp::And, ValueType::int(8))?;
        for i in 0..DEPTH {
            // Each level's constant carries its own address, so a dropped
            // level names itself in the failure.
            b.set_lift_addr(Some(addr_of(i)));
            let c = b.build_int_const(1u64 << i, ValueType::int(8))?;
            b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
            acc = b.build_int_binary_operation(acc, c, IntBinaryOp::Or, ValueType::int(8))?;
        }
        Ok(acc)
    })?;

    run_to_fixed_point(&KnownBits, &mut fg)?;

    // 1 | 2 | 4 | ... : every level contributed to the value.
    let expected = (1u64 << DEPTH) - 1;
    assert_returns_const(&fg, expected);

    let folded = fg.producer(fg.returned());
    let fp = fg.side_tables().asm_fingerprint(folded);
    let missing: Vec<u64> = (0..DEPTH)
        .map(addr_of)
        .filter(|a| !fp.contains(a))
        .collect();
    assert!(
        missing.is_empty(),
        "the folded constant lost {missing:#x?} from its proof; every level of a \
         nested-fold cone must be absorbed. Got {fp:#x?}"
    );
    Ok(())
}

/// Sleigh reference: `opbehavior.cc:411/432`, zero at or past the output width.
fn ref_shift(v: u128, k: u128, left: bool, bits: u32, mask: u128) -> u128 {
    if k >= u128::from(bits) {
        return 0;
    }
    if left {
        (v << k) & mask
    } else {
        (v & mask) >> k
    }
}

/// Builds `((x & free) | ones) shift k` at `I8` and returns `Some(reason)` when
/// the known-bits verdict claims a bit no concrete input produces.
fn kb_shift_mismatch(free: u128, ones: u128, k: u64, left: bool) -> Option<String> {
    let vn = reg_vn(0x80, 1);
    let mut b = RegisterSet::new()
        .tracked(vn)
        .build_fn_single_region()
        .unwrap();
    let tracked = b.read_variable(&vn).unwrap();
    let free_mask = b.build_int_const(free as u64, ValueType::int(8)).unwrap();
    let ones_mask = b.build_int_const(ones as u64, ValueType::int(8)).unwrap();
    let anded = b
        .build_int_binary_operation(tracked, free_mask, IntBinaryOp::And, ValueType::int(8))
        .unwrap();
    let base = b
        .build_int_binary_operation(anded, ones_mask, IntBinaryOp::Or, ValueType::int(8))
        .unwrap();
    let count = b.build_int_const(k, ValueType::int(8)).unwrap();
    let op = if left {
        IntBinaryOp::ShiftLeft
    } else {
        IntBinaryOp::ShiftRight
    };
    let shifted = b
        .build_int_binary_operation(base, count, op, ValueType::int(8))
        .unwrap();
    b.build_return(Some(shifted), &[]).unwrap();
    let f = b.build().unwrap();
    let kb = analyze(&f)[shifted];

    let (mut and_all, mut or_all) = (u128::MAX, 0u128);
    for concrete in 0u128..256 {
        let out = ref_shift((concrete & free) | ones, u128::from(k), left, 8, 0xff);
        and_all &= out;
        or_all |= out;
    }
    let bad_ones = kb.ones & !and_all;
    let bad_zeros = kb.zeros & or_all;
    (bad_ones != 0 || bad_zeros != 0).then(|| {
        format!(
            "free={free:#x} ones={ones:#x} k={k} left={left} kb={kb:?} \
             bad_ones={bad_ones:#x} bad_zeros={bad_zeros:#x}"
        )
    })
}

/// Counts at, past and far past the width are the cases `opbehavior.cc`
/// saturates and a modulo-reducing implementation would wrap.
#[test]
fn shift_known_bits_agree_with_a_concrete_reference() {
    let mut fails: Vec<String> = Vec::new();
    for &free in &[0x00u128, 0x0f, 0xf0, 0xff, 0x55, 0x81] {
        for &ones in &[0x00u128, 0x01, 0x80, 0x0f] {
            if free & ones != 0 {
                continue;
            }
            for left in [true, false] {
                for k in [0u64, 1, 4, 7, 8, 200, 255] {
                    fails.extend(kb_shift_mismatch(free, ones, k, left));
                }
            }
        }
    }
    assert!(fails.is_empty(), "known-bits shift failures: {fails:#?}");
}

/// p-code leaves a shift count's width free; `build_int_binary_operation` pins
/// every operand to the output width, so the lifter coerces the count first.
#[test]
fn shift_count_wider_than_the_output_is_refused() {
    let vn = reg_vn(0x80, 1);
    let mut b = RegisterSet::new()
        .tracked(vn)
        .build_fn_single_region()
        .unwrap();
    let base = b.build_int_const(1u64, ValueType::int(8)).unwrap();
    let wide_count = b.build_int_const(4u64, ValueType::int(16)).unwrap();
    let err = b
        .build_int_binary_operation(base, wide_count, IntBinaryOp::ShiftLeft, ValueType::int(8))
        .expect_err("a wider count must be refused, not silently truncated");
    assert!(
        err.to_string().contains("but the operation requires i8"),
        "unexpected refusal: {err}"
    );
}

/// `y = (x + 4) & 0xF0` stays live while `z = y & 0x0F` folds to 0.  The
/// contributors proving `z` are the new constant's, so `y`'s own fingerprint
/// must not pick up the `Add`'s address.
#[test]
fn a_surviving_cone_node_keeps_its_own_fingerprint() -> Result<()> {
    let var = reg_vn(0x1000, 4);
    let mut b = RegisterSet::new()
        .tracked(var)
        .arg(var)
        .build_fn_single_region()?;
    b.set_lift_addr(Some(0xA0));
    let x = b.read_variable(&var)?;
    let four = b.build_int_const(4u64, ValueType::int(32))?;
    let xa = b.build_int_binary_operation(x, four, IntBinaryOp::Add, ValueType::int(32))?;
    b.set_lift_addr(Some(0xB0));
    let mask_hi = b.build_int_const(0xF0u64, ValueType::int(32))?;
    let y = b.build_int_binary_operation(xa, mask_hi, IntBinaryOp::And, ValueType::int(32))?;
    b.set_lift_addr(Some(0xC0));
    let mask_lo = b.build_int_const(0x0Fu64, ValueType::int(32))?;
    let z = b.build_int_binary_operation(y, mask_lo, IntBinaryOp::And, ValueType::int(32))?;
    b.build_return(None, &[y, z])?;
    let mut fg = b.build()?;

    let y_node = fg.producer(y);
    let before = fingerprint_of(&fg, y_node);
    run_pass(&KnownBits, &mut fg)?;

    let ret = fg.first_kind(|k| matches!(k, NodeKind::Return));
    let returned = fg.node_inputs(ret);
    assert_eq!(fg.producer(returned[2]), y_node, "y survives the fold");
    let folded = returned[3];
    assert_eq!(fg.int_const_u128(folded), Some(0), "z folds to 0");
    assert_eq!(
        fingerprint_of(&fg, y_node),
        before,
        "a surviving cone node inherited its inputs' addresses"
    );
    // The fold's own constant carries the whole cone instead.
    let cone = fingerprint_of(&fg, fg.producer(folded));
    assert!(
        cone.contains(&0xA0) && cone.contains(&0xB0) && cone.contains(&0xC0),
        "the fold lost a contributor: {cone:?}"
    );
    Ok(())
}

/// Known bits of `v`'s producer with operand facts read off the IR alone.
fn one_hop(fg: &strider_ir::Function, v: strider_ir::node::ValueId) -> Option<KnownBitsFacts> {
    node_known_bits(fg, fg.producer(v), &mut const_only(fg)).map(|(_, kb)| kb)
}

/// The constant-only resolver answers for an interned constant and defaults
/// to "no info" for everything else; a one-hop caller needs no map.
#[test]
fn the_constant_only_resolver_reads_constants_and_nothing_else() -> Result<()> {
    let vn = reg_vn(0x1000, 4);
    let (fg, x) = make_fn_with_var(vn, |b, x| {
        let k = b.build_int_const(0xF0u64, ValueType::int(32))?;
        b.build_int_binary_operation(x, k, IntBinaryOp::Or, ValueType::int(32))
    })?;
    let resolve = const_only(&fg);
    let or = fg.producer(fg.returned());
    let [lhs, rhs] = fg.node_inputs_exact::<2>(or)?;
    assert_eq!(resolve(rhs).ones, 0xF0, "the interned constant resolves");
    assert_eq!(resolve(rhs).zeros, 0xFFFF_FF0F);
    assert_eq!(
        (resolve(lhs).ones, resolve(lhs).zeros),
        (0, 0),
        "a variable has no info"
    );
    assert_eq!(resolve(x).ones, 0, "the read-back variable has none either");
    Ok(())
}

/// Sleigh yields 0 for a shift at or past the output width, so every bit of
/// the result is known zero.
#[test]
fn a_shift_past_the_width_proves_every_bit_zero() -> Result<()> {
    let vn = reg_vn(0x1000, 4);
    let (fg, _) = make_fn_with_var(vn, |b, x| {
        b.build_const_binop(40u32, x, IntBinaryOp::ShiftLeft, ValueType::int(32))
    })?;
    let kb = one_hop(&fg, fg.returned()).expect("the shift is tracked");
    assert_eq!((kb.ones, kb.zeros), (0, 0xFFFF_FFFF));
    Ok(())
}

/// A right shift vacates its top bits, whatever the shifted value holds.
#[test]
fn a_right_shift_proves_its_vacated_bits_zero() -> Result<()> {
    let vn = reg_vn(0x1000, 4);
    let (fg, _) = make_fn_with_var(vn, |b, x| {
        b.build_const_binop(24u32, x, IntBinaryOp::ShiftRight, ValueType::int(32))
    })?;
    let kb = one_hop(&fg, fg.returned()).expect("the shift is tracked");
    assert_eq!((kb.ones, kb.zeros), (0, 0xFFFF_FF00));
    Ok(())
}

/// Past the `u128` carrier the transfer proves nothing rather than reporting
/// a mask no `u128` holds: the arm is skipped entirely, so no caller can read
/// a zeroed mask back as "every bit known".
#[test]
fn a_width_past_the_carrier_is_skipped_not_defaulted() -> Result<()> {
    let wide = ValueType::int(256);
    let vn = reg_vn(0x2000, 32);
    let (fg, _x) = make_fn_with_var(vn, |b, x| {
        b.build_const_binop(3u64, x, IntBinaryOp::And, wide)
    })?;
    let masked = fg.returned();
    assert_eq!(fg.value_type_opt(masked), Some(wide));
    assert!(
        node_known_bits(&fg, fg.producer(masked), &mut const_only(&fg)).is_none(),
        "an And the carrier cannot hold yields no facts at all"
    );
    let kb = analyze(&fg)[masked];
    assert_eq!((kb.ones, kb.zeros), (0, 0));
    Ok(())
}

/// `And(ZExt(x:I8), mask)` at `I32` after KnownBits, and the value the
/// Return reads.
fn masked_zext(mask: u64) -> Result<(strider_ir::Function, strider_ir::node::ValueId)> {
    let vn = reg_vn(0x1000, 1);
    let (mut fg, _) = make_fn_with_var(vn, |b, x| {
        let wide = b.extend_if_needed(x, ValueType::int(32), ExtendOp::ZeroExtend)?;
        b.build_const_binop(mask, wide, IntBinaryOp::And, ValueType::int(32))
    })?;
    run_to_fixed_point(&KnownBits, &mut fg)?;
    let ret = fg.returned();
    Ok((fg, ret))
}

/// A mask covering every bit its operand may set is redundant; one that
/// clears a bit the operand may set stays.
#[test]
fn a_mask_over_every_possible_bit_is_dropped() -> Result<()> {
    for (mask, dropped) in [(0xFF, true), (0x1FF, true), (0x7F, false), (0xF0, false)] {
        let (fg, ret) = masked_zext(mask)?;
        let kind = *fg.kind_of_value(ret);
        assert_eq!(
            kind == NodeKind::Extend(ExtendOp::ZeroExtend),
            dropped,
            "{mask:#x}: {kind:?}"
        );
    }
    Ok(())
}

/// `0x81:I8 << count` with the count typed `count_ty`, as p-code leaves it.
fn wide_count_shift(count: UBig, count_ty: ValueType) -> Result<strider_ir::Function> {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    make_fn(|b| {
        let x = b.build_int_const(0x81u64, ValueType::int(8))?;
        let amt = b.build_int_const(count, count_ty)?;
        let node = b.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            [x, amt],
            [ValueKind::Typed(ValueType::int(8))],
        );
        let [out] = b.node_outputs_exact::<1>(node)?;
        Ok(out)
    })
}

/// A known count past the width zeroes the result whatever the count's own
/// width, its bits past 32 included.
#[test]
fn a_wide_count_past_the_width_proves_every_bit_zero() -> Result<()> {
    let fg = wide_count_shift(UBig::ONE << 32, ValueType::int(128))?;
    let kb = analyze(&fg)[fg.returned()];
    assert_eq!((kb.ones, kb.zeros), (0, 0xFF));
    Ok(())
}

/// A count wider than the carrier is unknown, so even a constant shifted by
/// it proves nothing.
#[test]
fn a_count_past_the_carrier_proves_nothing() -> Result<()> {
    let fg = wide_count_shift(UBig::from(3u8), ValueType::int(256))?;
    assert_eq!(analyze(&fg)[fg.returned()], KnownBitsFacts::default());
    Ok(())
}

/// `x:I1` read off an `I8` register, and `Or(x, 1)`.
fn i1_operands(b: &mut FunctionBuilder, x: ValueId) -> Result<(ValueId, ValueId)> {
    let bit = b.truncate_if_needed(x, ValueType::I1)?;
    let one = b.build_boolean_const(true);
    let set = b.build_int_binary_operation(bit, one, IntBinaryOp::Or, ValueType::I1)?;
    Ok((bit, set))
}

/// Sign-extending a known-one `I1` fills every bit.
#[test]
fn a_sign_extended_known_one_bit_is_all_ones() -> Result<()> {
    let ty = ValueType::int(32);
    let (fg, _) = make_fn_with_var(reg_vn(0x1000, 1), |b, x| {
        let (_, set) = i1_operands(b, x)?;
        b.extend_if_needed(set, ty, ExtendOp::SignExtend)
    })?;
    let kb = analyze(&fg)[fg.returned()];
    let want = strider_ir::eval::sign_extend(&UBig::ONE, ValueType::I1, ty).expect("integer");
    assert_eq!((UBig::from(kb.ones), kb.zeros), (want, 0));
    Ok(())
}

/// `lzcount(0:I1)` is 8, Sleigh counting over the byte, so bit 3 may be set.
#[test]
fn an_i1_lzcount_may_set_bit_3() -> Result<()> {
    let ty = ValueType::int(8);
    let (fg, _) = make_fn_with_var(reg_vn(0x1000, 1), |b, x| {
        let (bit, _) = i1_operands(b, x)?;
        b.build_lzcount(bit, ty)
    })?;
    let kb = analyze(&fg)[fg.returned()];
    assert_eq!(
        strider_ir::eval::lzcount(&UBig::ZERO, ValueType::I1),
        Some(UBig::from(8u8))
    );
    assert_eq!((kb.ones, kb.zeros), (0, 0xF0));
    Ok(())
}

/// A known one at bit `h` caps a 32-bit `lzcount` at `31 - h`; known zeros
/// cap a `popcount` at the bits left.
#[test]
fn known_input_bits_tighten_a_count() -> Result<()> {
    let i32t = ValueType::int(32);
    for (op, zeros) in [(IntBinaryOp::Or, !0x1Fu128), (IntBinaryOp::And, !0xF)] {
        let (fg, _) = make_fn_with_var(reg_vn(0x80, 4), |b, x| {
            if op == IntBinaryOp::Or {
                let y = b.build_const_binop(1u64, x, op, i32t)?;
                b.build_lzcount(y, i32t)
            } else {
                let y = b.build_const_binop(0xFFu64, x, op, i32t)?;
                b.build_popcount(y, i32t)
            }
        })?;
        let kb = analyze(&fg)[fg.returned()];
        assert_eq!((kb.ones, kb.zeros), (0, zeros & 0xFFFF_FFFF), "{op:?}");
    }
    Ok(())
}

/// Two stacked redundant masks both drop in one run, whichever the pass
/// visits first.
#[test]
fn stacked_redundant_masks_drop_in_one_run() -> Result<()> {
    let mut zext = None;
    let (mut fg, _) = make_fn_with_var(reg_vn(0x40, 1), |b, x| {
        let wide = b.extend_if_needed(x, ValueType::int(32), ExtendOp::ZeroExtend)?;
        zext = Some(wide);
        let inner = b.build_const_binop(0xFFFu64, wide, IntBinaryOp::And, ValueType::int(32))?;
        b.build_const_binop(0xFFFFu64, inner, IntBinaryOp::And, ValueType::int(32))
    })?;
    run_pass(&KnownBits, &mut fg)?;
    assert_eq!(
        Some(return_value(fg.graph())?),
        zext,
        "{}",
        fg.to_text(false)
    );
    Ok(())
}
