//! `BuiltCallingConvention::try_new` validates the
//! cross-list disjointness invariants documented on the function.
//!
//! Pin that listing the SP varnode in `arg_passing_regs` produces
//! a clear validation `Err` rather than a downstream miscompile.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rsleigh::{Vn, VnSpace};
use target::{BuiltCallingConvention, CallingConvention, SleighArch};

fn vn(off: u64) -> Vn {
    Vn { addr_space: VnSpace::REGISTER, addr_off: off, size: 8 }
}

#[test]
fn try_new_rejects_sp_in_arg_passing_regs() {
    let sp = vn(0x40);
    let res = BuiltCallingConvention::try_new(
        vec![vn(0x10), sp, vn(0x20)],
        vec![],
        vec![vn(0x18)],
        vec![],
        sp,
        vec![],
        0,
        None,
        None,
        false,
    );
    assert!(res.is_err(), "SP listed in arg_passing_regs must be rejected");
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("stack_ptr_vn") && msg.contains("arg_passing_regs"),
        "error must name both stack_ptr_vn and the offending list, got: {msg}",
    );
}

#[test]
fn try_new_rejects_arg_overlapping_callee_saved() {
    let shared = vn(0x10);
    let res = BuiltCallingConvention::try_new(
        vec![shared],
        vec![shared],
        vec![],
        vec![],
        vn(0x40),
        vec![],
        0,
        None,
        None,
        false,
    );
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("arg_passing_regs") && msg.contains("callee_saved_regs"),
        "error should name both lists; got {msg}",
    );
}

#[test]
fn try_new_accepts_clean_layout() {
    BuiltCallingConvention::try_new(
        vec![vn(0x10), vn(0x18)],
        vec![vn(0x20)],
        vec![vn(0x28)],
        vec![],
        vn(0x40),
        vec![],
        0,
        None,
        None,
        false,
    )
    .expect("clean layout must validate");
}

#[test]
fn build_routes_through_validator_no_false_positives() {
    let regs = SleighArch::x86_64()
        .probe_regs()
        .expect("probe regs");
    CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("x86_64_systemv must build cleanly (build routes through try_new)");
}
