//! `one_of` alternation: a pattern position that matches whichever of several
//! shapes the value has — the "optional wrapper" case, e.g. an address that may
//! or may not be masked (`add(base, off)` vs `and(add(base, off), mask)`).

use strider_ir::{IntBinaryOp, IntUnaryOp};
use strider_ir_test_utils::Tb;
use strider_pattern::{
    Capture, MatchPat, Matcher, add, and, any_int_const, int_const, mul, neg, one_of, var,
};

/// At the root, `one_of` matches every node matching any alternative.
#[test]
fn one_of_matches_any_alternative_at_root() {
    // A graph containing both an `add(5, 3)` and a `mul(5, 3)`.
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let sum = t.add(a, b);
    let prod = t.mul(a, b);
    let root = t.add(sum, prod); // keep both reachable
    let f = t.ret_val(root);
    let m = Matcher::new(&f);

    let pat = one_of![
        add(int_const(5u128), int_const(3u128)),
        mul(int_const(5u128), int_const(3u128)),
    ]
    .into_pattern();

    // matches the add(5,3) and the mul(5,3) — the outer add(sum,prod) matches
    // neither alternative (its operands are not the two constants).
    assert_eq!(m.find_all(&pat).unwrap().len(), 2);
}

/// Nested `one_of` matches an optionally-wrapped inner shape, binding a shared
/// capture in whichever branch fires — the motivating "maybe-masked address".
#[test]
fn one_of_matches_optionally_masked_inner_with_shared_capture() {
    // neg(add(11, 7))                 -> the unmasked branch
    // neg(and(add(22, 7), 0xff))      -> the masked branch
    let mut t = Tb::empty();
    let add1 = {
        let c = t.u64(11);
        let seven = t.u64(7);
        t.add(c, seven)
    };
    let neg1 = t.int_un(add1, IntUnaryOp::Neg);

    let and2 = {
        let c = t.u64(22);
        let seven = t.u64(7);
        let inner = t.add(c, seven);
        let mask = t.u64(0xff);
        t.int_bin(inner, mask, IntBinaryOp::And)
    };
    let neg2 = t.int_un(and2, IntUnaryOp::Neg);

    let root = t.add(neg1, neg2); // keep both reachable
    let f = t.ret_val(root);
    let m = Matcher::new(&f);

    let base = Capture::new();
    // neg( one_of( add(base, 7) , and(add(base, 7), any_const) ) )
    let pat = neg(one_of![
        add(var(base), int_const(7u128)),
        and(add(var(base), int_const(7u128)), any_int_const()),
    ])
    .into_pattern();

    let hits = m.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 2, "both the masked and unmasked negs should match");
    // the shared capture is bound in whichever branch fired (both matches).
    let bound = hits.iter().filter(|h| h.value(base).is_some()).count();
    assert_eq!(bound, 2, "base captured in each match");
}
