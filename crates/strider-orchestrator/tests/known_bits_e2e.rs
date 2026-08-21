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
    use strider_ir::{IRViewer, IRWalker};
    assert!(
        has_constant(function, 1),
        "KnownBits should fold And(Or(x,1), 1) to the constant 1"
    );
    // Count only the fixture's mask, `And(Or(x,1), mask)`, identified by its
    // Or operand. The ARM `bx lr` epilogue models `setISAMode` reading a
    // separate `(lr & 1)` bit extract (no Or operand), which must not count.
    let fixture_ands = function
        .walk()
        .filter(|&n| {
            matches!(
                function.node_kind(n),
                strider_ir::node::NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And)
            ) && function.node_inputs(n).iter().any(|v| {
                matches!(
                    function.node_kind(function.producer(v)),
                    strider_ir::node::NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Or)
                )
            })
        })
        .count();
    assert_eq!(
        fixture_ands, 0,
        "KnownBits should have folded the fixture's And(Or(x,1), mask) away; an \
         And over an Or remains, so the bit-lattice fold did not happen"
    );
}
