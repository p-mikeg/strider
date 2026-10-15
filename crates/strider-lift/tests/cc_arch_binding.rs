//! A convention names its registers, and one arch's names resolve on another:
//! x86 `ESP` is a 4-byte view of x86-64 `RSP`, and ppc32 `r1` is the whole
//! 8-byte `r1` on ppc64. The lift refuses a convention whose stack geometry is
//! not its arch's.

use strider_lift::LiftOptions;
use strider_target::{BuiltCallingConvention, CallingConvention as Cc, SleighArch as Sa};

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
    let opts = LiftOptions {
        per_address_ccs: &per_address_ccs,
        ..LiftOptions::default()
    };
    let cfg = lifter.build_cfg(0x1000u64.into(), &opts.cfg, opts.per_address_ccs)?;
    lifter.build_ir_with(&cfg, built, &opts)?;
    Ok(())
}

const X86_RET: &[u8] = &[0xc3];
const ARM_BX_LR: &[u8] = &[0x1e, 0xff, 0x2f, 0xe1];
const AARCH64_RET: &[u8] = &[0xc0, 0x03, 0x5f, 0xd6];
const MIPS_BE_JR_RA: &[u8] = &[0x03, 0xe0, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00];
const PPC_BE_BLR: &[u8] = &[0x4e, 0x80, 0x00, 0x20];
const PPC_LE_BLR: &[u8] = &[0x20, 0x00, 0x80, 0x4e];

/// A preset's name, the preset, its arch, and a bare return on that arch.
type PresetRow = (&'static str, fn() -> Cc, fn() -> Sa, &'static [u8]);

/// Every shipped preset on the arch it was written for.
#[test]
fn every_preset_lifts_on_its_own_arch() {
    let table: &[PresetRow] = &[
        ("x86_64_systemv", Cc::x86_64_systemv, Sa::x86_64, X86_RET),
        ("x86_cdecl", Cc::x86_cdecl, Sa::x86, X86_RET),
        ("x86_linux_kernel", Cc::x86_linux_kernel, Sa::x86, X86_RET),
        ("arm_aapcs", Cc::arm_aapcs, Sa::arm, ARM_BX_LR),
        ("arm_aapcs_soft", Cc::arm_aapcs_soft, Sa::arm, ARM_BX_LR),
        (
            "aarch64_aapcs64",
            Cc::aarch64_aapcs64,
            Sa::aarch64,
            AARCH64_RET,
        ),
        ("mips_o32", Cc::mips_o32, Sa::mipsbe32, MIPS_BE_JR_RA),
        ("mips_n64", Cc::mips_n64, Sa::mipsbe64, MIPS_BE_JR_RA),
        (
            "powerpc_sysv32",
            Cc::powerpc_sysv32,
            Sa::ppc32be,
            PPC_BE_BLR,
        ),
        (
            "powerpc_sysv32_linux_kernel",
            Cc::powerpc_sysv32_linux_kernel,
            Sa::ppc32be,
            PPC_BE_BLR,
        ),
        (
            "powerpc64_elf_v1",
            Cc::powerpc64_elf_v1,
            Sa::ppc64be,
            PPC_BE_BLR,
        ),
        (
            "powerpc64_elf_v1_linux_kernel",
            Cc::powerpc64_elf_v1_linux_kernel,
            Sa::ppc64be,
            PPC_BE_BLR,
        ),
        (
            "powerpc64_elf_v2",
            Cc::powerpc64_elf_v2,
            Sa::ppc64le,
            PPC_LE_BLR,
        ),
        (
            "powerpc64_elf_v2_linux_kernel",
            Cc::powerpc64_elf_v2_linux_kernel,
            Sa::ppc64le,
            PPC_LE_BLR,
        ),
    ];
    assert_eq!(table.len(), 14, "one row per shipped convention");
    for &(name, cc, arch, code) in table {
        lift_at(arch(), code, cc(), None).unwrap_or_else(|e| panic!("{name}: {e:#}"));
    }
}

#[test]
fn x86_cdecl_is_refused_on_an_x86_64_lifter() {
    let err = lift_at(Sa::x86_64(), X86_RET, Cc::x86_cdecl(), None)
        .expect_err("a 4-byte ESP must not stand in for x86-64's 8-byte stack pointer");
    assert!(format!("{err:#}").contains("stack_vn"), "got: {err:#}");
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
    assert!(format!("{err:#}").contains("stack_vn"), "got: {err:#}");
}
