//! End-to-end validity test for the `KnownBits` optimizer pass.
//!
//! `KnownBits` propagates a bit-level zeros/ones lattice and uses it to fold
//! operations whose result bits are fully determined even though their inputs
//! are runtime-opaque.  `ConstantFold` alone CANNOT do these folds — it has no
//! bit lattice.
//!
//! Fixture: `known_bits.c::kb_or_then_mask`, which is `x |= 1; <barrier>;
//! return x & 1;`.  The `__asm__ volatile` barrier between the `|= 1` and the
//! `& 1` stops the C compiler from collapsing the pair at -O2: it emits a real
//! `or`/`ori` followed by a real `and`/`andi` (verified on x64 and mips32be —
//! the compiler does NOT pre-fold to `mov $1`).  The lifted IR is therefore
//! `And(Or(x, 1), 1)`.
//!
//! KnownBits knows bit 0 of `Or(x, 1)` is one, so it folds `And(_, 1)` to the
//! constant 1 and the `And` node disappears.  The assertions below pin exactly
//! that: the constant 1 is present (the folded result), and no `And` node is
//! left (the mask was folded away).
//!
//! Both assertions fail if KnownBits is removed, because ConstantFold cannot
//! fold `And(Or(x,1), 1)` — its operand `Or(x, 1)` is not a constant.
//!
//! Endianness-independent, but run across the full arch matrix — it passes
//! everywhere the `or`/`and` pair survives the compiler.

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
    // The `& 1` mask must have folded to the constant 1 …
    assert!(
        has_constant(function, 1),
        "KnownBits should fold And(Or(x,1), 1) to the constant 1"
    );
    // … and the And node must be gone.  ConstantFold cannot remove this And
    // (its operand Or(x,1) is not a constant), so a surviving And means
    // KnownBits did not fire.
    assert_eq!(
        count_int_binop(function, strider_ir::IntBinaryOp::And),
        0,
        "KnownBits should have folded the (… & 1) mask away; an And node \
         remains, so the bit-lattice fold did not happen"
    );
}
