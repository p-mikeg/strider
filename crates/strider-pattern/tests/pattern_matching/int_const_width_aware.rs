use strider_ir::IRBuilderExt;
use strider_ir::node::ValueType;
use strider_pattern::{MatchPat, Matcher, int_const, int_const_any_width};

use super::support::Tb;

/// I32 holding `0xffff_ffce`, i.e. -50 mod 2^32.
#[test]
fn negative_int_const_matches_at_u32_width() {
    let mut t = Tb::empty();
    let neg50_u32 = t.int_of(0xffff_ffceu64, ValueType::I32);
    let function = t.ret_val(neg50_u32);
    let hits = Matcher::new(&function)
        .find_all(&int_const_any_width(-50).into_pattern())
        .unwrap();
    assert!(
        !hits.is_empty(),
        "expected int_const_any_width(-50) to match 0xffff_ffce at I32 width"
    );
}

/// I64 holding `0xffff_ffff_ffff_ffce`, i.e. -50 mod 2^64.
#[test]
fn negative_int_const_matches_at_u64_width() {
    let mut t = Tb::empty();
    let neg50_u64 = t.u64(0xffff_ffff_ffff_ffceu64);
    let function = t.ret_val(neg50_u64);
    let hits = Matcher::new(&function)
        .find_all(&int_const_any_width(-50).into_pattern())
        .unwrap();
    assert!(
        !hits.is_empty(),
        "expected int_const_any_width(-50) to match 0xffff_ffff_ffff_ffce at I64 width"
    );
}

/// I128 holding -50 as a full 128-bit two's complement value.
#[test]
fn negative_int_const_matches_at_u128_width() {
    let mut t = Tb::empty();
    let neg50_at_u128: u128 = (-50i128) as u128;
    let neg50 = t
        .fb_mut()
        .build_int_const(neg50_at_u128, ValueType::I128)
        .unwrap();
    let function = t.ret_val(neg50);
    let hits = Matcher::new(&function)
        .find_all(&int_const_any_width(-50).into_pattern())
        .unwrap();
    assert!(
        !hits.is_empty(),
        "expected int_const_any_width(-50) to match at I128 width"
    );
}

/// A positive I32 `IntConst(50)`: `int_const(50)` matches;
/// `int_const_any_width(-50)` rejects.
#[test]
fn positive_int_const_matches_unchanged_and_negative_does_not() {
    let mut t = Tb::empty();
    let fifty = t.int_of(50u64, ValueType::I32);
    let function = t.ret_val(fifty);
    let m = Matcher::new(&function);
    assert!(
        !m.find_all(&int_const(50u128).into_pattern())
            .unwrap()
            .is_empty(),
        "expected int_const(50) to match"
    );
    assert!(
        m.find_all(&int_const_any_width(-50).into_pattern())
            .unwrap()
            .is_empty(),
        "int_const_any_width(-50) must not match +50"
    );
}

/// The axis is width extension, not sign: `128` is positive, yet stored
/// sign-extended from I8 it reads back as `0xffff_ffff_ffff_ff80`, which the
/// bit-exact `int_const(128)` misses.
#[test]
fn positive_value_stored_sign_extended_from_a_narrower_width() {
    let mut t = Tb::empty();
    let stored = t.u64(0xffff_ffff_ffff_ff80u64);
    let function = t.ret_val(stored);
    let m = Matcher::new(&function);
    assert!(
        m.find_all(&int_const(128u128).into_pattern())
            .unwrap()
            .is_empty(),
        "int_const(128) must not match the sign-extended form"
    );
    assert!(
        !m.find_all(&int_const_any_width(128).into_pattern())
            .unwrap()
            .is_empty(),
        "int_const_any_width(128) must match the I8 value widened by sign extension"
    );
}

/// The zero-extended direction: a 16-bit `-50` widened to I64 keeps its high
/// half clear, so the bit-exact `int_const(-50)` misses it.
#[test]
fn negative_value_stored_zero_extended_from_a_narrower_width() {
    let mut t = Tb::empty();
    let stored = t.u64(0x0000_0000_0000_ffceu64);
    let function = t.ret_val(stored);
    let m = Matcher::new(&function);
    assert!(
        m.find_all(&int_const((-50i128) as u128).into_pattern())
            .unwrap()
            .is_empty(),
        "int_const(-50) must not match the zero-extended narrow form"
    );
    assert!(
        !m.find_all(&int_const_any_width(-50).into_pattern())
            .unwrap()
            .is_empty(),
        "int_const_any_width(-50) must match the I16 value widened by zero extension"
    );
}

#[test]
fn int_const_any_width_set_membership() {
    let mut t = Tb::empty();
    let stored = t.u64(0xffff_ffff_ffff_ff80u64);
    let function = t.ret_val(stored);
    let m = Matcher::new(&function);
    assert!(
        !m.find_all(&int_const_any_width([-50, 128]).into_pattern())
            .unwrap()
            .is_empty(),
        "a member of the set must match"
    );
    assert!(
        m.find_all(&int_const_any_width([-50, 127]).into_pattern())
            .unwrap()
            .is_empty(),
        "no member of the set must not match"
    );
}
