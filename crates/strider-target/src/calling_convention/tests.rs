use super::*;

fn regs_for(arch: crate::arch::SleighArch) -> rsleigh::SleighRegs {
    arch.probe_regs().unwrap()
}

/// Float / vector register lists shared by the rows below, each cited where
/// the preset names it.
const NONE: &[&str] = &[];
const XMM0_7: &[&str] = &[
    "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5", "XMM6", "XMM7",
];
/// SysV AMD64 psABI 3.2.3: SSE class in XMM0/XMM1, X87 class in ST0, and the
/// COMPLEX_X87 imaginary half in ST1.
const X64_RET_F: &[&str] = &["XMM0", "XMM1", "ST0", "ST1"];
/// Intel386 psABI: every float argument is stack-passed (the SSE argument
/// class is a documented omission); X87 / COMPLEX_X87 return in ST0/ST1 and
/// the SSE class in XMM0.
const X86_RET_F: &[&str] = &["ST0", "ST1", "XMM0"];
/// AAPCS64 6.4.1 / 6.4.2: v0-v7 in, a 4-member HFA back in v0-v3.
const Q0_7: &[&str] = &["q0", "q1", "q2", "q3", "q4", "q5", "q6", "q7"];
const Q0_3: &[&str] = &["q0", "q1", "q2", "q3"];
/// AAPCS-VFP (IHI 0042 6.1.2.1): the double carriers d0-d7, a 4-member HFA
/// back in d0-d3.
const D0_7: &[&str] = &["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"];
const D0_3: &[&str] = &["d0", "d1", "d2", "d3"];
/// AAPCS 5.1.2.1 / AAPCS64 6.1.2: d8-d15, AAPCS64 preserving the low 64 bits
/// only, so these name the `d` views and never `q8`-`q15`.
const D8_D15: &[&str] = &["d8", "d9", "d10", "d11", "d12", "d13", "d14", "d15"];
/// MIPS o32 passes two floats in $f12 / $f14 and returns a `double` in the
/// 8-byte `f0_1` / `f2_3` pair registers the `FREGSIZE 4` sla declares.
const O32_ARG_F: &[&str] = &["f12", "f14"];
const O32_RET_F: &[&str] = &["f0_1", "f2_3"];
const F20_F31: &[&str] = &[
    "f20", "f21", "f22", "f23", "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31",
];
/// MIPS n64 widens the argument bank to $f12-$f19, narrows the preserved set
/// to $f24-$f31, and has 8-byte FPRs with no pair registers.
const N64_ARG_F: &[&str] = &["f12", "f13", "f14", "f15", "f16", "f17", "f18", "f19"];
const N64_RET_F: &[&str] = &["f0", "f2"];
const F24_F31: &[&str] = &["f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31"];
/// SysV PPC32 3-16 / ELFv1 3.2.2 / ELFv2 2.2.1: f14-f31 non-volatile.
const PPC_F14_F31: &[&str] = &[
    "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25", "f26",
    "f27", "f28", "f29", "f30", "f31",
];
/// SysV PPC32 passes eight floats, both PPC64 ABIs thirteen, then all three
/// pass vectors in v2-v13 (`vs34`-`vs45`).
const PPC32_ARG_F: &[&str] = &[
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "vs34", "vs35", "vs36", "vs37", "vs38", "vs39",
    "vs40", "vs41", "vs42", "vs43", "vs44", "vs45",
];
const PPC64_ARG_F: &[&str] = &[
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13", "vs34",
    "vs35", "vs36", "vs37", "vs38", "vs39", "vs40", "vs41", "vs42", "vs43", "vs44", "vs45",
];
/// `long double` is IBM double-double in the f1:f2 pair; a vector, and an
/// IEEE `__float128`, come back in v2.
const PPC_RET_F: &[&str] = &["f1", "f2", "vs34"];
/// ELFv2 2.2.3.3 returns a homogeneous float aggregate in f1-f8 and a
/// homogeneous vector aggregate in v2-v9.
const PPC_ELFV2_RET_F: &[&str] = &[
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "vs34", "vs35", "vs36", "vs37", "vs38", "vs39",
    "vs40", "vs41",
];
/// The same four windows with the AltiVec half dropped: a PowerPC Linux
/// kernel is built `-mno-altivec -mno-vsx`.
const PPC32_ARG_F_KERNEL: &[&str] = &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"];
const PPC64_ARG_F_KERNEL: &[&str] = &[
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13",
];
const PPC_RET_F_KERNEL: &[&str] = &["f1", "f2"];
const PPC_ELFV2_RET_F_KERNEL: &[&str] = &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"];

/// One supported convention plus everything `build()` should produce for it.
/// Adding a convention means adding one row; every invariant test picks it up.
struct Case {
    name: &'static str,
    cc: fn() -> CallingConvention,
    arch: fn() -> crate::arch::SleighArch,
    arg_count: usize,
    /// General-purpose only; the float file is `saved_float`.
    /// `callee_saved_regs` is built GPRs first, floats second.
    callee_saved_count: usize,
    /// The three float / vector lists by name, per psABI.  Counts alone would
    /// not say that x86-64 returns `long double` in ST0 or that AArch64
    /// preserves the `d` view of v8-v15 rather than the `q` one.
    args_float: &'static [&'static str],
    saved_float: &'static [&'static str],
    ret_float: &'static [&'static str],
    /// Neither GPR nor float: PowerPC's non-volatile condition fields, which
    /// are 1 byte each and sit after the floats.
    callee_saved_cond_count: usize,
    /// PowerPC's non-volatile vector registers, 16 bytes each, last in the
    /// list.
    callee_saved_vec_count: usize,
    ret_count: usize,
    reg_size_bytes: u32,
    stack_ptr_name: &'static str,
    stack_args: Option<StackArgs>,
    ret_stack_pop: i64,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "x86-64 SysV",
            cc: CallingConvention::x86_64_systemv,
            arch: crate::arch::SleighArch::x86_64,
            arg_count: 6,
            callee_saved_count: 6,
            args_float: XMM0_7,
            saved_float: NONE,
            ret_float: X64_RET_F,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "RSP",
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 8,
            }),
            ret_stack_pop: 8,
        },
        Case {
            name: "x86 cdecl",
            cc: CallingConvention::x86_cdecl,
            arch: crate::arch::SleighArch::x86,
            arg_count: 0,
            callee_saved_count: 4,
            args_float: NONE,
            saved_float: NONE,
            ret_float: X86_RET_F,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "ESP",
            stack_args: Some(StackArgs {
                base_offset: 4,
                increment: 4,
            }),
            ret_stack_pop: 4,
        },
        Case {
            name: "ARM AAPCS",
            cc: CallingConvention::arm_aapcs,
            arch: crate::arch::SleighArch::arm,
            arg_count: 4,
            callee_saved_count: 9,
            args_float: D0_7,
            saved_float: D8_D15,
            ret_float: D0_3,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "AArch64 AAPCS64",
            cc: CallingConvention::aarch64_aapcs64,
            arch: crate::arch::SleighArch::aarch64,
            arg_count: 8,
            callee_saved_count: 12,
            args_float: Q0_7,
            saved_float: D8_D15,
            ret_float: Q0_3,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "MIPS O32 (LE)",
            cc: CallingConvention::mips_o32,
            arch: crate::arch::SleighArch::mipsle32,
            arg_count: 4,
            callee_saved_count: 11,
            args_float: O32_ARG_F,
            saved_float: F20_F31,
            ret_float: O32_RET_F,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 16,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "MIPS O32 (BE)",
            cc: CallingConvention::mips_o32,
            arch: crate::arch::SleighArch::mipsbe32,
            arg_count: 4,
            callee_saved_count: 11,
            args_float: O32_ARG_F,
            saved_float: F20_F31,
            ret_float: O32_RET_F,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 16,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "MIPS N64 (LE)",
            cc: CallingConvention::mips_n64,
            arch: crate::arch::SleighArch::mipsle64,
            arg_count: 8,
            callee_saved_count: 11,
            args_float: N64_ARG_F,
            saved_float: F24_F31,
            ret_float: N64_RET_F,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "MIPS N64 (BE)",
            cc: CallingConvention::mips_n64,
            arch: crate::arch::SleighArch::mipsbe64,
            arg_count: 8,
            callee_saved_count: 11,
            args_float: N64_ARG_F,
            saved_float: F24_F31,
            ret_float: N64_RET_F,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "PowerPC SysV 32 (BE)",
            cc: CallingConvention::powerpc_sysv32,
            arch: crate::arch::SleighArch::ppc32be,
            arg_count: 8,
            // r2 + r13 (reserved TLS/SDA) + r14..r31 (18) + LR.
            callee_saved_count: 21,
            args_float: PPC32_ARG_F,
            saved_float: PPC_F14_F31,
            ret_float: PPC_RET_F,
            callee_saved_cond_count: 3,
            callee_saved_vec_count: 12,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "r1",
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "PowerPC SysV 32 (LE)",
            cc: CallingConvention::powerpc_sysv32,
            arch: crate::arch::SleighArch::ppc32le,
            arg_count: 8,
            callee_saved_count: 21,
            args_float: PPC32_ARG_F,
            saved_float: PPC_F14_F31,
            ret_float: PPC_RET_F,
            callee_saved_cond_count: 3,
            callee_saved_vec_count: 12,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "r1",
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "PowerPC ELFv1 (BE)",
            cc: CallingConvention::powerpc64_elf_v1,
            arch: crate::arch::SleighArch::ppc64be,
            arg_count: 8,
            // r2 (TOC) + r13 (TLS) + r14..r31 (18) + LR, the last per the
            // deliberate link-register tradeoff (consistent with PPC32).
            callee_saved_count: 21,
            args_float: PPC64_ARG_F,
            saved_float: PPC_F14_F31,
            ret_float: PPC_RET_F,
            callee_saved_cond_count: 3,
            callee_saved_vec_count: 12,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "r1",
            // 48-byte linkage area + the 8-doubleword r3-r10 parameter save
            // area; the first stack-ONLY argument is above both.
            stack_args: Some(StackArgs {
                base_offset: 112,
                increment: 8,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "PowerPC ELFv2 (LE)",
            cc: CallingConvention::powerpc64_elf_v2,
            arch: crate::arch::SleighArch::ppc64le,
            arg_count: 8,
            // See PowerPC ELFv1 (BE) above.
            callee_saved_count: 21,
            args_float: PPC64_ARG_F,
            saved_float: PPC_F14_F31,
            ret_float: PPC_ELFV2_RET_F,
            callee_saved_cond_count: 3,
            callee_saved_vec_count: 12,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "r1",
            // 32-byte linkage area + the same parameter save area.  Measured:
            // a 16-argument call stores arguments 9..16 at r1+96..152.
            stack_args: Some(StackArgs {
                base_offset: 96,
                increment: 8,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "AArch64 AAPCS64 (BE)",
            cc: CallingConvention::aarch64_aapcs64,
            arch: crate::arch::SleighArch::aarch64be,
            arg_count: 8,
            callee_saved_count: 12,
            args_float: Q0_7,
            saved_float: D8_D15,
            ret_float: Q0_3,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "ARM AAPCS (Thumb)",
            cc: CallingConvention::arm_aapcs,
            arch: crate::arch::SleighArch::arm_thumb,
            arg_count: 4,
            callee_saved_count: 9,
            args_float: D0_7,
            saved_float: D8_D15,
            ret_float: D0_3,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "ARM AAPCS (BE)",
            cc: CallingConvention::arm_aapcs,
            arch: crate::arch::SleighArch::arm_be,
            arg_count: 4,
            callee_saved_count: 9,
            args_float: D0_7,
            saved_float: D8_D15,
            ret_float: D0_3,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        // BE8 ARM: the register conventions are byte-order independent, so the
        // same preset has to resolve on this spec too.
        Case {
            name: "ARM AAPCS (BE8)",
            cc: CallingConvention::arm_aapcs,
            arch: crate::arch::SleighArch::arm_be_kernel,
            arg_count: 4,
            callee_saved_count: 9,
            args_float: D0_7,
            saved_float: D8_D15,
            ret_float: D0_3,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        // `-mfloat-abi=soft` / `softfp` passes and returns floats in the core
        // registers, so naming the VFP bank would seed d0-d7 on every function
        // and hang phantom float arguments off every call site.  d8-d15 stay
        // preserved: that is a property of the register file, not of the float
        // variant.
        Case {
            name: "ARM AAPCS (soft-float)",
            cc: CallingConvention::arm_aapcs_soft,
            arch: crate::arch::SleighArch::arm,
            arg_count: 4,
            callee_saved_count: 9,
            args_float: NONE,
            saved_float: D8_D15,
            ret_float: NONE,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        // The kernel rows: x86 32-bit differs from its userland counterpart in
        // the integer argument registers (regparm-3), PowerPC in the vector
        // windows alone.  Every other arch's kernel CC is a userland preset
        // above.
        Case {
            name: "x86 Linux kernel (regparm-3)",
            cc: CallingConvention::x86_linux_kernel,
            arch: crate::arch::SleighArch::x86,
            arg_count: 3,          // EAX, EDX, ECX
            callee_saved_count: 4, // EBX, ESI, EDI, EBP
            args_float: NONE,
            saved_float: NONE,
            ret_float: X86_RET_F,
            callee_saved_cond_count: 0,
            callee_saved_vec_count: 0,
            ret_count: 2, // EAX, EDX
            reg_size_bytes: 4,
            stack_ptr_name: "ESP",
            stack_args: Some(StackArgs {
                base_offset: 4,
                increment: 4,
            }),
            ret_stack_pop: 4,
        },
        Case {
            name: "PowerPC SysV 32 Linux kernel",
            cc: CallingConvention::powerpc_sysv32_linux_kernel,
            arch: crate::arch::SleighArch::ppc32be,
            arg_count: 8,
            callee_saved_count: 21,
            args_float: PPC32_ARG_F_KERNEL,
            saved_float: PPC_F14_F31,
            ret_float: PPC_RET_F_KERNEL,
            callee_saved_cond_count: 3,
            callee_saved_vec_count: 12,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "r1",
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "PowerPC ELFv1 Linux kernel",
            cc: CallingConvention::powerpc64_elf_v1_linux_kernel,
            arch: crate::arch::SleighArch::ppc64be,
            arg_count: 8,
            callee_saved_count: 21,
            args_float: PPC64_ARG_F_KERNEL,
            saved_float: PPC_F14_F31,
            ret_float: PPC_RET_F_KERNEL,
            callee_saved_cond_count: 3,
            callee_saved_vec_count: 12,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "r1",
            stack_args: Some(StackArgs {
                base_offset: 112,
                increment: 8,
            }),
            ret_stack_pop: 0,
        },
        Case {
            name: "PowerPC ELFv2 Linux kernel",
            cc: CallingConvention::powerpc64_elf_v2_linux_kernel,
            arch: crate::arch::SleighArch::ppc64le,
            arg_count: 8,
            callee_saved_count: 21,
            args_float: PPC64_ARG_F_KERNEL,
            saved_float: PPC_F14_F31,
            ret_float: PPC_ELFV2_RET_F_KERNEL,
            callee_saved_cond_count: 3,
            callee_saved_vec_count: 12,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "r1",
            stack_args: Some(StackArgs {
                base_offset: 96,
                increment: 8,
            }),
            ret_stack_pop: 0,
        },
    ]
}

fn build_case(case: &Case) -> (BuiltCallingConvention, rsleigh::SleighRegs) {
    let regs = regs_for((case.arch)());
    let built = (case.cc)()
        .build(&regs)
        .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", case.name));
    (built, regs)
}

/// Documented integer-register count and float / vector register NAMES per
/// category, the latter resolved so a name that does not exist on the arch
/// fails here rather than at lift time.
#[test]
fn presets_resolve_correct_register_sets() {
    for c in cases() {
        let (built, regs) = build_case(&c);
        assert_eq!(
            built.arg_passing_regs.len(),
            c.arg_count,
            "{}: args",
            c.name
        );
        assert_eq!(
            built.callee_saved_regs.len(),
            c.callee_saved_count
                + c.saved_float.len()
                + c.callee_saved_cond_count
                + c.callee_saved_vec_count,
            "{}: callee-saved",
            c.name
        );
        assert_eq!(
            built.ret_val_regs.len(),
            c.ret_count,
            "{}: return values",
            c.name
        );
        let names = |vns: &[rsleigh::Vn]| -> Vec<String> {
            vns.iter()
                .map(|vn| {
                    regs.vn_to_name(*vn)
                        .unwrap_or_else(|| panic!("{}: {vn:?} has no name", c.name))
                        .to_string()
                })
                .collect()
        };
        for (label, got, want) in [
            (
                "arg_passing_regs_float",
                names(&built.arg_passing_regs_float),
                c.args_float,
            ),
            (
                "ret_val_regs_float",
                names(&built.ret_val_regs_float),
                c.ret_float,
            ),
            (
                "callee_saved_regs float run",
                names(&built.callee_saved_regs[c.callee_saved_count..][..c.saved_float.len()]),
                c.saved_float,
            ),
        ] {
            assert_eq!(got, want, "{}: {label}", c.name);
        }
    }
}

/// Every resolved register, SP included, must be the arch's natural word
/// size.  SP matters because `StackOffsetDetect` and the stack-arg machinery
/// assume an SP-sized address: an undersized SP silently miscomputes offsets
/// downstream with no diagnostic from this crate.
#[test]
fn presets_resolved_registers_have_expected_size() {
    for c in cases() {
        let (built, _) = build_case(&c);
        // Callee-saved floats are the FP file's own width, not the integer
        // word size, so only the leading GPR run is checked here.
        let (saved_int, saved_float) = built.callee_saved_regs.split_at(c.callee_saved_count);
        for vn in built
            .arg_passing_regs
            .iter()
            .chain(saved_int)
            .chain(&built.ret_val_regs)
            .chain(std::iter::once(&built.stack_vn))
        {
            assert_eq!(
                vn.size, c.reg_size_bytes,
                "{}: expected {}-byte register, got {vn:?}",
                c.name, c.reg_size_bytes,
            );
        }
        let (saved_float, saved_cond) = saved_float.split_at(c.saved_float.len());
        assert_eq!(
            saved_float.len(),
            c.saved_float.len(),
            "{}: callee-saved float registers follow the GPRs",
            c.name,
        );
        let (saved_cond, saved_vec) = saved_cond.split_at(c.callee_saved_cond_count);
        assert_eq!(
            saved_cond.len(),
            c.callee_saved_cond_count,
            "{}: condition fields follow the floats",
            c.name,
        );
        for vn in saved_vec {
            assert_eq!(
                vn.size, 16,
                "{}: expected a 16-byte vector register, got {vn:?}",
                c.name,
            );
        }
        assert_eq!(
            saved_vec.len(),
            c.callee_saved_vec_count,
            "{}: vector registers follow the condition fields",
            c.name,
        );
    }
}

/// The SP varnode resolves to the arch's SP register and appears in none of
/// the resolved register lists: on stack-push ISAs the callee's `ret` pops the
/// return address so SP is not preserved, and on link-register ISAs the call
/// leaves SP alone but SP is still modelled not-callee-saved for uniformity
/// (with `ret_stack_pop = 0`).  Stack-arg offsets and `ret_stack_pop` must
/// round-trip unchanged from the preset.
#[test]
fn presets_stack_pointer_and_arg_offsets() {
    for c in cases() {
        let (built, regs) = build_case(&c);
        let sp = regs
            .name_to_vn(c.stack_ptr_name)
            .unwrap_or_else(|| panic!("{}: {} must resolve", c.name, c.stack_ptr_name));
        assert_eq!(built.stack_vn, sp, "{}: stack_vn", c.name);
        for (label, set) in [
            ("arg_passing_regs", &built.arg_passing_regs),
            ("callee_saved_regs", &built.callee_saved_regs),
            ("ret_val_regs", &built.ret_val_regs),
        ] {
            assert!(
                !set.contains(&built.stack_vn),
                "{}: stack pointer must not appear in {label}",
                c.name,
            );
        }
        assert_eq!(built.stack_args, c.stack_args, "{}: stack_args", c.name,);
        assert_eq!(
            built.ret_stack_pop, c.ret_stack_pop,
            "{}: ret_stack_pop",
            c.name,
        );
    }
}

/// An unknown register name in any category errors, on any architecture.
#[test]
fn build_returns_error_for_unknown_register_name() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    for bad_name in &["NOTAREG", "", "rax_FAKE"] {
        let cc = CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: std::slice::from_ref(bad_name),
            arg_passing_regs_float: &[],
            callee_saved_regs: &[],
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            stack_args: None,
            ret_stack_pop: 0,
            link_register_reg_name: None,
            preserves_memory: false,
            preserves_all_registers: false,
            entry_address_reg_name: None,
        };
        let result = cc.build(&regs);
        let err = result.expect_err("expected UnknownRegName error");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown sleigh register name") && msg.contains(bad_name),
            "expected UnknownRegName({bad_name:?}), got {err}"
        );
    }
}

/// The first unknown name short-circuits rather than silently succeeding on
/// the remaining valid ones.
#[test]
fn build_returns_error_even_when_some_names_are_valid() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention {
        stack_ptr_reg_name: "RSP",
        arg_passing_regs: &["RDI", "NOT_A_REG", "RSI"],
        arg_passing_regs_float: &[],
        callee_saved_regs: &[],
        ret_val_regs: &[],
        ret_val_regs_float: &[],
        stack_args: None,
        ret_stack_pop: 0,
        link_register_reg_name: None,
        preserves_memory: false,
        preserves_all_registers: false,
        entry_address_reg_name: None,
    };
    assert!(
        cc.build(&regs).is_err(),
        "a list with one bad name must fail"
    );
}

/// Expected link-register Sleigh name per preset, `None` for stack-push ISAs
/// that hold the return address on the stack.  Drives every link-register
/// test below; adding a preset means adding one row.
struct LinkRegCase {
    name: &'static str,
    cc: fn() -> CallingConvention,
    arch: fn() -> crate::arch::SleighArch,
    expected_lr_name: Option<&'static str>,
}

fn link_reg_cases() -> Vec<LinkRegCase> {
    vec![
        LinkRegCase {
            name: "ARM AAPCS",
            cc: CallingConvention::arm_aapcs,
            arch: crate::arch::SleighArch::arm,
            expected_lr_name: Some("lr"),
        },
        LinkRegCase {
            name: "ARM AAPCS (Thumb)",
            cc: CallingConvention::arm_aapcs,
            arch: crate::arch::SleighArch::arm_thumb,
            expected_lr_name: Some("lr"),
        },
        LinkRegCase {
            name: "ARM AAPCS (BE)",
            cc: CallingConvention::arm_aapcs,
            arch: crate::arch::SleighArch::arm_be,
            expected_lr_name: Some("lr"),
        },
        LinkRegCase {
            name: "AArch64 AAPCS64",
            cc: CallingConvention::aarch64_aapcs64,
            arch: crate::arch::SleighArch::aarch64,
            expected_lr_name: Some("x30"),
        },
        LinkRegCase {
            name: "AArch64 AAPCS64 (BE)",
            cc: CallingConvention::aarch64_aapcs64,
            arch: crate::arch::SleighArch::aarch64be,
            expected_lr_name: Some("x30"),
        },
        LinkRegCase {
            name: "MIPS O32 (LE)",
            cc: CallingConvention::mips_o32,
            arch: crate::arch::SleighArch::mipsle32,
            expected_lr_name: Some("ra"),
        },
        LinkRegCase {
            name: "MIPS O32 (BE)",
            cc: CallingConvention::mips_o32,
            arch: crate::arch::SleighArch::mipsbe32,
            expected_lr_name: Some("ra"),
        },
        LinkRegCase {
            name: "MIPS N64 (LE)",
            cc: CallingConvention::mips_n64,
            arch: crate::arch::SleighArch::mipsle64,
            expected_lr_name: Some("ra"),
        },
        LinkRegCase {
            name: "MIPS N64 (BE)",
            cc: CallingConvention::mips_n64,
            arch: crate::arch::SleighArch::mipsbe64,
            expected_lr_name: Some("ra"),
        },
        LinkRegCase {
            name: "PowerPC SysV 32 (BE)",
            cc: CallingConvention::powerpc_sysv32,
            arch: crate::arch::SleighArch::ppc32be,
            expected_lr_name: Some("LR"),
        },
        LinkRegCase {
            name: "PowerPC SysV 32 (LE)",
            cc: CallingConvention::powerpc_sysv32,
            arch: crate::arch::SleighArch::ppc32le,
            expected_lr_name: Some("LR"),
        },
        LinkRegCase {
            name: "PowerPC ELFv1 (BE)",
            cc: CallingConvention::powerpc64_elf_v1,
            arch: crate::arch::SleighArch::ppc64be,
            expected_lr_name: Some("LR"),
        },
        LinkRegCase {
            name: "PowerPC ELFv2 (LE)",
            cc: CallingConvention::powerpc64_elf_v2,
            arch: crate::arch::SleighArch::ppc64le,
            expected_lr_name: Some("LR"),
        },
        LinkRegCase {
            name: "x86-64 SysV",
            cc: CallingConvention::x86_64_systemv,
            arch: crate::arch::SleighArch::x86_64,
            expected_lr_name: None,
        },
        LinkRegCase {
            name: "x86 cdecl",
            cc: CallingConvention::x86_cdecl,
            arch: crate::arch::SleighArch::x86,
            expected_lr_name: None,
        },
    ]
}

/// Every link-register ISA preset resolves `link_register_vn` to the arch's
/// LR under the documented Sleigh name.  Pinning all of them catches a typo or
/// rename in any one `link_register_reg_name`.
#[test]
fn link_register_vn_set_for_link_register_presets() {
    for c in link_reg_cases() {
        let Some(expected_name) = c.expected_lr_name else {
            continue;
        };
        let regs = regs_for((c.arch)());
        let built = (c.cc)()
            .build(&regs)
            .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", c.name));
        let expected_vn = regs.name_to_vn(expected_name).unwrap_or_else(|| {
            panic!(
                "{}: expected LR name {:?} must resolve in arch's Sleigh regs",
                c.name, expected_name,
            )
        });
        assert_eq!(
            built.link_register_vn,
            Some(expected_vn),
            "{}: link_register_vn must be the {:?} varnode",
            c.name,
            expected_name,
        );
    }
}

/// Stack-push ISAs (x86, x86_64) keep the return address on the stack.
#[test]
fn link_register_vn_none_for_stack_push_presets() {
    for c in link_reg_cases() {
        if c.expected_lr_name.is_some() {
            continue;
        }
        let regs = regs_for((c.arch)());
        let built = (c.cc)()
            .build(&regs)
            .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", c.name));
        assert!(
            built.link_register_vn.is_none(),
            "{}: link_register_vn must be None on stack-push ISAs, got {:?}",
            c.name,
            built.link_register_vn,
        );
    }
}

/// An unknown `stack_ptr_reg_name` errors like any other unknown register.
/// The SP name has its own lookup path in `build()`, separate from
/// `regs_to_vns`.
#[test]
fn build_returns_error_for_unknown_stack_pointer_name() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention {
        stack_ptr_reg_name: "NOT_A_SP",
        arg_passing_regs: &[],
        arg_passing_regs_float: &[],
        callee_saved_regs: &[],
        ret_val_regs: &[],
        ret_val_regs_float: &[],
        stack_args: None,
        ret_stack_pop: 0,
        link_register_reg_name: None,
        preserves_memory: false,
        preserves_all_registers: false,
        entry_address_reg_name: None,
    };
    let result = cc.build(&regs);
    let err = result.expect_err("expected UnknownRegName error");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown sleigh register name") && msg.contains("NOT_A_SP"),
        "expected UnknownRegName(\"NOT_A_SP\"), got {err}"
    );
}

#[test]
fn preserves_all_sets_both_flags_regs_keeps_memory() {
    // __fentry__ / mcount-style hooks promise zero observable side-effects, so
    // the Call's memory output must be suppressible at IR-build time for
    // LoadReadOnly / LoadForward to forward across these calls.  preserves_regs
    // suppresses only the register clobbers.
    let all = CallingConvention::x86_64_systemv().preserves_all();
    assert!(all.preserves_memory && all.preserves_all_registers);
    let regs = CallingConvention::x86_64_systemv().preserves_regs();
    assert!(!regs.preserves_memory && regs.preserves_all_registers);
    // callee_saved is retained (the link-register invariant must survive).
    assert_eq!(
        all.callee_saved_regs,
        CallingConvention::x86_64_systemv().callee_saved_regs
    );
}

#[test]
fn standard_presets_have_preserves_memory_false() {
    // Standard presets keep the default so their Call nodes correctly clobber
    // memory.  Only preserves_all / preserves_regs opt out.
    let presets: &[(&str, CallingConvention)] = &[
        ("x86_64_systemv", CallingConvention::x86_64_systemv()),
        ("x86_cdecl", CallingConvention::x86_cdecl()),
        ("aarch64_aapcs64", CallingConvention::aarch64_aapcs64()),
        ("arm_aapcs", CallingConvention::arm_aapcs()),
        ("arm_aapcs_soft", CallingConvention::arm_aapcs_soft()),
        ("mips_o32", CallingConvention::mips_o32()),
        ("mips_n64", CallingConvention::mips_n64()),
        ("powerpc_sysv32", CallingConvention::powerpc_sysv32()),
        ("powerpc64_elf_v1", CallingConvention::powerpc64_elf_v1()),
        ("powerpc64_elf_v2", CallingConvention::powerpc64_elf_v2()),
    ];
    for (name, cc) in presets {
        assert!(
            !cc.preserves_memory,
            "{name}: standard presets must have preserves_memory = false"
        );
    }
}

#[test]
fn positional_arg_layout_empty_has_no_stack() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention::x86_64_systemv()
        .preserves_all()
        .build(&regs)
        .unwrap();
    assert!(cc.arg_passing_regs.is_empty());
    assert!(cc.stack_args.is_none());
}

/// Hand-computed literals for the two real-world strides (x86 cdecl 4/4,
/// x86_64 SysV 8/8).  The large-N row pins that the math has headroom at any
/// realistic index.
#[test]
fn stack_args_offset_of_literal_series() {
    use crate::calling_convention::StackArgs;
    let x86 = StackArgs {
        base_offset: 4,
        increment: 4,
    };
    assert_eq!(x86.offset_of(0), 4);
    assert_eq!(x86.offset_of(1), 8);
    assert_eq!(x86.offset_of(7), 32); // 4 + 7*4

    let x64 = StackArgs {
        base_offset: 8,
        increment: 8,
    };
    assert_eq!(x64.offset_of(0), 8);
    assert_eq!(x64.offset_of(1), 16);
    assert_eq!(x64.offset_of(7), 64); // 8 + 7*8

    // 2^40 stack args: 8 + 8*2^40 = 2^43 + 8.
    assert_eq!(x64.offset_of(1 << 40), 8_796_093_022_216);
}

/// Boundary semantics of `slot_of` (floor, no size bound), over the x86 (4/4)
/// and x86_64 (8/8) strides.
#[test]
fn stack_args_slot_boundaries_per_increment() {
    use crate::calling_convention::StackArgs;
    for (label, s) in [
        (
            "x86 4/4",
            StackArgs {
                base_offset: 4,
                increment: 4,
            },
        ),
        (
            "x86_64 8/8",
            StackArgs {
                base_offset: 8,
                increment: 8,
            },
        ),
    ] {
        let (base, inc) = (s.base_offset, s.increment);

        // `slot_of` takes no size argument, so a wider-than-slot argument
        // anchors at the slot of its first byte, giving the same answer as the
        // 1-byte probes below.
        assert_eq!(s.slot_of(base - 1), None, "{label}: below base");
        // A decoded negative SP delta must give `None` rather than wrap the
        // unsigned slot arithmetic, down to the extreme.
        assert_eq!(s.slot_of(i128::MIN), None, "{label}: i128::MIN");
        assert_eq!(s.slot_of(base), Some(0), "{label}: slot-0 start");
        assert_eq!(
            s.slot_of(base + inc - 1),
            Some(0),
            "{label}: slot-0 last byte floors"
        );
        assert_eq!(s.slot_of(base + inc), Some(1), "{label}: slot-1 start");
        assert_eq!(
            s.slot_of(base + 2 * inc + 1),
            Some(2),
            "{label}: mid-slot-2 floors"
        );
    }
}

/// `ceil(max(size,1) / increment)`, never below 1, across the 4- and 8-byte
/// strides.
#[test]
fn stack_args_slots_spanned_ceils_by_increment() {
    use crate::calling_convention::StackArgs;
    for (label, inc) in [("x86 4/4", 4i128), ("x86_64 8/8", 8i128)] {
        let s = StackArgs {
            base_offset: inc,
            increment: inc,
        };
        assert_eq!(
            s.slots_spanned(0),
            1,
            "{label}: zero-size occupies one slot"
        );
        assert_eq!(s.slots_spanned(1), 1, "{label}: one byte");
        assert_eq!(s.slots_spanned(inc), 1, "{label}: exactly one slot");
        assert_eq!(s.slots_spanned(inc + 1), 2, "{label}: spills into slot 2");
        assert_eq!(s.slots_spanned(2 * inc), 2, "{label}: exactly two slots");
        assert_eq!(
            s.slots_spanned(2 * inc + 1),
            3,
            "{label}: spills into slot 3"
        );
    }
}

/// A garbage decoded size near `i128::MAX` must not overflow the
/// `size + increment - 1` numerator; the span saturates instead, mirroring
/// `offset_of`.
#[test]
fn stack_args_slots_spanned_saturates_on_overflow() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs {
        base_offset: 8,
        increment: 8,
    };
    // `i128::MAX + 7` would overflow without the saturating add.
    let span = s.slots_spanned(i128::MAX);
    assert_eq!(span, (i128::MAX / 8) as usize);
}

/// Near-`i128::MAX` offsets come from binary content, not trusted input, so
/// they must saturate rather than panic in debug or wrap in release.  The
/// overflow sites are `offset_of`'s `base + n*increment` and the final
/// i128 -> usize narrowing in `slot_of` / `slots_spanned`.
#[test]
fn stack_args_slot_math_degrades_on_overflow_not_panics() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs {
        base_offset: 8,
        increment: 8,
    };
    // (i128::MAX - 8) / 8 exceeds usize::MAX and its low 64 bits are
    // usize::MAX - 1, so a wrapping `as usize` would silently answer that.
    assert_eq!(s.slot_of(i128::MAX), Some(usize::MAX));
    // `slots_spanned` documents a span >= 1: 2^67 / 8 is 2^64, whose low 64
    // bits are 0, which would stall every loop-advancing caller.
    assert_eq!(s.slots_spanned(1i128 << 67), usize::MAX);
    // The i128 intermediate is wide enough that (1<<62)*8 does not overflow,
    // so the saturating add scales exactly here.
    assert_eq!(s.offset_of(1usize << 62), (1i128 << 62) * 8 + 8);
}

/// `base_offset` flows in unvalidated from the Python
/// `CallingConvention.custom` (`stack_arg_base`).  Without this guard a
/// negative base lets `slot_of`'s `offset - base_offset` overflow on a garbage
/// offset decoded from a crafted binary.
#[test]
fn validate_rejects_negative_stack_arg_base_offset() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let sp = regs.name_to_vn("RSP").expect("x86_64 has RSP");
    let cc = BuiltCallingConvention {
        arg_passing_regs: vec![],
        callee_saved_regs: vec![],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: sp,
        stack_args: Some(StackArgs {
            base_offset: -8,
            increment: 8,
        }),
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    };
    assert!(
        cc.validate().is_err(),
        "negative stack-arg base_offset must be rejected by validate",
    );
}

/// GHIDRA's ARM sla lays `d0`-`d3` inside `q0`/`q1` (`q0` and `d0` share
/// offset 0x300), so a function touching `q0`/`q1` tracks those as the
/// containers.  Mapping the return list through `container_of` then yields
/// `q0, q0, q1, q1`, which the lifter concatenates into a `Call`'s output
/// varnodes and `validate_call_output_vns` rejects.
#[test]
fn aliased_float_ret_regs_collapse_to_one_container_each() {
    let regs = regs_for(crate::arch::SleighArch::arm());
    let built = CallingConvention::arm_aapcs()
        .build(&regs)
        .expect("arm_aapcs must build");
    let q0 = vn_for_name(&regs, "q0").expect("ARM sla defines q0");
    let q1 = vn_for_name(&regs, "q1").expect("ARM sla defines q1");
    let r0 = vn_for_name(&regs, "r0").expect("ARM sla defines r0");
    let r1 = vn_for_name(&regs, "r1").expect("ARM sla defines r1");
    let tracked = [r0, r1, q0, q1];
    let (ret_vals, _clobbers) = built.ret_and_clobber_vns(&tracked, |v| container_in(&tracked, v));
    assert_eq!(ret_vals, [r0, r1, q0, q1], "one slot per tracked container");
}

/// A float argument register is legitimately also a float RETURN register
/// (`XMM0`, `d0`, `f1`), so `validate` must keep that overlap legal while
/// still rejecting an argument register the callee is required to preserve.
#[test]
fn float_arg_regs_may_overlap_returns_but_not_callee_saved() {
    for (name, cc_fn, arch_fn) in [
        (
            "x86-64 SysV",
            CallingConvention::x86_64_systemv as fn() -> CallingConvention,
            crate::arch::SleighArch::x86_64 as fn() -> crate::arch::SleighArch,
        ),
        (
            "ARM AAPCS",
            CallingConvention::arm_aapcs,
            crate::arch::SleighArch::arm,
        ),
        (
            "PowerPC SysV 32",
            CallingConvention::powerpc_sysv32,
            crate::arch::SleighArch::ppc32be,
        ),
    ] {
        let built = cc_fn()
            .build(&regs_for(arch_fn()))
            .unwrap_or_else(|e| panic!("{name}: build failed: {e:?}"));
        assert!(
            built
                .arg_passing_regs_float
                .iter()
                .any(|vn| built.ret_val_regs_float.contains(vn)),
            "{name}: the first float argument register is also a float return \
             register, and validate must allow it",
        );
        for vn in &built.arg_passing_regs_float {
            assert!(
                !built.callee_saved_regs.contains(vn),
                "{name}: float arg reg {vn:?} must not be callee-saved",
            );
        }
    }
}

/// `FunctionBuilder::new`'s container map, i.e.
/// `vn_container::largest_container_in`.
fn container_in(tracked: &[rsleigh::Vn], v: &rsleigh::Vn) -> rsleigh::Vn {
    let end = v.addr_off + u64::from(v.size);
    tracked
        .iter()
        .copied()
        .filter(|c| {
            c.addr_space == v.addr_space
                && c.addr_off <= v.addr_off
                && c.addr_off + u64::from(c.size) >= end
        })
        .max_by_key(|c| c.size)
        .unwrap_or(*v)
}

/// AAPCS64 6.1.2 preserves only the LOW 64 bits of `v8`-`v15`.  A function
/// touching all of `q8` tracks the 128-bit container, and the callee is free
/// to trash its upper half, so the container belongs in the clobber set.
#[test]
fn aarch64_tracked_q8_is_clobbered() {
    let regs = regs_for(crate::arch::SleighArch::aarch64());
    let built = CallingConvention::aarch64_aapcs64()
        .build(&regs)
        .expect("aarch64_aapcs64 must build");
    let q8 = vn_for_name(&regs, "q8").expect("aarch64 sla defines q8");
    let d8 = vn_for_name(&regs, "d8").expect("aarch64 sla defines d8");
    let x19 = vn_for_name(&regs, "x19").expect("aarch64 sla defines x19");
    let tracked = [x19, q8];
    let (_ret_vals, clobbers) = built.ret_and_clobber_vns(&tracked, |v| container_in(&tracked, v));
    assert!(
        clobbers.contains(&q8),
        "only d8 (the low {} of q8's {} bytes) is preserved, so q8 is clobbered; \
         got {clobbers:?}",
        d8.size,
        q8.size,
    );
    assert!(
        !clobbers.contains(&x19),
        "x19 is callee-saved in full and must stay out of the clobber set",
    );
}

/// AAPCS 5.1.2.1 preserves BOTH halves of ARM `q4` (= `d8`|`d9`), so a
/// function tracking the 128-bit container keeps it out of the clobber set.
#[test]
fn arm_tracked_q4_stays_preserved() {
    let regs = regs_for(crate::arch::SleighArch::arm());
    let built = CallingConvention::arm_aapcs()
        .build(&regs)
        .expect("arm_aapcs must build");
    let q4 = vn_for_name(&regs, "q4").expect("ARM sla defines q4");
    let d8 = vn_for_name(&regs, "d8").expect("ARM sla defines d8");
    let d9 = vn_for_name(&regs, "d9").expect("ARM sla defines d9");
    assert_eq!(
        container_in(&[q4], &d8),
        q4,
        "d8 sits in q4 in GHIDRA's ARM register table",
    );
    assert_eq!(container_in(&[q4], &d9), q4, "d9 sits in q4");
    let r4 = vn_for_name(&regs, "r4").expect("ARM sla defines r4");
    let tracked = [r4, q4];
    let (_ret_vals, clobbers) = built.ret_and_clobber_vns(&tracked, |v| container_in(&tracked, v));
    assert!(
        !clobbers.contains(&q4),
        "d8 and d9 cover q4 entirely, so q4 is preserved; got {clobbers:?}",
    );
}

/// `container_of` is an O(|tracked|) scan under
/// `strider_ir::cc_ret_and_clobber_vns`, and `ret_and_clobber_vns` runs per
/// `Call` node built.  Mapping the callee-saved list through it makes that
/// |callee_saved| * |tracked| per call, 42 * |tracked| on PPC64.
#[test]
fn ret_and_clobber_vns_does_not_map_the_callee_saved_list() {
    let regs = regs_for(crate::arch::SleighArch::ppc64be());
    let cc = CallingConvention::powerpc64_elf_v1();
    let built = cc.build(&regs).expect("powerpc64_elf_v1 must build");
    let tracked: Vec<rsleigh::Vn> = built
        .callee_saved_regs
        .iter()
        .chain(built.ret_val_regs.iter())
        .chain(built.ret_val_regs_float.iter())
        .copied()
        .collect();
    let calls = std::cell::Cell::new(0usize);
    let (_ret_vals, _clobbers) = built.ret_and_clobber_vns(&tracked, |v| {
        calls.set(calls.get() + 1);
        container_in(&tracked, v)
    });
    let ret_regs = built.ret_val_regs.len() + built.ret_val_regs_float.len();
    assert!(
        calls.get() <= ret_regs,
        "container_of ran {} times for {ret_regs} return register(s) and {} \
         callee-saved one(s)",
        calls.get(),
        built.callee_saved_regs.len(),
    );
}

/// The entry-address contract is ELFv2's r12 and MIPS abicalls' t9 and nothing
/// else: ELFv1 names a descriptor word, not a register, and no other shipped
/// ABI promises anything about a register's entry value.
#[test]
fn only_elfv2_and_mips_carry_an_entry_address_register() {
    type Case = (
        &'static str,
        fn() -> crate::arch::SleighArch,
        fn() -> CallingConvention,
        Option<&'static str>,
    );
    let cases: &[Case] = &[
        (
            "ppc64le elfv2",
            crate::arch::SleighArch::ppc64le,
            CallingConvention::powerpc64_elf_v2,
            Some("r12"),
        ),
        (
            "ppc64be elfv1",
            crate::arch::SleighArch::ppc64be,
            CallingConvention::powerpc64_elf_v1,
            None,
        ),
        (
            "mips o32",
            crate::arch::SleighArch::mipsle32,
            CallingConvention::mips_o32,
            Some("t9"),
        ),
        (
            "mips n64",
            crate::arch::SleighArch::mipsle64,
            CallingConvention::mips_n64,
            Some("t9"),
        ),
        (
            "x86-64 sysv",
            crate::arch::SleighArch::x86_64,
            CallingConvention::x86_64_systemv,
            None,
        ),
        (
            "ppc32 sysv",
            crate::arch::SleighArch::ppc32be,
            CallingConvention::powerpc_sysv32,
            None,
        ),
        (
            "aarch64 aapcs64",
            crate::arch::SleighArch::aarch64,
            CallingConvention::aarch64_aapcs64,
            None,
        ),
    ];
    for &(name, arch, cc, expected) in cases {
        let regs = regs_for(arch());
        let built = cc().build(&regs).expect(name);
        assert_eq!(
            built.entry_address_vn,
            expected.map(|r| vn_for_name(&regs, r).expect(r)),
            "{name}",
        );
    }
}

/// A p-code CALL writes no Sleigh temporary, so a tracked UNIQUE varnode is
/// not a call clobber: it would give every `Call` an output per temporary and
/// make every call region a def site for all of them.
#[test]
fn tracked_unique_temporaries_are_not_call_clobbers() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let built = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("x86_64_systemv must build");
    let rcx = vn_for_name(&regs, "RCX").expect("x86-64 sla defines RCX");
    let temp = rsleigh::Vn {
        addr_space: rsleigh::VnSpace::UNIQUE,
        addr_off: 0x58600,
        size: 1,
    };
    let tracked = [rcx, temp];
    let (_ret_vals, clobbers) = built.ret_and_clobber_vns(&tracked, |v| container_in(&tracked, v));
    assert!(
        clobbers.contains(&rcx),
        "RCX is caller-saved and must stay a clobber; got {clobbers:?}",
    );
    assert!(
        !clobbers.contains(&temp),
        "a UNIQUE temporary must not be a call clobber; got {clobbers:?}",
    );
}
