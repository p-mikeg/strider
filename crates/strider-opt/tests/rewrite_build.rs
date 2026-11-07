//! RHS template construction: computed constants via `int_const_with!`,
//! nested templates, and the capture-binding checks `rewrite_rule_runtime`
//! makes at construction time.

use strider_ir::node::{NodeKind, ValueType as T};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::{make_empty_fn, make_fn_with_var, reg_vn};

use strider_opt::{EditFunction, UnboundCapture, rewrite_rule, rewrite_rule_runtime};
use strider_pattern::{
    Capture, MatchPat, Matcher, TemplatePat, int_add, int_const, int_const_with, one_of, template,
    var,
};

/// `add(var(x), int_const(0)) -> var(x)` fires and redirects uses.
#[test]
fn add_zero_identity_fires_and_redirects() {
    let x = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(7u64, T::int(64))?;
        let zero = b.build_int_const(0u64, T::int(64))?;
        let sum = b.build_int_binary_operation(a, zero, IntBinaryOp::Add, T::int(64))?;
        // A consumer of the Add, so the rewrite has a use to redirect.
        b.build_int_binary_operation(sum, a, IntBinaryOp::Or, T::int(64))
    })
    .unwrap();

    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));

    let add_root = {
        let m = Matcher::new(&fx);
        let pat = int_add(var(x), int_const(0u128)).into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert_eq!(hits.len(), 1);
        hits[0].root()
    };

    let mut ctx = EditFunction::new(&mut fx);
    let fired = rule(&mut ctx, add_root).unwrap().is_some();
    assert!(fired, "add-zero identity should fire");

    // The Or consumer now reads the IntConst(7) twice.
    let or_reads_const = ctx
        .function()
        .node_inputs(or_node(ctx.function()))
        .into_iter()
        .map(|inp| ctx.function().producer(inp))
        .all(|n| {
            let f = ctx.function();
            matches!(f.node_kind(n), NodeKind::IntConst(_))
                && f.int_const_u128(f.node_outputs(n)[0]) == Some(7)
        });
    assert!(or_reads_const, "Or should now read the redirected constant");
}

fn or_node(f: &strider_ir::Function) -> strider_ir::node::NodeId {
    f.walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Or)))
        .unwrap()
}

/// A binary op nesting a computed `int_const_with!` const is a valid
/// template RHS: `(x + 1) + 2` folds to `x + 3`.
#[test]
fn reassoc_rule_nests_computed_const_in_add() {
    let x = Capture::new();
    let c1 = Capture::new();
    let c2 = Capture::new();

    // `(x + 1) + 2` over a tracked register var.
    let (mut fx, _xval) = make_fn_with_var(reg_vn(0, 8), |b, xv| {
        let one = b.build_int_const(1u64, T::int(64))?;
        let inner = b.build_int_binary_operation(xv, one, IntBinaryOp::Add, T::int(64))?;
        let two = b.build_int_const(2u64, T::int(64))?;
        b.build_int_binary_operation(inner, two, IntBinaryOp::Add, T::int(64))
    })
    .unwrap();

    let rule = rewrite_rule(
        int_add(int_add(var(x), int_const(c1)), int_const(c2)),
        template::int_add(var(x), int_const_with!([c1: uint, c2: uint] => c1 + c2)),
    );

    let outer_root = {
        let m = Matcher::new(&fx);
        let pat = int_add(int_add(var(x), int_const(c1)), int_const(c2)).into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert!(!hits.is_empty(), "reassoc LHS should match (x + 1) + 2");
        hits[0].root()
    };

    let mut ctx = EditFunction::new(&mut fx);
    let fired = rule(&mut ctx, outer_root).unwrap().is_some();
    assert!(fired, "reassoc rule should fire on (x + 1) + 2");

    let has_three = ctx.function().walk().any(|n| {
        let f = ctx.function();
        matches!(f.node_kind(n), NodeKind::IntConst(_))
            && f.int_const_u128(f.node_outputs(n)[0]) == Some(3)
    });
    assert!(has_three, "(x + 1) + 2 should reassociate to x + 3");
}

/// `rewrite_rule` panics on an RHS capture the LHS does not bind;
/// `rewrite_rule_runtime` must surface the same rejection as an `Err`.
#[test]
fn rewrite_rule_runtime_rejects_unbound_capture_in_rhs() {
    let a = Capture::new();
    let b = Capture::new();
    // Never bound by the LHS.
    let c = Capture::new();

    let lhs = int_add(var(a), var(b)).into_pattern();
    let rhs = var(c).into_template();

    let err = match rewrite_rule_runtime(lhs, rhs) {
        Ok(_) => panic!("expected unbound-capture rejection, got Ok"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("does not bind"),
        "expected unbound-capture error mentioning \"does not bind\", got: {msg}"
    );
    let unbound = err.downcast_ref::<UnboundCapture>().expect("typed error");
    assert_eq!(unbound.capture, c);
    assert!(!unbound.on_some_arms);
}

/// A refused RHS fails the rule at construction, not on every firing.
#[test]
fn rewrite_rule_runtime_rejects_a_refused_template() {
    let a = Capture::new();
    let mut tb = strider_pattern::template::TemplateBuilder::new();
    let rhs = tb.capture(a);
    tb.set_value_ty(rhs, T::int(64));
    let err = match rewrite_rule_runtime(var(a).into_pattern(), tb.finish()) {
        Ok(_) => panic!("expected the template's refusal, got Ok"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("targets a capture"), "{err}");
}

/// A capture bound on only some `one_of` arms is rejected at construction,
/// since only one arm fires per match.
#[test]
fn rewrite_rule_runtime_rejects_capture_bound_on_only_some_arms() {
    let x = Capture::new();
    let k = Capture::new();

    // Arm 1 binds `k`; arm 2 binds only `x`.
    let lhs = one_of![int_add(var(x), int_const(k)), var(x)].into_pattern();
    let rhs = template::int_add(var(x), var(k)).into_template();

    let err = match rewrite_rule_runtime(lhs, rhs) {
        Ok(_) => panic!("expected partial-arm-binding rejection, got Ok"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("only on SOME alternation arms"),
        "expected a partial-arm-binding error, got: {msg}"
    );
    let unbound = err.downcast_ref::<UnboundCapture>().expect("typed error");
    assert_eq!(unbound.capture, k);
    assert!(unbound.on_some_arms);
}

/// The complement: a capture every arm binds is guaranteed, so the rule builds.
#[test]
fn rewrite_rule_runtime_accepts_capture_bound_on_every_arm() {
    let x = Capture::new();

    let lhs = one_of![
        int_add(var(x), int_const(0u128)),
        int_add(int_const(0u128), var(x))
    ]
    .into_pattern();
    let rhs = var(x).into_template();

    assert!(
        rewrite_rule_runtime(lhs, rhs).is_ok(),
        "a capture bound on every arm must be accepted"
    );
}

// A wildcard cannot reach `rewrite_rule_runtime`: its RHS is a `Template`, and
// a wildcard cannot be sealed into one. Both forms are compile-fail:
//
//     let rhs: strider_pattern::Template = any().into_template();
//     // error[E0277]: the trait bound `Any: TemplatePat` is not satisfied
//
//     let p: strider_pattern::Pattern = var(c).into_pattern();
//     rewrite_rule_runtime(lhs, p);
//     // error[E0308]: expected `Template`, found `Pattern`

/// A computed template constant is interned at the declared width, so a
/// rewrite at `I256` mints every limb instead of skipping past a carrier.
#[test]
fn computed_const_is_interned_at_an_i256_width() {
    use strider_ir::UBig;
    let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
    let ty = T::int(256);
    let big = UBig::from_words(&[1, 2, 3, 4]);

    let (mut fx, _xval) = make_fn_with_var(reg_vn(0, 32), |b, xv| {
        let one = b.build_int_const(big.clone(), ty)?;
        let inner = b.build_int_binary_operation(xv, one, IntBinaryOp::Add, ty)?;
        let two = b.build_int_const(UBig::from_words(&[5, 6, 7, 8]), ty)?;
        b.build_int_binary_operation(inner, two, IntBinaryOp::Add, ty)
    })
    .unwrap();

    let rule = rewrite_rule(
        int_add(int_add(var(x), int_const(c1)), int_const(c2)),
        template::int_add(var(x), int_const_with!([c1: uint, c2: uint] => c1 + c2)),
    );

    let outer_root = {
        let m = Matcher::new(&fx);
        let pat = int_add(int_add(var(x), int_const(c1)), int_const(c2)).into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert!(!hits.is_empty(), "the I256 reassoc LHS must match");
        hits[0].root()
    };

    let mut ctx = EditFunction::new(&mut fx);
    assert!(
        rule(&mut ctx, outer_root).unwrap().is_some(),
        "the I256 reassoc rule must fire"
    );
    // No limb carries, so the sum is limb-wise.
    let want = UBig::from_words(&[6, 8, 10, 12]);
    let minted = ctx.function().walk().any(|n| {
        let f = ctx.function();
        matches!(f.node_kind(n), NodeKind::IntConst(_))
            && f.int_const(f.node_outputs(n)[0]) == Some(&want)
    });
    assert!(minted, "the rewrite must intern the 256-bit sum");
}
