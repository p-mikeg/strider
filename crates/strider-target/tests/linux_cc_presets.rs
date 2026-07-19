//! `x86_linux_kernel` (`-mregparm=3`) is the one Linux calling convention
//! that diverges from a userland ABI.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_target::{CallingConvention, SleighArch};

/// No real binary needed: the register table is fixed by the `.sla` spec.
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
