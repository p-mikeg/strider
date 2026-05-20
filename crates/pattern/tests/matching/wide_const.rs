//! Wide-const pattern matching: `int_const_wide(value)`,
//! `any_wide_int_const(c)` + `Match::get_wide_bytes`.

use strider_ir::wide_const::WideConstStorage;
use pattern::*;

use super::support::Tb;

/// Build a function that returns one wide constant.  Returns the
/// `BuiltFunctionGraph` ready for the pattern matcher.
fn fn_returning_u256(limbs: [u64; 4]) -> strider_ir::BuiltFunctionGraph {
    let mut t = Tb::empty();
    let v = t.u256(limbs);
    t.ret_val(v)
}

#[test]
fn int_const_wide_matches_by_value() {
    let limbs = [0x1234_5678, 0xdead_beef, 0, 0];
    let g = fn_returning_u256(limbs);
    let m = Matcher::new(&g);
    let pat = int_const_wide(WideConstStorage::U256(limbs));
    let matches = m.find_all(&pat);
    assert_eq!(
        matches.len(),
        1,
        "int_const_wide(value) must match the IntConstWide producing that value"
    );
}

#[test]
fn int_const_wide_does_not_match_different_value() {
    let g = fn_returning_u256([1, 0, 0, 0]);
    let m = Matcher::new(&g);
    let pat = int_const_wide(WideConstStorage::U256([2, 0, 0, 0]));
    assert!(
        m.find_all(&pat).is_empty(),
        "int_const_wide(other) must not match a different value"
    );
}

#[test]
fn any_wide_int_const_captures_node_id() {
    let limbs = [1, 2, 3, 4];
    let g = fn_returning_u256(limbs);
    let c = Capture::new();
    let m = Matcher::new(&g);
    let pat = any_wide_int_const(c);
    let matches = m.find_all(&pat);
    assert_eq!(matches.len(), 1, "any_wide_int_const must match the IntConstWide");
    let bytes = matches[0]
        .get_wide_bytes(c, &g.graph)
        .expect("get_wide_bytes must yield the stored value");
    let expected = WideConstStorage::U256(limbs).to_le_bytes();
    assert_eq!(bytes, expected);
}

#[test]
fn match_get_wide_bytes_returns_none_for_narrow_const() {
    // Narrow IntConst captured + queried with get_wide_bytes returns None —
    // the typed split is the API contract.
    let mut t = Tb::empty();
    let n = t.u64(42);
    let g = t.ret_val(n);
    let c = Capture::new();
    let m = Matcher::new(&g);
    let pat = any_int_const(c);
    let matches = m.find_all(&pat);
    assert!(!matches.is_empty());
    assert!(
        matches[0].get_wide_bytes(c, &g.graph).is_none(),
        "get_wide_bytes must return None for narrow IntConst"
    );
}
