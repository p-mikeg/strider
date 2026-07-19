//! `one_of` alternation: one pattern position matching whichever of several
//! shapes the value has. The motivating case is an optional wrapper, e.g. an
//! address that may or may not be masked.

use strider_ir::{IntBinaryOp, IntUnaryOp};
use strider_ir_test_utils::Tb;
use strider_pattern::{
    Capture, MatchPat, Matcher, add, and, any_int_const, int_const, mul, neg, one_of, var, xor,
};

#[test]
fn one_of_matches_any_alternative_at_root() {
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

    // The outer add(sum, prod) matches neither alternative: its operands are
    // not the two constants.
    assert_eq!(m.find_all(&pat).unwrap().len(), 2);
}

/// A shared capture must bind in whichever alternative fires, so a nested
/// `one_of` can match an optionally-wrapped inner shape.
#[test]
fn one_of_matches_optionally_masked_inner_with_shared_capture() {
    // neg(add(11, 7))            unmasked branch
    // neg(and(add(22, 7), 0xff)) masked branch
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
    let pat = neg(one_of![
        add(var(base), int_const(7u128)),
        and(add(var(base), int_const(7u128)), any_int_const()),
    ])
    .into_pattern();

    let hits = m.find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        2,
        "both the masked and unmasked negs should match"
    );
    let bound = hits.iter().filter(|h| h.value(base).is_some()).count();
    assert_eq!(bound, 2, "base captured in each match");
}

/// An alternative may itself be a `one_of`; the whole matches the flattened
/// union of the leaves.
#[test]
fn one_of_nests_recursively() {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let sum = t.add(a, b);
    let prod = t.mul(a, b);
    let xored = t.bxor(a, b);
    let ab = t.add(sum, prod);
    let root = t.add(ab, xored); // keep all three reachable
    let f = t.ret_val(root);
    let m = Matcher::new(&f);

    let pat = one_of![
        add(int_const(5u128), int_const(3u128)),
        one_of![
            mul(int_const(5u128), int_const(3u128)),
            xor(int_const(5u128), int_const(3u128)),
        ],
    ]
    .into_pattern();

    assert_eq!(m.find_all(&pat).unwrap().len(), 3);
}
