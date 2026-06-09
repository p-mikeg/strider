//! End-to-end validity test for the `StackOffsetDetect` optimizer pass.
//!
//! `StackOffsetDetect` annotates SP-relative `Store` / `Load` nodes with their
//! byte offset in `Function::stack_offsets`.  This is the ONLY pass that
//! populates that side-table, so a non-empty count after the full pipeline
//! proves the pass fired on lifted real code — the assertion would fail if
//! `StackOffsetDetect` were removed from `default_pipeline()`.
//!
//! Fixture: `stack.c::escape_via_ptr`, which does `int local = seed*3;
//! external_take_ptr(&local); return local;`.  Taking `&local` and passing it
//! to an opaque (asm-volatile-barrier) external forces a real stack slot for
//! `local` on every arch — so the spill `Store`/reload `Load` is SP-relative
//! and `StackOffsetDetect` annotates it.

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
    // `&local` forces a stack spill/reload; StackOffsetDetect annotates the
    // SP-relative Store/Load with its offset.  >= 1 annotated node proves the
    // pass ran (ConstantFold/KnownBits/etc. never touch this side-table).
    assert!(
        count_stack_offsets(function) >= 1,
        "StackOffsetDetect should annotate >= 1 SP-relative Store/Load on \
         escape_via_ptr (&local forces a stack slot); got {}",
        count_stack_offsets(function)
    );
}
