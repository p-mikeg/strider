//! GCC builtins lowered to dedicated p-code opcodes.
//!
//! Compiler back-ends sometimes inline popcount / clz / ctz to a software
//! sequence on archs without the dedicated instruction.  Tests therefore
//! tolerate either lowering: a Popcount/Lzcount node when the arch has the
//! instruction, OR a non-trivial graph with shifts and ANDs when it doesn't.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("builtins", "popcount32",    popcount_lowers);
per_arch_test!("builtins", "popcount64",    popcount_lowers);
per_arch_test!("builtins", "clz32",         lzcount_lowers);
per_arch_test!("builtins", "clz64",         lzcount_lowers);
per_arch_test!("builtins", "ctz32",         ctz_lowers);
per_arch_test!("builtins", "expect_branch", expect_compiles_normally);

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
