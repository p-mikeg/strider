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
