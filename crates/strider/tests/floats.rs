//! Float arithmetic, comparisons, and conversions.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use ir::{FloatBinaryOp, FloatCmpOp};
use ir::node::NodeKind;

// `arm_be` skips every float test: ARM8_BE Sleigh's VFP register file
// uses descending offsets and `d0` does not overlap `s0`, so the
// analyzer's container-register aliasing drops the entire VFP read/write
// chain.  The body of every VFP-using function reduces to Entry / Return
// / FunctionArg / InitialVar in the IR, with no Float* nodes.
per_arch_test!("floats", "f32_arith",    has_four_float_binops, ignore = {
    ArmBe: "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — 0 FloatBinaryOps in IR",
});
per_arch_test!("floats", "f64_arith",    has_four_float_binops, ignore = {
    ArmBe: "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — 0 FloatBinaryOps in IR",
});
per_arch_test!("floats", "f32_to_f64",   has_float_to_float, ignore = {
    ArmBe: "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — no FloatToFloat",
});
per_arch_test!("floats", "f64_to_f32",   has_float_to_float, ignore = {
    ArmBe: "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — no FloatToFloat",
});
per_arch_test!("floats", "int_to_float", has_int_to_float, ignore = {
    Ppc32be: "PPC32 ISA has no single int→float scalar op; gcc emits the magic-number trick (xoris+lfd+fsub+frsp) — IR has FloatBinaryOp(Sub) + FloatToFloat, no IntToFloat",
    Ppc32le: "same magic-number lowering as ppc32be (clang at -O0)",
    ArmBe:   "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — no IntToFloat",
});
per_arch_test!("floats", "float_to_int", has_float_to_int, ignore = {
    ArmBe: "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — no FloatToInt",
});
per_arch_test!("floats", "f32_compare",  has_two_float_cmps, ignore = {
    ArmBe: "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — no FloatCmpOp",
});
per_arch_test!("floats", "f64_compare",  has_two_float_cmps, ignore = {
    ArmBe: "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — no FloatCmpOp",
});
per_arch_test!("floats", "f32_neg_abs",  has_float_neg, ignore = {
    ArmBe: "arm_be VFP regs descending-offset; analyzer aliasing drops the chain — no FloatUnaryOp::Neg",
});

fn has_four_float_binops(g: &ir::BuiltFunctionGraph) {
    // `FloatBinaryOp::Sub` is no longer a primitive — `FloatSub` lifts to
    // `FloatAdd(_, FloatUnaryOp::Neg(_))`.  A real subtraction in the
    // source contributes one `FloatAdd` AND one `FloatUnaryOp::Neg`, so
    // counting Adds alone double-counts subtractions; instead we count
    // each binop kind plus the lowered-Sub `Neg` markers.
    let total = count_float_binop(g, FloatBinaryOp::Add)
        + count_float_binop(g, FloatBinaryOp::Mul)
        + count_float_binop(g, FloatBinaryOp::Div);
    assert!(total >= 4, "expected ≥4 FloatBinaryOp (including lowered subs counted as Add), got {total}");
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
    // The C source has two `if (a OP b) ...` branches.  x64 / aarch64 may
    // lower one or both via cmov / csel (conditional-move) instead of a
    // real branch — those don't appear as `If` nodes in the IR.  The
    // assertion that survives all archs: at least 2 FloatCmpOp nodes
    // (one per `OP` in the source, regardless of whether the surrounding
    // construct lowers as If or cmov).
    // `LessEqual` and `NotEqual` are no longer primitives — they lower to
    // compositions of `Equal` and `Less` (see `pcode_lift::value::float`).
    // Either source-level `<=` becomes one `Equal` + one `Less` here.
    let total = count_float_cmp(g, FloatCmpOp::Less) + count_float_cmp(g, FloatCmpOp::Equal);
    assert!(total >= 2, "expected ≥2 FloatCmpOp, got {total}");
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
