mod common;
use common::*;
use strider_ir::node::NodeKind;
use strider_ir::{FloatBinaryOp, FloatCmpOp};

per_arch_test!("floats", "f32_arith", has_four_float_binops);
per_arch_test!("floats", "f64_arith", has_four_float_binops);
per_arch_test!("floats", "f32_to_f64", has_float_to_float);
per_arch_test!("floats", "f64_to_f32", has_float_to_float);
per_arch_test!("floats", "int_to_float", has_int_to_float, ignore = {
    Ppc32be: "PPC32 ISA has no single int->float scalar op; gcc emits the magic-number trick (xoris+lfd+fsub+frsp), so the IR has FloatBinaryOp(Sub) + FloatToFloat and no IntToFloat",
    Ppc32le: "same magic-number lowering as ppc32be (clang at -O0)",
});
per_arch_test!("floats", "float_to_int", has_float_to_int);
per_arch_test!("floats", "f32_compare", has_two_float_cmps);
per_arch_test!("floats", "f64_compare", has_two_float_cmps);
per_arch_test!("floats", "f32_neg_abs", has_float_neg);

fn has_four_float_binops(function: &strider_ir::Function) {
    // `FloatSub` lifts to `FloatAdd(_, FloatUnaryOp::Neg(_))`, so a real
    // subtraction contributes one Add and one Neg; counting Adds alone
    // would double-count subtractions against the source's binop count.
    let total = count_float_binop(function, FloatBinaryOp::Add)
        + count_float_binop(function, FloatBinaryOp::Mul)
        + count_float_binop(function, FloatBinaryOp::Div);
    assert!(
        total >= 4,
        "expected >=4 FloatBinaryOp (including lowered subs counted as Add), got {total}"
    );
}
fn has_float_to_float(function: &strider_ir::Function) {
    assert!(
        has_kind(function, |k| matches!(k, NodeKind::FloatToFloat)),
        "expected >=1 FloatToFloat node"
    );
}
fn has_int_to_float(function: &strider_ir::Function) {
    assert!(
        has_kind(function, |k| matches!(k, NodeKind::IntToFloat)),
        "expected >=1 IntToFloat node"
    );
}
fn has_float_to_int(function: &strider_ir::Function) {
    assert!(
        has_kind(function, |k| matches!(k, NodeKind::FloatToInt)),
        "expected >=1 FloatToInt node"
    );
}
fn has_two_float_cmps(function: &strider_ir::Function) {
    // The source has two `if (a OP b)` branches, but x64 / aarch64 may
    // lower one or both via cmov / csel instead of a real branch, so
    // counting `If` nodes is unreliable; count FloatCmpOp instead
    // (>= 2, one per source `OP`, regardless of If-vs-cmov lowering).
    // `LessEqual` and `NotEqual` aren't primitives: they lower to
    // Equal/Less compositions, so a source-level `<=` becomes one Equal
    // + one Less here.
    let total =
        count_float_cmp(function, FloatCmpOp::Less) + count_float_cmp(function, FloatCmpOp::Equal);
    assert!(total >= 2, "expected >=2 FloatCmpOp, got {total}");
}
fn has_float_neg(function: &strider_ir::Function) {
    // Float negation `-f` lowers either to FloatUnaryOp::Neg directly, or
    // to a Xor against the sign bit (0x80000000 / 0x80000000_00000000).
    // The sign mask may be a direct IntConst or a vector-load from .rodata
    // (x86_64 SSE's xorps against [.LC]), in which case the bit pattern
    // isn't a foldable IntConst, so we accept either a Neg node or any Xor.
    use strider_ir::FloatUnaryOp;
    let has_neg = count_float_unop(function, FloatUnaryOp::Neg) >= 1;
    let has_xor = count_int_binop(function, strider_ir::IntBinaryOp::Xor) >= 1;
    assert!(
        has_neg || has_xor,
        "expected FloatUnaryOp::Neg or any Xor (sign-bit toggle); \
             neg_count={}, xor_count={}",
        count_float_unop(function, FloatUnaryOp::Neg),
        count_int_binop(function, strider_ir::IntBinaryOp::Xor)
    );
}
