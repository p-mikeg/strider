//! Integration tests asserting that `FunctionBuilder::build` returns
//! a graph that passes `validate` for every node-kind variant.  These
//! exercise the IR construction API end-to-end (build → validate) and
//! catch silent breakage in either layer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatUnaryOp, IntBinaryOp, IntUnaryOp,
};
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::make_empty_fn;

#[test]
fn every_int_binary_op_validates() {
    for op in [
        IntBinaryOp::Add,
        IntBinaryOp::Mul,
        IntBinaryOp::Div,
        IntBinaryOp::Sdiv,
        IntBinaryOp::Rem,
        IntBinaryOp::Srem,
        IntBinaryOp::And,
        IntBinaryOp::Or,
        IntBinaryOp::Xor,
        IntBinaryOp::ShiftLeft,
        IntBinaryOp::ShiftRight,
        IntBinaryOp::SShiftRight,
    ] {
        make_empty_fn(|fb| {
            let lhs = fb.build_int_const(1u64, NodeOutputType::U64)?;
            let rhs = fb.build_int_const(2u64, NodeOutputType::U64)?;
            let result = fb
                .build_int_binary_operation(lhs, rhs, op, NodeOutputType::U64)
                .unwrap_or_else(|e| panic!("op {op:?} failed to build: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("op {op:?} built invalid IR: {e}"));
    }
}

#[test]
fn every_int_unary_op_validates() {
    for op in [IntUnaryOp::Neg, IntUnaryOp::BitNot] {
        make_empty_fn(|fb| {
            let x = fb.build_int_const(5u64, NodeOutputType::U64)?;
            let result = fb
                .build_int_unary_operation(x, op, NodeOutputType::U64)
                .unwrap_or_else(|e| panic!("op {op:?} failed: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("op {op:?} invalid: {e}"));
    }
}

#[test]
fn bool_ops_validate() {
    for op in [BoolBinaryOp::And, BoolBinaryOp::Or, BoolBinaryOp::Xor] {
        make_empty_fn(|fb| {
            let t = fb.build_boolean_const(true);
            let f = fb.build_boolean_const(false);
            let result = fb
                .build_boolean_operation(t, f, op)
                .unwrap_or_else(|e| panic!("op {op:?} failed: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("op {op:?} invalid: {e}"));
    }
    // BoolUnaryOp::Neg
    make_empty_fn(|fb| {
        let t = fb.build_boolean_const(true);
        let result = fb
            .build_boolean_unary_operation(t, BoolUnaryOp::Neg)
            .expect("bool unary neg");
        Ok(result)
    })
    .expect("bool unary neg validates");
}

#[test]
fn float_ops_validate() {
    // FloatBinaryOp variants.  Sub is absent (lowered to Add+Neg at lift time).
    for op in [FloatBinaryOp::Add, FloatBinaryOp::Mul, FloatBinaryOp::Div] {
        make_empty_fn(|fb| {
            let a = fb.build_float_const(0x3FF0_0000_0000_0000u64, NodeOutputType::F64); // 1.0
            let b = fb.build_float_const(0x4000_0000_0000_0000u64, NodeOutputType::F64); // 2.0
            let result = fb
                .build_float_binary_op(a, b, op, NodeOutputType::F64)
                .unwrap_or_else(|e| panic!("FloatBinaryOp::{op:?} failed: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("FloatBinaryOp::{op:?} invalid: {e}"));
    }
    // FloatUnaryOp variants.
    for op in [
        FloatUnaryOp::Neg,
        FloatUnaryOp::Abs,
        FloatUnaryOp::Sqrt,
        FloatUnaryOp::Ceil,
        FloatUnaryOp::Floor,
        FloatUnaryOp::Round,
    ] {
        make_empty_fn(|fb| {
            let x = fb.build_float_const(0x3FF0_0000_0000_0000u64, NodeOutputType::F64);
            let result = fb
                .build_float_unary_op(x, op, NodeOutputType::F64)
                .unwrap_or_else(|e| panic!("FloatUnaryOp::{op:?} failed: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("FloatUnaryOp::{op:?} invalid: {e}"));
    }
}

#[test]
fn loads_and_stores_validate() {
    use strider_ir_test_utils::{reg_vn, RegisterSet};

    let sp_vn = reg_vn(0x20, 8);
    let mut b = RegisterSet::new()
        .tracked(sp_vn)
        .callee_saved(sp_vn)
        .build_fn_single_region()
        .expect("builder");

    let sp_val = b.read_variable(&sp_vn).expect("read sp");
    let offset = b
        .build_int_const(8u64, NodeOutputType::U64)
        .expect("const 8");
    let addr = b
        .build_int_binary_operation(sp_val, offset, IntBinaryOp::Add, NodeOutputType::U64)
        .expect("addr");

    // Load
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
        .expect("load");

    // Store
    let data = b.build_int_const(0x42u64, NodeOutputType::U64).expect("data");
    b.build_store(addr, data, rsleigh::VnSpace::RAM)
        .expect("store");

    b.build_return(Some(loaded), &[]).expect("return");
    b.set_lift_addr(None);

    b.build().expect("loads_and_stores_validate: build() must succeed");
}

#[test]
fn region_join_with_phi_validates() {
    use strider_ir_test_utils::{reg_vn, RegisterSet, SENTINEL_LIFT_ADDR};

    // Diamond: entry → if(true) { region_t → var=1 } { region_f → var=2 } → join → return var
    // Uses a tracked register variable so FunctionBuilder's create_region
    // automatically creates a tagged Phi at the join point.
    let var_vn = reg_vn(0x10, 8);
    let mut b = RegisterSet::new()
        .tracked(var_vn)
        .build_fn()
        .expect("builder");

    let entry = b.create_region().expect("entry region");
    let region_t = b.create_region().expect("true region");
    let region_f = b.create_region().expect("false region");
    let join = b.create_region().expect("join region");

    b.set_entry_region(entry).expect("set entry region");
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let cond = b.build_boolean_const(true);
    b.build_if(cond, region_t, region_f).expect("build_if");

    // True arm: write var=1 and branch to join.
    b.set_region(region_t);
    let v1 = b.build_int_const(1u64, NodeOutputType::U64).expect("v1");
    b.write_variable(&var_vn, v1).expect("write var true");
    b.build_branch(join).expect("branch to join from true");

    // False arm: write var=2 and branch to join.
    b.set_region(region_f);
    let v2 = b.build_int_const(2u64, NodeOutputType::U64).expect("v2");
    b.write_variable(&var_vn, v2).expect("write var false");
    b.build_branch(join).expect("branch to join from false");

    // Join: read var — this produces the Region-scoped Phi output.
    b.set_region(join);
    let phi_val = b.read_variable(&var_vn).expect("read var at join");
    b.build_return(Some(phi_val), &[]).expect("return");
    b.set_lift_addr(None);

    b.build().expect("region_join_with_phi_validates: build() must succeed");
}
