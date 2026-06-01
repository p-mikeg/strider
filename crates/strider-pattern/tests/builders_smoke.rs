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
use strider_pattern::{any_int_const, int_const, var, Capture, Matcher};

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
