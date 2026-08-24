//! Pin every IntBinaryOp / IntUnaryOp variant the analyzer must lower: each
//! test asserts the optimised IR holds at least one node of the expected kind.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::useless_conversion
)]

mod common;
use common::*;
use strider_ir::{IntBinaryOp, IntUnaryOp};

per_arch_test!("arithmetic", "add", has_add);
per_arch_test!("arithmetic", "sub", has_sub);
per_arch_test!("arithmetic", "mul", has_mul);
per_arch_test!("arithmetic", "udiv", has_div);
per_arch_test!("arithmetic", "umod", has_rem);
per_arch_test!("arithmetic", "sdiv", has_sdiv);
per_arch_test!("arithmetic", "smod", has_srem);
per_arch_test!("arithmetic", "bit_and", has_and);
per_arch_test!("arithmetic", "bit_or", has_or);
per_arch_test!("arithmetic", "bit_xor", has_xor);
per_arch_test!("arithmetic", "bit_not", has_not);
per_arch_test!("arithmetic", "shl", has_shl);
per_arch_test!("arithmetic", "lshr", has_lshr);
per_arch_test!("arithmetic", "ashr", has_ashr);
per_arch_test!("arithmetic", "negate", has_neg);

fn has_add(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::Add) >= 1,
        "expected ≥1 Add"
    );
}
// Lift lowers `IntSub` to `Add(_, Neg(_))`, so "has subtraction" is a check
// for `IntUnaryOp::Neg`: every real subtraction contributes at least one.
fn has_sub(function: &strider_ir::Function) {
    assert!(
        count_int_unop(function, IntUnaryOp::Neg) >= 1,
        "expected ≥1 IntUnaryOp::Neg (the lowered Sub form)"
    );
}
fn has_mul(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::Mul) >= 1,
        "expected ≥1 Mul"
    );
}

// Unsigned divide.  Most arches emit a native Div node.  ARM soft-float
// targets emit a library call instead, so we accept Call as evidence.
fn has_div(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::Div) >= 1 || count_calls(function) >= 1,
        "expected ≥1 Div or a library Call for udiv"
    );
}

// Unsigned remainder.  x86/x64 produce Rem; AArch64 synthesises it as
// a - (a/b)*b so only Div is present; ARM uses a library call.
fn has_rem(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::Rem) >= 1
            || count_int_binop(function, IntBinaryOp::Div) >= 1
            || count_calls(function) >= 1,
        "expected ≥1 Rem, Div (synthesised mod), or a library Call for umod"
    );
}

// Signed divide.  Most arches emit Sdiv; ARM uses a library call.
fn has_sdiv(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::Sdiv) >= 1 || count_calls(function) >= 1,
        "expected ≥1 Sdiv or a library Call for sdiv"
    );
}

// Signed remainder.  x86/x64 produce Srem; AArch64 synthesises it as
// a - (a/b)*b so only Sdiv is present; ARM uses a library call.
fn has_srem(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::Srem) >= 1
            || count_int_binop(function, IntBinaryOp::Sdiv) >= 1
            || count_calls(function) >= 1,
        "expected ≥1 Srem, Sdiv (synthesised smod), or a library Call for smod"
    );
}

fn has_and(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::And) >= 1,
        "expected ≥1 And"
    );
}
fn has_or(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::Or) >= 1,
        "expected ≥1 Or"
    );
}
fn has_xor(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::Xor) >= 1,
        "expected ≥1 Xor"
    );
}

// Bitwise complement (~a).  Sleigh's `IntNeg` opcode lifts to the canonical
// `Xor(a, IntConst(all_ones))`.
fn has_not(function: &strider_ir::Function) {
    use strider_pattern::{MatchPat, Matcher, anything, int_not};
    let pat = int_not(anything()).into_pattern();
    let count = Matcher::new(function).find_all(&pat).unwrap().len();
    assert!(
        count >= 1,
        "expected ≥1 bit_not (bitwise complement Xor-with-all-ones)"
    );
}

fn has_shl(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::ShiftLeft) >= 1,
        "expected ≥1 ShiftLeft"
    );
}
fn has_lshr(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::ShiftRight) >= 1,
        "expected ≥1 ShiftRight"
    );
}
fn has_ashr(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, IntBinaryOp::SShiftRight) >= 1,
        "expected ≥1 SShiftRight"
    );
}

// Arithmetic negation (-a).  Sleigh's `Int2Comp` lifts to `IntUnaryOp::Neg`.
// ARM and MIPS synthesise it as `0 - a`, which lifts to `Add(0, Neg(a))` and
// collapses to `Neg(a)` under the `x + 0 -> x` identity, so both paths land on
// `IntUnaryOp::Neg`.
fn has_neg(function: &strider_ir::Function) {
    assert!(
        count_int_unop(function, IntUnaryOp::Neg) >= 1,
        "expected ≥1 Neg (two's-complement; ARM/MIPS 0-a synthesis collapses to the same shape)"
    );
}
