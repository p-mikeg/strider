use rsleigh::{Vn, VnSpace};
use strider_target::{BuiltCallingConvention, CallingConvention, SleighArch};

fn vn(off: u64) -> Vn {
    Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: off,
        size: 8,
    }
}

#[test]
fn validate_rejects_sp_in_arg_passing_regs() {
    let sp = vn(0x40);
    let cc = BuiltCallingConvention {
        arg_passing_regs: vec![vn(0x10), sp, vn(0x20)],
        callee_saved_regs: vec![],
        ret_val_regs: vec![vn(0x18)],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    };
    let res = cc.validate();
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
fn validate_rejects_arg_overlapping_callee_saved() {
    let shared = vn(0x10);
    let cc = BuiltCallingConvention {
        arg_passing_regs: vec![shared],
        callee_saved_regs: vec![shared],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    };
    let res = cc.validate();
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("arg_passing_regs") && msg.contains("callee_saved_regs"),
        "error should name both lists; got {msg}",
    );
}

#[test]
fn validate_rejects_ret_int_overlapping_ret_float() {
    // Integer and float returns are physically different register files on
    // every supported arch, so the same varnode in both is a CC-author bug.
    // Argument-vs-return overlap stays legal: x86_64 SysV RDX is both.
    let shared = vn(0x28);
    let cc = BuiltCallingConvention {
        arg_passing_regs: vec![vn(0x10)],
        callee_saved_regs: vec![vn(0x20)],
        ret_val_regs: vec![shared],
        ret_val_regs_float: vec![shared],
        stack_vn: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    };
    let res = cc.validate();
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("ret_val_regs") && msg.contains("ret_val_regs_float"),
        "error should name both ret lists; got {msg}",
    );
}

#[test]
fn validate_accepts_clean_layout() {
    BuiltCallingConvention {
        arg_passing_regs: vec![vn(0x10), vn(0x18)],
        callee_saved_regs: vec![vn(0x20)],
        ret_val_regs: vec![vn(0x28)],
        ret_val_regs_float: vec![],
        stack_vn: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    }
    .validate()
    .expect("clean layout must validate");
}

#[test]
fn build_routes_through_validator_no_false_positives() {
    let regs = SleighArch::x86_64().probe_regs().expect("probe regs");
    CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("x86_64_systemv must build cleanly (build routes through validate)");
}

/// A wider container holding a narrower one is not a distinct register:
/// AArch64's `q8` occupies the same bytes as `d8` plus eight more, so `d8`
/// callee-saved alongside `q8` argument-passing is the same contradiction as
/// naming one varnode twice.  `preserves_all_bytes_of` is byte-accurate at run time and
/// the doc says "disjoint", so the validator has to be byte-accurate too.
#[test]
fn validate_rejects_container_overlap_not_only_exact_equality() {
    let d8 = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x100,
        size: 8,
    };
    let q8 = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x100,
        size: 16,
    };
    let cc = BuiltCallingConvention {
        arg_passing_regs_float: vec![q8],
        callee_saved_regs: vec![d8],
        stack_vn: vn(0x40),
        ..Default::default()
    };
    let res = cc.validate();
    assert!(
        res.is_err(),
        "q8 in an argument list overlaps callee-saved d8 and must be rejected"
    );
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("arg_passing_regs_float") && msg.contains("callee_saved_regs"),
        "error must name both lists; got {msg}",
    );
}

/// Overlap is per address space and per byte range: a REGISTER varnode and a
/// same-offset varnode in another space are different storage, and adjacent
/// registers must stay legal or every preset would fail.
#[test]
fn validate_accepts_adjacent_and_cross_space_ranges() {
    let arg = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x100,
        size: 8,
    };
    let adjacent = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x108,
        size: 8,
    };
    let other_space = Vn {
        addr_space: VnSpace::UNIQUE,
        addr_off: 0x100,
        size: 8,
    };
    BuiltCallingConvention {
        arg_passing_regs: vec![arg],
        callee_saved_regs: vec![adjacent, other_space],
        stack_vn: vn(0x40),
        ..Default::default()
    }
    .validate()
    .expect("adjacent and cross-space ranges do not overlap");
}

/// Every shipped preset builds on its own arch, and `build` routes through
/// `validate`, so a validator tightened past a real ABI fails here.
#[test]
fn every_shipped_preset_still_validates() {
    type PresetCase = (&'static str, fn() -> CallingConvention, fn() -> SleighArch);
    let cases: &[PresetCase] = &[
        (
            "x86_64_systemv",
            CallingConvention::x86_64_systemv,
            SleighArch::x86_64,
        ),
        ("x86_cdecl", CallingConvention::x86_cdecl, SleighArch::x86),
        (
            "x86_linux_kernel",
            CallingConvention::x86_linux_kernel,
            SleighArch::x86,
        ),
        (
            "aarch64_aapcs64",
            CallingConvention::aarch64_aapcs64,
            SleighArch::aarch64,
        ),
        ("arm_aapcs", CallingConvention::arm_aapcs, SleighArch::arm),
        (
            "arm_aapcs_soft",
            CallingConvention::arm_aapcs_soft,
            SleighArch::arm,
        ),
        (
            "mips_o32",
            CallingConvention::mips_o32,
            SleighArch::mipsbe32,
        ),
        (
            "mips_n64",
            CallingConvention::mips_n64,
            SleighArch::mipsbe64,
        ),
        (
            "powerpc_sysv32",
            CallingConvention::powerpc_sysv32,
            SleighArch::ppc32be,
        ),
        (
            "powerpc64_elf_v1",
            CallingConvention::powerpc64_elf_v1,
            SleighArch::ppc64be,
        ),
        (
            "powerpc64_elf_v2",
            CallingConvention::powerpc64_elf_v2,
            SleighArch::ppc64le,
        ),
    ];
    assert_eq!(cases.len(), 11, "every shipped preset is listed");
    for (name, cc, arch) in cases {
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        let sleigh = rsleigh::Sleigh::new(arch().sla_spec(), arch().pspec(), reader)
            .unwrap_or_else(|e| panic!("{name}: Sleigh::new: {e:?}"));
        let regs = sleigh
            .regs()
            .unwrap_or_else(|e| panic!("{name}: Sleigh::regs: {e:?}"));
        cc().build(&regs)
            .unwrap_or_else(|e| panic!("{name}: build (which validates) failed: {e:?}"));
    }
}
