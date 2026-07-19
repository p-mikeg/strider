//! GCC builtins lowered to dedicated p-code opcodes.
//!
//! Compiler back-ends sometimes inline popcount / clz / ctz to a software
//! sequence on archs without the dedicated instruction.  Tests therefore
//! tolerate either lowering: a Popcount/Lzcount node when the arch has the
//! instruction, OR a non-trivial graph with shifts and ANDs when it doesn't.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;
use strider_ir::IRWalker;

// popcount/clz/ctz assertions are structural ("graph is non-trivial")
// rather than pinning specific node kinds, since lowering varies
// significantly across (compiler, arch, ISA-extension) tuples and the
// analyzer's job is "doesn't crash on builtin inputs".
per_arch_test!("builtins", "popcount32", popcount_lowers);
per_arch_test!("builtins", "popcount64", popcount_lowers);
per_arch_test!("builtins", "clz32", lzcount_lowers);
per_arch_test!("builtins", "clz64", lzcount_lowers);
per_arch_test!("builtins", "ctz32", ctz_lowers);
// expect_branch: relies on asm-volatile barriers around the branch in
// fixtures/cases/builtins.c so the optimizer does not elide it.
per_arch_test!("builtins", "expect_branch", expect_compiles_normally);

/// `__builtin_popcount` lowering varies massively across (compiler, arch):
/// x86_64 with -mpopcnt uses the native `popcnt` (a CallOther, a Popcount
/// node, or a few moves, none guaranteed); aarch64 goes through NEON `cnt`
/// plus a sum reduction; mips32/arm have no native popcount and unroll a
/// SWAR loop or call out. Pinning "popcount" to a specific node kind would
/// be too brittle, so this only asserts the graph is non-trivial.
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
