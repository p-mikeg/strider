//! Shared `FunctionBuilder` helpers for `ir` integration tests.
//!
//! These helpers exist so each black-box file in `tests/` can build a
//! minimal-but-valid graph in one or two lines without re-implementing
//! boilerplate. They use only the public API (`FunctionBuilder`,
//! `BuiltFunctionGraph`, etc.) — no `pub(crate)` access — exactly what a
//! downstream consumer of the crate would write.

#![allow(dead_code)] // Different test files use different subsets.
#![allow(clippy::unwrap_used)]

use ir::node::NodeOutputType;
use ir::{BuiltFunctionGraph, FunctionBuilder, IntBinaryOp};

/// Build a tiny entry-only graph with a single Return whose value input
/// is `IntConst(value)` with output type `ty`.
pub fn return_const(value: u64, ty: NodeOutputType) -> BuiltFunctionGraph {
    let mut b = FunctionBuilder::empty().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let v = b.build_int_const(value, ty).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    b.build().unwrap()
}

/// Build `IntConst(a) op IntConst(b)` and return through it.
pub fn return_binop(
    a: u64,
    b: u64,
    op: IntBinaryOp,
    ty: NodeOutputType,
) -> BuiltFunctionGraph {
    let mut bld = FunctionBuilder::empty().unwrap();
    let region = bld.create_region().unwrap();
    bld.set_entry_region(region).unwrap();
    bld.set_region(region);
    let va = bld.build_int_const(a, ty).unwrap();
    let vb = bld.build_int_const(b, ty).unwrap();
    let r = bld
        .build_int_binary_operation(va, vb, op, ty)
        .unwrap();
    bld.build_return(Some(r), &[]).unwrap();
    bld.build().unwrap()
}

/// Build a graph that returns the boolean comparison `IntConst(a) cmp IntConst(b)`,
/// going through `build_int_cmp_operation` so the result is a `Bool` value.
pub fn return_int_cmp(
    a: u64,
    b: u64,
    op: ir::IntCmpOp,
    operand_ty: NodeOutputType,
) -> BuiltFunctionGraph {
    let mut bld = FunctionBuilder::empty().unwrap();
    let region = bld.create_region().unwrap();
    bld.set_entry_region(region).unwrap();
    bld.set_region(region);
    let va = bld.build_int_const(a, operand_ty).unwrap();
    let vb = bld.build_int_const(b, operand_ty).unwrap();
    let r = bld
        .build_int_cmp_operation(va, vb, op, operand_ty)
        .unwrap();
    bld.build_return(Some(r), &[]).unwrap();
    bld.build().unwrap()
}
