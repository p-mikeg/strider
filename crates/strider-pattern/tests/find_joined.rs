//! Cross-pattern join: `Matcher::find_joined` returns tuples of
//! matches whose shared `Capture`s bind to the same IR output / node
//! across every pattern.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::FunctionBuilder;
use strider_ir::IntBinaryOp;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{Capture, Matcher, Pattern, add, int_const, var};

/// IR: `Add(IntConst(5), IntConst(7))`.
/// Pattern 1: `int_const(5).capture(c)`.
/// Pattern 2: `add(var(c), int_const(7))`.
/// Joining on shared capture `c` yields exactly one tuple.
#[test]
fn find_joined_two_patterns_sharing_capture() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(five, seven, IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let p1 = int_const(5u128).capture(c);
    let p2 = add(var(c), int_const(7u128));

    let m = Matcher::try_new(&function).unwrap();
    let tuples = m.find_joined(&[&p1 as &dyn Pattern, &p2 as &dyn Pattern]);
    assert_eq!(
        tuples.len(),
        1,
        "expected exactly one joined tuple, got {}",
        tuples.len()
    );
    // Inner tuple has one Match per input pattern.
    assert_eq!(tuples[0].len(), 2, "expected one Match per input pattern");
}

/// IR has *two* candidates for `int_const(5)` (a stray `5` plus the one
/// feeding the `Add`).  Pattern 1 captures `c=5`, Pattern 2 demands
/// `add(c, 7)`.  Only the `5` actually feeding the `Add` joins; the
/// stray `5` must be filtered out by the shared-capture agreement
/// check.
#[test]
fn find_joined_filters_non_matching_captures() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    // IR dedupes `IntConst(5)` by value — both "stray" and "addend"
    // refs resolve to the same node.  To force two distinct producers
    // we make the stray a different width and feed it into a separate
    // operation so it stays reachable.
    let five_i64 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let five_i32 = b.build_int_const(5u64, NodeOutputType::I32).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(five_i64, seven, IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    // Keep the I32 five reachable by feeding it into a no-op add with itself.
    let _stray = b
        .build_int_binary_operation(five_i32, five_i32, IntBinaryOp::Add, NodeOutputType::I32)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let p1 = int_const(5u128).capture(c);
    // p2 requires the captured `c` to be the I64 `5` feeding the `seven` add.
    let p2 = add(var(c), int_const(7u128));

    let m = Matcher::try_new(&function).unwrap();
    let tuples = m.find_joined(&[&p1 as &dyn Pattern, &p2 as &dyn Pattern]);
    assert_eq!(
        tuples.len(),
        1,
        "shared capture must filter cross-product to the joining 5: got {}",
        tuples.len()
    );
}

/// Empty pattern slice → empty result.
#[test]
fn find_joined_empty_pats_returns_empty() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let v = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    let m = Matcher::try_new(&function).unwrap();
    let tuples = m.find_joined(&[]);
    assert!(tuples.is_empty());
}

/// If any input pattern has zero matches, the joined result must be
/// empty.
#[test]
fn find_joined_zero_match_pattern_yields_empty() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn");
    let v = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();

    let p_hits = int_const(1u128); // matches the `1`
    let p_misses = int_const(999u128); // matches nothing

    let m = Matcher::try_new(&function).unwrap();
    let tuples = m.find_joined(&[&p_hits as &dyn Pattern, &p_misses as &dyn Pattern]);
    assert!(
        tuples.is_empty(),
        "any zero-match pattern collapses the join: got {}",
        tuples.len()
    );
}
