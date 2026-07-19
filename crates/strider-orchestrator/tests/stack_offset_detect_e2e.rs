//! End-to-end validity test for the `StackOffsetDetect` optimizer pass.
//!
//! `StackOffsetDetect` is the only pass that populates
//! `Function::stack_offsets`, so a non-empty count after the full pipeline
//! proves it fired on lifted real code.
//!
//! Fixture: `stack.c::escape_via_ptr` (`int local = seed*3;
//! external_take_ptr(&local); return local;`). Taking `&local` and passing it
//! to an opaque (asm-volatile-barrier) external forces a real stack slot for
//! `local` on every arch, so the spill Store / reload Load is SP-relative.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;

per_arch_test!("stack", "escape_via_ptr", escape_has_stack_offset);

fn escape_has_stack_offset(function: &strider_ir::Function) {
    assert!(
        count_stack_offsets(function) >= 1,
        "StackOffsetDetect should annotate >= 1 SP-relative Store/Load on \
         escape_via_ptr (&local forces a stack slot); got {}",
        count_stack_offsets(function)
    );
}
