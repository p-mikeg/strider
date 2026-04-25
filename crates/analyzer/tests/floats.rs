//! Float arithmetic, comparisons, and conversions.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use ir::{FloatBinaryOp, FloatCmpOp};
use ir::node::NodeKind;

per_arch_test!("floats", "f32_arith",    has_four_float_binops, ignore = {
    X86:      "BUG-8: x86 uses 80-bit x87 stack (10-byte registers); analyze_cfg errors on unsupported output size",
    Aarch64:  "BUG-8: float arithmetic instructions not lowered to FloatBinaryOp on aarch64",
});
per_arch_test!("floats", "f64_arith",    has_four_float_binops, ignore = {
    X86:      "BUG-8: x86 uses 80-bit x87 stack (10-byte registers); analyze_cfg errors on unsupported output size",
    Aarch64:  "BUG-8: float arithmetic instructions not lowered to FloatBinaryOp on aarch64",
    Mips32le: "BUG-8: float arithmetic instructions not lowered to FloatBinaryOp on mips32",
    Mips32be: "BUG-8: float arithmetic instructions not lowered to FloatBinaryOp on mips32",
});
per_arch_test!("floats", "f32_to_f64",   has_float_to_float, ignore = {
    X86:      "BUG-9: float conversion lowering type error (not a float value)",
    X64:      "BUG-9: float conversion lowering type error (not a float value)",
    Aarch64:  "BUG-9: float conversion lowering type error (not a float value)",
    Arm:      "BUG-9: float conversion lowering type error (not a float value)",
    Mips32le: "BUG-9: float conversion lowering type error (not a float value)",
    Mips32be: "BUG-9: float conversion lowering type error (not a float value)",
});
per_arch_test!("floats", "f64_to_f32",   has_float_to_float, ignore = {
    X86:      "BUG-9: float conversion lowering type error (not a float value)",
    X64:      "BUG-9: float conversion lowering type error (not a float value)",
    Aarch64:  "BUG-9: float conversion lowering type error (not a float value)",
    Arm:      "BUG-9: float conversion lowering type error (not a float value)",
    Mips32le: "BUG-9: float conversion lowering type error (not a float value)",
    Mips32be: "BUG-9: float conversion lowering type error (not a float value)",
});
per_arch_test!("floats", "int_to_float", has_int_to_float, ignore = {
    X86:      "BUG-9: x86 analyze_cfg errors on 10-byte x87 output size",
    Aarch64:  "BUG-9: int_to_float not producing IntToFloat node on aarch64",
});
per_arch_test!("floats", "float_to_int", has_float_to_int, ignore = {
    X86:      "BUG-9: float conversion lowering type error (not a float value)",
    X64:      "BUG-9: float conversion lowering type error (not a float value)",
    Aarch64:  "BUG-9: float conversion lowering type error (not a float value)",
    Arm:      "BUG-9: float conversion lowering type error (not a float value)",
    Mips32le: "BUG-9: float conversion lowering type error (not a float value)",
    Mips32be: "BUG-9: float conversion lowering type error (not a float value)",
});
per_arch_test!("floats", "f32_compare",  has_two_float_cmps, ignore = {
    X86:      "BUG-10: float comparison lowering error on x86 (analyze_cfg failure)",
    X64:      "BUG-10: float comparison has fewer than 2 conditionals on x64",
    Arm:      "BUG-10: float comparison lowering error on arm (not a bool value)",
});
per_arch_test!("floats", "f64_compare",  has_two_float_cmps, ignore = {
    X86:      "BUG-10: float comparison lowering error on x86 (analyze_cfg failure)",
    X64:      "BUG-10: float comparison has fewer than 2 conditionals on x64",
    Arm:      "BUG-10: float comparison lowering error on arm (analyze_cfg failure)",
});
per_arch_test!("floats", "f32_neg_abs",  has_float_neg, ignore = {
    X86:      "BUG-11: float negation not lowered to FloatUnaryOp::Neg on x86",
    X64:      "BUG-11: float negation not lowered to FloatUnaryOp::Neg on x64",
    Aarch64:  "BUG-11: float negation not lowered to FloatUnaryOp::Neg on aarch64",
    Mips32le: "BUG-11: float negation not lowered to FloatUnaryOp::Neg on mips32",
    Mips32be: "BUG-11: float negation not lowered to FloatUnaryOp::Neg on mips32",
});

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
