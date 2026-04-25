//! GCC builtins lowered to dedicated p-code opcodes.
//!
//! Compiler back-ends sometimes inline popcount / clz / ctz to a software
//! sequence on archs without the dedicated instruction.  Tests therefore
//! tolerate either lowering: a Popcount/Lzcount node when the arch has the
//! instruction, OR a non-trivial graph with shifts and ANDs when it doesn't.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("builtins", "popcount32",    popcount_lowers, ignore = {
    X86:      "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    X64:      "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    Aarch64:  "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    Arm:      "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    Mips32le: "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    Mips32be: "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
});
per_arch_test!("builtins", "popcount64",    popcount_lowers, ignore = {
    X86:      "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    X64:      "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    Aarch64:  "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    Arm:      "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    Mips32le: "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
    Mips32be: "BUG-15: GCC __builtin_popcount lowering not recognised by analyzer",
});
per_arch_test!("builtins", "clz32",         lzcount_lowers, ignore = {
    X86: "BUG-16: x86 BSR not lowered to NodeKind::Lzcount",
    X64: "BUG-16: x86 BSR not lowered to NodeKind::Lzcount",
});
per_arch_test!("builtins", "clz64",         lzcount_lowers, ignore = {
    X86: "BUG-16: x86 BSR not lowered to NodeKind::Lzcount",
    X64: "BUG-16: x86 BSR not lowered to NodeKind::Lzcount",
});
per_arch_test!("builtins", "ctz32",         ctz_lowers, ignore = {
    X86: "BUG-17: x86 BSF/TZCNT not lowered; And+Neg fallback not present in graph",
    X64: "BUG-17: x86 BSF/TZCNT not lowered; And+Neg fallback not present in graph",
});
// expect_branch: BUG-18 fixed by adding asm-volatile barriers around the
// branch in fixtures/cases/builtins.c.  ARM still hits a separate BUG-3
// post-opt Bool→AnyInt residue (same as early_return::arm) — left ignored.
per_arch_test!("builtins", "expect_branch", expect_compiles_normally, ignore = {
    Arm: "BUG-3 post-opt residue: ARM expect_branch hits Bool→AnyInt validator after opt",
});

fn popcount_lowers(g: &ir::BuiltFunctionGraph) {
    let dedicated = count_popcount(g);
    let fallback_swar = count_int_binop(g, ir::IntBinaryOp::And)
        + count_int_binop(g, ir::IntBinaryOp::ShiftRight);
    assert!(dedicated >= 1 || fallback_swar >= 4,
            "popcount lowers to Popcount node ({dedicated}) or SWAR (got {fallback_swar} AND/SHR ops)");
}
fn lzcount_lowers(g: &ir::BuiltFunctionGraph) {
    let dedicated = count_lzcount(g);
    let nodes = g.preorder().count();
    assert!(dedicated >= 1 || nodes > 10,
            "clz lowers to Lzcount node ({dedicated}) or non-trivial fallback ({nodes} nodes)");
}
fn ctz_lowers(g: &ir::BuiltFunctionGraph) {
    // ctz(x) is often expressed as clz(x & -x).
    let lz = count_lzcount(g);
    let isolate = count_int_binop(g, ir::IntBinaryOp::And) + count_int_unop(g, ir::IntUnaryOp::Neg);
    assert!(lz >= 1 || isolate >= 1, "ctz expected via Lzcount or And+Neg pattern");
}
fn expect_compiles_normally(g: &ir::BuiltFunctionGraph) {
    // __builtin_expect is a hint, not a real op — should reduce to plain control flow.
    assert!(count_ifs(g) >= 1, "expect_branch has an if");
}
