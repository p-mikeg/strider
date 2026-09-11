use strider_ir::IRViewer as _;
use strider_ir_test_utils::{Tb, reg_vn};
use strider_pattern::{
    Capture, MatchPat, Matcher, call_other, load, one_of, ret, value_of_width, var,
};

/// `ret(int_add(5, 3))`: the `Return` produces no value.
fn ret_over_add() -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let sum = t.add(a, b);
    t.ret_val(sum)
}

/// Baseline: a width constraint never matches the value-less `Return`.
#[test]
fn bare_width_pattern_skips_a_value_less_node() {
    let f = ret_over_add();
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
    let f = ret_over_add();
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
    let f = ret_over_add();
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

/// A value-anchored arm must bind a value edge, never the control or memory
/// edge of the same multi-output node.
#[test]
fn one_of_arm_binds_only_value_edges() {
    let out = reg_vn(0x10, 8);
    let mut t = Tb::with_vars(&[out]);
    let res = t
        .call_other("getval", 42, &[], Some(out), &[], &[])
        .expect("getval has a value output");
    let f = t.ret_val(res);

    let m = Matcher::new(&f);
    let k = Capture::new();
    let bare = m
        .find_all(&call_other().name("getval").capture(k).into_pattern())
        .unwrap();
    let wrapped = m
        .find_all(&one_of![call_other().name("getval").capture(k)].into_pattern())
        .unwrap();

    assert!(!bare.is_empty());
    for hit in &wrapped {
        let bound = hit.value(k).expect("a matching arm binds its capture");
        assert!(
            f.value_kind(bound).is_value(),
            "one_of arm bound a {:?} edge",
            f.value_kind(bound)
        );
    }
    assert_eq!(
        wrapped.len(),
        bare.len(),
        "one_of![p] must match exactly where p does"
    );
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
