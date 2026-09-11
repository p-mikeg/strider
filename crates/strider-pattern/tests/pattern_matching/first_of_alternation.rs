//! `first_of` (ordered choice) against `one_of` (union): the cut, and where it
//! must not fire.

use std::cell::Cell;
use std::rc::Rc;

use strider_ir::IntUnaryOp;
use strider_ir_test_utils::Tb;
use strider_pattern::matcher::MatcherBuilder;
use strider_pattern::{
    Capture, CaptureExt, MatchPat, Matcher, anything, first_of, int_add, int_neg, one_of, ret, var,
};

/// `int_neg(int_add(5, int_mul(7, 9)))`.
fn neg_add_5_mul() -> strider_ir::Function {
    let mut t = Tb::empty();
    let five = t.u64(5);
    let seven = t.u64(7);
    let nine = t.u64(9);
    let prod = t.mul(seven, nine);
    let sum = t.add(five, prod);
    let n = t.int_un(sum, IntUnaryOp::Neg);
    t.ret_val(n)
}

/// `int_add(5, 3)`.
fn add_5_3() -> strider_ir::Function {
    let mut t = Tb::empty();
    let five = t.u64(5);
    let three = t.u64(3);
    let sum = t.add(five, three);
    t.ret_val(sum)
}

/// A guard above the alternation rejects every configuration of arm 1, so the
/// choice must fall through to arm 2. `int_add(5, int_mul(7,9))` is commutative, so
/// arm 2 binds `z` two ways.
#[test]
fn first_of_falls_through_to_a_later_arm_when_a_guard_above_rejects() {
    let f = neg_add_5_mul();
    let m = Matcher::new(&f);
    let z = Capture::new();

    let union = int_neg(one_of![
        int_add(anything(), anything()),
        int_add(var(z), anything())
    ])
    .when_match(move |_m, _ty, b| b.get_value(z).is_some())
    .into_pattern();
    assert_eq!(m.find_all(&union).unwrap().len(), 2, "one_of backtracks");

    let ordered = int_neg(first_of![
        int_add(anything(), anything()),
        int_add(var(z), anything())
    ])
    .when_match(move |_m, _ty, b| b.get_value(z).is_some())
    .into_pattern();
    assert_eq!(m.find_all(&ordered).unwrap().len(), 2);
}

/// Same, with the guard on the alternation node itself.
#[test]
fn first_of_falls_through_when_the_alternations_own_guard_rejects() {
    let f = neg_add_5_mul();
    let m = Matcher::new(&f);
    let z = Capture::new();

    let ordered = int_neg(
        first_of![int_add(anything(), anything()), int_add(var(z), anything())]
            .when_match(move |_m, _ty, b| b.get_value(z).is_some()),
    )
    .into_pattern();
    assert_eq!(m.find_all(&ordered).unwrap().len(), 2);
}

/// With nothing rejecting it, arm 1 wins and arm 2 is never reported.
#[test]
fn first_of_reports_only_the_first_matching_arm() {
    let f = add_5_3();
    let m = Matcher::new(&f);
    let x = Capture::new();
    let y = Capture::new();

    let union = one_of![
        int_add(var(x), anything()).ordered(),
        int_add(anything(), var(y)).ordered()
    ]
    .into_pattern();
    assert_eq!(m.find_all(&union).unwrap().len(), 2);

    let ordered = first_of![
        int_add(var(x), anything()).ordered(),
        int_add(anything(), var(y)).ordered()
    ]
    .into_pattern();
    let hits = m.find_all(&ordered).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(x).is_some());
    assert!(hits[0].value(y).is_none());
}

/// Counts continuations reaching the root guard: `one_of` is multiplicative in
/// the number of nested alternations, `first_of` cuts at the first match.
fn root_continuations(f: &strider_ir::Function, ordered: bool) -> usize {
    let n = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&n);
    let pat = if ordered {
        int_add(
            first_of![anything(), anything()],
            first_of![anything(), anything()],
        )
        .ordered()
        .when_match(move |_m, _ty, _b| {
            seen.set(seen.get() + 1);
            true
        })
        .into_pattern()
    } else {
        int_add(
            one_of![anything(), anything()],
            one_of![anything(), anything()],
        )
        .ordered()
        .when_match(move |_m, _ty, _b| {
            seen.set(seen.get() + 1);
            true
        })
        .into_pattern()
    };
    let m = Matcher::new(f);
    let _ = m.find_all(&pat).unwrap();
    n.get()
}

#[test]
fn one_of_explores_every_arm_while_first_of_cuts() {
    let f = add_5_3();
    assert_eq!(root_continuations(&f, false), 4, "2 arms x 2 alternations");
    assert_eq!(root_continuations(&f, true), 1);
}

/// A capture on the alternation's own output vertex has nothing to bind when
/// the arm is a value-less kind, so the match must be rejected rather than
/// reported with the capture missing. The same vertex without an alternation
/// under it is the reference.
#[test]
fn an_alternation_root_never_reports_an_unbound_vertex_capture() {
    let f = add_5_3();
    let m = Matcher::new(&f);
    let c = Capture::new();

    let mut b = MatcherBuilder::new();
    let out = ret().compile(&mut b);
    b.capture_output(out, c);
    let direct = b.finish();
    assert_eq!(m.find_all(&direct).unwrap().len(), 0);

    let mut b = MatcherBuilder::new();
    let arm = ret().compile(&mut b);
    let alt = b.first_of(&[arm]);
    b.capture_output(alt, c);
    let alternation = b.finish();
    assert!(alternation.guaranteed_captures().unwrap().contains(&c));
    assert_eq!(
        m.find_all(&alternation).unwrap().len(),
        0,
        "guaranteed_captures reports `c`, so a reported match would be a lie"
    );
}

/// Caller-supplied logic holds the same `Matcher` and may run a query of its
/// own. Those matches must not count towards an enclosing arm's cut.
///
/// `satisfied` is shared across nested WALKS on purpose, so a `first_of` inside
/// an `IfPat` branch does not cut on the hand-off. Sharing it across nested
/// QUERIES too made an arm that produced NOTHING look like it had produced a
/// match: the cut fired and the later arm holding the real match was never
/// tried.
#[test]
fn a_nested_query_in_a_guard_does_not_fire_an_enclosing_cut() {
    let function = neg_add_5_mul();
    let m = Matcher::new(&function);

    let hits = |nested: bool| {
        let probe = int_add(anything(), anything()).into_pattern();
        let seen = Rc::new(Cell::new(0usize));
        let armed = Rc::clone(&seen);
        let k = Capture::new();
        let pat = first_of![
            // Structurally matches, then rejects: it produces no match, so the
            // cut must not fire.
            int_add(anything(), anything()).when_match(move |mm, _ty, _b| {
                if nested {
                    let _ = mm.find_all(&probe).unwrap();
                }
                armed.set(armed.get() + 1);
                false
            }),
            int_add(anything(), anything()).capture(k),
        ]
        .into_pattern();
        let n = m.find_all(&pat).unwrap().len();
        (n, seen.get())
    };

    let (plain, _) = hits(false);
    let (with_nested, guard_runs) = hits(true);
    assert!(guard_runs > 0, "the guard must actually have run");
    assert_eq!(
        with_nested, plain,
        "a query run inside caller logic must not change the result",
    );
    assert_eq!(plain, 1, "the second arm still matches");
}
