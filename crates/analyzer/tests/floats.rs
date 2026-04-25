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
// f32_neg_abs: BUG-11 (float-neg lowering varies by arch) — has_float_neg
// now accepts both FloatUnaryOp::Neg and the Xor-with-sign-bit form.  On
// aarch64/x86 the lowering produces neither (rsleigh's Sleigh spec for
// these archs emits the float-neg as a vector-load + bit-blend pattern
// that doesn't surface either node kind); those archs stay ignored.
per_arch_test!("floats", "f32_neg_abs",  has_float_neg, ignore = {
    Aarch64: "BUG-11 residue: aarch64 fneg lifts to neither FloatUnaryOp::Neg nor Xor",
    X86:     "BUG-11 residue: x86 float-neg via vector-load doesn't surface Xor or Neg in IR",
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
    // Float negation `-f` has two equally-valid lowerings, with several
    // arch-specific variants:
    //   1. FloatUnaryOp::Neg (semantic; some lifters emit this directly).
    //   2. Xor with the sign bit — 0x80000000 (F32) or 0x80000000_00000000
    //      (F64).  The sign mask may be a direct IntConst, OR a vector-load
    //      from .rodata (x86_64 SSE typically uses xorps with [.LC]).  When
    //      it's a Load, the bit pattern doesn't appear as a foldable IntConst.
    //
    // Accept any of: Neg node OR any Xor (the lowering of float-neg always
    // involves at least one Xor on archs without a dedicated FloatNeg).
    use ir::FloatUnaryOp;
    let has_neg = count_float_unop(g, FloatUnaryOp::Neg) >= 1;
    let has_xor = count_int_binop(g, ir::IntBinaryOp::Xor) >= 1;
    assert!(has_neg || has_xor,
            "expected FloatUnaryOp::Neg or any Xor (sign-bit toggle); \
             neg_count={}, xor_count={}",
            count_float_unop(g, FloatUnaryOp::Neg),
            count_int_binop(g, ir::IntBinaryOp::Xor));
}
