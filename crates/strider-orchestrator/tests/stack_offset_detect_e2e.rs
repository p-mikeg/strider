//! Fixture: `stack.c::escape_via_ptr` (`int local = seed*3;
//! external_take_ptr(&local); return local;`). Taking `&local` and passing it
//! to an opaque (asm-volatile-barrier) external forces a real stack slot for
//! `local` on every arch, so the spill Store / reload Load is SP-relative.

mod common;
use common::*;

per_arch_test!("stack", "escape_via_ptr", escape_has_stack_offset);

fn escape_has_stack_offset(function: &strider_ir::Function) {
    assert!(
        count_memory_offsets(function) >= 1,
        "StackOffsetDetect should annotate >= 1 SP-relative Store/Load on \
         escape_via_ptr (&local forces a stack slot); got {}",
        count_memory_offsets(function)
    );
}
