//! Integration coverage for the typed template side: a `TemplatePat`
//! RHS instantiated as fresh IR against a matched LHS.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeKind, NodeOutputType as T};
use strider_ir::IntBinaryOp;
use strider_ir_test_utils::make_empty_fn;

use strider_pattern::template::instantiate;
use strider_pattern::{add, bool_not, int_const, var, Capture, MatchPat, Matcher, TemplatePat};

/// `bool_not(var(c))` is a `TemplatePat`, so it is usable as a buildable
/// rewrite RHS (the type-checker rejects a non-buildable/wildcard RHS).
/// Sealing it into a template must succeed and produce a buildable
/// pattern.
#[test]
fn bool_not_is_a_buildable_template_rhs() {
    let c = Capture::new();
    let pat = bool_not(var(c)).into_template();
    strider_pattern::assert_buildable(&pat).expect("bool_not(var(c)) must be a buildable RHS");
}

/// Match `add(var(x), int_const(1))`, then instantiate
/// `add(var(x), int_const(2))` as fresh IR re-using the captured `x`.
#[test]
fn instantiate_add_const_builds_fresh_node() {
    let x = Capture::new();

    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(5u64, T::I64)?;
        let k = b.build_int_const(1u64, T::I64)?;
        b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::I64)
    })
    .unwrap();

    // Match the LHS.
    let lhs = add(var(x), int_const(1u128)).into_pattern();
    let (root_node, bindings) = {
        let m = Matcher::try_new(&fx).unwrap();
        let hits = m.find_all(&lhs);
        assert_eq!(hits.len(), 1);
        (hits[0].root(), hits[0].bindings_clone())
    };

    // Root single value output + type.
    let [root_out] = fx.node_outputs_exact::<1>(root_node).unwrap();
    let root_ty = fx.output_kind(root_out).as_value().unwrap();

    // Build the RHS as fresh IR.
    let rhs = add(var(x), int_const(2u128)).into_template();
    let new_out = instantiate(&rhs, &mut fx, &bindings, root_node, root_ty).unwrap();

    // The new output is an Add node.
    let new_node = fx.node_for_output(new_out);
    assert!(matches!(
        fx.node_kind(new_node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));

    // Its constant operand is the freshly built `IntConst(2)`.
    let has_two = fx
        .node_inputs(new_node)
        .into_iter()
        .map(|inp| fx.node_for_output(inp))
        .any(|n| matches!(fx.node_kind(n), NodeKind::IntConst(2)));
    assert!(has_two, "RHS should materialise IntConst(2)");
}

/// A bare `var(c)` template resolves to its bound output through the
/// `Bindings` — no fresh node is created.
#[test]
fn instantiate_bare_var_resolves_to_bound_output() {
    let c = Capture::new();

    let mut fx = make_empty_fn(|b| {
        let five = b.build_int_const(5u64, T::I64)?;
        let seven = b.build_int_const(7u64, T::I64)?;
        b.build_int_binary_operation(five, seven, IntBinaryOp::Add, T::I64)
    })
    .unwrap();

    // Match `add(int_const(5), var(c))` — `c` binds to the 7-operand.
    let lhs = add(int_const(5u128), var(c)).into_pattern();
    let (root_node, bindings) = {
        let m = Matcher::try_new(&fx).unwrap();
        let hits = m.find_all(&lhs);
        assert_eq!(hits.len(), 1);
        (hits[0].root(), hits[0].bindings_clone())
    };
    let bound = bindings.get(c).unwrap();

    // Instantiating a bare `var(c)` returns the bound output unchanged.
    let pre_count = fx.walk().count();
    let rhs = var(c).into_template();
    let resolved = instantiate(&rhs, &mut fx, &bindings, root_node, T::I64).unwrap();
    assert_eq!(resolved, bound, "var(c) must resolve to its bound output");
    assert_eq!(fx.walk().count(), pre_count, "no fresh node created");
}
