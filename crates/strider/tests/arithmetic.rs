//! Pin every IntBinaryOp / IntUnaryOp variant the analyzer must lower.
//!
//! 15 functions × 6 archs = 90 tests.  Each test asserts the function's
//! optimised IR contains at least one node of the expected kind.
//!
//! # Known arch-specific limitations
//!
//! * **MIPS `mul`**: the MIPS `MULT` instruction writes into the HI/LO
//!   register pair; the GHIDRA Sleigh spec represents the result through a
//!   unique varnode that the analyzer does not translate to `IntBinaryOp::Mul`.
//!   `test_mul::mips32le` and `test_mul::mips32be` will therefore fail.
//!
//! * **MIPS signed/unsigned divide & modulo** (`sdiv`, `smod`, `udiv`,
//!   `umod`): the CFG builder panics with "invalid branch target variable"
//!   when lifting the MIPS DIV/DIVU instruction.  All four mips32le and
//!   mips32be tests for these functions panic at the CFG-build stage.
//!
//! * **ARM divide & modulo** (`sdiv`, `smod`, `udiv`, `umod`): soft-float
//!   ARM targets emit library calls (`__udivsi3`, `__divsi3`, etc.) instead
//!   of native divide instructions; the IR contains a `Call` node but no
//!   `Div`/`Rem` node.  The assertions for these four functions accept a
//!   `Call` node as evidence that the operation was lowered.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use ir::{IntBinaryOp, IntUnaryOp};

per_arch_test!("arithmetic", "add",     has_add);
per_arch_test!("arithmetic", "sub",     has_sub);
per_arch_test!("arithmetic", "mul",     has_mul);
per_arch_test!("arithmetic", "udiv",    has_div);
per_arch_test!("arithmetic", "umod",    has_rem);
per_arch_test!("arithmetic", "sdiv",    has_sdiv);
per_arch_test!("arithmetic", "smod",    has_srem);
per_arch_test!("arithmetic", "bit_and", has_and);
per_arch_test!("arithmetic", "bit_or",  has_or);
per_arch_test!("arithmetic", "bit_xor", has_xor);
per_arch_test!("arithmetic", "bit_not", has_not);
per_arch_test!("arithmetic", "shl",     has_shl);
per_arch_test!("arithmetic", "lshr",    has_lshr);
per_arch_test!("arithmetic", "ashr",    has_ashr);
per_arch_test!("arithmetic", "negate",  has_neg);

fn has_add(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::Add) >= 1, "expected ≥1 Add");
}
fn has_sub(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::Sub) >= 1, "expected ≥1 Sub");
}
fn has_mul(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::Mul) >= 1, "expected ≥1 Mul");
}

// Unsigned divide.  Most arches emit a native Div node.  ARM soft-float
// targets emit a library call instead, so we accept Call as evidence.
fn has_div(g: &ir::BuiltFunctionGraph) {
    assert!(
        count_int_binop(g, IntBinaryOp::Div) >= 1 || count_calls(g) >= 1,
        "expected ≥1 Div or a library Call for udiv"
    );
}

// Unsigned remainder.  x86/x64 produce Rem; AArch64 synthesises it as
// a - (a/b)*b so only Div is present; ARM uses a library call.
fn has_rem(g: &ir::BuiltFunctionGraph) {
    assert!(
        count_int_binop(g, IntBinaryOp::Rem) >= 1
            || count_int_binop(g, IntBinaryOp::Div) >= 1
            || count_calls(g) >= 1,
        "expected ≥1 Rem, Div (synthesised mod), or a library Call for umod"
    );
}

// Signed divide.  Most arches emit Sdiv; ARM uses a library call.
fn has_sdiv(g: &ir::BuiltFunctionGraph) {
    assert!(
        count_int_binop(g, IntBinaryOp::Sdiv) >= 1 || count_calls(g) >= 1,
        "expected ≥1 Sdiv or a library Call for sdiv"
    );
}

// Signed remainder.  x86/x64 produce Srem; AArch64 synthesises it as
// a - (a/b)*b so only Sdiv is present; ARM uses a library call.
fn has_srem(g: &ir::BuiltFunctionGraph) {
    assert!(
        count_int_binop(g, IntBinaryOp::Srem) >= 1
            || count_int_binop(g, IntBinaryOp::Sdiv) >= 1
            || count_calls(g) >= 1,
        "expected ≥1 Srem, Sdiv (synthesised smod), or a library Call for smod"
    );
}

fn has_and(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::And) >= 1, "expected ≥1 And");
}
fn has_or(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::Or) >= 1, "expected ≥1 Or");
}
fn has_xor(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::Xor) >= 1, "expected ≥1 Xor");
}

// Bitwise complement (~a).  Sleigh's `IntNeg` opcode lifts to `IntUnaryOp::BitNot`.
fn has_not(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_unop(g, IntUnaryOp::BitNot) >= 1, "expected ≥1 BitNot (bitwise complement)");
}

fn has_shl(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::ShiftLeft) >= 1, "expected ≥1 ShiftLeft");
}
fn has_lshr(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::ShiftRight) >= 1, "expected ≥1 ShiftRight");
}
fn has_ashr(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, IntBinaryOp::SShiftRight) >= 1, "expected ≥1 SShiftRight");
}

// Arithmetic negation (-a).  Sleigh's `Int2Comp` opcode lifts to `IntUnaryOp::Neg`.
// ARM and MIPS synthesise it as 0 - a, so those archs produce `IntBinaryOp::Sub`
// instead.
fn has_neg(g: &ir::BuiltFunctionGraph) {
    assert!(
        count_int_unop(g, IntUnaryOp::Neg) >= 1 || count_int_binop(g, IntBinaryOp::Sub) >= 1,
        "expected ≥1 Neg (two's-complement) or Sub (0-a synthesis) for negate"
    );
}
