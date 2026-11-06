use super::support::{Tb, reg_vn, shapes};
use strider_ir::IRViewer as _;
use strider_ir::node::ValueKind;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{
    Capture, MatchPat, Matcher, call, call_other, first_of, load, one_of, ret, value_of_width, var,
};

/// Baseline: a width constraint never matches the value-less `Return`.
#[test]
fn bare_width_pattern_skips_a_value_less_node() {
    let f = shapes::add_consts(5, 3);
    let m = Matcher::new(&f);
    let pat = value_of_width(64).into_pattern();
    let hits = m.find_all(&pat).unwrap();
    for hit in &hits {
        assert!(
            !matches!(f.node_kind(hit.root()), strider_ir::node::NodeKind::Return),
            "a width constraint cannot hold at a Return"
        );
    }
}

/// Wrapping that same constraint in a single-arm `one_of` must not relax it.
#[test]
fn one_of_arm_width_constraint_survives_at_a_value_less_node() {
    let f = shapes::add_consts(5, 3);
    let m = Matcher::new(&f);
    let pat = one_of![value_of_width(64)].into_pattern();
    let hits = m.find_all(&pat).unwrap();
    for hit in &hits {
        assert!(
            !matches!(f.node_kind(hit.root()), strider_ir::node::NodeKind::Return),
            "one_of arm's width constraint was dropped: matched the Return"
        );
    }
}

/// A capture in an arm must be bound wherever the arm reports a match; the
/// `Return` has no value to bind, so the arm must not match there at all.
#[test]
fn one_of_arm_capture_is_never_reported_unbound() {
    let f = shapes::add_consts(5, 3);
    let m = Matcher::new(&f);
    let x = Capture::new();
    let pat = one_of![var(x)].into_pattern();
    for hit in m.find_all(&pat).unwrap() {
        assert!(
            hit.value(x).is_some(),
            "one_of arm reported a match with its capture unbound at {:?}",
            f.node_kind(hit.root())
        );
    }
}

/// A root arm binds what the bare pattern binds: a `CallOther`'s memory token.
#[test]
fn one_of_arm_binds_what_the_bare_root_binds() {
    let out = reg_vn(0x10, 8);
    let mut t = Tb::with_vars(&[out]);
    let res = t
        .call_other("getval", 42, &[], Some(out), &[], &[])
        .expect("getval has a value output");
    let f = t.ret_val(res);

    let m = Matcher::new(&f);
    let k = Capture::new();
    let bound = |pat| {
        m.find_all(&pat)
            .unwrap()
            .iter()
            .map(|h| h.value(k).expect("a match binds its capture"))
            .collect::<Vec<_>>()
    };
    let bare = bound(call_other().name("getval").capture(k).build());
    assert_eq!(bare.len(), 1);
    assert_eq!(
        bound(one_of![call_other().name("getval").capture(k)].into_pattern()),
        bare,
        "one_of![p] must match exactly where p does"
    );
}

/// A `Call` with a result and a clobber: a root matches once per call, on its memory token,
/// whether sealed by `build`, `into_pattern` or a one-arm alternation.
#[test]
fn a_call_root_matches_once_per_call_under_every_seal() {
    let (r0, r1) = (reg_vn(0, 8), reg_vn(8, 8));
    let mut t = Tb::from_rs(RegisterSet::new().tracked(r0).tracked(r1).ret(r0));
    let a = t.u64(5);
    t.call_at(0x1000);
    let f = t.ret_val(a);

    let m = Matcher::new(&f);
    let c = Capture::new();
    let bound = |pat| {
        m.find_all(&pat)
            .unwrap()
            .iter()
            .map(|h| f.value_kind(h.value(c).expect("a match binds its capture")))
            .collect::<Vec<_>>()
    };
    let bare = bound(call().capture(c).build());
    assert_eq!(bare, [ValueKind::Memory]);
    for (spelling, got) in [
        (
            "into_pattern",
            bound(MatchPat::into_pattern(call().capture(c))),
        ),
        ("one_of!", bound(one_of![call().capture(c)].into_pattern())),
        (
            "first_of!",
            bound(first_of![call().capture(c)].into_pattern()),
        ),
        (
            "res() one_of!",
            bound(one_of![call().res().capture(c)].into_pattern()),
        ),
    ] {
        assert_eq!(got, bare, "{spelling}");
    }
}

/// The mirror of the rule above: in a CONTROL slot the arms are retyped with
/// the alternation, so a value-anchored arm does not reject the control edge
/// its own alternation just bound.
#[test]
fn one_of_in_a_control_slot_matches_like_the_single_arm() {
    let out = reg_vn(0x10, 8);
    let mut t = Tb::with_vars(&[out]);
    let res = t
        .call_other("getval", 42, &[], Some(out), &[], &[])
        .expect("getval has a value output");
    let f = t.ret_val(res);

    let m = Matcher::new(&f);
    let single = m.find_all(&ret().ctrl(call_other()).build()).unwrap();
    let union = m
        .find_all(&ret().ctrl(one_of![load(), call_other()]).build())
        .unwrap();

    assert!(!single.is_empty(), "the bare control-slot arm must match");
    assert_eq!(
        union.len(),
        single.len(),
        "one_of in a control slot must match wherever its arm does"
    );
}

/// A `Call` under a convention with no return or clobber registers produces
/// only `[Control, Memory]`; a root anchors on that token.
#[test]
fn a_root_matches_a_call_with_no_value_output() {
    let mut t = Tb::empty();
    t.call_at(0x1000);
    let f = t.ret_nothing();

    let m = Matcher::new(&f);
    let bare = m.find_all(&call().build()).unwrap();
    assert_eq!(bare.len(), 1, "the fixture has one Call");

    for (spelling, pat) in [
        ("call()", MatchPat::into_pattern(call())),
        ("one_of![call()]", one_of![call()].into_pattern()),
        ("first_of![call()]", first_of![call()].into_pattern()),
    ] {
        assert_eq!(
            m.find_all(&pat).unwrap().len(),
            bare.len(),
            "{spelling} must match wherever call().build() does"
        );
    }
}
