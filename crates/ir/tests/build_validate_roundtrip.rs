//! Black-box: every public-API construction path must produce a graph that
//! `validate` accepts. `FunctionBuilder::build()` calls `validate` internally
//! on the assembled graph and returns its error if it fails, so reaching
//! `Ok(_)` from `build()` is itself a green for these tests.

#![allow(clippy::unwrap_used)]

mod common;

use ir::node::NodeOutputType;
use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, FunctionBuilder,
    IntBinaryOp, IntCmpOp, IntUnaryOp,
};

#[test]
fn const_then_return_validates() {
    let _ = common::return_const(0, NodeOutputType::U32);
    let _ = common::return_const(0xCAFE_BABE, NodeOutputType::U64);
}

#[test]
fn every_int_binary_op_validates() {
    for op in [
        IntBinaryOp::Add,
        IntBinaryOp::Sub,
        IntBinaryOp::Mul,
        IntBinaryOp::And,
        IntBinaryOp::Or,
        IntBinaryOp::Xor,
        IntBinaryOp::ShiftLeft,
        IntBinaryOp::ShiftRight,
        IntBinaryOp::SShiftRight,
    ] {
        let _ = common::return_binop(1, 2, op, NodeOutputType::U32);
    }
}

#[test]
fn every_int_unary_op_validates() {
    for op in [IntUnaryOp::Neg, IntUnaryOp::Not] {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let r = b.create_region().unwrap();
        b.set_entry_region(r).unwrap();
        b.set_region(r);
        let v = b.build_int_const(0xFF, NodeOutputType::U32).unwrap();
        let res = b
            .build_int_unary_operation(v, op, NodeOutputType::U32)
            .unwrap();
        b.build_return(Some(res), &[]).unwrap();
        b.build().unwrap();
    }
}

#[test]
fn every_int_cmp_op_validates() {
    for op in [
        IntCmpOp::Equal,
        IntCmpOp::Less,
        IntCmpOp::LessEqual,
        IntCmpOp::Sless,
        IntCmpOp::SlessEqual,
        IntCmpOp::Carry,
        IntCmpOp::Scarry,
        IntCmpOp::Borrow,
        IntCmpOp::Sborrow,
    ] {
        let _ = common::return_int_cmp(1, 2, op, NodeOutputType::U32);
    }
}

#[test]
fn bool_ops_validate() {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let r = b.create_region().unwrap();
    b.set_entry_region(r).unwrap();
    b.set_region(r);
    let t = b.build_boolean_const(true);
    let f = b.build_boolean_const(false);
    let and = b.build_boolean_operation(t, f, BoolBinaryOp::And).unwrap();
    let or = b.build_boolean_operation(t, f, BoolBinaryOp::Or).unwrap();
    let xor = b.build_boolean_operation(t, f, BoolBinaryOp::Xor).unwrap();
    let neg_and = b
        .build_boolean_unary_operation(and, BoolUnaryOp::Neg)
        .unwrap();
    let combined1 = b
        .build_boolean_operation(neg_and, or, BoolBinaryOp::Or)
        .unwrap();
    let combined = b
        .build_boolean_operation(combined1, xor, BoolBinaryOp::Xor)
        .unwrap();
    b.build_return(Some(combined), &[]).unwrap();
    b.build().unwrap();
}

#[test]
fn extend_and_truncate_validate() {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let r = b.create_region().unwrap();
    b.set_entry_region(r).unwrap();
    b.set_region(r);
    let v8 = b.build_int_const(0xFF, NodeOutputType::U8).unwrap();
    let v32_zero = b
        .extend_if_needed(v8, NodeOutputType::U32, ExtendOp::ZeroExtend)
        .unwrap();
    let v32_sign = b
        .extend_if_needed(v8, NodeOutputType::U32, ExtendOp::SignExtend)
        .unwrap();
    let back_to_u8 = b.truncate_if_needed(v32_sign, NodeOutputType::U8).unwrap();
    let combined = b
        .build_int_binary_operation(v32_zero, v32_sign, IntBinaryOp::Add, NodeOutputType::U32)
        .unwrap();
    let _ = back_to_u8;
    b.build_return(Some(combined), &[]).unwrap();
    b.build().unwrap();
}

#[test]
fn float_ops_validate() {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let r = b.create_region().unwrap();
    b.set_entry_region(r).unwrap();
    b.set_region(r);
    // 1.0 in F64 bit pattern.
    let one = b.build_float_const(0x3FF0_0000_0000_0000, NodeOutputType::F64);
    let two = b.build_float_const(0x4000_0000_0000_0000, NodeOutputType::F64);
    for op in [
        FloatBinaryOp::Add,
        FloatBinaryOp::Sub,
        FloatBinaryOp::Mul,
        FloatBinaryOp::Div,
    ] {
        let _ = b
            .build_float_binary_op(one, two, op, NodeOutputType::F64)
            .unwrap();
    }
    for op in [
        FloatUnaryOp::Neg,
        FloatUnaryOp::Abs,
        FloatUnaryOp::Sqrt,
        FloatUnaryOp::Floor,
        FloatUnaryOp::Ceil,
        FloatUnaryOp::Round,
    ] {
        let _ = b.build_float_unary_op(one, op, NodeOutputType::F64).unwrap();
    }
    for op in [
        FloatCmpOp::Equal,
        FloatCmpOp::NotEqual,
        FloatCmpOp::Less,
        FloatCmpOp::LessEqual,
    ] {
        let _ = b.build_float_cmp_op(one, two, op).unwrap();
    }
    let neg = b
        .build_float_unary_op(one, FloatUnaryOp::Neg, NodeOutputType::F64)
        .unwrap();
    b.build_return(Some(neg), &[]).unwrap();
    b.build().unwrap();
}

#[test]
fn float_int_conversions_validate() {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let r = b.create_region().unwrap();
    b.set_entry_region(r).unwrap();
    b.set_region(r);
    let i = b.build_int_const(42, NodeOutputType::U32).unwrap();
    let f = b.build_int_to_float(i, NodeOutputType::F32).unwrap();
    let back = b.build_float_to_int(f, NodeOutputType::U32).unwrap();
    let bits = b
        .build_int_bits_to_float(i, NodeOutputType::F32)
        .unwrap();
    let bits_back = b
        .build_float_bits_to_int(bits, NodeOutputType::U32)
        .unwrap();
    let f64v = b.build_float_to_float(f, NodeOutputType::F64).unwrap();
    let _ = (back, f64v, bits_back);
    b.build_return(Some(back), &[]).unwrap();
    b.build().unwrap();
}

#[test]
fn region_join_with_phi_validates() {
    // entry → if-true → join, if-false → join.
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let entry_region = b.create_region().unwrap();
    b.set_entry_region(entry_region).unwrap();
    b.set_region(entry_region);

    let cond = b.build_boolean_const(true);
    let true_region = b.create_region().unwrap();
    let false_region = b.create_region().unwrap();
    b.build_if(cond, true_region, false_region).unwrap();

    let join = b.create_region().unwrap();

    b.set_region(true_region);
    b.build_branch(join).unwrap();

    b.set_region(false_region);
    b.build_branch(join).unwrap();

    b.set_region(join);
    let v = b.build_int_const(7, NodeOutputType::U32).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    b.build().unwrap();
}

#[test]
fn loads_and_stores_validate() {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let r = b.create_region().unwrap();
    b.set_entry_region(r).unwrap();
    b.set_region(r);
    let addr = b.build_int_const(0x1000, NodeOutputType::U64).unwrap();
    let data = b.build_int_const(0xABCD, NodeOutputType::U32).unwrap();
    b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    b.build().unwrap();
}

/// Verifies the SSA-memory invariant: when a Store precedes a Load, the
/// Load's memory input must be produced by the Store, not by InitialMemory.
/// Without this thread of dependency, downstream passes (like StackLoadForward)
/// would forward stale loads.
#[test]
fn store_then_load_threads_memory_through_store() {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let r = b.create_region().unwrap();
    b.set_entry_region(r).unwrap();
    b.set_region(r);
    let addr = b.build_int_const(0x1000, NodeOutputType::U64).unwrap();
    let data = b.build_int_const(0xABCD, NodeOutputType::U32).unwrap();
    b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let fg = b.build().unwrap();

    let mut load = None;
    let mut store = None;
    for nid in fg.all_node_ids() {
        match fg.graph.node_kind(nid) {
            ir::node::NodeKind::Load(_) => load = Some(nid),
            ir::node::NodeKind::Store(_) => store = Some(nid),
            _ => {}
        }
    }
    let load = load.unwrap();
    let store = store.unwrap();
    let load_mem_input = fg.graph.node_inputs(load).into_iter().next().unwrap();
    let producer = fg.graph.get_node_from_output(load_mem_input);
    assert_eq!(
        producer, store,
        "Load's memory input must be produced by the Store, not InitialMemory"
    );
}
