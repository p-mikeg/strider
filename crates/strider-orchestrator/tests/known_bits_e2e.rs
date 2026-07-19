//! End-to-end validity test for the `KnownBits` optimizer pass.
//!
//! `KnownBits` propagates a bit-level zeros/ones lattice and folds operations
//! whose result bits are fully determined even though their inputs are
//! runtime-opaque; `ConstantFold` alone cannot do these folds, having no bit
//! lattice.
//!
//! Fixture: `known_bits.c::kb_or_then_mask` (`x |= 1; <barrier>; return x &
//! 1;`). The `__asm__ volatile` barrier stops the compiler from collapsing
//! the pair at -O2, so it emits a real `or`/`ori` then `and`/`andi` (verified
//! on x64 and mips32be) rather than pre-folding to `mov $1`. The lifted IR is
//! `And(Or(x, 1), 1)`.
//!
//! KnownBits knows bit 0 of `Or(x, 1)` is one, so it folds `And(_, 1)` to the
//! constant 1 and the `And` node disappears. Both assertions below fail if
//! KnownBits is removed: ConstantFold can't fold `And(Or(x,1), 1)` since its
//! operand `Or(x, 1)` isn't a constant.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;

per_arch_test!("known_bits", "kb_or_then_mask", kb_mask_folds_to_const);

fn kb_mask_folds_to_const(function: &strider_ir::Function) {
    assert!(
        has_constant(function, 1),
        "KnownBits should fold And(Or(x,1), 1) to the constant 1"
    );
    // A surviving And means KnownBits did not fire (ConstantFold can't
    // remove it: its operand Or(x,1) isn't a constant).
    assert_eq!(
        count_int_binop(function, strider_ir::IntBinaryOp::And),
        0,
        "KnownBits should have folded the (… & 1) mask away; an And node \
         remains, so the bit-lattice fold did not happen"
    );
}
