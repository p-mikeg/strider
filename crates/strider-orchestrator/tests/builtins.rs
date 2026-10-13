//! Compiler back-ends lower popcount / clz / ctz three different ways, and a
//! lift of any of them must show the work: a dedicated `Popcount` / `Lzcount`
//! node (aarch64 `cnt`, ARM / MIPS `clz`, ppc64le `popcntw`), a `Call` out to
//! the libgcc helper (`__popcountsi2` on arm / mips / ppc32 / x86-64), or the
//! bit sequence the back-end inlined instead (the SWAR multiply on x86-32 and
//! ppc64be, `bsr` + `xor` for x86 clz, `x & -x` for ctz).
//!
//! A node count is no evidence, an empty function already walking to several;
//! each of the three alternatives below is absent from one.

mod common;
use common::*;
use strider_ir::node::{IntBinaryOp, IntUnaryOp, NodeKind};
use strider_ir_test_utils::IrWalkerEx;

per_arch_test!("builtins", "popcount32", bit_count_is_lowered);
per_arch_test!("builtins", "popcount64", bit_count_is_lowered);
per_arch_test!("builtins", "clz32", bit_count_is_lowered);
per_arch_test!("builtins", "clz64", bit_count_is_lowered);
per_arch_test!("builtins", "ctz32", bit_count_is_lowered);
// expect_branch: relies on asm-volatile barriers around the branch in
// fixtures/cases/builtins.c so the optimizer does not elide it.
per_arch_test!("builtins", "expect_branch", expect_compiles_normally);

fn bit_count_is_lowered(function: &strider_ir::Function) {
    let dedicated = function.count_kind(|k| matches!(k, NodeKind::Popcount | NodeKind::Lzcount));
    let calls = function.count_kind(|k| matches!(k, NodeKind::Call { .. }));
    let bit_ops = function.count_kind(|k| {
        matches!(
            k,
            NodeKind::IntBinaryOp(
                IntBinaryOp::And
                    | IntBinaryOp::Or
                    | IntBinaryOp::Xor
                    | IntBinaryOp::ShiftLeft
                    | IntBinaryOp::ShiftRight
                    | IntBinaryOp::SShiftRight
            ) | NodeKind::IntUnaryOp(IntUnaryOp::Neg)
        )
    });
    assert!(
        dedicated >= 1 || calls >= 1 || bit_ops >= 2,
        "no bit-count lowering in the graph: {dedicated} Popcount/Lzcount nodes, \
         {calls} calls, {bit_ops} bitwise/shift ops"
    );
}

fn expect_compiles_normally(function: &strider_ir::Function) {
    // __builtin_expect is a hint, not a real op; reduces to plain control flow.
    assert!(count_ifs(function) >= 1, "expect_branch has an if");
}
