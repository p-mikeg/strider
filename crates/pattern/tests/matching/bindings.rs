//! Direct tests for the `pattern::Bindings` public API.
//!
//! Every family (13 total) has three contract checks:
//!   * first bind returns `true` and is retrievable;
//!   * idempotent rebind returns `true`;
//!   * conflicting rebind returns `false` AND preserves the original.
//!
//! Primitive families (`IntVar`, `BoolVar`, `FloatVar`) keep one-assertion-
//! per-test for maximum isolation; the 10 op-variant families share a macro
//! that generates one combined test per family.

use ir::node::NodeKind;
use pattern::*;

use super::support::Tb;

// ── IntVar ───────────────────────────────────────────────────────────────────

#[test]
fn int_var_bind_and_get() {
    let mut b = Bindings::default();
    let v = IntVar::new();

    assert_eq!(b.get_int(v), None);
    assert!(b.bind_int(v, 42));
    assert_eq!(b.get_int(v), Some(42));
}

#[test]
fn int_var_idempotent_rebind_preserves_binding() {
    let mut b = Bindings::default();
    let v = IntVar::new();
    assert!(b.bind_int(v, 42));
    assert!(b.bind_int(v, 42));
    assert_eq!(b.get_int(v), Some(42));
}

#[test]
fn int_var_conflict_fails_and_preserves_original() {
    let mut b = Bindings::default();
    let v = IntVar::new();
    assert!(b.bind_int(v, 42));
    assert!(!b.bind_int(v, 43));
    assert_eq!(b.get_int(v), Some(42));
}

// ── BoolVar ──────────────────────────────────────────────────────────────────

#[test]
fn bool_var_bind_and_get() {
    let mut b = Bindings::default();
    let v = BoolVar::new();
    assert_eq!(b.get_bool(v), None);
    assert!(b.bind_bool(v, true));
    assert_eq!(b.get_bool(v), Some(true));
}

#[test]
fn bool_var_idempotent_rebind_preserves_binding() {
    let mut b = Bindings::default();
    let v = BoolVar::new();
    assert!(b.bind_bool(v, false));
    assert!(b.bind_bool(v, false));
    assert_eq!(b.get_bool(v), Some(false));
}

#[test]
fn bool_var_conflict_fails_and_preserves_original() {
    let mut b = Bindings::default();
    let v = BoolVar::new();
    assert!(b.bind_bool(v, true));
    assert!(!b.bind_bool(v, false));
    assert_eq!(b.get_bool(v), Some(true));
}

// ── FloatVar ─────────────────────────────────────────────────────────────────

#[test]
fn float_var_bind_and_get() {
    let mut b = Bindings::default();
    let v = FloatVar::new();
    let bits = 1.0f64.to_bits();

    assert_eq!(b.get_float_bits(v), None);
    assert!(b.bind_float(v, bits));
    assert_eq!(b.get_float_bits(v), Some(bits));
}

#[test]
fn float_var_idempotent_rebind_preserves_binding() {
    let mut b = Bindings::default();
    let v = FloatVar::new();
    let bits = 2.0f64.to_bits();
    assert!(b.bind_float(v, bits));
    assert!(b.bind_float(v, bits));
    assert_eq!(b.get_float_bits(v), Some(bits));
}

#[test]
fn float_var_conflict_fails_and_preserves_original() {
    let mut b = Bindings::default();
    let v = FloatVar::new();
    let a = 1.0f64.to_bits();
    let c = 2.0f64.to_bits();
    assert!(b.bind_float(v, a));
    assert!(!b.bind_float(v, c));
    assert_eq!(b.get_float_bits(v), Some(a));
}

// ── Capture (unified node + output) ──────────────────────────────────────────

#[test]
fn capture_bind_and_get_with_real_output_ids() {
    // Build `return(IntConst(1) + IntConst(2))` to harvest two distinct
    // `NodeOutputId`s from the graph.
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let s = t.add(a, b);
    let g = t.ret_val(s);

    let na = g.graph.get_node_from_output(a);
    let nb = g.graph.get_node_from_output(b);

    let mut bindings = Bindings::default();
    let v = Capture::new();
    assert_eq!(bindings.get(v), None);
    let ba = pattern::Binding::new(na, Some(a));
    let bb = pattern::Binding::new(nb, Some(b));
    assert!(bindings.bind_capture(v, ba));
    assert_eq!(bindings.get(v), Some(a));

    // Idempotent with same output.
    assert!(bindings.bind_capture(v, ba));
    assert_eq!(bindings.get(v), Some(a));

    // Conflict preserves original.
    assert!(!bindings.bind_capture(v, bb));
    assert_eq!(bindings.get(v), Some(a));
}

#[test]
fn capture_bind_and_get_with_real_node_ids() {
    // Thread distinct values through an Add so both constants stay reachable.
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let s = t.add(a, b);
    let g = t.ret_val(s);

    let mut ids = g
        .preorder()
        .filter(|&n| matches!(g.graph.node_kind(n), NodeKind::IntConst(_)));
    let n1 = ids.next().expect("first const node");
    let n2 = ids.next().expect("second const node");
    assert_ne!(n1, n2);

    let mut bindings = Bindings::default();
    let v = Capture::new();
    assert_eq!(bindings.get_node(v), None);
    let b1 = pattern::Binding::new(n1, None);
    let b2 = pattern::Binding::new(n2, None);
    assert!(bindings.bind_capture(v, b1));
    assert_eq!(bindings.get_node(v), Some(n1));
    assert!(bindings.bind_capture(v, b1));
    assert!(!bindings.bind_capture(v, b2));
    assert_eq!(bindings.get_node(v), Some(n1));
}

// ── Op-variant families (10) ─────────────────────────────────────────────────
//
// Each family shares the same bind-and-get / idempotent / conflict contract.
// The macro generates a single `#[test]` per family that checks all three,
// trading per-assertion isolation for coverage breadth.

macro_rules! op_variant_contract {
    ($test_name:ident, $ty:ty, $bind:ident, $get:ident, $a:expr, $b:expr) => {
        #[test]
        fn $test_name() {
            let mut bindings = Bindings::default();
            let v = <$ty>::new();
            assert_eq!(bindings.$get(v), None);
            assert!(bindings.$bind(v, $a));
            assert_eq!(bindings.$get(v), Some($a));
            // Idempotent.
            assert!(bindings.$bind(v, $a));
            assert_eq!(bindings.$get(v), Some($a));
            // Conflict.
            assert!(!bindings.$bind(v, $b));
            assert_eq!(bindings.$get(v), Some($a));
        }
    };
}

op_variant_contract!(
    int_binary_op_var_contract,
    IntBinaryOpVar,
    bind_int_binary_op,
    get_int_binary_op,
    IntBinaryOp::Add,
    IntBinaryOp::Sub
);
op_variant_contract!(
    int_unary_op_var_contract,
    IntUnaryOpVar,
    bind_int_unary_op,
    get_int_unary_op,
    IntUnaryOp::Neg,
    IntUnaryOp::Not
);
op_variant_contract!(
    int_cmp_op_var_contract,
    IntCmpOpVar,
    bind_int_cmp_op,
    get_int_cmp_op,
    IntCmpOp::Equal,
    IntCmpOp::Less
);
op_variant_contract!(
    bool_binary_op_var_contract,
    BoolBinaryOpVar,
    bind_bool_binary_op,
    get_bool_binary_op,
    BoolBinaryOp::And,
    BoolBinaryOp::Or
);
// `BoolUnaryOp` has a single variant (`Neg`) — no "conflict" contract to
// exercise.  Just verify bind-and-get + idempotent rebind.
#[test]
fn bool_unary_op_var_bind_and_idempotent() {
    let mut b = Bindings::default();
    let v = BoolUnaryOpVar::new();
    assert_eq!(b.get_bool_unary_op(v), None);
    assert!(b.bind_bool_unary_op(v, BoolUnaryOp::Neg));
    assert_eq!(b.get_bool_unary_op(v), Some(BoolUnaryOp::Neg));
    assert!(b.bind_bool_unary_op(v, BoolUnaryOp::Neg));
    assert_eq!(b.get_bool_unary_op(v), Some(BoolUnaryOp::Neg));
}

op_variant_contract!(
    float_binary_op_var_contract,
    FloatBinaryOpVar,
    bind_float_binary_op,
    get_float_binary_op,
    FloatBinaryOp::Add,
    FloatBinaryOp::Sub
);
op_variant_contract!(
    float_unary_op_var_contract,
    FloatUnaryOpVar,
    bind_float_unary_op,
    get_float_unary_op,
    FloatUnaryOp::Neg,
    FloatUnaryOp::Abs
);
op_variant_contract!(
    float_cmp_op_var_contract,
    FloatCmpOpVar,
    bind_float_cmp_op,
    get_float_cmp_op,
    FloatCmpOp::Equal,
    FloatCmpOp::Less
);

// ── Default / multi-family ───────────────────────────────────────────────────

#[test]
fn default_bindings_return_none_for_every_var_type() {
    let b = Bindings::default();
    assert_eq!(b.get(Capture::new()), None);
    assert_eq!(b.get_node(Capture::new()), None);
    assert_eq!(b.get_int(IntVar::new()), None);
    assert_eq!(b.get_bool(BoolVar::new()), None);
    assert_eq!(b.get_float_bits(FloatVar::new()), None);
    assert_eq!(b.get_int_binary_op(IntBinaryOpVar::new()), None);
    assert_eq!(b.get_int_unary_op(IntUnaryOpVar::new()), None);
    assert_eq!(b.get_int_cmp_op(IntCmpOpVar::new()), None);
    assert_eq!(b.get_bool_binary_op(BoolBinaryOpVar::new()), None);
    assert_eq!(b.get_bool_unary_op(BoolUnaryOpVar::new()), None);
    assert_eq!(b.get_float_binary_op(FloatBinaryOpVar::new()), None);
    assert_eq!(b.get_float_unary_op(FloatUnaryOpVar::new()), None);
    assert_eq!(b.get_float_cmp_op(FloatCmpOpVar::new()), None);
}

#[test]
fn many_families_coexist_in_one_bindings() {
    let mut b = Bindings::default();
    let iv = IntVar::new();
    let bv = BoolVar::new();
    let fv = FloatVar::new();
    let ibin = IntBinaryOpVar::new();
    let iun = IntUnaryOpVar::new();
    let icmp = IntCmpOpVar::new();
    let bbin = BoolBinaryOpVar::new();
    let bun = BoolUnaryOpVar::new();
    let fbin = FloatBinaryOpVar::new();
    let fun = FloatUnaryOpVar::new();
    let fcmp = FloatCmpOpVar::new();

    assert!(b.bind_int(iv, 7));
    assert!(b.bind_bool(bv, true));
    assert!(b.bind_float(fv, 1.5f64.to_bits()));
    assert!(b.bind_int_binary_op(ibin, IntBinaryOp::Add));
    assert!(b.bind_int_unary_op(iun, IntUnaryOp::Neg));
    assert!(b.bind_int_cmp_op(icmp, IntCmpOp::Equal));
    assert!(b.bind_bool_binary_op(bbin, BoolBinaryOp::And));
    assert!(b.bind_bool_unary_op(bun, BoolUnaryOp::Neg));
    assert!(b.bind_float_binary_op(fbin, FloatBinaryOp::Mul));
    assert!(b.bind_float_unary_op(fun, FloatUnaryOp::Sqrt));
    assert!(b.bind_float_cmp_op(fcmp, FloatCmpOp::Less));

    assert_eq!(b.get_int(iv), Some(7));
    assert_eq!(b.get_bool(bv), Some(true));
    assert_eq!(b.get_float_bits(fv), Some(1.5f64.to_bits()));
    assert_eq!(b.get_int_binary_op(ibin), Some(IntBinaryOp::Add));
    assert_eq!(b.get_int_unary_op(iun), Some(IntUnaryOp::Neg));
    assert_eq!(b.get_int_cmp_op(icmp), Some(IntCmpOp::Equal));
    assert_eq!(b.get_bool_binary_op(bbin), Some(BoolBinaryOp::And));
    assert_eq!(b.get_bool_unary_op(bun), Some(BoolUnaryOp::Neg));
    assert_eq!(b.get_float_binary_op(fbin), Some(FloatBinaryOp::Mul));
    assert_eq!(b.get_float_unary_op(fun), Some(FloatUnaryOp::Sqrt));
    assert_eq!(b.get_float_cmp_op(fcmp), Some(FloatCmpOp::Less));
}

// ── Globally unique IDs ──────────────────────────────────────────────────────

/// Every capture-variable family shares a single atomic counter; allocating
/// many across all families must produce all-distinct IDs.  `Debug` output is
/// the only public handle on the raw ID, so the test uses it as a set key.
#[test]
fn capture_ids_are_globally_unique_across_families_and_many_allocations() {
    const N: usize = 64;
    let mut ids: Vec<String> = Vec::with_capacity(N * 5);
    for _ in 0..N {
        ids.push(format!("{:?}", IntVar::new()));
        ids.push(format!("{:?}", BoolVar::new()));
        ids.push(format!("{:?}", FloatVar::new()));
        ids.push(format!("{:?}", Capture::new()));
        ids.push(format!("{:?}", Capture::new()));
    }
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}
