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
use strider_pattern::{add, int_const, var, Capture, MatchPat, Matcher, TemplatePat};

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
