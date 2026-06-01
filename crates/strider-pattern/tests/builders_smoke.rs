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
    add, any_int_const, int_const, int_eq, int_le, int_lt, int_ne, lzcount, mul, popcount, var,
    xor, Capture, Matcher,
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
fn int_eq_builder_matches() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let lhs = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let rhs = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let cmp = b
        .build_int_cmp_operation(lhs, rhs, strider_ir::IntCmpOp::Equal, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(cmp), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = int_eq(int_const(2u128), var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn int_lt_builder_directional() {
    // `Less` is directional — commutative-retry does NOT swap operands.
    // Pattern `int_lt(int_const(2), var(c))` against IR `int_lt(5, 2)`
    // must miss; pattern `int_lt(int_const(5), var(c))` against the same
    // IR hits.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let two = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let cmp = b
        .build_int_cmp_operation(five, two, strider_ir::IntCmpOp::Less, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(cmp), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let miss = int_lt(int_const(2u128), var(Capture::default()));
    assert_eq!(matcher.find_all(&miss).len(), 0);

    let hit = int_lt(int_const(5u128), var(Capture::default()));
    assert_eq!(matcher.find_all(&hit).len(), 1);
}

#[test]
fn int_ne_matches_lifted_xor_eq() {
    // `int_ne(a, b)` expands to `xor(int_eq(a, b), int_const(1)):I1`.
    // Build that IR shape directly: cmp_eq(2, 3) (which yields I1) then
    // xor with IntConst(1):I1.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let lhs = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let rhs = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let eq = b
        .build_int_cmp_operation(lhs, rhs, strider_ir::IntCmpOp::Equal, NodeOutputType::I64)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, NodeOutputType::I1).unwrap();
    let not_eq = b
        .build_int_binary_operation(
            eq,
            one_i1,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I1,
        )
        .unwrap();
    b.build_return(Some(not_eq), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = int_ne(int_const(2u128), int_const(3u128));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "int_ne should match xor(eq, 1):I1 shape");
}

#[test]
fn int_le_matches_lifted_swap_xor() {
    // `int_le(a, b)` expands to `xor(int_lt(b, a), int_const(1)):I1`.
    // Build the IR: cmp_lt(rhs=3, lhs=2) then xor with IntConst(1):I1.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let a = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let bv = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let lt = b
        .build_int_cmp_operation(bv, a, strider_ir::IntCmpOp::Less, NodeOutputType::I64)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, NodeOutputType::I1).unwrap();
    let le = b
        .build_int_binary_operation(
            lt,
            one_i1,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I1,
        )
        .unwrap();
    b.build_return(Some(le), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = int_le(int_const(2u128), int_const(3u128));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "int_le should match the swap+xor shape");
}

#[test]
fn popcount_and_lzcount_match_unit_kinds() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let x = b.build_int_const(0x1234u64, NodeOutputType::I64).unwrap();
    let pc = b.build_popcount(x, NodeOutputType::I64).unwrap();
    let lz = b.build_lzcount(x, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(pc, lz, strider_ir::IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let pc_pat = popcount(var(Capture::default()));
    let lz_pat = lzcount(var(Capture::default()));
    assert_eq!(matcher.find_all(&pc_pat).len(), 1);
    assert_eq!(matcher.find_all(&lz_pat).len(), 1);
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
