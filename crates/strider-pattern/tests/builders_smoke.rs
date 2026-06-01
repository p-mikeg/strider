//! Smoke tests for the leaf chained builders.  Each test constructs a
//! tiny IR function and asserts the builder's `Pat<R>` finds the
//! expected hit(s) via `Matcher::find_all`.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{
    add, any_int_const, int_const, mul, var, xor, Capture, Matcher,
};

#[test]
fn int_const_builder_matches_via_find_all() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(five), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = int_const(5u128);
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn var_builder_captures() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let v = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = var(c);
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert!(!hits.is_empty());
    // Each hit should bind c to some NodeOutputId.
    assert!(hits[0].output(c).is_some());
}

#[test]
fn any_int_const_matches_multiple() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(
            five,
            seven,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = any_int_const();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert!(
        hits.len() >= 2,
        "expected at least 2 IntConst matches, got {}",
        hits.len()
    );
}

#[test]
fn add_builder_matches_chain() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(
            five,
            seven,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = add(int_const(5u128), var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn mul_builder_matches_chain() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let three = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let four = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
    let product = b
        .build_int_binary_operation(
            three,
            four,
            strider_ir::IntBinaryOp::Mul,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(product), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = mul(int_const(3u128), var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn xor_is_commutative_via_matcher_retry() {
    // Pattern `xor(int_const(0), var(x))` against IR `xor(var, IntConst(0))`.
    // First-order match fails (slot 0 wants IntConst, IR has var); commutative
    // retry swaps and succeeds — so we still get exactly one hit.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let nine = b.build_int_const(9u64, NodeOutputType::I64).unwrap();
    let zero = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
    // IR: xor(9, 0) — pattern wants xor(0, _), so slot 0 mismatches on
    // the first attempt and commutative retry must swap.
    let xor_out = b
        .build_int_binary_operation(
            nine,
            zero,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(xor_out), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = xor(int_const(0u128), var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(
        hits.len(),
        1,
        "commutative retry should match xor(9, 0) against xor(0, _)"
    );
    let out = hits[0].output(c).expect("x must be bound");
    let kind = function.kind_of_output(out);
    assert!(
        matches!(kind, strider_ir::node::NodeKind::IntConst(9)),
        "x should bind to the 9-output after commutative retry; got {kind:?}",
    );
}
