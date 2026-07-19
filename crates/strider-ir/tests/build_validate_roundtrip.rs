//! `FunctionBuilder::build` must return a graph that passes `validate`, for
//! every node-kind variant.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_ir::node::ValueType;
use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatUnaryOp, IRBuilderExt, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
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
            let lhs = fb.build_int_const(1u64, ValueType::I64)?;
            let rhs = fb.build_int_const(2u64, ValueType::I64)?;
            let result = fb
                .build_int_binary_operation(lhs, rhs, op, ValueType::I64)
                .unwrap_or_else(|e| panic!("op {op:?} failed to build: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("op {op:?} built invalid IR: {e}"));
    }
}

#[test]
fn every_int_unary_op_validates() {
    // `Neg` is the only variant: bitwise complement is `Xor(x, all_ones)`.
    let op = IntUnaryOp::Neg;
    make_empty_fn(|fb| {
        let x = fb.build_int_const(5u64, ValueType::I64)?;
        let result = fb
            .build_int_unary_operation(x, op, ValueType::I64)
            .unwrap_or_else(|e| panic!("op {op:?} failed: {e}"));
        Ok(result)
    })
    .unwrap_or_else(|e| panic!("op {op:?} invalid: {e}"));
}

#[test]
fn bool_ops_validate() {
    // Booleans are I1 integers, so logical and/or/xor are the bitwise ops
    // at I1 and logical NOT is `Xor(x, IntConst(1)):I1`.
    for op in [IntBinaryOp::And, IntBinaryOp::Or, IntBinaryOp::Xor] {
        make_empty_fn(|fb| {
            let t = fb.build_boolean_const(true);
            let f = fb.build_boolean_const(false);
            let result = fb
                .build_int_binary_operation(t, f, op, ValueType::I1)
                .unwrap_or_else(|e| panic!("op {op:?} failed: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("op {op:?} invalid: {e}"));
    }
    make_empty_fn(|fb| {
        let t = fb.build_boolean_const(true);
        let one = fb.build_int_const(u128::MAX, ValueType::I1)?;
        let result = fb
            .build_int_binary_operation(t, one, IntBinaryOp::Xor, ValueType::I1)
            .expect("bool logical not");
        Ok(result)
    })
    .expect("bool logical not validates");
}

#[test]
fn float_ops_validate() {
    // Sub is absent: lowered to Add+Neg at lift time.
    for op in [FloatBinaryOp::Add, FloatBinaryOp::Mul, FloatBinaryOp::Div] {
        make_empty_fn(|fb| {
            let a = fb.build_float_const(0x3FF0_0000_0000_0000u64, ValueType::F64); // 1.0
            let b = fb.build_float_const(0x4000_0000_0000_0000u64, ValueType::F64); // 2.0
            let result = fb
                .build_float_binary_op(a, b, op, ValueType::F64)
                .unwrap_or_else(|e| panic!("FloatBinaryOp::{op:?} failed: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("FloatBinaryOp::{op:?} invalid: {e}"));
    }
    for op in [
        FloatUnaryOp::Neg,
        FloatUnaryOp::Abs,
        FloatUnaryOp::Sqrt,
        FloatUnaryOp::Ceil,
        FloatUnaryOp::Floor,
        FloatUnaryOp::Round,
    ] {
        make_empty_fn(|fb| {
            let x = fb.build_float_const(0x3FF0_0000_0000_0000u64, ValueType::F64);
            let result = fb
                .build_float_unary_op(x, op, ValueType::F64)
                .unwrap_or_else(|e| panic!("FloatUnaryOp::{op:?} failed: {e}"));
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("FloatUnaryOp::{op:?} invalid: {e}"));
    }
}

#[test]
fn loads_and_stores_validate() {
    use strider_ir_test_utils::{RegisterSet, reg_vn};

    let sp_vn = reg_vn(0x20, 8);
    let mut b = RegisterSet::new()
        .tracked(sp_vn)
        .callee_saved(sp_vn)
        .build_fn_single_region()
        .expect("builder");

    let sp_val = b.read_variable(&sp_vn).expect("read sp");
    let offset = b.build_int_const(8u64, ValueType::I64).expect("const 8");
    let addr = b
        .build_int_binary_operation(sp_val, offset, IntBinaryOp::Add, ValueType::I64)
        .expect("addr");

    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .expect("load");

    let data = b.build_int_const(0x42u64, ValueType::I64).expect("data");
    b.build_store(addr, data, rsleigh::VnSpace::RAM)
        .expect("store");

    b.build_return(Some(loaded), &[]).expect("return");
    b.set_lift_addr(None);

    b.build()
        .expect("loads_and_stores_validate: build() must succeed");
}

#[test]
fn region_join_with_phi_validates() {
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

    // A diamond over a tracked register variable, so create_region mints a
    // tagged Phi at the join.
    let var_vn = reg_vn(0x10, 8);
    let mut b = RegisterSet::new()
        .tracked(var_vn)
        .build_fn()
        .expect("builder");

    let entry = b.create_region_all().expect("entry region");
    let region_t = b.create_region_all().expect("true region");
    let region_f = b.create_region_all().expect("false region");
    let join = b.create_region_all().expect("join region");

    b.set_entry_region_all(entry).expect("set entry region");
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let cond = b.build_boolean_const(true);
    b.build_if(cond, region_t, region_f).expect("build_if");

    b.set_region(region_t);
    let v1 = b.build_int_const(1u64, ValueType::I64).expect("v1");
    b.write_variable(&var_vn, v1).expect("write var true");
    b.build_branch(join).expect("branch to join from true");

    b.set_region(region_f);
    let v2 = b.build_int_const(2u64, ValueType::I64).expect("v2");
    b.write_variable(&var_vn, v2).expect("write var false");
    b.build_branch(join).expect("branch to join from false");

    // Reading at the join is what produces the Phi output.
    b.set_region(join);
    let phi_val = b.read_variable(&var_vn).expect("read var at join");
    b.build_return(Some(phi_val), &[]).expect("return");
    b.set_lift_addr(None);

    b.build()
        .expect("region_join_with_phi_validates: build() must succeed");
}

#[test]
fn const_then_return_validates() {
    for (val, ty) in [
        (0u128, ValueType::I32),
        (0xCAFE_BABE_u128, ValueType::I64),
        (0u128, ValueType::I8),
        (0xFF_u128, ValueType::I8),
    ] {
        make_empty_fn(|b| b.build_int_const(val, ty))
            .unwrap_or_else(|e| panic!("const_then_return failed for ({val:#x}, {ty:?}): {e}"));
    }
}

#[test]
fn every_int_cmp_op_validates() {
    // LessEqual / SlessEqual / NotEqual are absent on purpose: the lifter
    // lowers them.
    for op in [
        IntCmpOp::Equal,
        IntCmpOp::Less,
        IntCmpOp::Sless,
        IntCmpOp::Carry,
        IntCmpOp::Scarry,
        IntCmpOp::Sborrow,
    ] {
        make_empty_fn(|b| {
            let lhs = b.build_int_const(1u64, ValueType::I32)?;
            let rhs = b.build_int_const(2u64, ValueType::I32)?;
            b.build_int_cmp_operation(lhs, rhs, op, ValueType::I32)
        })
        .unwrap_or_else(|e| panic!("IntCmpOp::{op:?} invalid: {e}"));
    }
}

#[test]
fn extend_and_truncate_validate() {
    make_empty_fn(|b| {
        let v8 = b.build_int_const(0xFFu64, ValueType::I8)?;
        let v32_zero = b.extend_if_needed(v8, ValueType::I32, ExtendOp::ZeroExtend)?;
        let v32_sign = b.extend_if_needed(v8, ValueType::I32, ExtendOp::SignExtend)?;
        let _back_to_u8 = b.truncate_if_needed(v32_sign, ValueType::I8)?;
        // Combine both extensions so neither becomes dead.
        b.build_int_binary_operation(v32_zero, v32_sign, IntBinaryOp::Add, ValueType::I32)
    })
    .expect("extend_and_truncate must validate");
}

#[test]
fn switch_target_arity_mismatch_is_rejected() {
    use strider_ir::IRViewer;
    use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, empty_builder};

    // Build a valid switch, then corrupt its side table to N-1 addresses.
    let mut b = empty_builder().unwrap();
    let entry = b.create_region_all().unwrap();
    let a = b.create_region_all().unwrap();
    let c = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    b.build_switch(addr, &[(a, 0x1000), (c, 0x1020)]).unwrap();
    b.set_region(a);
    b.build_return(None, &[]).unwrap();
    b.set_region(c);
    b.build_return(None, &[]).unwrap();
    let mut f = b.build().unwrap();
    assert!(
        strider_ir::validate::validate(&f).is_ok(),
        "well-formed switch validates"
    );
    let sw = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), strider_ir::node::NodeKind::Switch))
        .unwrap();
    f.side_tables_mut().set_switch_targets(sw, vec![0x1000]); // now 1 addr, 2 outputs
    assert!(
        strider_ir::validate::validate(&f).is_err(),
        "arity mismatch rejected"
    );
}

#[test]
fn float_int_conversions_validate() {
    make_empty_fn(|b| {
        let i = b.build_int_const(42u64, ValueType::I32)?;
        let f32_via_to_float = b.build_int_to_float(i, ValueType::F32)?;
        let _back_u32 = b.build_float_to_int(f32_via_to_float, ValueType::I32)?;
        let f32_via_bits = b.build_int_bits_to_float(i, ValueType::F32)?;
        let _bits_back = b.build_float_bits_to_int(f32_via_bits, ValueType::I32)?;
        let f64v = b.build_float_to_float(f32_via_to_float, ValueType::F64)?;
        Ok(f64v)
    })
    .expect("float_int_conversions must validate");
}
