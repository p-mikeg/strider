//! Test for the one Linux calling-convention preset that diverges from a
//! userland ABI: `x86_linux_kernel` (`-mregparm=3`).
//!
//! Every other arch's kernel-internal CC is byte-identical to its userland
//! preset, so callers select the userland preset directly — there is no
//! kernel alias to test.  Syscall ABIs are not calling conventions: the
//! `syscall` / `int 0x80` / `svc` traps lift to `CallOther`, classified
//! through `call_other_abi`, so they have no preset here either.
//!
//! The unit tests in `calling_convention/tests.rs` already pin the
//! register *counts* for `x86_linux_kernel`; this integration test pins the
//! distinctive part — the exact `EAX, EDX, ECX` arg registers that set
//! regparm-3 apart from stack-only `x86_cdecl`.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_target::{CallingConvention, SleighArch};

/// Probe the arch's Sleigh against an empty memory reader to extract the
/// register table.  No real binary is needed — the register table is fixed
/// by the `.sla` spec.
fn regs_for(arch: SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");
    sleigh.regs().expect("Sleigh::regs")
}

#[test]
fn x86_linux_kernel_passes_first_three_args_in_eax_edx_ecx() {
    let regs = regs_for(SleighArch::x86());
    let names: Vec<String> = CallingConvention::x86_linux_kernel()
        .build(&regs)
        .expect("build")
        .arg_passing_regs
        .iter()
        .map(|vn| {
            regs.vn_to_name(*vn)
                .expect("every resolved arg vn must round-trip to a name")
                .to_string()
        })
        .collect();
    assert_eq!(names, vec!["EAX", "EDX", "ECX"]);
}
