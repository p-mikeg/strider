//! `BuiltCallingConvention::try_from_parts` validates the
//! cross-list disjointness invariants documented on the function.
//!
//! Pin that listing the SP varnode in `arg_passing_regs` produces
//! a clear validation `Err` rather than a downstream miscompile.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rsleigh::{Vn, VnSpace};
use target::{BuiltCallingConvention, BuiltCallingConventionParts, CallingConvention, SleighArch};

fn vn(off: u64) -> Vn {
    Vn { addr_space: VnSpace::REGISTER, addr_off: off, size: 8 }
}

fn parts_with_sp_in_arg_regs() -> BuiltCallingConventionParts {
    let sp = vn(0x40);
    BuiltCallingConventionParts {
        arg_passing_regs: vec![vn(0x10), sp, vn(0x20)],
        callee_saved_regs: vec![],
        ret_val_regs: vec![vn(0x18)],
        ret_val_regs_float: vec![],
        stack_ptr_vn: sp,
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        syscall_number_vn: None,
        no_memory_clobber: false,
    }
}

#[test]
fn try_from_parts_rejects_sp_in_arg_passing_regs() {
    let res = BuiltCallingConvention::try_from_parts(parts_with_sp_in_arg_regs());
    assert!(res.is_err(), "SP listed in arg_passing_regs must be rejected");
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("stack_ptr_vn") && msg.contains("arg_passing_regs"),
        "error must name both stack_ptr_vn and the offending list, got: {msg}",
    );
}

#[test]
fn try_from_parts_rejects_arg_overlapping_callee_saved() {
    let shared = vn(0x10);
    let parts = BuiltCallingConventionParts {
        arg_passing_regs: vec![shared],
        callee_saved_regs: vec![shared],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_ptr_vn: vn(0x40),
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        syscall_number_vn: None,
        no_memory_clobber: false,
    };
    let res = BuiltCallingConvention::try_from_parts(parts);
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("arg_passing_regs") && msg.contains("callee_saved_regs"),
        "error should name both lists; got {msg}",
    );
}

#[test]
fn try_from_parts_accepts_clean_layout() {
    let parts = BuiltCallingConventionParts {
        arg_passing_regs: vec![vn(0x10), vn(0x18)],
        callee_saved_regs: vec![vn(0x20)],
        ret_val_regs: vec![vn(0x28)],
        ret_val_regs_float: vec![],
        stack_ptr_vn: vn(0x40),
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        syscall_number_vn: None,
        no_memory_clobber: false,
    };
    BuiltCallingConvention::try_from_parts(parts).expect("clean layout must validate");
}

#[test]
fn build_routes_through_validator_no_false_positives() {
    let regs = SleighArch::x86_64()
        .probe_regs()
        .expect("probe regs");
    CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("x86_64_systemv must build cleanly (build routes through try_from_parts)");
}
