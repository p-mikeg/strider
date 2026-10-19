//! A convention names its registers, and one arch's names resolve on another:
//! x86 `ESP` is a 4-byte view of x86-64 `RSP`, and ppc32 `r1` is the whole
//! 8-byte `r1` on ppc64. The lift refuses a convention whose stack geometry is
//! not its arch's.

use strider_target::{
    ArchPreset, BuiltCallingConvention, CallingConvention as Cc, SleighArch as Sa,
};

/// Lifts `code` at 0x1000 under `cc`, with `over` as the per-address override
/// for its call target, both built against the lifter's own register table.
fn lift_at(arch: Sa, code: &[u8], cc: Cc, over: Option<(u64, Cc)>) -> anyhow::Result<()> {
    lift_built_at(arch, code, |regs| cc.build(regs), over)
}

/// [`lift_at`], with the default convention built by `build`.
fn lift_built_at(
    arch: Sa,
    code: &[u8],
    build: impl FnOnce(&rsleigh::SleighRegs) -> anyhow::Result<BuiltCallingConvention>,
    over: Option<(u64, Cc)>,
) -> anyhow::Result<()> {
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(code.to_vec(), 0x1000),
    )?;
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh)?;
    let built = build(lifter.sleigh_regs())?;
    let mut per_address_ccs = rustc_hash::FxHashMap::default();
    if let Some((addr, o)) = over {
        per_address_ccs.insert(addr, o.build(lifter.sleigh_regs())?);
    }
    let cfg = lifter.build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())?;
    lifter.build_ir_with(
        &cfg,
        built,
        &strider_cfg::CfgOptions::default(),
        &per_address_ccs,
    )?;
    Ok(())
}

const X86_RET: &[u8] = &[0xc3];
const ARM_BX_LR: &[u8] = &[0x1e, 0xff, 0x2f, 0xe1];
const ARM_BE_BX_LR: &[u8] = &[0xe1, 0x2f, 0xff, 0x1e];
const AARCH64_RET: &[u8] = &[0xc0, 0x03, 0x5f, 0xd6];
const MIPS_BE_JR_RA: &[u8] = &[0x03, 0xe0, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00];
const MIPS_LE_JR_RA: &[u8] = &[0x08, 0x00, 0xe0, 0x03, 0x00, 0x00, 0x00, 0x00];
const PPC_BE_BLR: &[u8] = &[0x4e, 0x80, 0x00, 0x20];
const PPC_LE_BLR: &[u8] = &[0x20, 0x00, 0x80, 0x4e];

type Ccs = &'static [(&'static str, fn() -> Cc)];

const X86_CCS: Ccs = &[
    ("x86_cdecl", Cc::x86_cdecl),
    ("x86_linux_kernel", Cc::x86_linux_kernel),
];
const X86_64_CCS: Ccs = &[("x86_64_systemv", Cc::x86_64_systemv)];
const ARM_CCS: Ccs = &[
    ("arm_aapcs", Cc::arm_aapcs),
    ("arm_aapcs_soft", Cc::arm_aapcs_soft),
];
const AARCH64_CCS: Ccs = &[("aarch64_aapcs64", Cc::aarch64_aapcs64)];
const O32_CCS: Ccs = &[("mips_o32", Cc::mips_o32)];
const N64_CCS: Ccs = &[("mips_n64", Cc::mips_n64)];
const PPC32_CCS: Ccs = &[
    ("powerpc_sysv32", Cc::powerpc_sysv32),
    (
        "powerpc_sysv32_linux_kernel",
        Cc::powerpc_sysv32_linux_kernel,
    ),
];
const PPC64_CCS: Ccs = &[
    ("powerpc64_elf_v1", Cc::powerpc64_elf_v1),
    (
        "powerpc64_elf_v1_linux_kernel",
        Cc::powerpc64_elf_v1_linux_kernel,
    ),
    ("powerpc64_elf_v2", Cc::powerpc64_elf_v2),
    (
        "powerpc64_elf_v2_linux_kernel",
        Cc::powerpc64_elf_v2_linux_kernel,
    ),
];

/// Each arch preset, a bare return on it, and the conventions written for it.
/// An even entry decodes ARM state on every ARM preset, Thumb included.
const PRESETS: &[(ArchPreset, &[u8], Ccs)] = &[
    (ArchPreset::X86, X86_RET, X86_CCS),
    (ArchPreset::X86_64, X86_RET, X86_64_CCS),
    (ArchPreset::Arm, ARM_BX_LR, ARM_CCS),
    (ArchPreset::ArmBe, ARM_BE_BX_LR, ARM_CCS),
    (ArchPreset::ArmBeKernel, ARM_BX_LR, ARM_CCS),
    (ArchPreset::ArmThumb, ARM_BX_LR, ARM_CCS),
    (ArchPreset::Aarch64, AARCH64_RET, AARCH64_CCS),
    (ArchPreset::Aarch64Be, AARCH64_RET, AARCH64_CCS),
    (ArchPreset::MipsBe32, MIPS_BE_JR_RA, O32_CCS),
    (ArchPreset::MipsLe32, MIPS_LE_JR_RA, O32_CCS),
    (ArchPreset::MipsBe64, MIPS_BE_JR_RA, N64_CCS),
    (ArchPreset::MipsLe64, MIPS_LE_JR_RA, N64_CCS),
    (ArchPreset::Ppc32Be, PPC_BE_BLR, PPC32_CCS),
    (ArchPreset::Ppc32Le, PPC_LE_BLR, PPC32_CCS),
    (ArchPreset::Ppc64Be, PPC_BE_BLR, PPC64_CCS),
    (ArchPreset::Ppc64Le, PPC_LE_BLR, PPC64_CCS),
];

/// Every arch preset lifts under every shipped convention written for it.
#[test]
fn every_arch_preset_lifts_under_its_conventions() {
    for preset in ArchPreset::ALL {
        assert!(
            PRESETS.iter().any(|row| row.0 == *preset),
            "{preset:?} has no row"
        );
    }
    let mut names = std::collections::BTreeSet::new();
    for &(preset, code, ccs) in PRESETS {
        for &(name, cc) in ccs {
            lift_at(preset.arch(), code, cc(), None)
                .unwrap_or_else(|e| panic!("{preset:?} {name}: {e:#}"));
            names.insert(name);
        }
    }
    assert_eq!(names.len(), 14, "one name per shipped convention");
}

#[test]
fn x86_cdecl_is_refused_on_an_x86_64_lifter() {
    let err = lift_at(Sa::x86_64(), X86_RET, Cc::x86_cdecl(), None)
        .expect_err("a 4-byte ESP must not stand in for x86-64's 8-byte stack pointer");
    assert!(format!("{err:#}").contains("stack pointer"), "got: {err:#}");
}

#[test]
fn powerpc_sysv32_is_refused_on_a_ppc64_lifter() {
    let err = lift_at(Sa::ppc64be(), PPC_BE_BLR, Cc::powerpc_sysv32(), None)
        .expect_err("4-byte stack slots must not be read off ppc64's 8-byte r1");
    assert!(
        format!("{err:#}").contains("stack-arg increment"),
        "got: {err:#}"
    );
}

#[test]
fn a_stack_push_convention_that_pops_no_return_address_is_refused() {
    let pop_nothing = |regs: &rsleigh::SleighRegs| {
        Ok(BuiltCallingConvention {
            ret_stack_pop: 0,
            ..Cc::x86_64_systemv().build(regs)?
        })
    };
    let err = lift_built_at(Sa::x86_64(), X86_RET, pop_nothing, None)
        .expect_err("with no link register, `ret` pops the 8-byte return address");
    assert!(format!("{err:#}").contains("ret_stack_pop"), "got: {err:#}");
}

/// `call g ; ret ; g: ret`: the override names `g`.
#[test]
fn a_per_address_convention_for_another_arch_is_refused() {
    let code = [0xe8, 0x01, 0x00, 0x00, 0x00, 0xc3, 0xc3];
    lift_at(Sa::x86_64(), &code, Cc::x86_64_systemv(), None).expect("no override lifts");
    let err = lift_at(
        Sa::x86_64(),
        &code,
        Cc::x86_64_systemv(),
        Some((0x1006, Cc::x86_cdecl())),
    )
    .expect_err("an override with a 4-byte ESP must not reach an x86-64 call");
    assert!(format!("{err:#}").contains("stack pointer"), "got: {err:#}");
}

/// The default convention is validated at the lift: a struct literal reaches
/// it otherwise unchecked, and an argument register that is also callee-saved
/// would lift a value the callee both receives and preserves.
#[test]
fn a_default_convention_that_fails_validate_is_refused() {
    let arg_also_saved = |regs: &rsleigh::SleighRegs| -> anyhow::Result<BuiltCallingConvention> {
        let mut cc = Cc::x86_64_systemv().build(regs)?;
        cc.callee_saved_regs.push(cc.arg_passing_regs[0]);
        Ok(cc)
    };
    let err = lift_built_at(Sa::x86_64(), X86_RET, arg_also_saved, None)
        .expect_err("a register both passed and preserved must be refused");
    let err = format!("{err:#}");
    assert!(err.contains("default convention: "), "got: {err}");
    assert!(err.contains("callee_saved_regs"), "got: {err}");
}
