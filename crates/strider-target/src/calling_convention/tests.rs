use super::*;

fn regs_for(arch: crate::arch::SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .unwrap()
        .regs()
        .unwrap()
}

/// PPC System V (32-bit) and PPC64 ELFv1 return `long double` (IBM
/// double-double, the gcc default) and `_Complex double` in the f1:f2 pair.
/// With `f2` unlisted nothing roots the low half's cone and DCE drops it, the
/// same way an unlisted `ST1` dropped the x87 imaginary half.  ELFv1 returns a
/// homogeneous float aggregate through a hidden pointer, so the pair is the
/// whole story there.
#[test]
fn ppc_float_return_covers_the_double_double_pair() {
    for cc in [
        CallingConvention::powerpc_sysv32(),
        CallingConvention::powerpc64_elf_v1(),
    ] {
        assert_eq!(
            cc.ret_val_regs_float,
            &["f1", "f2"],
            "PPC returns long double / _Complex double in f1:f2",
        );
    }
}

/// ELFv2 2.2.3.3 returns a homogeneous float aggregate of up to eight members
/// in f1-f8; `powerpc64le-linux-gnu-gcc -O1` reads members 7 and 8 out of
/// f7/f8.  Unlisted, those land in the clobber group and the call reports two
/// return values where the ABI defines eight.
#[test]
fn ppc64_elf_v2_float_return_covers_all_eight_aggregate_members() {
    assert_eq!(
        CallingConvention::powerpc64_elf_v2().ret_val_regs_float,
        &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"],
    );
}

/// AAPCS64 6.4.2 returns a homogeneous floating-point aggregate of up to four
/// members in v0..v3, and AAPCS-VFP does the same in d0..d3.  Listing only the
/// first two leaves the rest unrooted at `Return`, so DCE deletes them.
#[test]
fn hfa_float_return_covers_all_four_members() {
    assert_eq!(
        CallingConvention::aarch64_aapcs64().ret_val_regs_float,
        &["q0", "q1", "q2", "q3"],
        "AAPCS64 returns a 4-member HFA in v0..v3",
    );
    assert_eq!(
        CallingConvention::arm_aapcs().ret_val_regs_float,
        &["d0", "d1", "d2", "d3"],
        "AAPCS-VFP returns a 4-member HFA in d0..d3",
    );
}

/// SysV AMD64 psABI 3.2.3 returns an X87-class value in `%st0`.  Without it in
/// the list nothing roots the x87 cone, so every `long double` return is
/// dropped.
#[test]
fn x86_64_float_return_includes_st0() {
    let cc = CallingConvention::x86_64_systemv();
    assert!(
        cc.ret_val_regs_float.contains(&"ST0"),
        "x86-64 long double returns in ST0, got {:?}",
        cc.ret_val_regs_float,
    );
    let built = cc
        .build(&regs_for(crate::arch::SleighArch::x86_64()))
        .expect("x86-64 SysV must build");
    assert_eq!(built.ret_val_regs_float.len(), cc.ret_val_regs_float.len());
}

/// One supported convention plus everything `build()` should produce for it.
/// Adding a convention means adding one row; every invariant test picks it up.
struct Case {
    name: &'static str,
    cc: fn() -> CallingConvention,
    arch: fn() -> crate::arch::SleighArch,
    arg_count: usize,
    /// General-purpose only; the float file is `callee_saved_float_count`.
    /// `callee_saved_regs` is built GPRs first, floats second.
    callee_saved_count: usize,
    callee_saved_float_count: usize,
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
            callee_saved_float_count: 0,
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
            callee_saved_float_count: 0,
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
            callee_saved_float_count: 8,
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
            callee_saved_float_count: 8,
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
            callee_saved_float_count: 12,
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
            callee_saved_float_count: 12,
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
            callee_saved_float_count: 8,
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
            callee_saved_float_count: 8,
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
            callee_saved_float_count: 18,
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
            callee_saved_float_count: 18,
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
            callee_saved_float_count: 18,
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
            callee_saved_float_count: 18,
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
            callee_saved_float_count: 8,
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
            callee_saved_float_count: 8,
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
            callee_saved_float_count: 8,
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        // `x86_linux_kernel` (regparm-3) is the only kernel CC whose register
        // set differs from its userland counterpart, so it is the sole kernel
        // row; every other arch's kernel CC is a userland preset above.
        Case {
            name: "x86 Linux kernel (regparm-3)",
            cc: CallingConvention::x86_linux_kernel,
            arch: crate::arch::SleighArch::x86,
            arg_count: 3,          // EAX, EDX, ECX
            callee_saved_count: 4, // EBX, ESI, EDI, EBP
            callee_saved_float_count: 0,
            ret_count: 2, // EAX, EDX
            reg_size_bytes: 4,
            stack_ptr_name: "ESP",
            stack_args: Some(StackArgs {
                base_offset: 4,
                increment: 4,
            }),
            ret_stack_pop: 4,
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

#[track_caller]
fn assert_all_distinct(set: &[rsleigh::Vn], label: &str) {
    for i in 0..set.len() {
        for j in (i + 1)..set.len() {
            assert_ne!(
                set[i], set[j],
                "{label}: varnodes at positions {i} and {j} are the same"
            );
        }
    }
}

#[track_caller]
fn assert_disjoint(
    a: &[rsleigh::Vn],
    b: &[rsleigh::Vn],
    a_label: &str,
    b_label: &str,
    case_name: &str,
) {
    for vn in a {
        assert!(
            !b.contains(vn),
            "{case_name}: {a_label} reg {vn:?} also appears in {b_label}",
        );
    }
}

/// Documented register count per category, pairwise distinct varnodes, and
/// disjoint arg / callee-saved sets.
#[test]
fn presets_resolve_correct_register_sets() {
    for c in cases() {
        let (built, _) = build_case(&c);
        assert_eq!(
            built.arg_passing_regs.len(),
            c.arg_count,
            "{}: args",
            c.name
        );
        assert_eq!(
            built.callee_saved_regs.len(),
            c.callee_saved_count + c.callee_saved_float_count,
            "{}: callee-saved",
            c.name
        );
        assert_eq!(
            built.ret_val_regs.len(),
            c.ret_count,
            "{}: return values",
            c.name
        );
        assert_all_distinct(&built.arg_passing_regs, c.name);
        assert_all_distinct(&built.callee_saved_regs, c.name);
        assert_all_distinct(&built.ret_val_regs, c.name);
        assert_disjoint(
            &built.arg_passing_regs,
            &built.callee_saved_regs,
            "arg_passing_regs",
            "callee_saved_regs",
            c.name,
        );
        assert_disjoint(
            &built.ret_val_regs,
            &built.callee_saved_regs,
            "ret_val_regs",
            "callee_saved_regs",
            c.name,
        );
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
        assert_eq!(
            saved_float.len(),
            c.callee_saved_float_count,
            "{}: callee-saved float registers follow the GPRs",
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

/// `aarch64_aapcs64`, `arm_aapcs`, MIPS o32/n64, and the PowerPC presets list
/// their LR in `callee_saved_regs` even though the official ABI specs mark it
/// caller-saved/volatile, so the indirect-branch resolver's `LinkRegister` arm
/// fires on functions returning via the entry LR.  Pins that the two lookup
/// paths agree: `link_register_reg_name` resolution AND the
/// `callee_saved_regs` list.
#[test]
fn link_register_vn_resolves_to_callee_saved_lr() {
    for c in link_reg_cases() {
        let Some(_) = c.expected_lr_name else {
            // Covered by `link_register_vn_none_for_stack_push_presets`.
            continue;
        };
        let regs = regs_for((c.arch)());
        let built = (c.cc)()
            .build(&regs)
            .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", c.name));
        let lr_vn = built
            .link_register_vn
            .unwrap_or_else(|| panic!("{}: link_register_vn must be Some", c.name));
        assert!(
            built.callee_saved_regs.contains(&lr_vn),
            "{}: link-register varnode must be present in callee_saved_regs \
             (the deliberate-tradeoff invariant); got callee_saved_regs={:?}",
            c.name,
            built.callee_saved_regs,
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
fn every_preset_factory_resolves() {
    // Catches a wrapper appended without its `CC_PRESETS` row (or with a
    // misspelled name) before the production panic in `cc_from_table` fires.
    let factories: &[(&str, fn() -> CallingConvention)] = &[
        ("x86_64_systemv", CallingConvention::x86_64_systemv),
        ("aarch64_aapcs64", CallingConvention::aarch64_aapcs64),
        ("arm_aapcs", CallingConvention::arm_aapcs),
        ("arm_aapcs_soft", CallingConvention::arm_aapcs_soft),
        ("mips_o32", CallingConvention::mips_o32),
        ("mips_n64", CallingConvention::mips_n64),
        ("powerpc_sysv32", CallingConvention::powerpc_sysv32),
        ("powerpc64_elf_v1", CallingConvention::powerpc64_elf_v1),
        ("powerpc64_elf_v2", CallingConvention::powerpc64_elf_v2),
        ("x86_cdecl", CallingConvention::x86_cdecl),
        ("x86_linux_kernel", CallingConvention::x86_linux_kernel),
    ];
    for (name, factory) in factories {
        let row = lookup_preset(name)
            .unwrap_or_else(|| panic!("preset {name:?} missing from CC_PRESETS"));
        assert_eq!(
            row.cc,
            factory(),
            "preset {name:?}: CC_PRESETS row does not match factory output",
        );
    }
    assert_eq!(
        CC_PRESETS.len(),
        factories.len(),
        "CC_PRESETS has {} rows but every_preset_factory_resolves lists {} factories",
        CC_PRESETS.len(),
        factories.len(),
    );
}

/// Positional-argument layout is derived on demand from `arg_passing_regs`
/// plus the `stack_args` formula.  x86_64 SysV (6 register args, stack from
/// +8) and x86 cdecl (stack-only, from +4) between them cover every path.
#[test]
fn positional_arg_layout_x86_64_systemv() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention::x86_64_systemv().build(&regs).unwrap();
    assert_eq!(cc.arg_passing_regs.len(), 6);
    let stack = cc.stack_args.unwrap();
    // The first stack positional is ordinal 6, after the 6 register args.
    assert_eq!(stack.offset_of(0), 8);
    assert_eq!(stack.offset_of(2), 24);
    assert_eq!(cc.arg_passing_regs[0], regs.name_to_vn("RDI").unwrap());
}

#[test]
fn positional_arg_layout_x86_cdecl_stack_only() {
    let regs = regs_for(crate::arch::SleighArch::x86());
    let cc = CallingConvention::x86_cdecl().build(&regs).unwrap();

    // No register args: slots start at index 0, offset +4, 4-byte stride.
    assert!(cc.arg_passing_regs.is_empty());
    let stack = cc.stack_args.unwrap();
    assert_eq!(stack.offset_of(0), 4);
    assert_eq!(stack.offset_of(1), 8);
}

/// MIPS O32's 16-byte shadow space puts the first stack positional (ordinal
/// 4, after the 4 register args) at SP+16.  Pins that `base_offset: 16` flows
/// through the register-then-stack indexing.
#[test]
fn positional_arg_layout_mips_o32_first_stack_arg_at_sp_plus_16() {
    let regs = regs_for(crate::arch::SleighArch::mipsbe32());
    let cc = CallingConvention::mips_o32().build(&regs).unwrap();
    assert_eq!(cc.arg_passing_regs.len(), 4);
    let stack = cc.stack_args.unwrap();
    assert_eq!(stack.offset_of(0), 16);
    assert_eq!(stack.offset_of(1), 20);
}

/// A below-base offset (a decoded negative SP delta) gives `None` rather than
/// wrapping the unsigned slot arithmetic.
#[test]
fn stack_args_below_base_negative_offset_is_none() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs {
        base_offset: 0,
        increment: 8,
    };
    assert_eq!(s.slot_of(-8), None);
    assert_eq!(s.slot_of(i128::MIN), None);
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

#[test]
fn stack_args_offset_of_series() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs {
        base_offset: 8,
        increment: 8,
    };
    assert_eq!(s.offset_of(0), 8);
    assert_eq!(s.offset_of(3), 32);
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

#[test]
fn stack_args_slot_of_floors_by_increment() {
    use crate::calling_convention::StackArgs;
    // 4-byte cdecl stride.  With no upper size bound, an 8-byte `double`
    // anchored at sp+4 lands in slot 0 despite spanning slots 0 and 1, and a
    // mid-slot sub-field read lands in the slot it starts in.
    let s = StackArgs {
        base_offset: 4,
        increment: 4,
    };
    assert_eq!(s.slot_of(4), Some(0)); // arg 0's first byte
    assert_eq!(s.slot_of(5), Some(0)); // mid-slot sub-field read of arg 0
    assert_eq!(s.slot_of(7), Some(0)); // last byte still in slot 0
    assert_eq!(s.slot_of(8), Some(1)); // next slot boundary
    assert_eq!(s.slot_of(12), Some(2));
    assert_eq!(s.slot_of(0), None); // below base
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

/// GHIDRA's ARM sla puts `d0`/`d1` and `q0` at the same register offset
/// (0x300), so a function touching `q0`/`q1` tracks those as the containers
/// and `d0..d3` collapse onto them.  Mapping the return list through
/// `container_of` then yields `q0, q0, q1, q1`, which the lifter concatenates
/// into a `Call`'s output varnodes and `validate_call_output_vns` rejects.
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
    assert_all_distinct(&ret_vals, "ret_vals with q0/q1 tracked");
}

/// Float / vector argument registers, per psABI, plus proof each name resolves
/// on every arch that uses the preset.  A float argument sits in a register
/// file the integer `arg_passing_regs` never names, so without this list the
/// whole argument cone is unrooted at the call site and DCE deletes it.
#[test]
fn float_arg_registers_match_the_psabi() {
    type CcFn = fn() -> CallingConvention;
    type ArchFn = fn() -> crate::arch::SleighArch;
    const XMM: &[&str] = &[
        "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5", "XMM6", "XMM7",
    ];
    const Q: &[&str] = &["q0", "q1", "q2", "q3", "q4", "q5", "q6", "q7"];
    const D: &[&str] = &["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"];
    // SysV PPC32 passes eight floats in registers; both PPC64 ABIs pass 13.
    const F1_F8: &[&str] = &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"];
    const F1_F13: &[&str] = &[
        "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13",
    ];
    const O32: &[&str] = &["f12", "f14"];
    const N64: &[&str] = &["f12", "f13", "f14", "f15", "f16", "f17", "f18", "f19"];

    let cases: &[(&str, CcFn, ArchFn, &[&str])] = &[
        // SysV AMD64 psABI 3.2.3: SSE-class arguments in %xmm0..%xmm7.
        (
            "x86-64 SysV",
            CallingConvention::x86_64_systemv,
            crate::arch::SleighArch::x86_64,
            XMM,
        ),
        // Intel386 psABI: every float argument goes on the stack.
        (
            "x86 cdecl",
            CallingConvention::x86_cdecl,
            crate::arch::SleighArch::x86,
            &[],
        ),
        (
            "x86 Linux kernel",
            CallingConvention::x86_linux_kernel,
            crate::arch::SleighArch::x86,
            &[],
        ),
        // AAPCS64 6.4.1: SIMD/FP arguments in v0..v7.
        (
            "AArch64 AAPCS64",
            CallingConvention::aarch64_aapcs64,
            crate::arch::SleighArch::aarch64,
            Q,
        ),
        (
            "AArch64 AAPCS64 (BE)",
            CallingConvention::aarch64_aapcs64,
            crate::arch::SleighArch::aarch64be,
            Q,
        ),
        // AAPCS-VFP: d0..d7 (also viewed as s0..s15).
        (
            "ARM AAPCS",
            CallingConvention::arm_aapcs,
            crate::arch::SleighArch::arm,
            D,
        ),
        (
            "ARM AAPCS (BE)",
            CallingConvention::arm_aapcs,
            crate::arch::SleighArch::arm_be,
            D,
        ),
        (
            "ARM AAPCS (Thumb)",
            CallingConvention::arm_aapcs,
            crate::arch::SleighArch::arm_thumb,
            D,
        ),
        // MIPS O32: the first two float arguments in $f12 / $f14.
        (
            "MIPS O32 (LE)",
            CallingConvention::mips_o32,
            crate::arch::SleighArch::mipsle32,
            O32,
        ),
        (
            "MIPS O32 (BE)",
            CallingConvention::mips_o32,
            crate::arch::SleighArch::mipsbe32,
            O32,
        ),
        // MIPS N64: eight float argument registers, $f12..$f19.
        (
            "MIPS N64 (LE)",
            CallingConvention::mips_n64,
            crate::arch::SleighArch::mipsle64,
            N64,
        ),
        (
            "MIPS N64 (BE)",
            CallingConvention::mips_n64,
            crate::arch::SleighArch::mipsbe64,
            N64,
        ),
        // PowerPC SysV 32: f1..f8; ELFv1 / ELFv2: f1..f13.
        (
            "PowerPC SysV 32 (BE)",
            CallingConvention::powerpc_sysv32,
            crate::arch::SleighArch::ppc32be,
            F1_F8,
        ),
        (
            "PowerPC SysV 32 (LE)",
            CallingConvention::powerpc_sysv32,
            crate::arch::SleighArch::ppc32le,
            F1_F8,
        ),
        (
            "PowerPC ELFv1 (BE)",
            CallingConvention::powerpc64_elf_v1,
            crate::arch::SleighArch::ppc64be,
            F1_F13,
        ),
        (
            "PowerPC ELFv2 (LE)",
            CallingConvention::powerpc64_elf_v2,
            crate::arch::SleighArch::ppc64le,
            F1_F13,
        ),
    ];

    for &(name, cc_fn, arch_fn, expected) in cases {
        let cc = cc_fn();
        assert_eq!(
            cc.arg_passing_regs_float, expected,
            "{name}: float argument registers",
        );
        let built = cc
            .build(&regs_for(arch_fn()))
            .unwrap_or_else(|e| panic!("{name}: build failed: {e:?}"));
        assert_eq!(
            built.arg_passing_regs_float.len(),
            expected.len(),
            "{name}: every float argument register name must resolve",
        );
        assert_all_distinct(&built.arg_passing_regs_float, name);
    }
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

/// Every float-capable psABI reserves part of the FP register file, and real
/// compilers rely on it: `aarch64-gcc -O2` keeps a `double` in `d8` across a
/// `bl` with no reload.  Without these entries the post-call read of the
/// register resolves to a `Call` output instead of the incoming value.
#[test]
fn callee_saved_float_registers_match_the_psabi() {
    type CcFn = fn() -> CallingConvention;
    type ArchFn = fn() -> crate::arch::SleighArch;
    // AAPCS 5.1.2.1 / AAPCS64 6.1.2: d8-d15 (AAPCS64 preserves the low 64
    // bits only).
    const D8_D15: &[&str] = &["d8", "d9", "d10", "d11", "d12", "d13", "d14", "d15"];
    // MIPS o32: $f20-$f31.
    const F20_F31: &[&str] = &[
        "f20", "f21", "f22", "f23", "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31",
    ];
    // MIPS n64: $f24-$f31.
    const F24_F31: &[&str] = &["f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31"];
    // PowerPC SysV / ELFv1 / ELFv2: f14-f31.
    const F14_F31: &[&str] = &[
        "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25", "f26",
        "f27", "f28", "f29", "f30", "f31",
    ];

    let cases: &[(&str, CcFn, ArchFn, &[&str])] = &[
        (
            "ARM AAPCS",
            CallingConvention::arm_aapcs,
            crate::arch::SleighArch::arm,
            D8_D15,
        ),
        // The VFP file is preserved the same way under either float variant.
        (
            "ARM AAPCS (soft-float)",
            CallingConvention::arm_aapcs_soft,
            crate::arch::SleighArch::arm,
            D8_D15,
        ),
        (
            "ARM AAPCS (BE32)",
            CallingConvention::arm_aapcs,
            crate::arch::SleighArch::arm_be,
            D8_D15,
        ),
        (
            "ARM AAPCS (BE8)",
            CallingConvention::arm_aapcs,
            crate::arch::SleighArch::arm_be_kernel,
            D8_D15,
        ),
        (
            "ARM AAPCS (Thumb)",
            CallingConvention::arm_aapcs,
            crate::arch::SleighArch::arm_thumb,
            D8_D15,
        ),
        (
            "AArch64 AAPCS64",
            CallingConvention::aarch64_aapcs64,
            crate::arch::SleighArch::aarch64,
            D8_D15,
        ),
        (
            "AArch64 AAPCS64 (BE)",
            CallingConvention::aarch64_aapcs64,
            crate::arch::SleighArch::aarch64be,
            D8_D15,
        ),
        (
            "MIPS O32 (LE)",
            CallingConvention::mips_o32,
            crate::arch::SleighArch::mipsle32,
            F20_F31,
        ),
        (
            "MIPS O32 (BE)",
            CallingConvention::mips_o32,
            crate::arch::SleighArch::mipsbe32,
            F20_F31,
        ),
        (
            "MIPS N64 (LE)",
            CallingConvention::mips_n64,
            crate::arch::SleighArch::mipsle64,
            F24_F31,
        ),
        (
            "MIPS N64 (BE)",
            CallingConvention::mips_n64,
            crate::arch::SleighArch::mipsbe64,
            F24_F31,
        ),
        (
            "PowerPC SysV 32 (BE)",
            CallingConvention::powerpc_sysv32,
            crate::arch::SleighArch::ppc32be,
            F14_F31,
        ),
        (
            "PowerPC ELFv1 (BE)",
            CallingConvention::powerpc64_elf_v1,
            crate::arch::SleighArch::ppc64be,
            F14_F31,
        ),
        (
            "PowerPC ELFv2 (LE)",
            CallingConvention::powerpc64_elf_v2,
            crate::arch::SleighArch::ppc64le,
            F14_F31,
        ),
    ];

    for &(name, cc_fn, arch_fn, expected) in cases {
        let cc = cc_fn();
        for want in expected {
            assert!(
                cc.callee_saved_regs.contains(want),
                "{name}: {want} is callee-saved by the psABI but absent from \
                 callee_saved_regs",
            );
        }
        let regs = regs_for(arch_fn());
        let built = cc
            .build(&regs)
            .unwrap_or_else(|e| panic!("{name}: build failed: {e:?}"));
        for want in expected {
            let vn = vn_for_name(&regs, want).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(
                built.callee_saved_regs.contains(&vn),
                "{name}: {want} did not resolve into callee_saved_regs",
            );
        }
        assert_all_distinct(&built.callee_saved_regs, name);
    }
}

/// AAPCS64 6.1.2 preserves only the LOW 64 bits of v8-v15, so naming the `q`
/// views here would claim their upper halves preserved.
#[test]
fn aarch64_callee_saved_float_regs_name_the_low_64_bit_views() {
    let regs = regs_for(crate::arch::SleighArch::aarch64());
    let built = CallingConvention::aarch64_aapcs64()
        .build(&regs)
        .expect("aarch64_aapcs64 must build");
    for n in 8..16u32 {
        let d = vn_for_name(&regs, &format!("d{n}")).expect("aarch64 sla defines d8..d15");
        assert_eq!(d.size, 8, "d{n} is the low 64-bit view");
        assert!(
            built.callee_saved_regs.contains(&d),
            "d{n} must be callee-saved",
        );
        let q = vn_for_name(&regs, &format!("q{n}")).expect("aarch64 sla defines q8..q15");
        assert!(
            !built.callee_saved_regs.contains(&q),
            "q{n} covers 128 bits, of which only the low 64 are preserved",
        );
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
/// |callee_saved| * |tracked| per call, 39 * |tracked| on PPC64.
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

/// `-mfloat-abi=soft` / `softfp` passes floats and returns them in the core
/// registers, so naming the VFP bank would seed `d0`-`d7` on every function
/// and hang phantom float arguments off every call site.
#[test]
fn arm_soft_float_preset_names_no_vfp_registers() {
    let cc = CallingConvention::arm_aapcs_soft();
    assert_eq!(cc.arg_passing_regs_float, &[] as &[&str]);
    assert_eq!(cc.ret_val_regs_float, &[] as &[&str]);
    assert_eq!(
        cc.arg_passing_regs,
        CallingConvention::arm_aapcs().arg_passing_regs,
        "the core-register geometry is the hard-float preset's",
    );
    for arch in [
        crate::arch::SleighArch::arm,
        crate::arch::SleighArch::arm_be,
        crate::arch::SleighArch::arm_be_kernel,
        crate::arch::SleighArch::arm_thumb,
    ] {
        let built = cc
            .build(&regs_for(arch()))
            .expect("arm_aapcs_soft must build on every ARM32 preset");
        assert!(built.arg_passing_regs_float.is_empty());
        assert!(built.ret_val_regs_float.is_empty());
    }
}
