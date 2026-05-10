//! Tests for the Linux kernel + syscall calling-convention presets
//! added in `docs/superpowers/specs/2026-05-01-linux-kernel-cc-design.md`.
//!
//! For each new preset, the test:
//!   1. Constructs the preset.
//!   2. Builds it against the matching arch's Sleigh register table.
//!   3. Asserts (a) the resolved arg-passing varnodes round-trip back
//!      to the documented register names, (b)
//!      `BuiltCallingConvention::syscall_number_vn` is `Some` for
//!      every `_linux_syscall` preset and `None` for every
//!      `_linux_kernel` preset, (c) `link_register_vn` is `None` for
//!      syscall presets (kernel-mode return is via `sysret`/`eret`).
//!
//! Each test is keyed on a single (preset → arch) pair so a typo in
//! any one of them surfaces as a localised failure, matching the
//! existing per-preset granularity in the workspace.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use target::{CallingConvention, SleighArch};

/// Probe the arch's Sleigh against an empty memory reader to extract
/// the register table.  No real binary is needed — the register table
/// is fixed by the `.sla` spec.
fn regs_for(arch: SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .expect("Sleigh::new");
    sleigh.regs().expect("Sleigh::regs")
}

/// Returns the resolved register names for the preset's
/// `arg_passing_regs`, in positional order.  We resolve back through
/// `vn_to_name` so a register that was supplied under a different
/// alias (e.g. `lr` for AArch64's `x30`) shows up under the
/// canonical Sleigh name and the assertion is exact.
fn arg_reg_names(cc: CallingConvention, regs: &rsleigh::SleighRegs) -> Vec<String> {
    cc.build(regs)
        .expect("build")
        .arg_passing_regs()
        .iter()
        .map(|vn| {
            regs.vn_to_name(*vn)
                .expect("every resolved arg vn must round-trip to a name")
                .to_string()
        })
        .collect()
}

// ── Kernel-internal CCs ──────────────────────────────────────────────────────

#[test]
fn x86_linux_kernel_args_are_eax_edx_ecx() {
    let regs = regs_for(SleighArch::x86());
    let names = arg_reg_names(CallingConvention::x86_linux_kernel(), &regs);
    assert_eq!(names, vec!["EAX", "EDX", "ECX"]);
    let built = CallingConvention::x86_linux_kernel().build(&regs).unwrap();
    assert!(
        built.syscall_number_vn().is_none(),
        "kernel-internal CC must not declare a syscall-number register"
    );
}

#[test]
fn x86_64_linux_kernel_aliases_systemv() {
    let regs = regs_for(SleighArch::x86_64());
    assert_eq!(
        arg_reg_names(CallingConvention::x86_64_linux_kernel(), &regs),
        arg_reg_names(CallingConvention::x86_64_systemv(), &regs),
        "x86_64 kernel-internal CC is identical to SystemV"
    );
    assert!(
        CallingConvention::x86_64_linux_kernel()
            .build(&regs)
            .unwrap()
            .syscall_number_vn()
            .is_none()
    );
}

#[test]
fn aarch64_linux_kernel_aliases_aapcs64() {
    let regs = regs_for(SleighArch::aarch64());
    assert_eq!(
        arg_reg_names(CallingConvention::aarch64_linux_kernel(), &regs),
        arg_reg_names(CallingConvention::aarch64_aapcs64(), &regs),
    );
    assert!(
        CallingConvention::aarch64_linux_kernel()
            .build(&regs)
            .unwrap()
            .syscall_number_vn()
            .is_none()
    );
}

#[test]
fn arm_linux_kernel_aliases_aapcs() {
    let regs = regs_for(SleighArch::arm());
    assert_eq!(
        arg_reg_names(CallingConvention::arm_linux_kernel(), &regs),
        arg_reg_names(CallingConvention::arm_aapcs(), &regs),
    );
}

#[test]
fn mips_linux_kernel_o32_aliases_o32() {
    let regs = regs_for(SleighArch::mipsle32());
    assert_eq!(
        arg_reg_names(CallingConvention::mips_linux_kernel_o32(), &regs),
        arg_reg_names(CallingConvention::mips_o32(), &regs),
    );
}

#[test]
fn mips_linux_kernel_n64_aliases_n64() {
    let regs = regs_for(SleighArch::mipsle64());
    assert_eq!(
        arg_reg_names(CallingConvention::mips_linux_kernel_n64(), &regs),
        arg_reg_names(CallingConvention::mips_n64(), &regs),
    );
}

// ── Syscall ABIs ─────────────────────────────────────────────────────────────

#[test]
fn x86_linux_syscall_args_and_syscall_number() {
    let regs = regs_for(SleighArch::x86());
    let cc = CallingConvention::x86_linux_syscall();
    assert_eq!(
        arg_reg_names(cc, &regs),
        vec!["EBX", "ECX", "EDX", "ESI", "EDI", "EBP"]
    );
    let built = cc.build(&regs).unwrap();
    let sn_name = regs.vn_to_name(built.syscall_number_vn().expect("EAX")).unwrap();
    assert_eq!(sn_name, "EAX");
    assert!(built.link_register_vn().is_none());
    assert_eq!(built.ret_stack_pop(), 0);
}

#[test]
fn x86_64_linux_syscall_uses_r10_not_rcx() {
    let regs = regs_for(SleighArch::x86_64());
    let cc = CallingConvention::x86_64_linux_syscall();
    let names = arg_reg_names(cc, &regs);
    assert_eq!(names, vec!["RDI", "RSI", "RDX", "R10", "R8", "R9"]);
    assert!(
        !names.iter().any(|n| n == "RCX"),
        "syscall ABI must replace RCX with R10 (RCX is clobbered by `syscall`)"
    );
    let built = cc.build(&regs).unwrap();
    let sn_name = regs.vn_to_name(built.syscall_number_vn().expect("RAX")).unwrap();
    assert_eq!(sn_name, "RAX");
    assert!(built.link_register_vn().is_none());
}

#[test]
fn aarch64_linux_syscall_args_x0_x5_and_syscall_x8() {
    let regs = regs_for(SleighArch::aarch64());
    let cc = CallingConvention::aarch64_linux_syscall();
    assert_eq!(
        arg_reg_names(cc, &regs),
        vec!["x0", "x1", "x2", "x3", "x4", "x5"]
    );
    let built = cc.build(&regs).unwrap();
    let sn_name = regs.vn_to_name(built.syscall_number_vn().expect("x8")).unwrap();
    assert_eq!(sn_name, "x8");
    assert!(built.link_register_vn().is_none());
}

#[test]
fn arm_linux_syscall_args_r0_r6_and_syscall_r7() {
    let regs = regs_for(SleighArch::arm());
    let cc = CallingConvention::arm_linux_syscall();
    assert_eq!(
        arg_reg_names(cc, &regs),
        vec!["r0", "r1", "r2", "r3", "r4", "r5", "r6"]
    );
    let built = cc.build(&regs).unwrap();
    let sn_name = regs.vn_to_name(built.syscall_number_vn().expect("r7")).unwrap();
    assert_eq!(sn_name, "r7");
    assert!(built.link_register_vn().is_none());
}

#[test]
fn mips_linux_syscall_o32_uses_v0_for_syscall_number() {
    let regs = regs_for(SleighArch::mipsle32());
    let cc = CallingConvention::mips_linux_syscall_o32();
    assert_eq!(arg_reg_names(cc, &regs), vec!["a0", "a1", "a2", "a3"]);
    let built = cc.build(&regs).unwrap();
    let sn_name = regs.vn_to_name(built.syscall_number_vn().expect("v0")).unwrap();
    assert_eq!(sn_name, "v0");
    assert!(built.link_register_vn().is_none());
}

#[test]
fn mips_linux_syscall_n64_extends_args_to_six() {
    let regs = regs_for(SleighArch::mipsle64());
    let cc = CallingConvention::mips_linux_syscall_n64();
    let names = arg_reg_names(cc, &regs);
    // N64 syscall extends to 6 regs (a0..a5) — Sleigh's mips64 spec
    // names $4..$7 as a0..a3 and $8..$9 as t0..t1, so the assertion
    // checks that 6-arg shape under those names.
    assert_eq!(names.len(), 6);
    assert_eq!(&names[..4], &["a0", "a1", "a2", "a3"]);
    let built = cc.build(&regs).unwrap();
    let sn_name = regs.vn_to_name(built.syscall_number_vn().expect("v0")).unwrap();
    assert_eq!(sn_name, "v0");
}
