use strider_ir::{IntBinaryOp, IntUnaryOp};
use strider_ir_test_utils::Tb;
use strider_pattern::{
    Capture, CaptureExt, MatchPat, Matcher, OneOf, any_int_const, anything, int_add, int_and,
    int_const, int_mul, int_neg, int_xor, one_of, ret, var,
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
        int_add(int_const(5u128), int_const(3u128)),
        int_mul(int_const(5u128), int_const(3u128)),
    ]
    .into_pattern();

    // The outer int_add(sum, prod) matches neither alternative: its operands are
    // not the two constants.
    assert_eq!(m.find_all(&pat).unwrap().len(), 2);
}

/// A shared capture must bind in whichever alternative fires, so a nested
/// `one_of` can match an optionally-wrapped inner shape.
#[test]
fn one_of_matches_optionally_masked_inner_with_shared_capture() {
    // int_neg(int_add(11, 7))            unmasked branch
    // int_neg(int_and(int_add(22, 7), 0xff)) masked branch
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
    let pat = int_neg(one_of![
        int_add(var(base), int_const(7u128)),
        int_and(int_add(var(base), int_const(7u128)), any_int_const()),
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

/// Overlapping arms both fire: `one_of` is a union, not a first-match cut. A
/// permissive arm and a narrower arm that match the same node each contribute
/// their own binding, so a downstream constraint can select the one it needs.
#[test]
fn one_of_enumerates_every_matching_arm() {
    // int_add(int_mul(5, 3), 7): a single add node that both arms below match.
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let prod = t.mul(a, b);
    let seven = t.u64(7);
    let node = t.add(prod, seven);
    let f = t.ret_val(node);
    let m = Matcher::new(&f);

    let off = Capture::new();
    // `.ordered()` on each arm keeps the count clean (no commutative retry).
    let pat = one_of![
        int_add(anything(), anything()).ordered(), // matches, binds no `off`
        int_add(var(off), any_int_const()).ordered(), // matches, off = the mul operand
    ]
    .into_pattern();

    let hits = m.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 2, "both arms fire on the one add node");
    let bound = hits.iter().filter(|h| h.value(off).is_some()).count();
    assert_eq!(bound, 1, "only the specific arm binds `off`");
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
        int_add(int_const(5u128), int_const(3u128)),
        one_of![
            int_mul(int_const(5u128), int_const(3u128)),
            int_xor(int_const(5u128), int_const(3u128)),
        ],
    ]
    .into_pattern();

    assert_eq!(m.find_all(&pat).unwrap().len(), 3);
}

/// A `when_match` guard attached to the alternation itself must run.
///
/// The alternation branch of `try_match_at` returns before `finalize`, so the
/// guard has to be consulted on the alternation path of its own; skipping it
/// reports every arm match.
#[test]
fn when_match_on_the_alternation_itself_is_honoured() {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let sum = t.add(a, b);
    let prod = t.mul(a, b);
    let root = t.add(sum, prod);
    let f = t.ret_val(root);
    let m = Matcher::new(&f);

    let accepting = one_of![
        int_add(int_const(5u128), int_const(3u128)),
        int_mul(int_const(5u128), int_const(3u128)),
    ]
    .when_match(|_, _, _| true)
    .into_pattern();
    assert_eq!(
        m.find_all(&accepting).unwrap().len(),
        2,
        "baseline: an accepting guard keeps both arm matches"
    );

    let rejecting = one_of![
        int_add(int_const(5u128), int_const(3u128)),
        int_mul(int_const(5u128), int_const(3u128)),
    ]
    .when_match(|_, _, _| false)
    .into_pattern();
    assert!(
        m.find_all(&rejecting).unwrap().is_empty(),
        "a rejecting guard on the alternation must suppress every arm match"
    );
}

/// An arm is a pattern, whatever slot flavour it roots on: a node-rooted
/// control builder alternates with a value shape.
#[test]
fn one_of_takes_a_control_rooted_arm() {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let sum = t.add(a, b);
    let f = t.ret_val(sum);
    let m = Matcher::new(&f);

    let pat = one_of![ret(), int_add(int_const(5u128), int_const(3u128))].into_pattern();
    assert_eq!(m.find_all(&pat).unwrap().len(), 2);
}

/// No alternative can fire, at the root as much as nested, so a caller
/// assembling the arms at runtime needs no empty-list special case.
#[test]
fn empty_alternation_matches_nothing() {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let sum = t.add(a, b);
    let f = t.ret_val(sum);
    let m = Matcher::new(&f);

    let root = OneOf::new(Vec::new()).into_pattern();
    assert!(m.find_all(&root).unwrap().is_empty());

    let nested = int_add(OneOf::new(Vec::new()), anything()).into_pattern();
    assert!(m.find_all(&nested).unwrap().is_empty());

    let ordered = OneOf::first(Vec::new()).into_pattern();
    assert!(m.find_all(&ordered).unwrap().is_empty());
}
