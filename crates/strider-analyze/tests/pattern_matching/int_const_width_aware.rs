//! Width-aware `int_const` matching: `int_const(-50)` matches an
//! `IntConst` at any declared width without explicit per-arch pinning.

use strider_analyze::pattern::{Matcher, int_const};
use strider_ir::node::NodeOutputType;

use super::support::Tb;

/// A I32 node holding the bit pattern `0xffff_ffce` (= -50 mod 2^32).
/// The pattern `int_const(-50)` must match it.
#[test]
fn negative_int_const_matches_at_u32_width() {
    let mut t = Tb::empty();
    let neg50_u32 = t.int_of(0xffff_ffceu64, NodeOutputType::I32);
    let function = t.ret_val(neg50_u32);
    let hits = Matcher::try_new(&function).unwrap().find_all(&int_const(-50));
    assert!(
        !hits.is_empty(),
        "expected int_const(-50) to match 0xffff_ffce at I32 width"
    );
}

/// A I64 node holding the bit pattern `0xffff_ffff_ffff_ffce` (= -50 mod 2^64).
/// The pattern `int_const(-50)` must match it.
#[test]
fn negative_int_const_matches_at_u64_width() {
    let mut t = Tb::empty();
    let neg50_u64 = t.u64(0xffff_ffff_ffff_ffceu64);
    let function = t.ret_val(neg50_u64);
    let hits = Matcher::try_new(&function).unwrap().find_all(&int_const(-50));
    assert!(
        !hits.is_empty(),
        "expected int_const(-50) to match 0xffff_ffff_ffff_ffce at I64 width"
    );
}

/// A I128 node holding -50 as a full 128-bit two's-complement value.
/// The pattern `int_const(-50)` must match it.
#[test]
fn negative_int_const_matches_at_u128_width() {
    let mut t = Tb::empty();
    let neg50_at_u128: u128 = (-50i128) as u128;
    let neg50 = t
        .fb_mut()
        .build_int_const(neg50_at_u128, NodeOutputType::I128)
        .unwrap();
    let function = t.ret_val(neg50);
    let hits = Matcher::try_new(&function).unwrap().find_all(&int_const(-50));
    assert!(
        !hits.is_empty(),
        "expected int_const(-50) to match at I128 width"
    );
}

/// A positive I32 `IntConst(50)`: `int_const(50)` matches;
/// `int_const(-50)` rejects.
#[test]
fn positive_int_const_matches_unchanged_and_negative_does_not() {
    let mut t = Tb::empty();
    let fifty = t.int_of(50u64, NodeOutputType::I32);
    let function = t.ret_val(fifty);
    let m = Matcher::try_new(&function).unwrap();
    assert!(
        !m.find_all(&int_const(50)).is_empty(),
        "expected int_const(50) to match"
    );
    assert!(
        m.find_all(&int_const(-50)).is_empty(),
        "int_const(-50) must not match +50"
    );
}
