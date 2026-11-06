use strider_ir::UBig;
use strider_ir::node::NodeKind;
use strider_ir::{FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};
use strider_pattern::matcher::{KindSpec, MatcherBuilder, PatValueRef};
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

/// Every binary op matches its pattern written either way round iff
/// `NodeKind::is_commutative`, and `.ordered()` pins the written order.
#[test]
fn every_binary_op_commutes_iff_is_commutative() {
    #[track_caller]
    fn check(
        function: &strider_ir::Function,
        kind: &NodeKind,
        pat: impl Fn(bool, bool) -> Pattern,
    ) {
        let count = |p: Pattern| Matcher::new(function).find_all(&p).unwrap().len();
        let swapped = usize::from(kind.is_commutative());
        assert_eq!(count(pat(false, false)), 1, "{kind:?}");
        assert_eq!(count(pat(true, false)), swapped, "{kind:?}");
        assert_eq!(count(pat(false, true)), 1, "{kind:?}");
        assert_eq!(count(pat(true, true)), 0, "{kind:?}");
    }
    fn pin<P: MatchPat + 'static>(p: P, ordered: bool) -> Pattern {
        if ordered {
            p.ordered().into_pattern()
        } else {
            p.into_pattern()
        }
    }
    let ints = |swap: bool| {
        let (l, r) = if swap { (3u128, 5u128) } else { (5, 3) };
        (int_const(l), int_const(r))
    };
    let floats = |swap: bool| {
        let (l, r) = if swap { (5.0f64, 2.0f64) } else { (2.0, 5.0) };
        (float_const(l.to_bits()), float_const(r.to_bits()))
    };
    for op in IntBinaryOp::ALL {
        check(
            &shapes::int_bin(5, 3, op),
            &NodeKind::IntBinaryOp(op),
            |swap, ordered| {
                let (l, r) = ints(swap);
                pin(int_binary(op, l, r), ordered)
            },
        );
    }
    for op in IntCmpOp::ALL {
        check(
            &shapes::int_cmp_5_3(op),
            &NodeKind::IntCmpOp(op),
            |swap, ordered| {
                let (l, r) = ints(swap);
                pin(int_cmp(op, l, r), ordered)
            },
        );
    }
    for op in [IntBinaryOp::And, IntBinaryOp::Or, IntBinaryOp::Xor] {
        check(
            &shapes::bool_bin(true, false, op),
            &NodeKind::IntBinaryOp(op),
            |swap, ordered| {
                pin(
                    bool_binary(op, bool_const(!swap), bool_const(swap)),
                    ordered,
                )
            },
        );
    }
    for op in FloatBinaryOp::ALL {
        check(
            &shapes::float_bin(2.0, 5.0, op),
            &NodeKind::FloatBinaryOp(op),
            |swap, ordered| {
                let (l, r) = floats(swap);
                pin(float_binary(op, l, r), ordered)
            },
        );
    }
    for op in FloatCmpOp::ALL {
        check(
            &graph_float_cmp(2.0, 5.0, op),
            &NodeKind::FloatCmpOp(op),
            |swap, ordered| {
                let (l, r) = floats(swap);
                pin(float_cmp(op, l, r), ordered)
            },
        );
    }
}

/// A capture on one commutative operand binds each operand in turn.
#[test]
fn commutative_capture_reports_both_operand_bindings() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    let k = Capture::new();
    let hits = a::matches(
        &function,
        int_add(anything().capture(k), anything()).into_pattern(),
        2,
    );
    let mut bound: Vec<Option<UBig>> = hits
        .iter()
        .map(|m| m.bindings().get_uint(k, &function))
        .collect();
    // Enumeration order is pinned: natural ordering first, then swapped.
    assert_eq!(
        bound,
        vec![Some(UBig::from(5u8)), Some(UBig::from(3u8))],
        "both operands must bind"
    );
    bound.sort_unstable();
    bound.dedup();
    assert_eq!(bound.len(), 2, "the two bindings must be DISTINCT");
}

/// `.ordered()` suppresses the commutative retry entirely, so a capture-bearing
/// pattern yields exactly the one ordering it pins.
#[test]
fn ordered_capture_reports_only_the_pinned_ordering() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    let k = Capture::new();
    let hits = a::matches(
        &function,
        int_add(anything().capture(k), anything())
            .ordered()
            .into_pattern(),
        1,
    );
    assert_eq!(
        hits[0].bindings().get_uint(k, &function),
        Some(UBig::from(5u8)),
        "ordered() pins the capture to operand slot 0",
    );
}

/// `matches()` is lazy: `.next()` yields the natural ordering without
/// enumerating the swapped one. `find_all` collects this iterator, so this is
/// what pins `find_all(..)[0]` to the natural ordering.
#[test]
fn matches_iterator_yields_natural_ordering_first() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    let k = Capture::new();
    let pat = int_add(anything().capture(k), anything()).into_pattern();
    let first = Matcher::new(&function)
        .matches(&pat)
        .expect("matches")
        .next()
        .expect("at least one hit");
    assert_eq!(
        first.bindings().get_uint(k, &function),
        Some(UBig::from(5u8)),
        "the first hit must be the natural operand ordering",
    );
}

/// `pattern::int_sub(a, b)` is an alias for the lowered `Add(a, Neg(b))`, so the
/// fixture builds that shape directly.
#[test]
fn sub_does_not_commute() {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let lowered = t.sub(l, r);
    let function = t.ret_val(lowered);
    a::none(
        &function,
        int_sub(int_const(3u128), int_const(5u128)).into_pattern(),
    );
    a::matches(
        &function,
        int_sub(int_const(5u128), int_const(3u128)).into_pattern(),
        1,
    );
}

/// `FloatBinaryOp::Sub` is not a primitive: `pattern::float_sub` is an alias
/// for the lowered `FloatAdd(a, Neg(b))`, which the fixture builds by hand.
#[test]
fn float_sub_does_not_commute() {
    let g_sub = {
        let mut t = Tb::empty();
        let a = t.f64(5.0);
        let b = t.f64(2.0);
        let neg_b = t.fun(
            b,
            strider_ir::FloatUnaryOp::Neg,
            strider_ir::node::ValueType::float(64),
        );
        let lowered = t.fbin(
            a,
            neg_b,
            FloatBinaryOp::Add,
            strider_ir::node::ValueType::float(64),
        );
        let as_int = t.float_to_int(lowered, strider_ir::node::ValueType::int(64));
        t.ret_val(as_int)
    };
    a::none(
        &g_sub,
        float_sub(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into_pattern(),
    );
}

/// `int_add(int_sub(a, b), c)`: the outer `int_add` can rearrange `(sub-result, c)`, the
/// inner `int_sub` cannot.
#[test]
fn commutative_outer_non_commutative_inner() {
    let mut t = Tb::empty();
    let a = t.u64(10);
    let b = t.u64(3);
    let c = t.u64(5);
    let d = t.sub(a, b);
    let s = t.add(d, c);
    let function = t.ret_val(s);

    a::matches(
        &function,
        int_add(
            int_sub(int_const(10u128), int_const(3u128)),
            int_const(5u128),
        )
        .into_pattern(),
        1,
    );
    // Outer-add swapped: still matches.
    a::matches(
        &function,
        int_add(
            int_const(5u128),
            int_sub(int_const(10u128), int_const(3u128)),
        )
        .into_pattern(),
        1,
    );
    // Inner-sub swapped: no match.
    a::none(
        &function,
        int_add(
            int_sub(int_const(3u128), int_const(10u128)),
            int_const(5u128),
        )
        .into_pattern(),
    );
    // Inner swap plus outer swap: still no match.
    a::none(
        &function,
        int_add(
            int_const(5u128),
            int_sub(int_const(3u128), int_const(10u128)),
        )
        .into_pattern(),
    );
}

/// The swap retry must start from clean bindings after a partial bind on the
/// first ordering. Otherwise `var(x)` used twice spuriously matches distinct
/// operands.
#[test]
fn commutative_swap_does_not_leak_bindings() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    let x = Capture::new();
    a::none(&function, int_add(var(x), var(x)).into_pattern());
}

/// Constant dedup makes both operands of `int_add(5, 5)` the same output, so the
/// identity capture `int_add(var(x), var(x))` does match here.
#[test]
fn commutative_swap_matches_identical_operand_with_identity_capture() {
    let function = shapes::int_bin(5, 5, IntBinaryOp::Add);
    let x = Capture::new();
    assert_eq!(
        a::unique_uint(&function, int_add(var(x), var(x)).into_pattern(), x),
        Some(UBig::from(5u8))
    );
}

/// A `when_match` guard on a CHILD operand that rejects the natural ordering
/// does not kill the match: the swap retry re-drives the child against the
/// other operand, where the guard passes.
#[test]
fn child_when_match_rejection_still_tries_swapped_order() {
    use strider_ir::IRViewer;

    // The guarded child sits on pattern slot 0: natural order maps it to the
    // 2-operand (guard fails), the swap retry to the 3-operand (guard passes).
    let function = shapes::int_bin(2, 3, IntBinaryOp::Add);
    let c = Capture::new();
    let guarded = anything().capture(c).when_match(move |m, _ty, b| {
        let Some(v) = b.get_value(c) else {
            return false;
        };
        m.function().int_const_u128(v) == Some(3)
    });
    let m = a::unique(&function, int_add(guarded, int_const(2u128)).into_pattern());
    assert_eq!(
        m.bindings().get_uint(c, &function),
        Some(UBig::from(3u8)),
        "swap retry must rebind the guarded child to the 3-operand",
    );
}

/// `.ordered()` disables the swap re-drive even when a root guard rejects:
/// the forced-ordered node fails outright.
#[test]
fn ordered_root_when_match_rejection_does_not_redrive_swap() {
    use strider_ir::IRViewer;

    let function = shapes::int_bin(2, 3, IntBinaryOp::Add);
    let l = Capture::new();
    let pat = int_add(anything().capture(l), anything())
        .ordered()
        .when_match(move |m, _ty, b| {
            let Some(v) = b.get_value(l) else {
                return false;
            };
            m.function().int_const_u128(v) == Some(3)
        })
        .into_pattern();
    a::none(&function, pat);
}

/// A guard on a UNARY parent (nothing of its own to swap) must still re-drive
/// a commutative child's operand order: backtracking propagates DOWN past the
/// unary node.
#[test]
fn unary_parent_guard_redrives_commutative_child() {
    use strider_ir::IRViewer;
    use strider_ir::node::ValueType;

    // zext(int_add(2, 3)): natural order binds x to 2, but the guard wants 3, so
    // the re-drive must reach down through the zext to swap the add.
    let function = {
        let mut t = Tb::empty();
        let two = t.u64(2);
        let three = t.u64(3);
        let sum = t.add(two, three);
        let z = t.zext_to(sum, ValueType::int(128));
        t.ret_val(z)
    };
    let x = Capture::new();
    let pat = int_zero_extend(int_add(anything().capture(x), anything()))
        .when_match(move |m, _ty, b| {
            b.get_value(x).and_then(|v| m.function().int_const_u128(v)) == Some(3)
        })
        .into_pattern();
    let m = a::unique(&function, pat);
    assert_eq!(
        m.bindings().get_uint(x, &function),
        Some(UBig::from(3u8)),
        "unary-parent guard must re-drive the commutative child's operand order",
    );
}

/// `a OP b`, its `I1` result widened to `I64` for the return.
fn graph_float_cmp(l: f64, r: f64, op: FloatCmpOp) -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.f64(l);
    let b = t.f64(r);
    let v = t.fcmp(a, b, op);
    let as_int = t.as_int(v, strider_ir::node::ValueType::int(64));
    t.ret_val(as_int)
}

/// `FloatCmpOp::NotEqual` is not a primitive: `pattern::float_ne` is an alias
/// for `Xor(FloatEqual(_, _), 1):I1`, which the fixture builds by hand.
#[test]
fn float_ne_commutes() {
    let function = {
        let mut t = Tb::empty();
        let a = t.f64(1.0);
        let b = t.f64(2.0);
        let eq = t.fcmp(a, b, FloatCmpOp::Equal);
        let ne = t.bool_not(eq);
        let as_int = t.as_int(ne, strider_ir::node::ValueType::int(64));
        t.ret_val(as_int)
    };
    a::matches_both_orders(
        &function,
        float_ne(
            float_const(1.0_f64.to_bits()),
            float_const(2.0_f64.to_bits()),
        )
        .into_pattern(),
        float_ne(
            float_const(2.0_f64.to_bits()),
            float_const(1.0_f64.to_bits()),
        )
        .into_pattern(),
    );
}

/// A commutative node with ONE pinned operand still reaches the other slot,
/// so the same query cannot answer differently by which slot it names.
#[test]
fn single_pinned_operand_reaches_both_slots() {
    let function = shapes::int_bin(1, 2, IntBinaryOp::Add);
    for slot in [0, 1] {
        let mut b = MatcherBuilder::new();
        let k = MatchPat::compile(int_const(2u128), &mut b);
        let n = b.node(KindSpec::Exact(NodeKind::IntBinaryOp(IntBinaryOp::Add)));
        b.input(n, slot, k);
        b.value_output(n, 0);
        a::matches(&function, b.finish(), 1);
    }
}

/// Structurally identical capture-free operands bind the same either way, so
/// the swapped ordering is skipped and the enumeration stays linear.
#[test]
fn symmetric_wildcard_tree_completes() {
    const DEPTH: u32 = 7;
    let mut t = Tb::empty();
    let mut next = 0u64;
    let root = ir_add_tree(&mut t, DEPTH, &mut next);
    let function = t.ret_val(root);

    let mut b = MatcherBuilder::new();
    pat_add_tree(&mut b, DEPTH);
    a::matches(&function, b.finish(), 1);
}

fn ir_add_tree(t: &mut Tb, depth: u32, next: &mut u64) -> strider_ir::node::ValueId {
    if depth == 0 {
        *next += 1;
        return t.u64(*next);
    }
    let l = ir_add_tree(t, depth - 1, next);
    let r = ir_add_tree(t, depth - 1, next);
    t.add(l, r)
}

fn pat_add_tree(b: &mut MatcherBuilder, depth: u32) -> PatValueRef {
    if depth == 0 {
        let o = b.leaf(KindSpec::Any);
        b.set_output_any(o);
        return o;
    }
    let l = pat_add_tree(b, depth - 1);
    let r = pat_add_tree(b, depth - 1);
    b.binary(IntBinaryOp::Add, l, r)
}

/// Operands that differ keep both orderings.
#[test]
fn asymmetric_and_capturing_operands_keep_both_orders() {
    let function = shapes::add_consts(5, 3);
    a::matches(
        &function,
        int_add(int_const(5u128), anything()).into_pattern(),
        1,
    );
    a::matches(
        &function,
        int_add(anything(), int_const(5u128)).into_pattern(),
        1,
    );
}

/// `return(load[0x1000] + load[0x2000])`.
fn add_of_two_loads() -> strider_ir::Function {
    use strider_ir::node::ValueType;
    let mut t = Tb::empty();
    let a = t.u64(0x1000);
    let b = t.u64(0x2000);
    let la = t.load_ram(a, ValueType::int(64));
    let lb = t.load_ram(b, ValueType::int(64));
    let s = t.add(la, lb);
    t.ret_val(s)
}

/// A capture on a SIBLING output vertex (`.output(slot)` / `.any_output()`)
/// is as order-sensitive as one on the anchor, so the interchangeable cut must
/// not swallow the swapped ordering: all three spellings bind both loads.
#[test]
fn sibling_output_capture_keeps_both_orders() {
    let function = add_of_two_loads();
    let k = Capture::new();
    let bound = |pat| {
        let vs: Vec<_> = a::matches(&function, pat, 2)
            .iter()
            .map(|m| m.bindings().get_value(k).expect("bound"))
            .collect();
        assert_ne!(vs[0], vs[1], "the two bindings must be DISTINCT");
        vs
    };
    let anchor = bound(int_add(load().capture(k), load()).into_pattern());
    assert_eq!(
        bound(int_add(load().output(0).capture(k), load()).into_pattern()),
        anchor,
    );
    assert_eq!(
        bound(int_add(load().any_output().capture(k), load()).into_pattern()),
        anchor,
    );
}

/// The dropped ordering turns into a dropped join ROW as soon as another
/// pattern pins the capture, so both spellings must join the same.
#[test]
fn sibling_output_capture_joins_like_the_anchor() {
    let function = add_of_two_loads();
    let m = Matcher::new(&function);
    let c = Capture::new();
    let rows = |sum: Pattern| {
        let second = load().addr(int_const(0x2000u128)).capture(c).build();
        m.find_joined_constrained(&[&sum, &second], &[])
            .expect("join")
            .len()
    };
    assert_eq!(rows(int_add(load().capture(c), load()).into_pattern()), 1);
    assert_eq!(
        rows(int_add(load().output(0).capture(c), load()).into_pattern()),
        1,
    );
}

/// `load().space(s)` pins the kind exactly, so two of them are one operand
/// shape and the swapped ordering is cut, as it is for two bare `load()`s.
#[test]
fn space_pinned_operands_are_cut_like_bare_ones() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let function = add_of_two_loads();
    let counted = |pat: Pattern| -> (usize, usize) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let pat = pat.with_root_post_match(Box::new(move |_, _, _, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            true
        }));
        let hits = Matcher::new(&function).find_all(&pat).unwrap().len();
        (hits, calls.load(Ordering::Relaxed))
    };
    let ram = || load().space(rsleigh::VnSpace::RAM);
    // The root capture keeps `find_all` enumerating, so the count is the
    // number of orderings driven.
    let r = Capture::new();
    assert_eq!(
        counted(int_add(load(), load()).capture(r).into_pattern()),
        (1, 1)
    );
    let r = Capture::new();
    assert_eq!(
        counted(int_add(ram(), ram()).capture(r).into_pattern()),
        (1, 1)
    );
}

/// A capture on one of two space-pinned loads still tells the orderings
/// apart, so the cut leaves both bindings.
#[test]
fn space_pinned_capture_keeps_both_orders() {
    let function = add_of_two_loads();
    let ram = || load().space(rsleigh::VnSpace::RAM);
    let k = Capture::new();
    let vs: Vec<_> = a::matches(
        &function,
        int_add(ram().capture(k), ram()).into_pattern(),
        2,
    )
    .iter()
    .map(|m| m.bindings().get_value(k).expect("bound"))
    .collect();
    assert_ne!(vs[0], vs[1], "the two bindings must be DISTINCT");
}

/// `v_{i+1} = v_i + v_i`: both slots of every add hold one value, so the
/// swapped ordering assigns every operand what the natural one did. Retrying
/// it anyway doubles the work per nested level for a single match.
#[test]
fn identical_operand_slots_are_not_retried_swapped() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const LEVELS: usize = 16;
    let mut t = Tb::empty();
    let mut acc = t.u64(1);
    for _ in 0..LEVELS {
        acc = t.add(acc, acc);
    }
    let function = t.ret_val(acc);

    let mut b = MatcherBuilder::new();
    let mut o = b.leaf(KindSpec::Any);
    b.set_output_any(o);
    for _ in 0..LEVELS {
        let leaf = b.leaf(KindSpec::Any);
        b.set_output_any(leaf);
        o = b.binary(IntBinaryOp::Add, o, leaf);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let pat = b.finish().with_root_post_match(Box::new(move |_, _, _, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        true
    }));

    a::matches(&function, pat, 1);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "one configuration per match"
    );
}

/// `float_is_nan` fans its operand out to both equality inputs and pins the
/// identity with a builder-minted capture. The swap moves the two pins between
/// the `And`'s operands, which is not a second match: nothing a caller can see
/// differs.
#[test]
fn an_identity_pin_moved_by_the_swap_is_not_a_second_match() {
    let mut t = Tb::empty();
    let a = t.f64(1.5);
    let c = t.f64(2.5);
    let one = t.boolean(true);
    let eq_a = t.fcmp(a, a, FloatCmpOp::Equal);
    let nan_a = t.bool_bin(eq_a, one, IntBinaryOp::Xor);
    let eq_c = t.fcmp(c, c, FloatCmpOp::Equal);
    let nan_c = t.bool_bin(eq_c, one, IntBinaryOp::Xor);
    let both = t.bool_bin(nan_a, nan_c, IntBinaryOp::And);
    let function = t.ret_val(both);

    a::matches(
        &function,
        bool_and(float_is_nan(anything()), float_is_nan(anything())).into_pattern(),
        1,
    );
    // A capture on each arm DOES tell the two orderings apart.
    let (l, r) = (Capture::new(), Capture::new());
    a::matches(
        &function,
        bool_and(
            float_is_nan(anything().capture(l)),
            float_is_nan(anything().capture(r)),
        )
        .into_pattern(),
        2,
    );
}

/// Binding nothing a caller can see, every operand ordering carries the same
/// signature, so the first one to reach the root is the whole answer and the
/// rest are not driven. `interchangeable` cannot cut these: the two operand
/// sub-patterns have different shapes.
#[test]
fn a_capture_free_pattern_stops_at_the_first_ordering() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    let counted = |pat: Pattern| -> (usize, usize) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let pat = pat.with_root_post_match(Box::new(move |_, _, _, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            true
        }));
        let hits = Matcher::new(&function).find_all(&pat).unwrap().len();
        (hits, calls.load(Ordering::Relaxed))
    };

    assert_eq!(
        counted(int_add(any_int_const(), anything()).into_pattern()),
        (1, 1),
    );
    let c = Capture::new();
    assert_eq!(
        counted(int_add(any_int_const().capture(c), anything()).into_pattern()),
        (2, 2),
    );
}

/// `return(zext(Xor(Equal(5, 3), 1)))`: `int_ne(5, 3)` as the lifter lowers it.
fn lowered_int_ne_5_3() -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let eq = t.int_cmp(a, b, IntCmpOp::Equal);
    let ne = t.bool_not(eq);
    let wide = t.zext_to(ne, strider_ir::node::ValueType::int(64));
    t.ret_val(wide)
}

/// The pair `int_ne` was written with sits on its `Equal`, not on the `Xor`
/// the lowering adds, so that is what `.ordered()` pins.
#[test]
fn ordered_pins_the_comparison_a_lowered_int_ne_holds() {
    let function = lowered_int_ne_5_3();
    let swapped = || int_ne(int_const(3u128), int_const(5u128));
    a::matches(&function, swapped().into_pattern(), 1);
    a::none(&function, swapped().ordered().into_pattern());
    a::matches(
        &function,
        int_ne(int_const(5u128), int_const(3u128))
            .ordered()
            .into_pattern(),
        1,
    );
}

/// The pin lands under every wrapper, on either side of `.ordered()`.
#[test]
fn ordered_pins_a_lowered_int_ne_under_its_wrappers() {
    let function = lowered_int_ne_5_3();
    let c = Capture::new();
    let swapped = || int_ne(int_const(3u128), int_const(5u128));
    a::none(&function, swapped().capture(c).ordered().into_pattern());
    a::none(&function, swapped().ordered().capture(c).into_pattern());
    a::none(&function, swapped().of_width(1).ordered().into_pattern());
    a::none(&function, swapped().ordered().of_width(1).into_pattern());
    a::none(
        &function,
        inputs_of_width(1, swapped()).ordered().into_pattern(),
    );
    a::none(
        &function,
        inputs_of_width(1, swapped().ordered()).into_pattern(),
    );

    let written = || int_ne(int_const(5u128), int_const(3u128));
    a::matches(&function, written().capture(c).ordered().into_pattern(), 1);
    a::matches(&function, written().ordered().capture(c).into_pattern(), 1);
    a::matches(&function, written().of_width(1).ordered().into_pattern(), 1);
    a::matches(&function, written().ordered().of_width(1).into_pattern(), 1);
    a::matches(
        &function,
        inputs_of_width(1, written()).ordered().into_pattern(),
        1,
    );
    a::matches(
        &function,
        inputs_of_width(1, written().ordered()).into_pattern(),
        1,
    );
}

/// Nested as an operand, the pin still reaches the inner comparison.
#[test]
fn ordered_pins_a_lowered_int_ne_nested_as_an_operand() {
    let function = lowered_int_ne_5_3();
    a::none(
        &function,
        int_zero_extend(int_ne(int_const(3u128), int_const(5u128)).ordered()).into_pattern(),
    );
    a::matches(
        &function,
        int_zero_extend(int_ne(int_const(5u128), int_const(3u128)).ordered()).into_pattern(),
        1,
    );
}

/// `float_ne(1.0, 2.0)` is `Xor(FloatEqual(1.0, 2.0), 1)`: as for `int_ne`.
#[test]
fn ordered_pins_the_comparison_a_lowered_float_ne_holds() {
    let function = {
        let mut t = Tb::empty();
        let a = t.f64(1.0);
        let b = t.f64(2.0);
        let eq = t.fcmp(a, b, FloatCmpOp::Equal);
        let ne = t.bool_not(eq);
        let as_int = t.as_int(ne, strider_ir::node::ValueType::int(64));
        t.ret_val(as_int)
    };
    let ne = |l: f64, r: f64| float_ne(float_const(l.to_bits()), float_const(r.to_bits()));
    a::none(&function, ne(2.0, 1.0).ordered().into_pattern());
    a::matches(&function, ne(1.0, 2.0).ordered().into_pattern(), 1);
}

/// `float_le(l, r)` is `Or(FloatLess(l, r), FloatEqual(l, r))`: both
/// comparisons hold the written pair, so `.ordered()` pins the `Equal` (the
/// `Less` orders its pair already). The `Or` is not the pair holder, so its
/// operands may still come either way round.
fn lowered_float_le(eq_swapped: bool) -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.f64(1.0);
    let b = t.f64(2.0);
    let lt = t.fcmp(a, b, FloatCmpOp::Less);
    let eq = if eq_swapped {
        t.fcmp(b, a, FloatCmpOp::Equal)
    } else {
        t.fcmp(a, b, FloatCmpOp::Equal)
    };
    let le = t.bool_bin(eq, lt, IntBinaryOp::Or);
    let as_int = t.as_int(le, strider_ir::node::ValueType::int(64));
    t.ret_val(as_int)
}

#[test]
fn ordered_pins_both_comparisons_of_float_le() {
    let le = || {
        float_le(
            float_const(1.0_f64.to_bits()),
            float_const(2.0_f64.to_bits()),
        )
    };
    let written = lowered_float_le(false);
    a::matches(&written, le().into_pattern(), 1);
    a::matches(&written, le().ordered().into_pattern(), 1);
    let swapped = lowered_float_le(true);
    a::matches(&swapped, le().into_pattern(), 1);
    a::none(&swapped, le().ordered().into_pattern());
}

/// `float_is_nan(x)` is `Xor(FloatEqual(x, x), 1)`, one operand in both
/// equality slots, so `.ordered()` adds nothing, and an `Xor` holding its
/// `1` first still matches.
#[test]
fn ordered_is_a_no_op_on_float_is_nan() {
    let function = {
        let mut t = Tb::empty();
        let x = t.f64(1.0);
        let one = t.boolean(true);
        let eq = t.fcmp(x, x, FloatCmpOp::Equal);
        let nan = t.bool_bin(one, eq, IntBinaryOp::Xor);
        let as_int = t.as_int(nan, strider_ir::node::ValueType::int(64));
        t.ret_val(as_int)
    };
    let nan = || float_is_nan(float_const(1.0_f64.to_bits()));
    a::matches(&function, nan().into_pattern(), 1);
    a::matches(&function, nan().ordered().into_pattern(), 1);
}

/// `return(zext(Xor(1, cmp(l, r))))`: a lowered `!=` / `<=` with its `Xor`
/// operands the other way round from the lifter's.
fn one_first_negated_cmp(l: u64, r: u64, op: IntCmpOp) -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.u64(l);
    let b = t.u64(r);
    let one = t.boolean(true);
    let cmp = t.int_cmp(a, b, op);
    let negated = t.bool_bin(one, cmp, IntBinaryOp::Xor);
    let wide = t.zext_to(negated, strider_ir::node::ValueType::int(64));
    t.ret_val(wide)
}

/// The `Xor` a lowered `int_ne` adds stays commutative under `.ordered()`,
/// and a second `.ordered()` pins nothing more.
#[test]
fn ordered_leaves_the_xor_of_a_lowered_int_ne_commutative() {
    let function = one_first_negated_cmp(5, 3, IntCmpOp::Equal);
    let written = || int_ne(int_const(5u128), int_const(3u128));
    a::matches(&function, written().ordered().into_pattern(), 1);
    a::matches(&function, written().ordered().ordered().into_pattern(), 1);
    a::none(
        &function,
        int_ne(int_const(3u128), int_const(5u128))
            .ordered()
            .into_pattern(),
    );
}

/// `int_le(l, r)` is `Xor(Less(r, l), 1)`: the `Less` orders its pair
/// already, so `.ordered()` matches what the plain pattern does, and the
/// `Xor` still swaps. `int_sle` is the same over `Sless`.
#[test]
fn ordered_is_a_no_op_on_int_le_and_int_sle() {
    fn check<P: MatchPat + 'static>(op: IntCmpOp, le: impl Fn(u128, u128) -> P) {
        let function = one_first_negated_cmp(3, 5, op);
        a::matches(&function, le(5, 3).into_pattern(), 1);
        a::matches(&function, le(5, 3).ordered().into_pattern(), 1);
        a::none(&function, le(3, 5).into_pattern());
        a::none(&function, le(3, 5).ordered().into_pattern());
    }
    check(IntCmpOp::Less, |l, r| int_le(int_const(l), int_const(r)));
    check(IntCmpOp::Sless, |l, r| int_sle(int_const(l), int_const(r)));
}

/// A raw kind-`Any` node can hold an existential beside one fixed operand.
/// Matched at an `Add`, the fixed operand roams both slots, so neither is
/// pinned away from the existential, which takes whichever one is left.
#[test]
fn an_existential_beside_a_commutative_operand_takes_the_other_slot() {
    let function = shapes::int_bin(3, 5, IntBinaryOp::Add);
    let mut b = MatcherBuilder::new();
    let five = MatchPat::compile(int_const(5u128), &mut b);
    let three = MatchPat::compile(int_const(3u128), &mut b);
    let node = b.node(KindSpec::Any);
    b.input(node, 0, five);
    b.input_any(node, three);
    let _out = b.value_output(node, 0);
    a::matches(&function, b.finish(), 1);
}

/// The slot the roaming operand took is claimed, so a wildcard existential
/// beside it takes only the other one: one configuration, not one per slot.
#[test]
fn an_existential_wildcard_skips_the_slot_the_roaming_operand_took() {
    let function = shapes::int_bin(3, 5, IntBinaryOp::Add);
    let k = Capture::new();
    let mut b = MatcherBuilder::new();
    let five = MatchPat::compile(int_const(5u128), &mut b);
    let rest = MatchPat::compile(anything().capture(k), &mut b);
    let node = b.node(KindSpec::Any);
    b.input(node, 0, five);
    b.input_any(node, rest);
    let _out = b.value_output(node, 0);
    let hits = a::matches(&function, b.finish(), 1);
    assert_eq!(
        hits[0].bindings().get_uint(k, &function),
        Some(UBig::from(3u8))
    );
}
