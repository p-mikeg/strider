//! Integration coverage for the shared [`IRBuilderExt`] construction
//! vocabulary: the same `build_*` constructors must work through every
//! [`IRBuilder`] implementor (the lift builder and the in-place editing
//! context).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{EditFunction, IRBuilderExt, IntBinaryOp};
use strider_ir_test_utils::{RegisterSet, make_empty_fn};

/// `build_int_const` masks its value to the declared type's bit width before
/// constructing, so two semantically-equal constants (`0x1FF` and `0xFF` at
/// `I8`) dedup to the SAME `ValueId` through the dedup cache.
#[test]
fn build_int_const_masks_and_dedups() {
    let mut b = RegisterSet::new().build_fn_single_region().unwrap();
    let a = b.build_int_const(0x1FFu64, ValueType::I8).unwrap();
    let c = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
    assert_eq!(a, c, "masked-equal I8 constants must dedup to one ValueId");
}

/// A binary op built through a `FunctionBuilder` produces a node whose output
/// carries the requested type.
#[test]
fn build_binary_op_through_function_builder() {
    let mut b = RegisterSet::new().build_fn_single_region().unwrap();
    let lhs = b.build_int_const(7u64, ValueType::I32).unwrap();
    let rhs = b.build_int_const(9u64, ValueType::I32).unwrap();
    let add = b
        .build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    assert_eq!(
        b.function().value_kind(add).as_value(),
        Some(ValueType::I32),
        "the Add output must carry the requested I32 type",
    );
    let node = b.function().producer(add);
    assert!(matches!(
        b.function().node_kind(node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));
}

/// `build_int_const` through an `EditFunction` registers the freshly created
/// node into the cached live set (the editing context's bookkeeping).
#[test]
fn build_int_const_through_edit_function_tracks_live() {
    // A minimal built function to edit.
    let mut function = make_empty_fn(|b| b.build_int_const(1u64, ValueType::I64)).unwrap();

    let mut ctx = EditFunction::try_for_built(&mut function).unwrap();
    let value = ctx.build_int_const(0x1234u64, ValueType::I64).unwrap();
    let node = ctx.function().producer(value);
    assert!(ctx.is_live(node), "freshly built IntConst is tracked live");
    assert!(ctx.is_root(node), "an input-less const is a root");
}

/// `build_int_const` masking/dedup holds through the in-place editing
/// context as well: 0xABCD and an over-wide value masking to the same I16
/// payload share one `ValueId`.
#[test]
fn build_int_const_masks_and_dedups_through_edit_function() {
    let mut function = make_empty_fn(|b| b.build_int_const(1u64, ValueType::I64)).unwrap();
    let mut ctx = EditFunction::try_for_built(&mut function).unwrap();
    let value = ctx.build_int_const(0xABCDu64, ValueType::I16).unwrap();
    assert_eq!(ctx.function().value_kind(value).as_value(), Some(ValueType::I16));
    // Masking: 0xABCD masked to I16 stays 0xABCD; an over-wide value masks down.
    let masked = ctx.build_int_const(0x1_ABCDu64, ValueType::I16).unwrap();
    assert_eq!(value, masked, "masked-equal I16 constants dedup");
}
