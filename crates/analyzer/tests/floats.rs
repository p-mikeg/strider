//! Float arithmetic, comparisons, and conversions.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use ir::{FloatBinaryOp, FloatCmpOp};
use ir::node::NodeKind;

per_arch_test!("floats", "f32_arith",    has_four_float_binops);
per_arch_test!("floats", "f64_arith",    has_four_float_binops);
per_arch_test!("floats", "f32_to_f64",   has_float_to_float);
per_arch_test!("floats", "f64_to_f32",   has_float_to_float);
per_arch_test!("floats", "int_to_float", has_int_to_float);
per_arch_test!("floats", "float_to_int", has_float_to_int);
per_arch_test!("floats", "f32_compare",  has_two_float_cmps);
per_arch_test!("floats", "f64_compare",  has_two_float_cmps);
per_arch_test!("floats", "f32_neg_abs",  has_float_neg);

fn has_four_float_binops(g: &ir::BuiltFunctionGraph) {
    let total = count_float_binop(g, FloatBinaryOp::Add)
        + count_float_binop(g, FloatBinaryOp::Sub)
        + count_float_binop(g, FloatBinaryOp::Mul)
        + count_float_binop(g, FloatBinaryOp::Div);
    assert!(total >= 4, "expected ≥4 FloatBinaryOp, got {total}");
}
fn has_float_to_float(g: &ir::BuiltFunctionGraph) {
    assert!(has_kind(g, |k| matches!(k, NodeKind::FloatToFloat)),
            "expected ≥1 FloatToFloat node");
}
fn has_int_to_float(g: &ir::BuiltFunctionGraph) {
    assert!(has_kind(g, |k| matches!(k, NodeKind::IntToFloat)),
            "expected ≥1 IntToFloat node");
}
fn has_float_to_int(g: &ir::BuiltFunctionGraph) {
    assert!(has_kind(g, |k| matches!(k, NodeKind::FloatToInt)),
            "expected ≥1 FloatToInt node");
}
fn has_two_float_cmps(g: &ir::BuiltFunctionGraph) {
    let total = count_float_cmp(g, FloatCmpOp::Less)
        + count_float_cmp(g, FloatCmpOp::LessEqual)
        + count_float_cmp(g, FloatCmpOp::Equal)
        + count_float_cmp(g, FloatCmpOp::NotEqual);
    assert!(total >= 2, "expected ≥2 FloatCmpOp, got {total}");
    assert!(count_ifs(g) >= 2, "f32/f64_compare has 2 conditionals");
}
fn has_float_neg(g: &ir::BuiltFunctionGraph) {
    use ir::FloatUnaryOp;
    assert!(count_float_unop(g, FloatUnaryOp::Neg) >= 1, "expected ≥1 FloatUnaryOp::Neg");
}
