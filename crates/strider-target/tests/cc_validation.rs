//! `BuiltCallingConvention::try_new` validates the
//! cross-list disjointness invariants documented on the function.
//!
//! Pin that listing the SP varnode in `arg_passing_regs` produces
//! a clear validation `Err` rather than a downstream miscompile.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rsleigh::{Vn, VnSpace};
use strider_target::{
    BuiltCallingConvention, BuiltCallingConventionParts, CallingConvention, SleighArch,
};

fn vn(off: u64) -> Vn {
    Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: off,
        size: 8,
    }
}

#[test]
fn try_new_rejects_sp_in_arg_passing_regs() {
    let sp = vn(0x40);
    let res = BuiltCallingConvention::try_new(BuiltCallingConventionParts {
        arg_passing_regs: vec![vn(0x10), sp, vn(0x20)],
        callee_saved_regs: vec![],
        ret_val_regs: vec![vn(0x18)],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    });
    assert!(
        res.is_err(),
        "SP listed in arg_passing_regs must be rejected"
    );
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("stack_vn") && msg.contains("arg_passing_regs"),
        "error must name both stack_vn and the offending list, got: {msg}",
    );
}

#[test]
fn try_new_rejects_arg_overlapping_callee_saved() {
    let shared = vn(0x10);
    let res = BuiltCallingConvention::try_new(BuiltCallingConventionParts {
        arg_passing_regs: vec![shared],
        callee_saved_regs: vec![shared],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    });
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("arg_passing_regs") && msg.contains("callee_saved_regs"),
        "error should name both lists; got {msg}",
    );
}

#[test]
fn try_new_rejects_ret_int_overlapping_ret_float() {
    // An integer return register and a float return register are physically
    // different register files on every supported arch; the same varnode in
    // both lists is a CC-author bug and must be rejected.  (arg ∩ ret is left
    // unchecked on purpose — x86_64 SysV RDX is legitimately both.)
    let shared = vn(0x28);
    let res = BuiltCallingConvention::try_new(BuiltCallingConventionParts {
        arg_passing_regs: vec![vn(0x10)],
        callee_saved_regs: vec![vn(0x20)],
        ret_val_regs: vec![shared],
        ret_val_regs_float: vec![shared],
        stack_vn: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    });
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("ret_val_regs") && msg.contains("ret_val_regs_float"),
        "error should name both ret lists; got {msg}",
    );
}

#[test]
fn try_new_accepts_clean_layout() {
    BuiltCallingConvention::try_new(BuiltCallingConventionParts {
        arg_passing_regs: vec![vn(0x10), vn(0x18)],
        callee_saved_regs: vec![vn(0x20)],
        ret_val_regs: vec![vn(0x28)],
        ret_val_regs_float: vec![],
        stack_vn: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    })
    .expect("clean layout must validate");
}

#[test]
fn build_routes_through_validator_no_false_positives() {
    let regs = SleighArch::x86_64().probe_regs().expect("probe regs");
    CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("x86_64_systemv must build cleanly (build routes through try_new)");
}
