//! `first_of` (ordered choice) against `one_of` (union): the cut, and where it
//! must not fire.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::support::{Tb, shapes};
use strider_ir::IRViewer;
use strider_ir::node::{NodeKind, ValueKind};
use strider_ir::{IntBinaryOp, IntUnaryOp};
use strider_pattern::matcher::{KindSpec, MatcherBuilder, PatValueRef};
use strider_pattern::{
    Capture, CaptureExt, MatchPat, Matcher, anything, first_of, int_add, int_neg, int_xor, one_of,
    ret, var,
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
    let f = shapes::add_consts(5, 3);
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
/// the number of nested alternations, the first `first_of` built cuts at its
/// first match.
///
/// The root capture is what keeps the count meaningful: a pattern binding
/// nothing a caller can see has one answer per root, and `find_all` then stops
/// at the first configuration instead of driving them all.
fn root_continuations(f: &strider_ir::Function, ordered: bool) -> usize {
    let n = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&n);
    let c = Capture::new();
    let pat = if ordered {
        int_add(
            first_of![anything(), anything()],
            first_of![anything(), anything()],
        )
        .ordered()
        .capture(c)
        .when_match(move |_m, _ty, _b| {
            seen.fetch_add(1, Ordering::Relaxed);
            true
        })
        .into_pattern()
    } else {
        int_add(
            one_of![anything(), anything()],
            one_of![anything(), anything()],
        )
        .ordered()
        .capture(c)
        .when_match(move |_m, _ty, _b| {
            seen.fetch_add(1, Ordering::Relaxed);
            true
        })
        .into_pattern()
    };
    let m = Matcher::new(f);
    let _ = m.find_all(&pat).unwrap();
    n.load(Ordering::Relaxed)
}

#[test]
fn one_of_explores_every_arm_while_first_of_cuts() {
    let f = shapes::add_consts(5, 3);
    assert_eq!(root_continuations(&f, false), 4, "2 arms x 2 alternations");
    assert_eq!(
        root_continuations(&f, true),
        2,
        "the second first_of's arms all run"
    );
}

/// A capture on the alternation's own output vertex has nothing to bind when
/// the arm is a value-less kind, so the match must be rejected rather than
/// reported with the capture missing. The same vertex without an alternation
/// under it is the reference.
#[test]
fn an_alternation_root_never_reports_an_unbound_vertex_capture() {
    let f = shapes::add_consts(5, 3);
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
/// own. Those matches must not count towards an enclosing arm's cut: counted,
/// an arm that produced NOTHING looks like it produced a match, the cut fires
/// and the later arm holding the real match is never tried.
#[test]
fn a_nested_query_in_a_guard_does_not_fire_an_enclosing_cut() {
    let function = neg_add_5_mul();
    let m = Matcher::new(&function);

    let hits = |nested: bool| {
        let probe = int_add(anything(), anything()).into_pattern();
        let seen = Arc::new(AtomicUsize::new(0));
        let armed = Arc::clone(&seen);
        let k = Capture::new();
        let pat = first_of![
            // Structurally matches, then rejects: it produces no match, so the
            // cut must not fire.
            int_add(anything(), anything()).when_match(move |mm, _ty, _b| {
                if nested {
                    let _ = mm.find_all(&probe).unwrap();
                }
                armed.fetch_add(1, Ordering::Relaxed);
                false
            }),
            int_add(anything(), anything()).capture(k),
        ]
        .into_pattern();
        let n = m.find_all(&pat).unwrap().len();
        (n, seen.load(Ordering::Relaxed))
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

/// `add(x, add(x, z))`.
fn add_x_add_x_z() -> strider_ir::Function {
    let mut t = Tb::empty();
    let x = t.u64(5);
    let z = t.u64(7);
    let inner = t.add(x, z);
    let outer = t.add(x, inner);
    t.ret_val(outer)
}

/// A `first_of` in one slot of a commutative node and its sibling in the other.
/// Whichever slot spells the alternation, the sibling's configurations are the
/// same set, so the cut must leave the same arms and the query the same answer.
#[test]
fn first_of_answers_the_same_in_both_mirror_spellings() {
    let f = add_x_add_x_z();
    let m = Matcher::new(&f);
    let k = Capture::new();

    // Enumeration order follows the spelling; the SET of bindings must not.
    let bound = |pat: strider_pattern::Pattern| -> Vec<Option<u32>> {
        let mut v: Vec<_> = m
            .find_all(&pat)
            .unwrap()
            .iter()
            .map(|h| h.value(k).map(|v| v.as_u32()))
            .collect();
        v.sort();
        v
    };

    // Arm 1 takes the inner `add` and arm 2 the bare operand, so arm 1 matching
    // for one ordering must leave arm 2 reachable for the other. An arm 1 that
    // matched everything would make the cut unobservable here.
    let alternation = || {
        first_of![
            int_add(anything(), anything()).capture(k),
            anything().capture(k)
        ]
    };
    let alternation_first = bound(int_add(alternation(), anything()).into_pattern());
    let alternation_second = bound(int_add(anything(), alternation()).into_pattern());
    assert_eq!(
        alternation_first.len(),
        4,
        "the alternation reaches both operands of both adds"
    );
    assert_eq!(alternation_first, alternation_second);
}

/// `add(int_neg(5), int_xor(7, 9))`.
fn add_neg_xor() -> strider_ir::Function {
    let mut t = Tb::empty();
    let five = t.u64(5);
    let seven = t.u64(7);
    let nine = t.u64(9);
    let neg = t.int_un(five, IntUnaryOp::Neg);
    let xor = t.int_bin(seven, nine, strider_ir::IntBinaryOp::Xor);
    let sum = t.add(neg, xor);
    t.ret_val(sum)
}

/// The alternation's two arms each match one operand of a commutative node, so
/// the two orderings offer it two DIFFERENT IR nodes. An arm that matched one
/// of them must stay available for the other, and the count must not depend on
/// which slot spells the alternation.
#[test]
fn first_of_keeps_a_later_arm_for_the_other_commutative_operand() {
    let f = add_neg_xor();
    let m = Matcher::new(&f);

    let c = Capture::new();
    let alternation = || {
        first_of![
            int_neg(anything()).capture(c),
            int_xor(anything(), anything()).capture(c)
        ]
    };
    // Enumeration order follows the spelling; the SET of bindings must not.
    let bound = |pat: strider_pattern::Pattern| -> Vec<Option<u32>> {
        let mut v: Vec<_> = m
            .find_all(&pat)
            .unwrap()
            .iter()
            .map(|h| h.value(c).map(|v| v.as_u32()))
            .collect();
        v.sort();
        v
    };

    let in_slot_0 = bound(int_add(alternation(), anything()).into_pattern());
    let in_slot_1 = bound(int_add(anything(), alternation()).into_pattern());
    assert_eq!(
        in_slot_0.len(),
        2,
        "one match per operand the alternation can take"
    );
    assert_eq!(
        in_slot_0, in_slot_1,
        "the slot spelling the alternation cannot change the answer"
    );
}

/// A root `first_of` cuts per anchoring output: the control arm matching the
/// `Call`'s control output must not keep the memory arm from its memory
/// output.
#[test]
fn a_root_first_of_cut_does_not_carry_to_the_next_output() {
    let mut t = Tb::empty();
    t.call_at(0x1234);
    let function = t.ret_nothing();
    let call = || KindSpec::Variant(std::mem::discriminant(&NodeKind::Call { cc: None }));
    let c = Capture::new();
    let mut b = MatcherBuilder::new();
    let ctl_node = b.node(call());
    let ctl = b.control_output(ctl_node, 0);
    let mem_node = b.node(call());
    let mem = b.memory_output(mem_node, 1);
    let alt = b.first_of(&[ctl, mem]);
    b.capture_output(alt, c);
    let pat = b.finish();
    let kinds: Vec<_> = Matcher::new(&function)
        .find_all(&pat)
        .unwrap()
        .iter()
        .map(|m| function.value_kind(m.value(c).expect("c binds")))
        .collect();
    assert_eq!(kinds, [ValueKind::Control, ValueKind::Memory]);
}

/// `add(neg(5), 5)` when `neg_first`, else `add(5, neg(5))`.
fn add_neg_5_and_5(neg_first: bool) -> strider_ir::Function {
    let mut t = Tb::empty();
    let five = t.u64(5);
    let neg = t.int_un(five, IntUnaryOp::Neg);
    let sum = if neg_first {
        t.add(neg, five)
    } else {
        t.add(five, neg)
    };
    t.ret_val(sum)
}

/// The cut holds for the whole attempt: once arm 1 matched an IR node, a later
/// configuration of a sibling enumerated AROUND the alternation re-offers that
/// node without reaching arm 2, as when the sibling is enumerated inside it.
#[test]
fn a_first_of_cut_outlives_the_invocation_that_recorded_it() {
    let x = Capture::new();
    let y = Capture::new();
    // Arm 1 of the sibling binds `x` to `5`, arm 2 to `neg(5)`.
    let sibling = || one_of![int_neg(var(x)), var(x)];
    let alternation = || first_of![var(x), var(y)];
    let rows = |f: &strider_ir::Function, pat: strider_pattern::Pattern| -> Vec<(bool, bool)> {
        Matcher::new(f)
            .find_all(&pat)
            .unwrap()
            .iter()
            .map(|h| (h.value(x).is_some(), h.value(y).is_some()))
            .collect()
    };
    let around = rows(
        &add_neg_5_and_5(true),
        int_add(sibling(), alternation()).ordered().into_pattern(),
    );
    let inside = rows(
        &add_neg_5_and_5(false),
        int_add(alternation(), sibling()).ordered().into_pattern(),
    );
    assert_eq!(inside, [(true, false)]);
    assert_eq!(around, inside, "arm 2 is never reached once arm 1 matched");
}

/// The sibling's arms reversed: arm 1 of the sibling now binds `x` to
/// `neg(5)`, so arm 1 of the alternation first fails and arm 2 matches, and
/// only the sibling's next configuration brings arm 1 a match. A union's arm
/// order carries no meaning, so the answer must not move with it.
#[test]
fn a_first_of_answer_does_not_depend_on_a_siblings_arm_order() {
    let x = Capture::new();
    let y = Capture::new();
    let sibling = || one_of![var(x), int_neg(var(x))];
    let alternation = || first_of![var(x), var(y)];
    let rows = |f: &strider_ir::Function, pat: strider_pattern::Pattern| -> Vec<(bool, bool)> {
        Matcher::new(f)
            .find_all(&pat)
            .unwrap()
            .iter()
            .map(|h| (h.value(x).is_some(), h.value(y).is_some()))
            .collect()
    };
    let around = rows(
        &add_neg_5_and_5(true),
        int_add(sibling(), alternation()).ordered().into_pattern(),
    );
    let inside = rows(
        &add_neg_5_and_5(false),
        int_add(alternation(), sibling()).ordered().into_pattern(),
    );
    assert_eq!(inside, [(true, false)]);
    assert_eq!(around, inside, "the lowest arm that matches wins");
}

/// `match_at` stops at its first match, which arm 2 of the alternation
/// reaches before the sibling's next configuration brings arm 1 one.
#[test]
fn match_at_takes_the_lowest_first_of_arm_that_matches() {
    let x = Capture::new();
    let y = Capture::new();
    let f = add_neg_5_and_5(true);
    let pat = int_add(one_of![var(x), int_neg(var(x))], first_of![var(x), var(y)])
        .ordered()
        .into_pattern();
    let any_add = int_add(anything(), anything()).into_pattern();
    let sum = Matcher::new(&f).find_all(&any_add).unwrap()[0].root();
    let hit = Matcher::new(&f)
        .match_at(sum, &pat)
        .unwrap()
        .expect("matches");
    assert!(hit.value(y).is_none(), "arm 2 lost to arm 1");
    assert_eq!(f.int_const_u128(hit.value(x).unwrap()), Some(5));
}

/// `add(xor(b, a), add(a, b))`: the sibling under `rows` binds `x` to `b` in
/// one configuration and to `a` in the other.
fn xor_ba_plus_add_ab() -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let s = t.int_bin(b, a, IntBinaryOp::Xor);
    let inner = t.add(a, b);
    let sum = t.add(s, inner);
    t.ret_val(sum)
}

/// `(x, y bound, w bound)` per match of `int_add(sibling, inner)`, the sibling
/// binding `x` to `b` first, then to `a`, or the reverse.
fn under_sibling(
    f: &strider_ir::Function,
    b_first: bool,
    inner: impl Fn(&mut MatcherBuilder) -> PatValueRef,
    (x, y, w): (Capture, Capture, Capture),
) -> Vec<(Option<u128>, bool, bool)> {
    let to_b = || int_xor(var(x), anything()).ordered();
    let to_a = || int_xor(anything(), var(x)).ordered();
    let sibling = if b_first {
        one_of![to_b(), to_a()]
    } else {
        one_of![to_a(), to_b()]
    };
    let mut b = MatcherBuilder::new();
    let sibling = MatchPat::compile(sibling, &mut b);
    let inner = inner(&mut b);
    let sum = b.binary(IntBinaryOp::Add, sibling, inner);
    b.set_force_ordered(sum);
    Matcher::new(f)
        .find_all(&b.finish())
        .unwrap()
        .iter()
        .map(|h| {
            let x = h.value(x).and_then(|v| f.int_const_u128(v));
            (x, h.value(y).is_some(), h.value(w).is_some())
        })
        .collect()
}

/// Two `first_of`s whose arms conflict through `x`, under a sibling that binds
/// `x` in either order. They settle in the order the pattern spells them: the
/// left one keeps arm 1 (`x` = `a`), which leaves the right one arm 2.
#[test]
fn two_first_ofs_settle_left_to_right_whatever_the_enumeration_order() {
    let f = xor_ba_plus_add_ab();
    let caps = (Capture::new(), Capture::new(), Capture::new());
    let (x, y, w) = caps;
    let inner = |b: &mut MatcherBuilder| {
        let sum = int_add(first_of![var(x), var(y)], first_of![var(x), var(w)]);
        MatchPat::compile(sum.ordered(), b)
    };
    let b_first = under_sibling(&f, true, inner, caps);
    assert_eq!(b_first, [(Some(1), false, true)]);
    assert_eq!(under_sibling(&f, false, inner, caps), b_first);
}

/// One `first_of` reached at `a` and at `b` in the same match settles at the
/// earlier-built node first: `a` keeps arm 1, which leaves `b` arm 2.
#[test]
fn a_shared_first_of_settles_per_node_whatever_the_enumeration_order() {
    let f = xor_ba_plus_add_ab();
    let caps = (Capture::new(), Capture::new(), Capture::new());
    let (x, y, _) = caps;
    let inner = |b: &mut MatcherBuilder| {
        let shared = MatchPat::compile(first_of![var(x), var(y)], b);
        let sum = b.binary(IntBinaryOp::Add, shared, shared);
        b.set_force_ordered(sum);
        sum
    };
    let b_first = under_sibling(&f, true, inner, caps);
    assert_eq!(b_first, [(Some(1), true, false)]);
    assert_eq!(under_sibling(&f, false, inner, caps), b_first);
}

/// The kinds `x` binds in `pat`'s matches, sorted.
fn x_kinds(f: &strider_ir::Function, pat: &strider_pattern::Pattern, x: Capture) -> Vec<String> {
    let mut v: Vec<String> = Matcher::new(f)
        .find_all(pat)
        .unwrap()
        .iter()
        .map(|h| format!("{:?}", f.node_kind(f.producer(h.value(x).unwrap()))))
        .collect();
    v.sort();
    v
}

/// The cut `first_of` sits under a cast ladder. Skipping an arm that a later
/// configuration would have matched must not send the ladder through the cast
/// to a match the uncut search never offers, so the answer does not move with
/// a sibling union's arm order.
#[test]
fn a_cut_first_of_under_a_cast_ladder_ignores_a_siblings_arm_order() {
    let vn = super::support::reg_vn(0x40, 4);
    let mut t = Tb::with_vars(&[vn]);
    let r = t.read_var(&vn);
    let zx = t.zext_to(r, strider_ir::node::ValueType::int(64));
    let five = t.u64(5);
    let sum = t.add(five, zx);
    let f = t.ret_val(sum);
    let x = Capture::new();
    let rows = |var_first: bool| {
        let sibling = if var_first {
            one_of![var(x), anything()]
        } else {
            one_of![anything(), var(x)]
        };
        let pat = int_add(sibling, first_of![var(x), anything()])
            .ordered()
            .into_pattern()
            .ignore_casts();
        x_kinds(&f, &pat, x)
    };
    assert_eq!(rows(true), rows(false));
}

/// A `first_of` shared by both operands of one node is offered the same IR
/// node twice per match and may take a different arm each time. A guard
/// needing both arms leaves no match through arm 0 at both, so the one
/// through arm 0, then arm 1, is kept.
#[test]
fn a_first_of_offered_one_node_twice_keeps_a_guarded_match() {
    let mut t = Tb::empty();
    let x = t.f64(1.5);
    let eq = t.fcmp(x, x, strider_ir::FloatCmpOp::Equal);
    let one = t.boolean(true);
    let nan = t.bool_bin(eq, one, IntBinaryOp::Xor);
    let f = t.ret_val(nan);
    let (a, b) = (Capture::new(), Capture::new());
    let pat = strider_pattern::float_is_nan(first_of![var(a), var(b)])
        .when_match(move |_, _, bd| bd.get_value(a).is_some() && bd.get_value(b).is_some())
        .into_pattern();
    assert_eq!(Matcher::new(&f).find_all(&pat).unwrap().len(), 1);
}

/// Two outputs of one node are two values offered: an arm's match at one
/// proves nothing at the other. At `v1` only the second arm matches, so it
/// stays beside the first arm's match at `v0`.
#[test]
fn two_outputs_of_one_node_settle_a_first_of_apart() {
    let rax = super::support::reg_vn(0, 8);
    let rdx = super::support::reg_vn(16, 8);
    let mut t = Tb::with_vars(&[rax, rdx]);
    t.call_other("op", 1, &[], Some(rax), &[], &[rdx]);
    let v0 = t.read_var(&rax);
    let v1 = t.read_var(&rdx);
    let sum = t.add(v0, v1);
    let r = t.int_bin(sum, v0, IntBinaryOp::Xor);
    let f = t.ret_val(r);
    assert_eq!(f.producer(v0), f.producer(v1));
    let (fc, x) = (Capture::new(), Capture::new());
    let pat = int_xor(
        int_add(first_of![var(x), anything()].capture(fc), anything()),
        var(x),
    )
    .ordered()
    .into_pattern();
    let mut got: Vec<_> = Matcher::new(&f)
        .find_all(&pat)
        .unwrap()
        .iter()
        .map(|h| h.value(fc).unwrap().as_u32())
        .collect();
    got.sort_unstable();
    let mut want = [v0.as_u32(), v1.as_u32()];
    want.sort_unstable();
    assert_eq!(got, want);
}

/// A single-answer query stops at its first match when that match took the
/// first arm of every choice: the filter keeps it first. Here the natural
/// ordering of the commutative `add(5, mul)` is that match, so the swapped one,
/// through the second arm, never runs.
#[test]
fn a_single_answer_query_stops_at_a_first_arm_match() {
    let f = neg_add_5_mul();
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&runs);
    let pat = int_add(
        first_of![strider_pattern::any_int_const(), anything()],
        anything(),
    )
    .when_match(move |_, _, _| {
        seen.fetch_add(1, Ordering::Relaxed);
        true
    })
    .into_pattern();
    let m = Matcher::new(&f);
    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
    assert_eq!(runs.swap(0, Ordering::Relaxed), 1);
    let add = (f.graph().all_node_ids())
        .find(|&n| matches!(f.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();
    assert!(m.match_at(add, &pat).unwrap().is_some());
    assert_eq!(runs.load(Ordering::Relaxed), 1);
}

/// The match that stops a single-answer query need not be the first found: any
/// match through the first arm of every choice is kept. On `add(5, mul)` the
/// first union arm offers its `first_of` the `mul`, so that match takes the
/// second arm; the second union arm's first-arm match at `5` then stops the
/// query before its own second arm runs.
#[test]
fn a_single_answer_query_stops_at_a_later_first_arm_match() {
    let f = neg_add_5_mul();
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = |p: strider_pattern::MatchOnly| {
        let seen = Arc::clone(&runs);
        p.when_match(move |_, _, _| {
            seen.fetch_add(1, Ordering::Relaxed);
            true
        })
    };
    let ordered = int_add(
        anything(),
        first_of![strider_pattern::any_int_const(), anything()],
    )
    .ordered();
    let swapped = int_add(
        first_of![strider_pattern::any_int_const(), anything()],
        anything(),
    )
    .ordered();
    let pat = one_of![counted(ordered), counted(swapped)].into_pattern();
    let add = (f.graph().all_node_ids())
        .find(|&n| matches!(f.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();
    assert!(Matcher::new(&f).match_at(add, &pat).unwrap().is_some());
    assert_eq!(runs.load(Ordering::Relaxed), 2);
}

/// A union arm beside a shared `first_of` keeps the `first_of`'s own match:
/// `float_is_nan` reaches its operand once per slot, so the match through
/// `var(x)` at both slots ties with one that reached it at a single slot.
#[test]
fn a_union_beside_a_shared_first_of_keeps_its_match() {
    let mut t = Tb::empty();
    let v = t.f64(1.5);
    let eq = t.fcmp(v, v, strider_ir::FloatCmpOp::Equal);
    let one = t.boolean(true);
    let nan = t.bool_bin(eq, one, IntBinaryOp::Xor);
    let f = t.ret_val(nan);
    let (x, y) = (Capture::new(), Capture::new());
    let rows = |pat: strider_pattern::Pattern| {
        let mut v: Vec<(bool, bool)> = Matcher::new(&f)
            .find_all(&pat)
            .unwrap()
            .iter()
            .map(|h| (h.value(x).is_some(), h.value(y).is_some()))
            .collect();
        v.sort_unstable();
        v
    };
    let fo = || first_of![var(x), anything()];
    let want = [(false, true), (true, false), (true, true)];
    let by_var = strider_pattern::float_is_nan(one_of![fo(), var(y)]);
    assert_eq!(rows(by_var.into_pattern()), want);
    let by_capture = strider_pattern::float_is_nan(one_of![fo(), anything().capture(y)]);
    assert_eq!(rows(by_capture.into_pattern()), want);
}

/// `float_is_nan(a)` over `a = int_bits_to_float(r0)`.
fn nan_of_bits() -> strider_ir::Function {
    let r0 = super::support::reg_vn(0, 8);
    let mut t = Tb::with_vars(&[r0]);
    let i = t.read_var(&r0);
    let a = t.int_bits_to_float(i, strider_ir::node::ValueType::float(64));
    let eq = t.fcmp(a, a, strider_ir::FloatCmpOp::Equal);
    let nan = t.bool_not(eq);
    t.ret_val(nan)
}

/// Which of `caps` each match of `pat` binds, sorted.
fn bound(
    f: &strider_ir::Function,
    pat: strider_pattern::Pattern,
    caps: &[Capture],
) -> Vec<Vec<bool>> {
    let mut v: Vec<Vec<bool>> = Matcher::new(f)
        .find_all(&pat)
        .unwrap()
        .iter()
        .map(|h| caps.iter().map(|&c| h.value(c).is_some()).collect())
        .collect();
    v.sort_unstable();
    v
}

/// A guard below each arm reads the bindings from before that arm binds, so
/// the two reads of `float_is_nan` differ: after arm 0 at the first read,
/// arms 0 and 1 fail at the second. The first arm that matches at each read
/// is arm 0, then arm 2, though other matches took arm 1 at both.
#[test]
fn a_shared_first_of_takes_the_first_matching_arm_at_each_read() {
    let f = nan_of_bits();
    let (x, y, z) = (Capture::new(), Capture::new(), Capture::new());
    let g = move || anything().when_match(move |_, _, b| b.get_value(x).is_none());
    let pat = strider_pattern::float_is_nan(first_of![
        strider_pattern::int_bits_to_float(g()).capture(x),
        strider_pattern::int_bits_to_float(g()).capture(y),
        var(z)
    ])
    .into_pattern();
    assert_eq!(bound(&f, pat, &[x, y, z]), [vec![true, false, true]]);
}

/// Arm 0's guard needs `c`, which only arm 1 binds: it fails at the first
/// read and holds at the second, after arm 1.
#[test]
fn a_shared_first_of_arm_a_guard_admits_at_the_second_read_wins_there() {
    let f = nan_of_bits();
    let (x, c) = (Capture::new(), Capture::new());
    let pat = strider_pattern::float_is_nan(first_of![
        var(x).when_match(move |_, _, b| b.get_value(c).is_some()),
        anything().capture(c)
    ])
    .into_pattern();
    assert_eq!(bound(&f, pat, &[x, c]), [vec![true, true]]);
}

/// A root guard needing `z`, which only the `first_of`'s arm 1 binds: arm 0
/// at the first read, then arm 1, is the first arm that matches at each.
#[test]
fn a_shared_first_of_under_a_root_guard_takes_the_first_arm_at_each_read() {
    let f = nan_of_bits();
    let (x, y, z) = (Capture::new(), Capture::new(), Capture::new());
    let pat = strider_pattern::float_is_nan(one_of![
        first_of![var(x), anything().capture(z)],
        anything().capture(y)
    ])
    .when_match(move |_, _, b| b.get_value(z).is_some())
    .into_pattern();
    assert_eq!(bound(&f, pat, &[x, y, z]), [vec![true, false, true]]);
}
