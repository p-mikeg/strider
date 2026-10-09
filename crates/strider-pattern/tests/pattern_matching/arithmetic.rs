use strider_ir::{IntBinaryOp, IntCmpOp};
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

#[test]
fn add_wrong_operand_rejects() {
    let function = shapes::add_consts(5, 3);
    a::none(
        &function,
        int_add(int_const(5u128), int_const(99u128)).into_pattern(),
    );
}
#[test]
fn wrong_op_rejects() {
    // Mul is a stand-in wrong op; the rejection check is op-agnostic.
    let function = shapes::int_bin_5_3(IntBinaryOp::Mul);
    a::none(
        &function,
        int_add(int_const(5u128), int_const(3u128)).into_pattern(),
    );
}

/// `return(~5):I64`, where complement is the canonical
/// `Xor(IntConst(5), IntConst(all_ones))`.
fn int_bit_not_5() -> strider_ir::Function {
    let mut t = Tb::empty();
    let v = t.u64(5);
    let nv = t.bit_not_at(v, strider_ir::node::ValueType::I64);
    t.ret_val(nv)
}
#[test]
fn bit_not_wrong_operand_rejects() {
    let function = int_bit_not_5();
    a::none(&function, int_not(int_const(99u128)).into_pattern());
}

#[test]
fn unary_wrong_op_rejects() {
    // The canonical bit-not shape is a binary Xor, so a unary neg pattern
    // must reject it.
    let function = int_bit_not_5();
    a::none(&function, int_neg(int_const(5u128)).into_pattern());
}
#[test]
fn cmp_wrong_op_rejects() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Equal);
    a::none(
        &function,
        int_lt(int_const(5u128), int_const(3u128)).into_pattern(),
    );
}

#[test]
fn nested_add_three_levels_matches() {
    let function = shapes::add_nested_3(1, 2, 3);
    a::matches(
        &function,
        int_add(
            int_add(int_const(1u128), int_const(2u128)),
            int_const(3u128),
        )
        .into_pattern(),
        1,
    );
}

#[test]
fn nested_pattern_depth_five() {
    // (((((1+2)+3)+4)+5)+6)
    let mut t = Tb::empty();
    let a1 = t.u64(1);
    let a2 = t.u64(2);
    let s = t.add(a1, a2);
    let a3 = t.u64(3);
    let s = t.add(s, a3);
    let a4 = t.u64(4);
    let s = t.add(s, a4);
    let a5 = t.u64(5);
    let s = t.add(s, a5);
    let a6 = t.u64(6);
    let s = t.add(s, a6);
    let function = t.ret_val(s);

    a::matches(
        &function,
        int_add(
            int_add(
                int_add(
                    int_add(
                        int_add(int_const(1u128), int_const(2u128)),
                        int_const(3u128),
                    ),
                    int_const(4u128),
                ),
                int_const(5u128),
            ),
            int_const(6u128),
        )
        .into_pattern(),
        1,
    );

    a::none(
        &function,
        int_add(
            int_add(
                int_add(
                    int_add(
                        int_add(int_const(1u128), int_const(999u128)),
                        int_const(3u128),
                    ),
                    int_const(4u128),
                ),
                int_const(5u128),
            ),
            int_const(6u128),
        )
        .into_pattern(),
    );
}

#[test]
fn nested_any_partial_matches() {
    let function = shapes::add_nested_3(1, 2, 3);
    let inner = Capture::new();
    let m = a::unique(
        &function,
        int_add(anything().capture(inner), int_const(3u128)).into_pattern(),
    );
    assert!(m.value(inner).is_some());
}

/// `bit_mask_u128` saturates at 128 bits, so at I256 an `IntConst(2^128-1)`
/// is NOT the complement's all-ones; matching it would report a rewrite LHS
/// that does not hold.
#[test]
fn bit_not_rejects_a_saturated_mask_past_the_carrier() {
    let mut t = Tb::empty();
    let v = t.int_of(5, strider_ir::node::ValueType::I256);
    // `build_int_const` masks to the declared width, so this is 2^128-1.
    let nv = t.bit_not_at(v, strider_ir::node::ValueType::I256);
    let narrowed = t.trunc_to(nv, strider_ir::node::ValueType::I64);
    let function = t.ret_val(narrowed);
    a::none(&function, int_not(anything()).into_pattern());
}
