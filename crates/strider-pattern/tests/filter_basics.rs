//! Pre-match `filter` hook fires after the kind + `output_ty` check
//! and BEFORE the matcher walks into child inputs.  This test pins
//! the short-circuit behaviour: when a parent pattern's `filter`
//! rejects, the child sub-pattern's own `filter` must NOT fire even
//! once — proving the early-exit happens before child recursion.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use std::cell::Cell;
use std::rc::Rc;

use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{add, any, int_const, IntBinaryOp, Matcher};

#[test]
fn filter_short_circuits_before_child_recursion() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let lhs = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let rhs = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    // Count how many times the CHILD pattern's filter runs.  If the
    // root's filter short-circuits before child recursion, the child
    // filter must not fire even once.
    //
    // `Rc<Cell<usize>>` is the right primitive here: a `Box<dyn Fn>`
    // can't be cloned across threads, so we don't need atomics — and
    // the strider-pattern crate's core is single-threaded by design.
    let child_invocations: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let counter = child_invocations.clone();

    let child = any().filter(move |_m, _n, _ty| {
        counter.set(counter.get() + 1);
        true
    });
    // Root's filter always fails, BEFORE walking the child.
    let root = add(int_const(99u128), child).filter(|_m, _n, _ty| false);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&root);
    assert_eq!(hits.len(), 0, "root filter should reject every Add");
    assert_eq!(
        child_invocations.get(),
        0,
        "child filter must NOT fire when the root filter short-circuits",
    );
}

#[test]
fn filter_accepts_match_when_predicate_returns_true() {
    // Companion test: when the filter says yes, the match proceeds
    // and the child gets visited.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let lhs = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let rhs = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    let child_invocations: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let counter = child_invocations.clone();
    let child = any().filter(move |_m, _n, _ty| {
        counter.set(counter.get() + 1);
        true
    });
    let root = add(int_const(5u128), child).filter(|_m, _n, _ty| true);

    let m = Matcher::try_new(&function).unwrap();
    let hits = m.find_all(&root);
    assert_eq!(hits.len(), 1, "Add(5, 7) should match Add(5, *)");
    assert!(
        child_invocations.get() >= 1,
        "child filter fires once child recursion proceeds (got {})",
        child_invocations.get(),
    );
}
