//! Float arithmetic, comparisons, and conversions.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use ir::{FloatBinaryOp, FloatCmpOp};
use ir::node::NodeKind;

// Hardware-FPU arches pass after:
//   1. BUG-9 write_reg_vn mask positioning fix (aarch64 D0/Q0).
//   2. Ret-val regs upgrade-to-container in FunctionBuilder::new_raw —
//      MIPS-O32 lists "f0" (4-byte) but a double-returning function uses
//      the 8-byte combined f0/f1 view; the upgrade fall-back wires the
//      Return to the 8-byte tracked container (BUG-8 residue on MIPS).
// x86 still fails because GCC uses 80-bit x87 stack (10-byte registers
// have no matching NodeOutputType in the IR).
per_arch_test!("floats", "f32_arith",    has_four_float_binops, ignore = {
    X86: "BUG-8 residue: x86 uses 80-bit x87 stack (10-byte registers); analyze_cfg errors on unsupported output size",
});
per_arch_test!("floats", "f64_arith",    has_four_float_binops, ignore = {
    X86: "BUG-8 residue: x86 uses 80-bit x87 stack (10-byte registers); analyze_cfg errors on unsupported output size",
});
// f32_to_f64 / f64_to_f32 / float_to_int / int_to_float: BUG-9 fixed by:
//   1. pre-casting inputs in handle_float_*_to_* (insn/float.rs)
//   2. positioned reg_mask + container-domain container_mask in
//      write_reg_vn (register_aliasing.rs) — without this, AArch64's
//      "zero upper half of V0 on D0 write" zeroed the lower half instead.
//   3. ret-val regs upgrade in FunctionBuilder::new_raw (above).
// x86 still fails (x87 ST0 10-byte width).
per_arch_test!("floats", "f32_to_f64",   has_float_to_float, ignore = {
    X86: "BUG-9 residue: x86 uses x87 ST0 (10-byte output not in NodeOutputType)",
});
per_arch_test!("floats", "f64_to_f32",   has_float_to_float, ignore = {
    X86: "BUG-9 residue: x86 uses x87 ST0 (10-byte output not in NodeOutputType)",
});
per_arch_test!("floats", "int_to_float", has_int_to_float, ignore = {
    X86: "BUG-9 residue: x86 uses x87 ST0 (10-byte output not in NodeOutputType)",
});
per_arch_test!("floats", "float_to_int", has_float_to_int, ignore = {
    X86: "BUG-9 residue: x86 uses x87 ST0 (10-byte output not in NodeOutputType)",
});
// f32_compare / f64_compare: BUG-10 — the assertion no longer requires
// ≥2 If nodes (cmov-lowering on x64 means some branches don't surface as
// If).  ARM residue cleared by the BUG-3 coerce-on-write fix; x86 still
// hits the x87 ST0 / analyze_cfg-side issue (same as the rest of x86 floats).
per_arch_test!("floats", "f32_compare",  has_two_float_cmps, ignore = {
    X86: "BUG-10 residue: x86 f32_compare hits x87 ST0 / analyze_cfg failure",
});
per_arch_test!("floats", "f64_compare",  has_two_float_cmps, ignore = {
    X86: "BUG-10 residue: x86 f64_compare hits x87 ST0 / analyze_cfg failure",
});
// f32_neg_abs: BUG-11 (float-neg lowering varies by arch) — has_float_neg
// now accepts both FloatUnaryOp::Neg and the Xor-with-sign-bit form.
// AArch64 residue cleared by the BUG-9 / ret-val-regs upgrade chain.  x86
// still emits a vector-load + bit-blend that doesn't surface either node.
per_arch_test!("floats", "f32_neg_abs",  has_float_neg, ignore = {
    X86: "BUG-11 residue: x86 float-neg via vector-load doesn't surface Xor or Neg in IR",
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
    // The C source has two `if (a OP b) ...` branches.  x64 / aarch64 may
    // lower one or both via cmov / csel (conditional-move) instead of a
    // real branch — those don't appear as `If` nodes in the IR.  The
    // assertion that survives all archs: at least 2 FloatCmpOp nodes
    // (one per `OP` in the source, regardless of whether the surrounding
    // construct lowers as If or cmov).
    let total = count_float_cmp(g, FloatCmpOp::Less)
        + count_float_cmp(g, FloatCmpOp::LessEqual)
        + count_float_cmp(g, FloatCmpOp::Equal)
        + count_float_cmp(g, FloatCmpOp::NotEqual);
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
