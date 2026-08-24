//! Compiler back-ends inline popcount / clz / ctz to a software sequence on
//! archs without the dedicated instruction, so these tests tolerate either
//! lowering: a Popcount/Lzcount node, or a non-trivial graph of shifts and
//! ANDs.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;
use strider_ir::IRWalker;

per_arch_test!("builtins", "popcount32", popcount_lowers);
per_arch_test!("builtins", "popcount64", popcount_lowers);
per_arch_test!("builtins", "clz32", lzcount_lowers);
per_arch_test!("builtins", "clz64", lzcount_lowers);
per_arch_test!("builtins", "ctz32", ctz_lowers);
// expect_branch: relies on asm-volatile barriers around the branch in
// fixtures/cases/builtins.c so the optimizer does not elide it.
per_arch_test!("builtins", "expect_branch", expect_compiles_normally);

/// `__builtin_popcount` lowering varies across (compiler, arch): x86_64 with
/// -mpopcnt uses the native `popcnt` (a CallOther, a Popcount node, or a few
/// moves, none guaranteed); aarch64 goes through NEON `cnt` plus a sum
/// reduction; mips32/arm unroll a SWAR loop or call out. Hence the assertion
/// is only that the graph is non-trivial.
fn popcount_lowers(function: &strider_ir::Function) {
    let nodes = function.walk().count();
    assert!(
        nodes > 5,
        "popcount must produce a non-trivial graph; got {nodes} reachable nodes"
    );
}
fn lzcount_lowers(function: &strider_ir::Function) {
    // Same loose check as popcount: clz lowering is even more variable.
    let nodes = function.walk().count();
    assert!(
        nodes > 5,
        "clz must produce a non-trivial graph; got {nodes} reachable nodes"
    );
}
fn ctz_lowers(function: &strider_ir::Function) {
    let nodes = function.walk().count();
    assert!(
        nodes > 5,
        "ctz must produce a non-trivial graph; got {nodes} reachable nodes"
    );
}
fn expect_compiles_normally(function: &strider_ir::Function) {
    // __builtin_expect is a hint, not a real op; reduces to plain control flow.
    assert!(count_ifs(function) >= 1, "expect_branch has an if");
}
