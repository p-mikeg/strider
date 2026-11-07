//! `strider_ir::eval`'s integer ops against a `u128` / `i128` reference at
//! every supported width from 8 to 128 bits.

use proptest::prelude::*;
use strider_ir::eval;
use strider_ir::{IntBinaryOp, IntCmpOp, IntUnaryOp, UBig, ValueType};

/// `I1` is left out: its shift guard and sign read diverge from Sleigh's
/// 1-byte bool, pinned in `eval`'s unit tests.
const WIDTHS: [u16; 13] = [8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 96, 112, 128];

const BOPS: [IntBinaryOp; 12] = [
    IntBinaryOp::Add,
    IntBinaryOp::And,
    IntBinaryOp::Or,
    IntBinaryOp::Xor,
    IntBinaryOp::Div,
    IntBinaryOp::Sdiv,
    IntBinaryOp::Rem,
    IntBinaryOp::Srem,
    IntBinaryOp::ShiftRight,
    IntBinaryOp::SShiftRight,
    IntBinaryOp::ShiftLeft,
    IntBinaryOp::Mul,
];

const COPS: [IntCmpOp; 6] = [
    IntCmpOp::Equal,
    IntCmpOp::Sless,
    IntCmpOp::Less,
    IntCmpOp::Carry,
    IntCmpOp::Scarry,
    IntCmpOp::Sborrow,
];

fn m(w: u32) -> u128 {
    if w >= 128 {
        u128::MAX
    } else {
        (1u128 << w) - 1
    }
}

#[allow(clippy::cast_possible_wrap)]
fn sx(v: u128, w: u32) -> i128 {
    let v = v & m(w);
    if w == 128 || (v >> (w - 1)) & 1 == 0 {
        v as i128
    } else {
        (v as i128) - (1i128 << w)
    }
}

fn smin(w: u32) -> i128 {
    if w == 128 {
        i128::MIN
    } else {
        -(1i128 << (w - 1))
    }
}

fn smax(w: u32) -> i128 {
    if w == 128 {
        i128::MAX
    } else {
        (1i128 << (w - 1)) - 1
    }
}

#[allow(clippy::cast_sign_loss)]
fn ref_binary(op: IntBinaryOp, l: u128, r: u128, w: u32) -> Option<u128> {
    let (l, rm) = (l & m(w), r & m(w));
    let wide = r >= u128::from(w);
    let signed = |f: fn(i128, i128) -> i128| {
        let (a, b) = (sx(l, w), sx(rm, w));
        (b != 0 && !(a == smin(w) && b == -1)).then(|| f(a, b) as u128)
    };
    let res = match op {
        IntBinaryOp::Add => l.wrapping_add(rm),
        IntBinaryOp::Mul => l.wrapping_mul(rm),
        IntBinaryOp::And => l & rm,
        IntBinaryOp::Or => l | rm,
        IntBinaryOp::Xor => l ^ rm,
        IntBinaryOp::ShiftLeft | IntBinaryOp::ShiftRight if wide => 0,
        IntBinaryOp::ShiftLeft => l << r,
        IntBinaryOp::ShiftRight => l >> r,
        IntBinaryOp::SShiftRight if wide => (sx(l, w) >> 127) as u128,
        IntBinaryOp::SShiftRight => (sx(l, w) >> r) as u128,
        IntBinaryOp::Div => l.checked_div(rm)?,
        IntBinaryOp::Rem => l.checked_rem(rm)?,
        IntBinaryOp::Sdiv => signed(|a, b| a / b)?,
        IntBinaryOp::Srem => signed(|a, b| a % b)?,
    };
    Some(res & m(w))
}

fn ref_cmp(op: IntCmpOp, l: u128, r: u128, w: u32) -> bool {
    let (l, r) = (l & m(w), r & m(w));
    let out_of_range = |s: Option<i128>| s.is_none_or(|s| s > smax(w) || s < smin(w));
    match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::Sless => sx(l, w) < sx(r, w),
        IntCmpOp::Carry => l.checked_add(r).is_none_or(|s| s > m(w)),
        IntCmpOp::Scarry => out_of_range(sx(l, w).checked_add(sx(r, w))),
        IntCmpOp::Sborrow => out_of_range(sx(l, w).checked_sub(sx(r, w))),
    }
}

fn edge() -> impl Strategy<Value = u128> {
    prop_oneof![
        any::<u128>(),
        0u128..300,
        Just(u128::MAX),
        Just(1u128 << 127),
        Just((1u128 << 127) - 1),
        (0u32..128).prop_map(|b| 1u128 << b),
        (0u32..128).prop_map(|b| (1u128 << b) - 1),
    ]
}

fn small(v: Option<UBig>) -> Option<u128> {
    v.map(|v| u128::try_from(&v).unwrap())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20_000))]

    #[test]
    fn eval_matches_reference(
        wi in 0..WIDTHS.len(),
        oi in 0..BOPS.len(),
        ci in 0..COPS.len(),
        l in edge(),
        r in edge(),
    ) {
        let ty = ValueType::int(WIDTHS[wi]);
        let w = u32::from(WIDTHS[wi]);
        let (ul, ur) = (UBig::from(l), UBig::from(r));
        let (op, cop) = (BOPS[oi], COPS[ci]);
        prop_assert_eq!(
            small(eval::int_binary(op, &ul, &ur, ty)),
            ref_binary(op, l, r, w),
            "{:?} i{} l={:#x} r={:#x}", op, w, l, r
        );
        prop_assert_eq!(
            eval::int_cmp(cop, &ul, &ur, ty),
            Some(ref_cmp(cop, l, r, w)),
            "{:?} i{} l={:#x} r={:#x}", cop, w, l, r
        );
        let lm = l & m(w);
        prop_assert_eq!(
            small(eval::int_unary(IntUnaryOp::Neg, &ul, ty)),
            Some(lm.wrapping_neg() & m(w))
        );
        prop_assert_eq!(small(eval::popcount(&ul, ty)), Some(u128::from(lm.count_ones())));
        prop_assert_eq!(
            small(eval::lzcount(&ul, ty)),
            Some(u128::from(lm.leading_zeros() - (128 - w)))
        );
        for &ow in WIDTHS.iter().filter(|&&o| u32::from(o) >= w) {
            #[allow(clippy::cast_sign_loss)]
            let want = (sx(l, w) as u128) & m(u32::from(ow));
            prop_assert_eq!(
                small(eval::sign_extend(&ul, ty, ValueType::int(ow))),
                Some(want)
            );
        }
    }
}

#[test]
fn int_min_by_minus_one_is_left_unfolded() {
    for w in [8u16, 16, 32, 64] {
        let (min, minus_one) = (UBig::from(1u128 << (w - 1)), UBig::from(m(w.into())));
        for op in [IntBinaryOp::Sdiv, IntBinaryOp::Srem] {
            assert_eq!(
                eval::int_binary(op, &min, &minus_one, ValueType::int(w)),
                None,
                "{op:?} i{w}"
            );
        }
    }
}
