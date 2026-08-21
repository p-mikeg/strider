//! Float-return coverage for the x87 stack, and the invariants the CC
//! transforms and `validate` must hold.
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_target::{CallingConvention, SleighArch};

fn regs_for(arch: SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");
    sleigh.regs().expect("Sleigh::regs")
}

fn float_ret_names(cc: CallingConvention, arch: SleighArch) -> Vec<String> {
    let regs = regs_for(arch);
    cc.build(&regs)
        .expect("build")
        .ret_val_regs_float
        .iter()
        .map(|vn| {
            regs.vn_to_name(*vn)
                .expect("every resolved float-ret vn must round-trip to a name")
                .to_string()
        })
        .collect()
}

/// SysV AMD64 psABI 3.2.3: a COMPLEX_X87 return puts the real part in `%st0`
/// and the imaginary part in `%st1`. Without ST1 in the list nothing roots the
/// imaginary half's cone and DCE deletes it.
#[test]
fn x86_64_returns_complex_long_double_imaginary_half_in_st1() {
    let names = float_ret_names(CallingConvention::x86_64_systemv(), SleighArch::x86_64());
    assert!(
        names.iter().any(|n| n == "ST1"),
        "ST1 must be a float return register, got {names:?}"
    );
}

/// Same rule in the Intel386 psABI.
#[test]
fn x86_returns_complex_long_double_imaginary_half_in_st1() {
    let names = float_ret_names(CallingConvention::x86_cdecl(), SleighArch::x86());
    assert!(
        names.iter().any(|n| n == "ST1"),
        "ST1 must be a float return register, got {names:?}"
    );
}

/// `preserves_regs` clobbers memory, and the load-forwarding gate is
/// escape-based: dropping the argument registers hides a frame address handed
/// to the callee, so a spill wrongly forwards across the call.
#[test]
fn preserves_regs_keeps_the_argument_registers() {
    let regs = regs_for(SleighArch::x86_64());
    let base = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build");
    let preserved = CallingConvention::x86_64_systemv()
        .preserves_regs()
        .build(&regs)
        .expect("build");
    assert!(
        !base.arg_passing_regs.is_empty(),
        "baseline must pass arguments in registers"
    );
    assert_eq!(
        preserved.arg_passing_regs, base.arg_passing_regs,
        "preserves_regs must keep the argument registers: memory is still clobbered, \
         so frame-escape evidence has to survive"
    );
}

/// `preserves_all` preserves memory too, so every escape is covered.
#[test]
fn preserves_all_still_drops_the_argument_registers() {
    let regs = regs_for(SleighArch::x86_64());
    let cc = CallingConvention::x86_64_systemv()
        .preserves_all()
        .build(&regs)
        .expect("build");
    assert!(cc.arg_passing_regs.is_empty());
    assert!(cc.preserves_memory);
}

/// `is_clobbered` short-circuits on `preserves_all_registers`, so it silently
/// wins over a populated return list and the `Call` emits no ret-val output.
#[test]
fn validate_rejects_preserve_all_alongside_return_registers() {
    let regs = regs_for(SleighArch::x86_64());
    let mut cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build");
    assert!(!cc.ret_val_regs.is_empty(), "baseline returns in registers");
    cc.preserves_all_registers = true;
    let err = cc
        .validate()
        .expect_err("preserves_all_registers with a populated ret list is contradictory");
    let msg = err.to_string();
    assert!(
        msg.contains("preserves_all_registers"),
        "error must name the offending field, got {msg:?}"
    );
}
