//! The AltiVec non-volatile vector registers (v20-v31, spelled `vs52`..`vs63`
//! in the sla) are callee-saved on every PowerPC preset.

use strider_target::{CallingConvention, SleighArch};

fn regs_for(arch: SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");
    sleigh.regs().expect("Sleigh::regs")
}

/// `v20`..`v31` overlay the top VSX registers, so the sla names them
/// `vs52`..`vs63` at `0x4340` + 16 each.
#[test]
fn ppc_presets_preserve_the_non_volatile_vector_registers() {
    for (arch, cc) in [
        (SleighArch::ppc32be(), CallingConvention::powerpc_sysv32()),
        (SleighArch::ppc32le(), CallingConvention::powerpc_sysv32()),
        (SleighArch::ppc64be(), CallingConvention::powerpc64_elf_v1()),
        (SleighArch::ppc64le(), CallingConvention::powerpc64_elf_v2()),
    ] {
        let regs = regs_for(arch);
        let built = cc.build(&regs).expect("build");
        for (i, name) in (52..=63).map(|n| (n, format!("vs{n}"))) {
            let vn = regs.name_to_vn(&name).expect("sla must define the name");
            assert_eq!(
                (vn.addr_off, vn.size),
                (0x4340 + (i - 52) * 16, 16),
                "{name} moved"
            );
            assert!(
                built.callee_saved_regs.contains(&vn),
                "{name} missing from callee_saved_regs"
            );
        }
    }
}
