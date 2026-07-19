//! `signed_int_const(-50)` matches an `IntConst` at any declared width, with
//! no per-arch pinning.

use strider_ir::IRBuilderExt;
use strider_ir::node::ValueType;
use strider_pattern::{MatchPat, Matcher, int_const, signed_int_const};

use super::support::Tb;

/// I32 holding `0xffff_ffce`, i.e. -50 mod 2^32.
#[test]
fn negative_int_const_matches_at_u32_width() {
    let mut t = Tb::empty();
    let neg50_u32 = t.int_of(0xffff_ffceu64, ValueType::I32);
    let function = t.ret_val(neg50_u32);
    let hits = Matcher::new(&function)
        .find_all(&signed_int_const(-50).into_pattern())
        .unwrap();
    assert!(
        !hits.is_empty(),
        "expected signed_int_const(-50) to match 0xffff_ffce at I32 width"
    );
}

/// I64 holding `0xffff_ffff_ffff_ffce`, i.e. -50 mod 2^64.
#[test]
fn negative_int_const_matches_at_u64_width() {
    let mut t = Tb::empty();
    let neg50_u64 = t.u64(0xffff_ffff_ffff_ffceu64);
    let function = t.ret_val(neg50_u64);
    let hits = Matcher::new(&function)
        .find_all(&signed_int_const(-50).into_pattern())
        .unwrap();
    assert!(
        !hits.is_empty(),
        "expected signed_int_const(-50) to match 0xffff_ffff_ffff_ffce at I64 width"
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
        .find_all(&signed_int_const(-50).into_pattern())
        .unwrap();
    assert!(
        !hits.is_empty(),
        "expected signed_int_const(-50) to match at I128 width"
    );
}

/// A positive I32 `IntConst(50)`: `int_const(50)` matches;
/// `signed_int_const(-50)` rejects.
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
        m.find_all(&signed_int_const(-50).into_pattern())
            .unwrap()
            .is_empty(),
        "signed_int_const(-50) must not match +50"
    );
}
