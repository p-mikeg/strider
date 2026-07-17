//! `find_joined_constrained` — CFG relational constraints (dominance + forward
//! control reachability) over a joined match tuple. The motivating query:
//! "this call is on the true branch of the guard, not the false branch and not
//! after the merge."

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer};
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{Capture, JoinConstraint, Matcher, call, if_node};

/// Diamond CFG with a `Call` in the true arm (`0xAAAA`), the false arm
/// (`0xBBBB`), and after the merge (`0xCCCC`). Returns the function + the `If`
/// node id.
fn diamond_with_calls() -> (strider_ir::Function, NodeId) {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn().unwrap();
    let region_a = b.create_region_all().unwrap();
    let region_b = b.create_region_all().unwrap();
    let region_c = b.create_region_all().unwrap();
    let region_d = b.create_region_all().unwrap();
    b.set_entry_region_all(region_a).unwrap();

    b.set_region(region_a);
    b.set_lift_addr(Some(0x1000));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, region_b, region_c).unwrap();
    b.set_lift_addr(None);

    b.set_region(region_b); // true arm
    b.set_lift_addr(Some(0x1010));
    let t1 = b.build_int_const(0xAAAAu64, ValueType::I64).unwrap();
    b.build_call_cc(t1, None).unwrap();
    b.build_branch(region_d).unwrap();
    b.set_lift_addr(None);

    b.set_region(region_c); // false arm
    b.set_lift_addr(Some(0x1020));
    let t2 = b.build_int_const(0xBBBBu64, ValueType::I64).unwrap();
    b.build_call_cc(t2, None).unwrap();
    b.build_branch(region_d).unwrap();
    b.set_lift_addr(None);

    b.set_region(region_d); // merge
    b.set_lift_addr(Some(0x1030));
    let t3 = b.build_int_const(0xCCCCu64, ValueType::I64).unwrap();
    b.build_call_cc(t3, None).unwrap();
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    let function = b.build().unwrap();
    let if_id = function
        .graph()
        .all_node_ids()
        .find(|&n| matches!(function.node_kind(n), NodeKind::If))
        .unwrap();
    (function, if_id)
}

/// The call-target address bound to a joined tuple's `call` match (position 1).
fn call_addr(tuple: &[strider_pattern::Match], c: Capture, f: &strider_ir::Function) -> u64 {
    let call_node = tuple[1].node(c, f.graph()).expect("call node");
    // Call inputs: [ctrl, mem, target, ...]; the target is the third input.
    let target = f.node_inputs(call_node).into_iter().nth(2).expect("target");
    f.int_const_u128(target).expect("const target") as u64
}

#[test]
fn dominated_by_branch_isolates_the_true_arm_in_one_constraint() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c) = (Capture::new(), Capture::new());
    let guard = if_node().capture_true(t).build();
    let callp = call().capture(c).build();

    let tuples = m
        .find_joined_constrained(
            &[&guard, &callp],
            &[&JoinConstraint::DominatedByBranch { branch: t, node: c }],
        )
        .unwrap();
    let addrs: Vec<u64> = tuples
        .iter()
        .map(|tp| call_addr(tp, c, &function))
        .collect();
    // Only the true-arm call (AAAA): the merge (CCCC) is reachable from the
    // false edge too, so it is NOT dominated by the true edge's target. One
    // constraint does what reaches + not_reaches did.
    assert_eq!(addrs, vec![0xAAAA]);
}

#[test]
fn dominates_if_selects_all_three_calls() {
    let (function, if_id) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (g, c) = (Capture::new(), Capture::new());
    let guard = if_node().capture(g).build();
    let callp = call().capture(c).build();

    let tuples = m
        .find_joined_constrained(
            &[&guard, &callp],
            &[&JoinConstraint::Dominates { a: g, b: c }],
        )
        .unwrap();
    // The If dominates both arms and the merge — every call.
    assert_eq!(tuples.len(), 3);
    // Sanity: g really bound the If.
    assert_eq!(tuples[0][0].node(g, function.graph()).unwrap(), if_id);
}

#[test]
fn negate_inverts_dominated_by_branch() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c) = (Capture::new(), Capture::new());
    let guard = if_node().capture_true(t).build();
    let callp = call().capture(c).build();

    let inner = JoinConstraint::DominatedByBranch { branch: t, node: c };
    let tuples = m
        .find_joined_constrained(&[&guard, &callp], &[&JoinConstraint::Not(Box::new(inner))])
        .unwrap();
    let mut addrs: Vec<u64> = tuples
        .iter()
        .map(|tp| call_addr(tp, c, &function))
        .collect();
    addrs.sort_unstable();
    // Exactly the complement of the positive constraint's `[0xAAAA]`.
    assert_eq!(addrs, vec![0xBBBB, 0xCCCC]);
}

#[test]
fn double_negation_is_the_identity() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c) = (Capture::new(), Capture::new());
    let guard = if_node().capture_true(t).build();
    let callp = call().capture(c).build();

    let inner = JoinConstraint::DominatedByBranch { branch: t, node: c };
    let double = JoinConstraint::Not(Box::new(JoinConstraint::Not(Box::new(inner))));
    let tuples = m
        .find_joined_constrained(&[&guard, &callp], &[&double])
        .unwrap();
    let addrs: Vec<u64> = tuples
        .iter()
        .map(|tp| call_addr(tp, c, &function))
        .collect();
    assert_eq!(addrs, vec![0xAAAA]);
}

#[test]
fn negating_an_unbound_capture_is_rejected_not_vacuously_true() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c, unbound) = (Capture::new(), Capture::new(), Capture::new());
    let guard = if_node().capture_true(t).build();
    let callp = call().capture(c).build();

    // `unbound` is bound by no pattern in the join. A naive negation-as-failure
    // would make this hold VACUOUSLY for every tuple; range restriction must
    // reject it instead.
    let inner = JoinConstraint::DominatedByBranch {
        branch: t,
        node: unbound,
    };
    let err = m
        .find_joined_constrained(&[&guard, &callp], &[&JoinConstraint::Not(Box::new(inner))])
        .err()
        .expect("must be rejected, not vacuously true")
        .to_string();
    assert!(err.contains("negate"), "unexpected error: {err}");
}

#[test]
fn negating_a_binding_constraint_is_rejected() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c) = (Capture::new(), Capture::new());
    let guard = if_node().capture_true(t).build();
    let callp = call().capture(c).build();

    let inner = JoinConstraint::PhiInputFromEdge {
        phi: c,
        edge: t,
        value: strider_pattern::ValueSpec::Pattern(Box::new(call().build())),
    };
    let err = m
        .find_joined_constrained(&[&guard, &callp], &[&JoinConstraint::Not(Box::new(inner))])
        .err()
        .expect("must be rejected, not vacuously true")
        .to_string();
    assert!(err.contains("negate"), "unexpected error: {err}");
}
