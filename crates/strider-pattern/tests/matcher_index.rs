//! Smoke tests for the lazy `KindIndex` cache that `Matcher::find_all`
//! consults for discriminant-rooted patterns.  The index is built on
//! first query and reused for every subsequent query; these tests
//! exercise the happy path (empty bucket short-circuits, cache reuse
//! across queries at different discriminants) without instrumenting the
//! cache directly.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
)]

use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{add, any_int_const, var, Capture, Matcher};

/// Build a function with only `IntConst` value nodes; query for an
/// `Add` pattern.  Should return zero matches without entering the
/// recursive walker — the kind-bucket for `IntBinaryOp` is empty.
#[test]
fn kind_index_returns_empty_when_no_match() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(five), &[]).unwrap();
    let function = b.build().unwrap();

    let matcher = Matcher::try_new(&function).unwrap();

    // Pattern roots at `IntBinaryOp::Add` — no such node in the IR.
    let pat = add(var(Capture::default()), var(Capture::default()));
    let hits = matcher.find_all(&pat);
    assert!(
        hits.is_empty(),
        "no Add node in the function — expected zero matches, got {}",
        hits.len(),
    );
}

/// Run `find_all` twice with patterns at different discriminants
/// against the same matcher.  The lazy `KindIndex` is built on the
/// first query and reused on the second; both queries should return
/// their correct hit counts.  Mostly a regression check that the
/// cached index still serves later queries correctly.
#[test]
fn kind_index_reused_across_queries() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
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

    let matcher = Matcher::try_new(&function).unwrap();

    // Query 1: any IntConst — two hits (5 and 7).
    let pat_const = any_int_const();
    let hits1 = matcher.find_all(&pat_const);
    assert_eq!(
        hits1.len(),
        2,
        "any_int_const() should match both constants; got {}",
        hits1.len(),
    );

    // Query 2: Add(_, _) — one hit.  After the first query the
    // KindIndex is already built; this exercises the cached-path.
    let pat_add = add(var(Capture::default()), var(Capture::default()));
    let hits2 = matcher.find_all(&pat_add);
    assert_eq!(
        hits2.len(),
        1,
        "add(_, _) should match exactly once; got {}",
        hits2.len(),
    );

    // Query 3: re-run the IntConst query — still two hits via cache.
    let hits3 = matcher.find_all(&pat_const);
    assert_eq!(
        hits3.len(),
        2,
        "re-running any_int_const() after the cache was built should still match both; got {}",
        hits3.len(),
    );
}
