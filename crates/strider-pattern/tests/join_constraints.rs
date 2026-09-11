use std::rc::Rc;
use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer};
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{
    Capture, CaptureExt, JoinConstraint, JoinedMatch, MatchPat, Matcher, anything, bool_const,
    call, if_else, int_const, one_of,
};

/// Diamond CFG with a `Call` in the true arm (`0xAAAA`), the false arm
/// (`0xBBBB`), and after the merge (`0xCCCC`).
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

/// `if (cond) { } else { call 0xBBBB }` with an EMPTY true arm, so the `If`'s
/// true output runs straight into the merge region, which calls `0xCCCC`.
///
/// This is what breaks node-dominance-as-edge-dominance: the true edge's
/// consumer IS the merge, and the merge dominates everything after it.
fn empty_true_arm_with_calls() -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn().unwrap();
    let region_a = b.create_region_all().unwrap();
    let region_c = b.create_region_all().unwrap();
    let region_d = b.create_region_all().unwrap();
    b.set_entry_region_all(region_a).unwrap();

    b.set_region(region_a);
    b.set_lift_addr(Some(0x1000));
    let cond = b.build_boolean_const(true);
    // TRUE goes straight to the merge (the empty arm); FALSE runs the else.
    b.build_if(cond, region_d, region_c).unwrap();
    b.set_lift_addr(None);

    b.set_region(region_c); // false arm
    b.set_lift_addr(Some(0x1020));
    let t2 = b.build_int_const(0xBBBBu64, ValueType::I64).unwrap();
    b.build_call_cc(t2, None).unwrap();
    b.build_branch(region_d).unwrap();
    b.set_lift_addr(None);

    b.set_region(region_d); // merge, reachable through BOTH arms
    b.set_lift_addr(Some(0x1030));
    let t3 = b.build_int_const(0xCCCCu64, ValueType::I64).unwrap();
    b.build_call_cc(t3, None).unwrap();
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    b.build().unwrap()
}

/// Edge dominance asks whether every path traverses the EDGE, and answers no
/// for the post-merge `0xCCCC` call: it is reachable through the false arm too.
/// Asking instead "does the edge's TARGET dominate the node?" claims that call
/// sits in the true block, the target being the merge itself here.
#[test]
fn dominates_edge_rejects_calls_past_a_join_with_an_empty_arm() {
    let function = empty_true_arm_with_calls();
    let m = Matcher::new(&function);
    let (t, c) = (Capture::new(), Capture::new());
    let guard = if_else().capture_true(t).build();
    let callp = call().capture(c).build();

    let tuples = m
        .find_joined_constrained(
            &[&guard, &callp],
            &[JoinConstraint::Dominates {
                dominator: t,
                dominated: c,
            }],
        )
        .unwrap();
    let addrs: Vec<u64> = tuples
        .iter()
        .map(|tp| call_addr(tp, c, &function))
        .collect();

    assert_eq!(
        addrs,
        Vec::<u64>::new(),
        "the true arm is EMPTY, so no call is in the true block. 0xCCCC sits \
         past the merge and is reachable through the false arm too; only the \
         true edge's target (the merge itself) dominates it, which is exactly \
         the confusion the old node-dominance proxy made."
    );
}

/// Nested diamond: an OUTER `If(cond=true)` whose true arm holds an INNER
/// `If(cond=false)`. The two conditions are distinct constant nodes, so a
/// `.cond(bool_const(..))` filter pins each `If` uniquely.
///
/// ```text
///        outer If(true)
///        /            \
///   [true] region_b   [false] region_c
///     inner If(false)          |
///      /        \              |
///  region_d   region_e         |
///      \        |             /
///        \      |            /
///           region_merge (return)
/// ```
///
/// The outer's TRUE edge dominates the inner's true edge (the inner sits
/// exclusively in the outer's true block); the outer's FALSE edge does not.
fn nested_diamond() -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn().unwrap();
    let region_a = b.create_region_all().unwrap();
    let region_b = b.create_region_all().unwrap();
    let region_c = b.create_region_all().unwrap();
    let region_d = b.create_region_all().unwrap();
    let region_e = b.create_region_all().unwrap();
    let region_merge = b.create_region_all().unwrap();
    b.set_entry_region_all(region_a).unwrap();

    b.set_region(region_a);
    b.set_lift_addr(Some(0x2000));
    let outer_cond = b.build_boolean_const(true);
    b.build_if(outer_cond, region_b, region_c).unwrap();
    b.set_lift_addr(None);

    b.set_region(region_b); // outer true arm, holds the inner If
    b.set_lift_addr(Some(0x2010));
    let inner_cond = b.build_boolean_const(false);
    b.build_if(inner_cond, region_d, region_e).unwrap();
    b.set_lift_addr(None);

    for (r, addr) in [
        (region_d, 0x2020u64),
        (region_e, 0x2030),
        (region_c, 0x2040),
    ] {
        b.set_region(r);
        b.set_lift_addr(Some(addr));
        b.build_branch(region_merge).unwrap();
        b.set_lift_addr(None);
    }

    b.set_region(region_merge);
    b.set_lift_addr(Some(0x2050));
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    b.build().unwrap()
}

/// Edge-to-edge dominance stated directly: two control-output captures resolve
/// to `CtrlKey::Edge`, so `dominates` routes through the split tree.
#[test]
fn dominates_edge_over_edge_tracks_nesting() {
    let function = nested_diamond();
    let m = Matcher::new(&function);
    let (t_out, f_out, t_in) = (Capture::new(), Capture::new(), Capture::new());
    let outer = if_else()
        .cond(bool_const(true))
        .capture_true(t_out)
        .capture_false(f_out)
        .build();
    let inner = if_else().cond(bool_const(false)).capture_true(t_in).build();

    let dominates_ok = m
        .find_joined_constrained(
            &[&outer, &inner],
            &[JoinConstraint::Dominates {
                dominator: t_out,
                dominated: t_in,
            }],
        )
        .unwrap();
    assert_eq!(
        dominates_ok.len(),
        1,
        "outer true edge dominates the inner true edge"
    );

    let false_edge = m
        .find_joined_constrained(
            &[&outer, &inner],
            &[JoinConstraint::Dominates {
                dominator: f_out,
                dominated: t_in,
            }],
        )
        .unwrap();
    assert!(
        false_edge.is_empty(),
        "outer false edge does not dominate an edge in the true block"
    );

    let reversed = m
        .find_joined_constrained(
            &[&outer, &inner],
            &[JoinConstraint::Dominates {
                dominator: t_in,
                dominated: t_out,
            }],
        )
        .unwrap();
    assert!(
        reversed.is_empty(),
        "the inner true edge does not dominate the outer true edge"
    );
}

/// The call-target address bound to a joined tuple's `call` match (position 1).
fn call_addr(tuple: &[strider_pattern::Match], c: Capture, f: &strider_ir::Function) -> u64 {
    let call_node = tuple[1].node(c, f.graph()).expect("call node");
    // Call inputs are [ctrl, mem, target, ...].
    let target = f.node_inputs(call_node).into_iter().nth(2).expect("target");
    f.int_const_u128(target).expect("const target") as u64
}

#[test]
fn dominates_edge_isolates_the_true_arm_in_one_constraint() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c) = (Capture::new(), Capture::new());
    let guard = if_else().capture_true(t).build();
    let callp = call().capture(c).build();

    let tuples = m
        .find_joined_constrained(
            &[&guard, &callp],
            &[JoinConstraint::Dominates {
                dominator: t,
                dominated: c,
            }],
        )
        .unwrap();
    let addrs: Vec<u64> = tuples
        .iter()
        .map(|tp| call_addr(tp, c, &function))
        .collect();
    // Only the true-arm call: the merge's CCCC is reachable from the false edge
    // too, so the true EDGE does not dominate it.
    assert_eq!(addrs, vec![0xAAAA]);
}

#[test]
fn dominates_if_selects_all_three_calls() {
    let (function, if_id) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (g, c) = (Capture::new(), Capture::new());
    let guard = if_else().capture(g).build();
    let callp = call().capture(c).build();

    let tuples = m
        .find_joined_constrained(
            &[&guard, &callp],
            &[JoinConstraint::Dominates {
                dominator: g,
                dominated: c,
            }],
        )
        .unwrap();
    // The If dominates both arms and the merge, so every call.
    assert_eq!(tuples.len(), 3);
    assert_eq!(tuples[0][0].node(g, function.graph()).unwrap(), if_id);
}

#[test]
fn negate_inverts_dominates_edge() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c) = (Capture::new(), Capture::new());
    let guard = if_else().capture_true(t).build();
    let callp = call().capture(c).build();

    let inner = JoinConstraint::Dominates {
        dominator: t,
        dominated: c,
    };
    let tuples = m
        .find_joined_constrained(&[&guard, &callp], &[JoinConstraint::Not(Box::new(inner))])
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
    let guard = if_else().capture_true(t).build();
    let callp = call().capture(c).build();

    let inner = JoinConstraint::Dominates {
        dominator: t,
        dominated: c,
    };
    let double = JoinConstraint::Not(Box::new(JoinConstraint::Not(Box::new(inner))));
    let tuples = m
        .find_joined_constrained(&[&guard, &callp], &[double])
        .unwrap();
    let addrs: Vec<u64> = tuples
        .iter()
        .map(|tp| call_addr(tp, c, &function))
        .collect();
    assert_eq!(addrs, vec![0xAAAA]);
}

/// Range restriction is one rule over every constraint, not a negation
/// carve-out: an unbound capture in a positive constraint can never be
/// satisfied, so it would drop every tuple and return an empty set that reads
/// as "no such shape". Reject it loudly instead.
#[test]
fn a_positive_constraint_with_an_unbound_capture_is_rejected_not_silently_empty() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c, unbound) = (Capture::new(), Capture::new(), Capture::new());
    let guard = if_else().capture_true(t).build();
    let callp = call().capture(c).build();

    let err = m
        .find_joined_constrained(
            &[&guard, &callp],
            &[JoinConstraint::Dominates {
                dominator: t,
                dominated: unbound,
            }],
        )
        .err()
        .expect("must be rejected, not silently empty")
        .to_string();
    assert!(
        err.contains("no pattern in the join binds"),
        "unexpected error: {err}"
    );
}

#[test]
fn negating_an_unbound_capture_is_rejected_not_vacuously_true() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let (t, c, unbound) = (Capture::new(), Capture::new(), Capture::new());
    let guard = if_else().capture_true(t).build();
    let callp = call().capture(c).build();

    // Naive negation-as-failure would make an unbound capture hold vacuously for
    // every tuple; range restriction must reject it.
    let inner = JoinConstraint::Dominates {
        dominator: t,
        dominated: unbound,
    };
    let err = m
        .find_joined_constrained(&[&guard, &callp], &[JoinConstraint::Not(Box::new(inner))])
        .err()
        .expect("must be rejected, not vacuously true")
        .to_string();
    assert!(err.contains("negate"), "unexpected error: {err}");
}

/// A user `Where` predicate filters the joined tuple, declares its correlated
/// captures, and composes under `Not`.
#[test]
fn where_predicate_filters_and_composes() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let c = Capture::new();
    let callp = call().capture(c).build();

    // Keep only the call to 0xAAAA. `captures: [c]` correlates on the call.
    let only_aaaa = || JoinConstraint::Where {
        captures: vec![c],
        pred: Rc::new(move |f: &strider_ir::Function, tuple: &JoinedMatch| {
            let node = tuple[0].node(c, f.graph()).expect("call node");
            let target = f.node_inputs(node).into_iter().nth(2).expect("target");
            Some(f.int_const_u128(target).expect("const") as u64 == 0xAAAA)
        }),
    };

    let kept: Vec<u64> = m
        .find_joined_constrained(&[&callp], &[only_aaaa()])
        .unwrap()
        .iter()
        .map(|tp| tp[0].node(c, function.graph()).unwrap())
        .map(|n| {
            let t = function.node_inputs(n).into_iter().nth(2).unwrap();
            function.int_const_u128(t).unwrap() as u64
        })
        .collect();
    assert_eq!(kept, vec![0xAAAA], "bare Where keeps just the 0xAAAA call");

    // Under Not: every call EXCEPT 0xAAAA.
    let mut rest: Vec<u64> = m
        .find_joined_constrained(&[&callp], &[JoinConstraint::Not(Box::new(only_aaaa()))])
        .unwrap()
        .iter()
        .map(|tp| tp[0].node(c, function.graph()).unwrap())
        .map(|n| {
            let t = function.node_inputs(n).into_iter().nth(2).unwrap();
            function.int_const_u128(t).unwrap() as u64
        })
        .collect();
    rest.sort_unstable();
    assert_eq!(rest, vec![0xBBBB, 0xCCCC], "Not(Where) inverts it");
}

/// A `Where` declaring a capture bound only under one `one_of` arm must not be
/// handed rows where that capture is unbound: like every built-in constraint,
/// an unbound declared capture makes the row unanswerable, so it is dropped
/// before `pred` runs. Without the guard the predicate below panics on the
/// 0xBBBB / 0xCCCC calls, where `d` never binds.
#[test]
fn where_drops_rows_with_an_unbound_declared_capture() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);
    let c = Capture::new(); // every call
    let d = Capture::new(); // binds only when the target is the 0xAAAA const

    let callp = call()
        .target(one_of![int_const(0xAAAAu128).capture(d), anything()])
        .capture(c)
        .build();

    let reads_d = || JoinConstraint::Where {
        captures: vec![c, d],
        pred: Rc::new(move |_f: &strider_ir::Function, tuple: &JoinedMatch| {
            // Panics if ever handed a row where `d` is unbound.
            tuple[0].value(d).expect("d must be bound");
            Some(true)
        }),
    };

    let kept: Vec<u64> = m
        .find_joined_constrained(&[&callp], &[reads_d()])
        .unwrap()
        .iter()
        .map(|tp| tp[0].node(c, function.graph()).unwrap())
        .map(|n| {
            let t = function.node_inputs(n).into_iter().nth(2).unwrap();
            function.int_const_u128(t).unwrap() as u64
        })
        .collect();
    assert_eq!(kept, vec![0xAAAA], "only the row where `d` binds survives");
}

/// A constraint is applied as soon as every pattern that can bind its captures
/// is in the row, so a selective one prunes the rest of the product instead of
/// filtering a materialised cross-product.
///
/// The work counter is a `Where` predicate over the LAST pattern's capture: it
/// can only run on a full row, so its call count is exactly the number of rows
/// the product built.
#[test]
fn a_constraint_prunes_before_the_rest_of_the_product_is_built() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);

    let (t, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
    let guard = if_else().capture_true(t).build();
    let in_branch = call().capture(c1).build();
    let anywhere = call().capture(c2).build();

    assert_eq!(m.find_all(&in_branch).unwrap().len(), 3);
    assert_eq!(m.find_all(&anywhere).unwrap().len(), 3);

    let rows_seen = Rc::new(std::cell::Cell::new(0usize));
    let counter = Rc::clone(&rows_seen);
    let tuples = m
        .find_joined_constrained(
            &[&guard, &in_branch, &anywhere],
            &[
                JoinConstraint::Where {
                    captures: vec![c1, c2],
                    pred: Rc::new(move |_f, _tuple| {
                        counter.set(counter.get() + 1);
                        Some(true)
                    }),
                },
                JoinConstraint::Dominates {
                    dominator: t,
                    dominated: c1,
                },
            ],
        )
        .unwrap();

    // Only the true arm's call survives `Dominates`, times the three
    // unconstrained ones.
    assert_eq!(tuples.len(), 3);
    for tuple in &tuples {
        assert_eq!(call_addr(tuple, c1, &function), 0xAAAA);
    }
    assert_eq!(
        rows_seen.get(),
        3,
        "the full 1 x 3 x 3 product must not be built"
    );
}

/// The join is indexed on the captures both sides always bind, which prunes
/// pairs that cannot agree. Agreement implies the bindings resolve to the same
/// node, so the index can only ever remove impossible pairs -- but that is the
/// kind of claim worth checking rather than asserting.
///
/// This compares the real join against a brute-force cartesian reference over
/// shapes chosen to exercise the tricky cases: a shared capture, no shared
/// capture at all, a capture one side leaves unbound (an alternation arm that
/// did not fire), and three-pattern joins.
#[test]
fn the_indexed_join_returns_exactly_the_cartesian_product() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);

    /// Every tuple the join could yield, filtered by the same agreement rule,
    /// computed without any indexing.
    fn reference(
        m: &Matcher<'_>,
        pats: &[&strider_pattern::Pattern],
        function: &strider_ir::Function,
    ) -> Vec<Vec<Vec<(u32, u32)>>> {
        let per: Vec<Vec<strider_pattern::Match>> =
            pats.iter().map(|p| m.find_all(p).unwrap()).collect();
        let mut rows: Vec<Vec<&strider_pattern::Match>> = vec![vec![]];
        for level in &per {
            let mut next = Vec::new();
            for row in &rows {
                for cand in level {
                    // Two matches agree when every capture both bind resolves
                    // to the same node.
                    let cand_sig = cand.capture_signature(function.graph());
                    let ok = row.iter().all(|prev| {
                        prev.capture_signature(function.graph())
                            .iter()
                            .all(|(cap, node)| {
                                cand_sig
                                    .iter()
                                    .find(|(c, _)| c == cap)
                                    .is_none_or(|(_, n)| n == node)
                            })
                    });
                    if ok {
                        let mut r = row.clone();
                        r.push(cand);
                        next.push(r);
                    }
                }
            }
            rows = next;
        }
        let mut out: Vec<Vec<Vec<(u32, u32)>>> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|mm| mm.capture_signature(function.graph()))
                    .collect()
            })
            .collect();
        out.sort();
        // The API reports each distinct tuple once, keyed on what it binds; a
        // capture-free pattern's matches are indistinguishable by that key.
        out.dedup();
        out
    }

    let (a, c) = (Capture::new(), Capture::new());
    let shapes: Vec<Vec<strider_pattern::Pattern>> = vec![
        // shared capture on both sides
        vec![call().capture(a).build(), call().capture(a).build()],
        // one side binds nothing at all
        vec![call().capture(a).build(), call().build()],
        // three patterns, shared across all
        vec![
            call().capture(a).build(),
            call().capture(a).build(),
            call().capture(a).build(),
        ],
        // an alternation arm that does not fire leaves `c` unbound
        vec![
            call().capture(a).build(),
            MatchPat::into_pattern(one_of![call().capture(a), int_const(0xDEADu128).capture(c)]),
        ],
    ];

    for (n, pats) in shapes.iter().enumerate() {
        let refs: Vec<&strider_pattern::Pattern> = pats.iter().collect();
        let mut got: Vec<Vec<Vec<(u32, u32)>>> = m
            .find_joined_constrained(&refs, &[])
            .unwrap()
            .iter()
            .map(|t: &JoinedMatch| {
                t.iter()
                    .map(|mm| mm.capture_signature(function.graph()))
                    .collect()
            })
            .collect();
        got.sort();
        got.dedup();
        assert_eq!(
            got,
            reference(&m, &refs, &function),
            "shape {n}: the indexed join must equal the cartesian product",
        );
    }
}

/// A pattern that binds no capture is still identified by where it matched, so
/// a join holding one reports every one of its matches. Keying the dedup on
/// bindings alone made all of them collapse to an arbitrary survivor -- and
/// then adding a capture to ONE side SHRANK the reported cross product, which
/// is the opposite of what a capture does.
#[test]
fn a_capture_free_pattern_does_not_collapse_the_cross_product() {
    let (function, _) = diamond_with_calls();
    let m = Matcher::new(&function);

    let bare = call().build();
    let n_calls = m.find_all(&bare).unwrap().len();
    assert!(
        n_calls >= 2,
        "the fixture needs several calls, got {n_calls}"
    );

    let both_bare = m
        .find_joined_constrained(&[&bare, &bare], &[])
        .unwrap()
        .len();

    let c = Capture::new();
    let captured = call().capture(c).build();
    let n_captured = m.find_all(&captured).unwrap().len();
    let mixed = m
        .find_joined_constrained(&[&captured, &bare], &[])
        .unwrap()
        .len();

    assert_eq!(
        mixed,
        n_captured * n_calls,
        "a capture-free side must contribute all {n_calls} of its matches"
    );
    assert_eq!(
        mixed, both_bare,
        "capturing one side must not change how many tuples exist"
    );
}

/// `if (true) { store } else { }`, joining at a merge region.
fn diamond_with_a_store_in_the_true_arm() -> strider_ir::Function {
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
    let addr = b.build_int_const(0x2000u64, ValueType::I64).unwrap();
    let data = b.build_int_const(0xAAu64, ValueType::I64).unwrap();
    b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
    b.build_branch(region_d).unwrap();
    b.set_lift_addr(None);

    b.set_region(region_c); // empty false arm
    b.set_lift_addr(Some(0x1020));
    b.build_branch(region_d).unwrap();
    b.set_lift_addr(None);

    b.set_region(region_d);
    b.set_lift_addr(Some(0x1030));
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    b.build().unwrap()
}

/// A `Store` has no control edge, so it has no vertex in the dominator tree and
/// the relation is unanswerable. Answering `false` would be worse than saying
/// nothing: the negation then reports the very store the caller asked to
/// exclude.
#[test]
fn dominates_over_a_node_with_no_cfg_position_answers_neither_way() {
    let function = diamond_with_a_store_in_the_true_arm();
    let m = Matcher::new(&function);
    let (t, s) = (Capture::new(), Capture::new());
    let guard = if_else().capture_true(t).build();
    let storep = strider_pattern::store().capture(s).build();

    let positive = JoinConstraint::Dominates {
        dominator: t,
        dominated: s,
    };
    let negated = JoinConstraint::Not(Box::new(JoinConstraint::Dominates {
        dominator: t,
        dominated: s,
    }));

    assert_eq!(
        m.find_joined_constrained(&[&guard, &storep], &[positive])
            .unwrap()
            .len(),
        0,
        "unanswerable, so no row"
    );
    assert_eq!(
        m.find_joined_constrained(&[&guard, &storep], &[negated])
            .unwrap()
            .len(),
        0,
        "the negation of an unanswerable relation is still unanswerable"
    );
}
