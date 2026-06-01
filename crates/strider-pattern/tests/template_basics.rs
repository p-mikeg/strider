//! `PatGraph<Concrete>::instantiate` verification — materialise a
//! template as fresh IR nodes, resolving captures through `Bindings`.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{FunctionBuilder, IntBinaryOp};
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{Capture, Matcher, Template, add, int_const, var};

#[test]
fn instantiate_var_resolves_through_bindings() {
    // IR `Add(5, 7)`.  Match `add(int_const(5), var(c))` — `c` binds to
    // the second operand.  Then `var(c).instantiate(...)` should return
    // exactly the bound `NodeOutputId`.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(five, seven, IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let mut function = b.build().unwrap();

    let c = Capture::new();
    let lhs = add(int_const(5u128), var(c));
    let bindings = {
        let m = Matcher::try_new(&function).unwrap();
        let hits = m.find_all(&lhs);
        assert_eq!(hits.len(), 1);
        hits[0].bindings_clone()
    };

    // var(c) instantiated against these bindings resolves to `c`'s
    // bound output — no fresh node is created.
    let rhs = var(c);
    let lhs_root = function.entry().unwrap();
    let resolved = rhs
        .instantiate(&mut function, &bindings, lhs_root, NodeOutputType::I64)
        .unwrap();
    let bound = bindings.get(c).unwrap();
    assert_eq!(resolved, bound);
}

#[test]
fn instantiate_int_const_template_creates_fresh_node() {
    // Template `int_const(42)` — no captures; should create a fresh
    // IntConst(42) node typed I64.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let dummy = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(dummy), &[]).unwrap();
    let mut function = b.build().unwrap();

    let bindings = strider_pattern::Bindings::default();
    let template = int_const(42u128);
    let lhs_root = function.entry().unwrap();
    let new_out = template
        .instantiate(&mut function, &bindings, lhs_root, NodeOutputType::I64)
        .unwrap();

    let new_node = function.node_for_output(new_out);
    assert!(matches!(
        function.node_kind(new_node),
        NodeKind::IntConst(42)
    ));
}

#[test]
fn instantiate_add_template_creates_arithmetic_subgraph() {
    // IR `Add(7, 7)` so the matcher's commutative retry binds `x` to a
    // value-output edge.  Match `add(int_const(7), var(x))`, then
    // instantiate `add(int_const(0), var(x))` — a fresh Add node typed
    // I64 with one operand the bound output and the other a fresh
    // IntConst(0).
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let eight = b.build_int_const(8u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(seven, eight, IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let mut function = b.build().unwrap();

    let x = Capture::new();
    let lhs = add(int_const(7u128), var(x));
    let bindings = {
        let m = Matcher::try_new(&function).unwrap();
        let hits = m.find_all(&lhs);
        assert_eq!(hits.len(), 1);
        hits[0].bindings_clone()
    };
    // `x` is bound to `eight`'s output.
    assert_eq!(bindings.get(x).unwrap(), eight);

    let template = add(int_const(0u128), var(x));
    let lhs_root = function.entry().unwrap();
    let root_out = template
        .instantiate(&mut function, &bindings, lhs_root, NodeOutputType::I64)
        .unwrap();

    let root_node = function.node_for_output(root_out);
    assert!(matches!(
        function.node_kind(root_node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));

    // Walk the freshly created Add's inputs: one should be IntConst(0),
    // the other the bound `eight` output.
    let inputs: Vec<_> = function.node_inputs(root_node).into_iter().collect();
    assert_eq!(inputs.len(), 2);
    let kinds: Vec<&NodeKind> = inputs
        .iter()
        .map(|&out| function.node_kind(function.node_for_output(out)))
        .collect();
    assert!(kinds.iter().any(|k| matches!(k, NodeKind::IntConst(0))));
    assert!(inputs.contains(&eight));
}
