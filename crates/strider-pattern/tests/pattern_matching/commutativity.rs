use strider_ir::{FloatBinaryOp, FloatCmpOp, IRBuilderExt, IntBinaryOp, IntCmpOp};
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

#[test]
fn add_commutes() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    a::matches_both_orders(
        &function,
        int_add(int_const(5u128), int_const(3u128)).into_pattern(),
        int_add(int_const(3u128), int_const(5u128)).into_pattern(),
    );
}

#[test]
fn mul_commutes() {
    let function = shapes::int_bin(7, 9, IntBinaryOp::Mul);
    a::matches_both_orders(
        &function,
        int_mul(int_const(7u128), int_const(9u128)).into_pattern(),
        int_mul(int_const(9u128), int_const(7u128)).into_pattern(),
    );
}

#[test]
fn and_or_xor_commute() {
    let g_and = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::And);
    let g_or = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::Or);
    let g_xor = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::Xor);
    a::matches_both_orders(
        &g_and,
        int_and(int_const(0xF0u128), int_const(0x0Fu128)).into_pattern(),
        int_and(int_const(0x0Fu128), int_const(0xF0u128)).into_pattern(),
    );
    a::matches_both_orders(
        &g_or,
        int_or(int_const(0xF0u128), int_const(0x0Fu128)).into_pattern(),
        int_or(int_const(0x0Fu128), int_const(0xF0u128)).into_pattern(),
    );
    a::matches_both_orders(
        &g_xor,
        int_xor(int_const(0xF0u128), int_const(0x0Fu128)).into_pattern(),
        int_xor(int_const(0x0Fu128), int_const(0xF0u128)).into_pattern(),
    );
}

#[test]
fn ordered_rejects_swap() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    a::none(
        &function,
        int_add(int_const(3u128), int_const(5u128))
            .ordered()
            .into_pattern(),
    );
    a::matches(
        &function,
        int_add(int_const(5u128), int_const(3u128))
            .ordered()
            .into_pattern(),
        1,
    );
}

#[test]
fn ordered_mul_rejects_swap() {
    let function = shapes::int_bin(7, 9, IntBinaryOp::Mul);
    a::none(
        &function,
        int_mul(int_const(9u128), int_const(7u128))
            .ordered()
            .into_pattern(),
    );
}

/// `int_add(anything(), anything())` on `int_add(5, 3)`: no captures, so both orderings produce
/// the same binding map and collapse to one hit. Over-counting here means the
/// swap retry is emitting duplicates.
#[test]
fn commutative_match_emits_single_match_per_root() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    a::matches(&function, int_add(anything(), anything()).into_pattern(), 1);
}

/// Constant dedup makes both operands of `int_add(5, 5)` one `ValueId`, so the
/// swap retry must still yield a single hit.
#[test]
fn commutative_match_with_identical_operands_emits_one() {
    let function = shapes::int_bin(5, 5, IntBinaryOp::Add);
    a::matches(
        &function,
        int_add(int_const(5u128), int_const(5u128)).into_pattern(),
        1,
    );
}
/// `find_all` dedups by the capture->binding MAP, not by root, so a capture on
/// one operand of a commutative node yields one hit per operand it can bind, so
/// a single hit really does mean the binding is unambiguous.
#[test]
fn commutative_capture_reports_both_operand_bindings() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    let k = Capture::new();
    let hits = a::matches(
        &function,
        int_add(anything().capture(k), anything()).into_pattern(),
        2,
    );
    let mut bound: Vec<Option<u128>> = hits
        .iter()
        .map(|m| m.bindings().get_uint(k, &function))
        .collect();
    // Enumeration order is pinned: natural ordering first, then swapped.
    assert_eq!(bound, vec![Some(5), Some(3)], "both operands must bind");
    bound.sort_unstable();
    bound.dedup();
    assert_eq!(bound.len(), 2, "the two bindings must be DISTINCT");
}

/// Both operands the same value (constant dedup) means the two orderings
/// produce an identical binding map, hence one match, not two.
#[test]
fn commutative_capture_identical_operands_dedups_to_one() {
    let function = shapes::int_bin(5, 5, IntBinaryOp::Add);
    let k = Capture::new();
    let hits = a::matches(
        &function,
        int_add(anything().capture(k), anything()).into_pattern(),
        1,
    );
    assert_eq!(hits[0].bindings().get_uint(k, &function), Some(5));
}

/// Arrangements MULTIPLY across nested commutative nodes: `int_add(int_add(w,x),
/// int_add(y,z))` over `int_add(int_add(5,3), int_add(7,9))` has three independent swap
/// choices, so 2*2*2 = 8 distinct binding maps, not one node's worth. The
/// single-node tests above cannot observe this.
#[test]
fn nested_commutative_arrangements_multiply() {
    let mut t = Tb::empty();
    let (c5, c3) = (t.u64(5), t.u64(3));
    let (c7, c9) = (t.u64(7), t.u64(9));
    let lhs = t.int_bin(c5, c3, IntBinaryOp::Add);
    let rhs = t.int_bin(c7, c9, IntBinaryOp::Add);
    let root = t.int_bin(lhs, rhs, IntBinaryOp::Add);
    let function = t.ret_val(root);

    let (w, x, y, z) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let pat = int_add(
        int_add(anything().capture(w), anything().capture(x)),
        int_add(anything().capture(y), anything().capture(z)),
    )
    .into_pattern();
    let hits = a::matches(&function, pat, 8);

    // 8 hits must mean 8 different binding maps, not one map reported 8 times.
    let mut seen: Vec<(u128, u128, u128, u128)> = hits
        .iter()
        .map(|m| {
            let g = |c| m.bindings().get_uint(c, &function).expect("bound const");
            (g(w), g(x), g(y), g(z))
        })
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        8,
        "all 8 arrangements must be distinct: {seen:?}"
    );

    // Each inner pair stays together (the outer swap carries both), never a
    // cross-node mix.
    for (wv, xv, yv, zv) in &seen {
        let mut left = [*wv, *xv];
        let mut right = [*yv, *zv];
        left.sort_unstable();
        right.sort_unstable();
        assert!(
            (left == [3, 5] && right == [7, 9]) || (left == [7, 9] && right == [3, 5]),
            "operands must not migrate across the inner nodes: {wv},{xv} / {yv},{zv}"
        );
    }
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
        Some(5),
        "ordered() pins the capture to operand slot 0",
    );
}

/// A capture on a NON-commutative node has one ordering, so the enumeration
/// must not manufacture duplicates.
#[test]
fn non_commutative_capture_stays_single() {
    let function = shapes::int_bin(20, 4, IntBinaryOp::Div);
    let k = Capture::new();
    let hits = a::matches(
        &function,
        int_div(anything().capture(k), anything()).into_pattern(),
        1,
    );
    assert_eq!(hits[0].bindings().get_uint(k, &function), Some(20));
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
        Some(5),
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

#[test]
fn div_shl_shr_do_not_commute() {
    let g_div = shapes::int_bin(20, 4, IntBinaryOp::Div);
    let g_shl = shapes::int_bin(1, 8, IntBinaryOp::ShiftLeft);
    let g_shr = shapes::int_bin(256, 2, IntBinaryOp::ShiftRight);

    a::none(
        &g_div,
        int_div(int_const(4u128), int_const(20u128)).into_pattern(),
    );
    a::none(
        &g_shl,
        int_shl(int_const(8u128), int_const(1u128)).into_pattern(),
    );
    a::none(
        &g_shr,
        int_shr(int_const(2u128), int_const(256u128)).into_pattern(),
    );
}

#[test]
fn bool_and_or_xor_commute() {
    let g_and = shapes::bool_bin(true, false, IntBinaryOp::And);
    let g_or = shapes::bool_bin(true, false, IntBinaryOp::Or);
    let g_xor = shapes::bool_bin(true, false, IntBinaryOp::Xor);
    a::matches_both_orders(
        &g_and,
        bool_and(bool_const(true), bool_const(false)).into_pattern(),
        bool_and(bool_const(false), bool_const(true)).into_pattern(),
    );
    a::matches_both_orders(
        &g_or,
        bool_or(bool_const(true), bool_const(false)).into_pattern(),
        bool_or(bool_const(false), bool_const(true)).into_pattern(),
    );
    a::matches_both_orders(
        &g_xor,
        bool_xor(bool_const(true), bool_const(false)).into_pattern(),
        bool_xor(bool_const(false), bool_const(true)).into_pattern(),
    );
}

/// `bool_binary` is chainable: bare it matches commutatively, `.ordered()`
/// pins the operand slots.
#[test]
fn bool_binary_ordered_rejects_swap() {
    let function = shapes::bool_bin(true, false, IntBinaryOp::And);
    a::matches(
        &function,
        bool_binary(IntBinaryOp::And, bool_const(true), bool_const(false)).into_pattern(),
        1,
    );
    a::matches(
        &function,
        bool_binary(IntBinaryOp::And, bool_const(false), bool_const(true)).into_pattern(),
        1,
    );
    a::none(
        &function,
        bool_binary(IntBinaryOp::And, bool_const(false), bool_const(true))
            .ordered()
            .into_pattern(),
    );
    a::matches(
        &function,
        bool_binary(IntBinaryOp::And, bool_const(true), bool_const(false))
            .ordered()
            .into_pattern(),
        1,
    );
}

#[test]
fn bool_and_ordered_rejects_swap() {
    let function = shapes::bool_bin(true, false, IntBinaryOp::And);
    a::none(
        &function,
        bool_and(bool_const(false), bool_const(true))
            .ordered()
            .into_pattern(),
    );
    a::matches(
        &function,
        bool_and(bool_const(true), bool_const(false))
            .ordered()
            .into_pattern(),
        1,
    );
}

/// The `I1` output guard survives `.ordered()`: neither form may match a
/// same-shaped wide `And`, even with the operand order lined up.
#[test]
fn bool_binary_ordered_requires_i1_output() {
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::RegisterSet;

    let mut b = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let x = b.build_int_const(0xFFu64, ValueType::I64).expect("x");
    let one = b.build_int_const(1u64, ValueType::I64).expect("one");
    let wide_and = b
        .build_int_binary_operation(x, one, IntBinaryOp::And, ValueType::I64)
        .expect("wide and");
    b.build_return(Some(wide_and), &[]).expect("ret");
    let function = b.build().expect("build");

    a::none(
        &function,
        bool_binary(IntBinaryOp::And, anything(), anything()).into_pattern(),
    );
    a::none(
        &function,
        bool_binary(IntBinaryOp::And, anything(), anything())
            .ordered()
            .into_pattern(),
    );
}

// `IntCmpOp::Carry` / `Scarry` are commutative in `NodeKind::is_commutative`:
// an addition carry/overflow commutes because addition does.

#[test]
fn int_carry_commutes() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Carry);
    a::matches_both_orders(
        &function,
        int_carry(int_const(5u128), int_const(3u128)).into_pattern(),
        int_carry(int_const(3u128), int_const(5u128)).into_pattern(),
    );
}

#[test]
fn ordered_int_carry_rejects_swap() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Carry);
    a::none(
        &function,
        int_carry(int_const(3u128), int_const(5u128))
            .ordered()
            .into_pattern(),
    );
    a::matches(
        &function,
        int_carry(int_const(5u128), int_const(3u128))
            .ordered()
            .into_pattern(),
        1,
    );
}

#[test]
fn int_scarry_commutes() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Scarry);
    a::matches_both_orders(
        &function,
        int_scarry(int_const(5u128), int_const(3u128)).into_pattern(),
        int_scarry(int_const(3u128), int_const(5u128)).into_pattern(),
    );
}

#[test]
fn ordered_int_scarry_rejects_swap() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Scarry);
    a::none(
        &function,
        int_scarry(int_const(3u128), int_const(5u128))
            .ordered()
            .into_pattern(),
    );
    a::matches(
        &function,
        int_scarry(int_const(5u128), int_const(3u128))
            .ordered()
            .into_pattern(),
        1,
    );
}

#[test]
fn float_add_and_mul_commute() {
    let g_add = shapes::float_bin(2.0, 5.0, FloatBinaryOp::Add);
    let g_mul = shapes::float_bin(2.0, 5.0, FloatBinaryOp::Mul);
    a::matches_both_orders(
        &g_add,
        float_add(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into_pattern(),
        float_add(float_const(5.0f64.to_bits()), float_const(2.0f64.to_bits())).into_pattern(),
    );
    a::matches_both_orders(
        &g_mul,
        float_mul(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into_pattern(),
        float_mul(float_const(5.0f64.to_bits()), float_const(2.0f64.to_bits())).into_pattern(),
    );
}

/// `FloatBinaryOp::Sub` is not a primitive: `pattern::float_sub` is an alias
/// for the lowered `FloatAdd(a, Neg(b))`, which the fixture builds by hand.
#[test]
fn float_sub_and_div_do_not_commute() {
    let g_sub = {
        let mut t = Tb::empty();
        let a = t.f64(5.0);
        let b = t.f64(2.0);
        let neg_b = t.fun(
            b,
            strider_ir::FloatUnaryOp::Neg,
            strider_ir::node::ValueType::F64,
        );
        let lowered = t.fbin(
            a,
            neg_b,
            FloatBinaryOp::Add,
            strider_ir::node::ValueType::F64,
        );
        let as_int = t.float_to_int(lowered, strider_ir::node::ValueType::I64);
        t.ret_val(as_int)
    };
    a::none(
        &g_sub,
        float_sub(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into_pattern(),
    );

    let g_div = shapes::float_bin(10.0, 4.0, FloatBinaryOp::Div);
    a::none(
        &g_div,
        float_div(
            float_const(4.0f64.to_bits()),
            float_const(10.0f64.to_bits()),
        )
        .into_pattern(),
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
        Some(5)
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
        Some(3),
        "swap retry must rebind the guarded child to the 3-operand",
    );
}

/// A ROOT `when_match` guard runs after the inputs resolved in SOME order; a
/// rejection on a commutative node re-drives the swapped order before giving
/// up, so an order-sensitive root guard still finds a valid ordering.
#[test]
fn root_when_match_rejection_redrives_swap() {
    use strider_ir::IRViewer;

    let function = shapes::int_bin(2, 3, IntBinaryOp::Add);
    let l = Capture::new();
    // Inputs match naturally with l bound to 2; the guard demands 3, which
    // only the swapped order satisfies.
    let pat = int_add(anything().capture(l), anything())
        .when_match(move |m, _ty, b| {
            let Some(v) = b.get_value(l) else {
                return false;
            };
            m.function().int_const_u128(v) == Some(3)
        })
        .into_pattern();
    let m = a::unique(&function, pat);
    assert_eq!(
        m.bindings().get_uint(l, &function),
        Some(3),
        "root guard rejection must re-drive the swapped operand order",
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
        let z = t.zext_to(sum, ValueType::I128);
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
        Some(3),
        "unary-parent guard must re-drive the commutative child's operand order",
    );
}

/// `a OP b` returned as the boolean result, cast to u64 for typability.
fn graph_float_cmp(l: f64, r: f64, op: FloatCmpOp) -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.f64(l);
    let b = t.f64(r);
    let v = t.fcmp(a, b, op);
    let as_int = t.as_int(v, strider_ir::node::ValueType::I64);
    t.ret_val(as_int)
}

#[test]
fn float_eq_commutes() {
    let function = graph_float_cmp(1.0, 2.0, FloatCmpOp::Equal);
    a::matches_both_orders(
        &function,
        float_cmp(
            FloatCmpOp::Equal,
            float_const(1.0_f64.to_bits()),
            float_const(2.0_f64.to_bits()),
        )
        .into_pattern(),
        float_cmp(
            FloatCmpOp::Equal,
            float_const(2.0_f64.to_bits()),
            float_const(1.0_f64.to_bits()),
        )
        .into_pattern(),
    );
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
        let as_int = t.as_int(ne, strider_ir::node::ValueType::I64);
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

#[test]
fn float_lt_does_not_commute() {
    let function = graph_float_cmp(1.0, 2.0, FloatCmpOp::Less);
    a::matches(
        &function,
        float_cmp(
            FloatCmpOp::Less,
            float_const(1.0_f64.to_bits()),
            float_const(2.0_f64.to_bits()),
        )
        .into_pattern(),
        1,
    );
    // Less is directional.
    a::none(
        &function,
        float_cmp(
            FloatCmpOp::Less,
            float_const(2.0_f64.to_bits()),
            float_const(1.0_f64.to_bits()),
        )
        .into_pattern(),
    );
}
