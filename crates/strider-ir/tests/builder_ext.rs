//! The `build_*` vocabulary must behave the same through every [`IRBuilder`]
//! implementor: the lift builder and the in-place editing context.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{EditFunction, IRBuilderExt, IRViewer, IntBinaryOp};
use strider_ir_test_utils::{RegisterSet, make_empty_fn};

/// Values are masked to the declared width before construction, so the dedup
/// cache sees `0x1FF` and `0xFF` at `I8` as one constant.
#[test]
fn build_int_const_masks_and_dedups() {
    let mut b = RegisterSet::new().build_fn_single_region().unwrap();
    let a = b.build_int_const(0x1FFu64, ValueType::I8).unwrap();
    let c = b.build_int_const(0xFFu64, ValueType::I8).unwrap();
    assert_eq!(a, c, "masked-equal I8 constants must dedup to one ValueId");
}

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

/// Building through an `EditFunction` must register the new node in its cached
/// live set.
#[test]
fn build_int_const_through_edit_function_tracks_live() {
    let mut function = make_empty_fn(|b| b.build_int_const(1u64, ValueType::I64)).unwrap();

    let mut ctx = EditFunction::new(&mut function);
    let value = ctx.build_int_const(0x1234u64, ValueType::I64).unwrap();
    let node = ctx.function().producer(value);
    assert!(ctx.is_live(node), "freshly built IntConst is tracked live");
    assert!(ctx.is_root(node), "an input-less const is a root");
}

/// Masking and dedup hold through the editing context too.
#[test]
fn build_int_const_masks_and_dedups_through_edit_function() {
    let mut function = make_empty_fn(|b| b.build_int_const(1u64, ValueType::I64)).unwrap();
    let mut ctx = EditFunction::new(&mut function);
    let value = ctx.build_int_const(0xABCDu64, ValueType::I16).unwrap();
    assert_eq!(
        ctx.function().value_kind(value).as_value(),
        Some(ValueType::I16)
    );
    let masked = ctx.build_int_const(0x1_ABCDu64, ValueType::I16).unwrap();
    assert_eq!(value, masked, "masked-equal I16 constants dedup");
}
