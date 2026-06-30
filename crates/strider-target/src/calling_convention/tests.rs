use super::*;

fn regs_for(arch: crate::arch::SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .unwrap()
        .regs()
        .unwrap()
}

/// PPC System V (32-bit and both PPC64 ELF variants) returns a scalar
/// floating-point value in `f1` only.  `f2`–`f13` are volatile
/// argument/scratch float registers, not return registers — so the
/// float-return list must be exactly `["f1"]`.
#[test]
fn ppc_float_return_is_f1_only() {
    for cc in [
        CallingConvention::powerpc_sysv32(),
        CallingConvention::powerpc64_elf_v1(),
        CallingConvention::powerpc64_elf_v2(),
    ] {
        let cc = cc.expect("PPC preset exists");
        assert_eq!(
            cc.ret_val_regs_float,
            &["f1"],
            "PPC SysV returns floats only in f1, not f2",
        );
    }
}

/// One row describes a supported calling convention and everything we
/// expect `build()` to produce for it.  Adding a new convention means
/// adding one entry here — every invariant test picks it up.
struct Case {
    name: &'static str,
    cc: fn() -> std::result::Result<CallingConvention, MissingPresetError>,
    arch: fn() -> crate::arch::SleighArch,
    arg_count: usize,
    callee_saved_count: usize,
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
            callee_saved_count: 19,
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
            callee_saved_count: 19,
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
            // r2 + r14..r31 (18) + LR — added LR per
            // CLAUDE.md deliberate-tradeoff (consistent with PPC32).
            callee_saved_count: 20,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "r1",
            stack_args: Some(StackArgs {
                base_offset: 48,
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
            callee_saved_count: 20,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "r1",
            stack_args: Some(StackArgs {
                base_offset: 32,
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
            ret_count: 2,
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
        },
        // ── Linux kernel-internal preset ──────────────────────────
        // Only `x86_linux_kernel` (regparm-3) declares a register set
        // distinct from its userland counterpart, so it is the sole
        // kernel row here; every other arch's kernel CC is identical to
        // the userland preset covered above.
        Case {
            name: "x86 Linux kernel (regparm-3)",
            cc: CallingConvention::x86_linux_kernel,
            arch: crate::arch::SleighArch::x86,
            arg_count: 3,          // EAX, EDX, ECX
            callee_saved_count: 4, // EBX, ESI, EDI, EBP
            ret_count: 2,          // EAX, EDX
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
        .unwrap()
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

/// Every preset must resolve to the documented number of registers in
/// each category, with pairwise distinct varnodes and disjoint arg/
/// callee-saved sets.
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
            c.callee_saved_count,
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

/// Every register resolved by a preset (including the stack pointer) must
/// have the architecture's natural word size.  SP is included because
/// `StackOffsetDetect` and the analyzer's stack-arg machinery assume an
/// SP-sized address — an undersized SP would silently miscompute offsets
/// downstream and produce no diagnostic from this crate.
#[test]
fn presets_resolved_registers_have_expected_size() {
    for c in cases() {
        let (built, _) = build_case(&c);
        for vn in built
            .arg_passing_regs
            .iter()
            .chain(&built.callee_saved_regs)
            .chain(&built.ret_val_regs)
            .chain(std::iter::once(&built.stack_vn))
        {
            assert_eq!(
                vn.size, c.reg_size_bytes,
                "{}: expected {}-byte register, got {vn:?}",
                c.name, c.reg_size_bytes,
            );
        }
    }
}

/// The stack-pointer varnode must resolve to the architecture's SP
/// register and must NOT appear in any of the three resolved register
/// lists (`arg_passing_regs`, `callee_saved_regs`, `ret_val_regs`) —
/// the callee's `ret` pops the return address on stack-push ISAs so
/// SP is not preserved across a call, and on link-register ISAs the
/// call doesn't touch SP but SP is still modeled as not callee-saved
/// for uniformity (with `ret_stack_pop = 0`).  Stack-arg offsets and
/// `ret_stack_pop` must round-trip unchanged from the preset.
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

/// An unknown register name in any category must return an error,
/// regardless of architecture.
#[test]
fn build_returns_error_for_unknown_register_name() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    for bad_name in &["NOTAREG", "", "rax_FAKE"] {
        let cc = CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: std::slice::from_ref(bad_name),
            callee_saved_regs: &[],
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            stack_args: None,
            ret_stack_pop: 0,
            link_register_reg_name: None,
            preserves_memory: false,
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

/// An error on the first unknown name must short-circuit rather than
/// silently succeeding for the remaining valid names.
#[test]
fn build_returns_error_even_when_some_names_are_valid() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention {
        stack_ptr_reg_name: "RSP",
        arg_passing_regs: &["RDI", "NOT_A_REG", "RSI"],
        callee_saved_regs: &[],
        ret_val_regs: &[],
        ret_val_regs_float: &[],
        stack_args: None,
        ret_stack_pop: 0,
        link_register_reg_name: None,
        preserves_memory: false,
    };
    assert!(
        cc.build(&regs).is_err(),
        "a list with one bad name must fail"
    );
}

// To inspect Sleigh register names for an arch during development, build a
// `rsleigh::Sleigh` from the arch's `.sla` + `.pspec`, call `regs()`, and probe
// `name_to_vn(...)` for each candidate name.

/// One row per calling-convention preset, recording the expected
/// link-register Sleigh name (or `None` for stack-push ISAs that hold
/// the return address on the stack).  Drives every link-register
/// invariant test below; adding a new preset means adding one row here
/// and every test picks it up.
struct LinkRegCase {
    name: &'static str,
    cc: fn() -> std::result::Result<CallingConvention, MissingPresetError>,
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

/// Every link-register ISA preset must resolve `link_register_vn` to
/// `Some(...)`, and the resolved varnode must match the architecture's
/// LR register under the documented Sleigh name.  Pinning every preset
/// here catches a typo or rename in any single convention's
/// `link_register_reg_name` field.
#[test]
fn link_register_vn_set_for_link_register_presets() {
    for c in link_reg_cases() {
        let Some(expected_name) = c.expected_lr_name else {
            continue;
        };
        let regs = regs_for((c.arch)());
        let built = (c.cc)()
            .unwrap()
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

/// Stack-push ISA presets (x86, x86_64) must have `link_register_vn`
/// resolve to `None`.  Their return address lives on the stack, not in
/// a register, so there is no LR to expose.
#[test]
fn link_register_vn_none_for_stack_push_presets() {
    for c in link_reg_cases() {
        if c.expected_lr_name.is_some() {
            continue;
        }
        let regs = regs_for((c.arch)());
        let built = (c.cc)()
            .unwrap()
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

/// CLAUDE.md "Note (link-register handling)" documents that
/// `aarch64_aapcs64`, `arm_aapcs`, MIPS o32/n64, and the PowerPC
/// presets list their LR in `callee_saved_regs` — even though the
/// official ABI specs mark them caller-saved/volatile — so the
/// indirect-branch resolver's `LinkRegister` arm fires on functions
/// returning via the entry LR.  Pin that the two lookup paths (the
/// `link_register_reg_name` resolution AND the `callee_saved_regs`
/// list) agree for every link-register preset.  AArch64 / MIPS / PPC
/// previously could drop their LR from `callee_saved_regs` without
/// triggering this test; this regression case pins the agreement.
#[test]
fn link_register_vn_resolves_to_callee_saved_lr() {
    for c in link_reg_cases() {
        let Some(_) = c.expected_lr_name else {
            // Stack-push ISAs (x86 / x86_64) have no LR — already
            // covered by `link_register_vn_none_for_stack_push_presets`.
            continue;
        };
        let regs = regs_for((c.arch)());
        let built = (c.cc)()
            .unwrap()
            .build(&regs)
            .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", c.name));
        let lr_vn = built
            .link_register_vn
            .unwrap_or_else(|| panic!("{}: link_register_vn must be Some", c.name));
        assert!(
            built.callee_saved_regs.contains(&lr_vn),
            "{}: link-register varnode must be present in callee_saved_regs \
             (CLAUDE.md deliberate-tradeoff invariant); got callee_saved_regs={:?}",
            c.name,
            built.callee_saved_regs,
        );
    }
}

/// An unknown `stack_ptr_reg_name` must surface as `UnknownRegName`, the
/// same way an unknown entry in any of the three register lists does.
/// Guards the open-coded `ok_or_else` in `build()` — the SP name has its
/// own lookup path separate from `regs_to_vns`.
#[test]
fn build_returns_error_for_unknown_stack_pointer_name() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention {
        stack_ptr_reg_name: "NOT_A_SP",
        arg_passing_regs: &[],
        callee_saved_regs: &[],
        ret_val_regs: &[],
        ret_val_regs_float: &[],
        stack_args: None,
        ret_stack_pop: 0,
        link_register_reg_name: None,
        preserves_memory: false,
    };
    let result = cc.build(&regs);
    let err = result.expect_err("expected UnknownRegName error");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown sleigh register name") && msg.contains("NOT_A_SP"),
        "expected UnknownRegName(\"NOT_A_SP\"), got {err}"
    );
}

// ── preserves_memory field ──────────────────────────────────────────────────

#[test]
fn x86_64_all_preserving_has_preserves_memory_true() {
    // The "all-preserving" CC (used for __fentry__ / mcount-style hooks)
    // promises zero observable side-effects.  The Call's memory output must
    // be suppressible at IR-build time so LoadReadOnly / LoadForward
    // can forward across these calls.
    assert!(
        CallingConvention::x86_64_all_preserving()
            .unwrap()
            .preserves_memory(),
        "x86_64_all_preserving must declare preserves_memory = true"
    );
}

#[test]
fn standard_presets_have_preserves_memory_false() {
    // Every standard preset must keep the default preserves_memory = false
    // so its Call nodes correctly clobber memory.  Only x86_64_all_preserving
    // opts out.
    let presets: &[(&str, CallingConvention)] = &[
        (
            "x86_64_systemv",
            CallingConvention::x86_64_systemv().unwrap(),
        ),
        ("x86_cdecl", CallingConvention::x86_cdecl().unwrap()),
        (
            "aarch64_aapcs64",
            CallingConvention::aarch64_aapcs64().unwrap(),
        ),
        ("arm_aapcs", CallingConvention::arm_aapcs().unwrap()),
        ("mips_o32", CallingConvention::mips_o32().unwrap()),
        ("mips_n64", CallingConvention::mips_n64().unwrap()),
        (
            "powerpc_sysv32",
            CallingConvention::powerpc_sysv32().unwrap(),
        ),
        (
            "powerpc64_elf_v1",
            CallingConvention::powerpc64_elf_v1().unwrap(),
        ),
        (
            "powerpc64_elf_v2",
            CallingConvention::powerpc64_elf_v2().unwrap(),
        ),
    ];
    for (name, cc) in presets {
        assert!(
            !cc.preserves_memory(),
            "{name}: standard presets must have preserves_memory = false"
        );
    }
}

#[test]
fn every_preset_factory_resolves() {
    // Sanity guard for the data-table layout: every named factory's
    // expected lookup string must be present in `CC_PRESETS`.  If a
    // future edit appends a wrapper without appending a row (or
    // misspells the name), this test catches it before the production
    // panic at `cc_from_table` fires.
    let factories: &[(
        &str,
        fn() -> std::result::Result<CallingConvention, MissingPresetError>,
    )] = &[
        ("x86_64_systemv", CallingConvention::x86_64_systemv),
        (
            "x86_64_all_preserving",
            CallingConvention::x86_64_all_preserving,
        ),
        ("aarch64_aapcs64", CallingConvention::aarch64_aapcs64),
        ("arm_aapcs", CallingConvention::arm_aapcs),
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
            factory().unwrap(),
            "preset {name:?}: CC_PRESETS row does not match factory output",
        );
    }
    // And the table itself must contain exactly the factories we list.
    assert_eq!(
        CC_PRESETS.len(),
        factories.len(),
        "CC_PRESETS has {} rows but every_preset_factory_resolves lists {} factories",
        CC_PRESETS.len(),
        factories.len(),
    );
}

/// The convention's positional-argument layout is derived on-demand from
/// `arg_passing_regs` (register slots) plus `stack_args` (the unbounded
/// stack-arg formula).  Verify on x86_64 SysV (6 register args + stack args
/// from +8) and x86 cdecl (stack-only, from +4) — between them they exercise
/// every layout path.
#[test]
fn positional_arg_layout_x86_64_systemv() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention::x86_64_systemv()
        .unwrap()
        .build(&regs)
        .unwrap();
    assert_eq!(cc.arg_passing_regs.len(), 6);
    let stack = cc.stack_args.unwrap();
    // First stack positional sits at ordinal 6 (after the 6 register args).
    assert_eq!(stack.offset_of(0), 8);
    assert_eq!(stack.offset_of(2), 24);
    assert_eq!(cc.arg_passing_regs[0], regs.name_to_vn("RDI").unwrap());
}

#[test]
fn positional_arg_layout_x86_cdecl_stack_only() {
    let regs = regs_for(crate::arch::SleighArch::x86());
    let cc = CallingConvention::x86_cdecl()
        .unwrap()
        .build(&regs)
        .unwrap();

    // No register args; stack slots start at index 0, offset +4 with a
    // 4-byte stride.
    assert!(cc.arg_passing_regs.is_empty());
    let stack = cc.stack_args.unwrap();
    assert_eq!(stack.offset_of(0), 4);
    assert_eq!(stack.offset_of(1), 8);
}

/// MIPS O32 reserves a 16-byte shadow space, so the first *stack* positional
/// argument (ordinal 4, after the 4 register args) sits at SP+16 — pins that
/// the `base_offset: 16` flows through the register-then-stack indexing.
#[test]
fn positional_arg_layout_mips_o32_first_stack_arg_at_sp_plus_16() {
    let regs = regs_for(crate::arch::SleighArch::mipsbe32());
    let cc = CallingConvention::mips_o32().unwrap().build(&regs).unwrap();
    assert_eq!(cc.arg_passing_regs.len(), 4);
    let stack = cc.stack_args.unwrap();
    assert_eq!(stack.offset_of(0), 16);
    assert_eq!(stack.offset_of(1), 20);
}

/// Below-base offsets (a decoded negative SP delta) degrade to `None` rather
/// than wrapping the unsigned slot arithmetic.
#[test]
fn stack_args_below_base_negative_offset_is_none() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs {
        base_offset: 0,
        increment: 8,
    };
    assert_eq!(s.index_of(-8, 8), None);
    assert_eq!(s.slot_of(-8), None);
    assert_eq!(s.slot_of(i128::MIN), None);
}

/// Layout with no positional args at all: empty register list and no stack
/// formula.
#[test]
fn positional_arg_layout_empty_has_no_stack() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention::x86_64_all_preserving()
        .unwrap()
        .build(&regs)
        .unwrap();
    assert!(cc.arg_passing_regs.is_empty());
    assert!(cc.stack_args.is_none());
}

#[test]
fn stack_args_offset_and_index() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs {
        base_offset: 8,
        increment: 8,
    };
    assert_eq!(s.offset_of(0), 8);
    assert_eq!(s.offset_of(3), 32);
    assert_eq!(s.index_of(8, 8), Some(0));
    assert_eq!(s.index_of(32, 4), Some(3)); // 4-byte load inside the 8-byte slot 3
    assert_eq!(s.index_of(0, 8), None); // below base
    assert_eq!(s.index_of(12, 8), None); // [12,20) straddles the 8|16 boundary
}

/// `offset_of` is the plain arithmetic series anchored at `base_offset`.
/// Expected values are hand-computed literals for the two real-world
/// strides (x86 cdecl 4/4, x86_64 SysV 8/8); the large-N row pins that
/// the `i64` math has plenty of headroom at any realistic index.
#[test]
fn stack_args_offset_of_literal_series() {
    use crate::calling_convention::StackArgs;
    let x86 = StackArgs {
        base_offset: 4,
        increment: 4,
    };
    assert_eq!(x86.offset_of(0), 4); // offset_of(0) == base_offset
    assert_eq!(x86.offset_of(1), 8);
    assert_eq!(x86.offset_of(7), 32); // 4 + 7*4

    let x64 = StackArgs {
        base_offset: 8,
        increment: 8,
    };
    assert_eq!(x64.offset_of(0), 8); // offset_of(0) == base_offset
    assert_eq!(x64.offset_of(1), 16);
    assert_eq!(x64.offset_of(7), 64); // 8 + 7*8

    // Large-but-reasonable N (2^40 stack args): 8 + 8*2^40 = 2^43 + 8.
    assert_eq!(x64.offset_of(1 << 40), 8_796_093_022_216);
}

/// Boundary semantics of `index_of` (strict within-one-slot) and
/// `slot_of` (floor, no size bound), parametrized over the x86 (4/4)
/// and x86_64 (8/8) strides.
#[test]
fn stack_args_index_and_slot_boundaries_per_increment() {
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

        // index_of: strict within-one-slot containment.
        assert_eq!(s.index_of(base, inc), Some(0), "{label}: exact-fit slot 0");
        assert_eq!(
            s.index_of(base - 1, 1),
            None,
            "{label}: one byte below base"
        );
        assert_eq!(
            s.index_of(base + 1, 1),
            Some(0),
            "{label}: mid-slot 1-byte read"
        );
        assert_eq!(
            s.index_of(base + inc - 1, 1),
            Some(0),
            "{label}: slot-0 last byte"
        );
        assert_eq!(
            s.index_of(base, inc + 1),
            None,
            "{label}: increment+1 bytes from base spans into slot 1 → rejected",
        );
        assert_eq!(
            s.index_of(base + inc - 1, 2),
            None,
            "{label}: 2-byte access straddling the slot boundary → rejected",
        );
        // Pinned: a zero-size access trivially fits the slot its offset
        // lands in, so `index_of(_, 0)` is `Some` for any offset >= base.
        assert_eq!(s.index_of(base, 0), Some(0), "{label}: zero-size at base");
        assert_eq!(
            s.index_of(base + inc, 0),
            Some(1),
            "{label}: zero-size at slot-1 start"
        );

        // slot_of: floor onto the containing slot.  The method takes no
        // size argument at all — the doc's "no size bound" claim — so a
        // wider-than-slot argument anchors at the slot of its first byte
        // (same answer as the 1-byte probes below).
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
    // 4-byte-stride cdecl-style.  `slot_of` floors the start byte to its
    // containing slot with no upper size bound: an 8-byte argument (a
    // `double`) anchored at sp+4 lands in slot 0 even though it spans slots
    // 0 and 1, and a mid-slot sub-field read lands in the slot it starts in.
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

/// `slots_spanned` is the cursor-advance companion to `slot_of`: how many
/// consecutive slots a `size`-byte argument occupies, `ceil(max(size,1) /
/// increment)`, never below 1.  Pinned across the 4- and 8-byte strides.
#[test]
fn stack_args_slots_spanned_ceils_by_increment() {
    use crate::calling_convention::StackArgs;
    for (label, inc) in [("x86 4/4", 4i128), ("x86_64 8/8", 8i128)] {
        let s = StackArgs {
            base_offset: inc,
            increment: inc,
        };
        // Zero / one byte → one slot (never zero).
        assert_eq!(
            s.slots_spanned(0),
            1,
            "{label}: zero-size occupies one slot"
        );
        assert_eq!(s.slots_spanned(1), 1, "{label}: one byte");
        // Exactly one slot wide.
        assert_eq!(s.slots_spanned(inc), 1, "{label}: exactly one slot");
        // One byte into the next slot rounds up to two.
        assert_eq!(s.slots_spanned(inc + 1), 2, "{label}: spills into slot 2");
        // Exactly two slots, then a byte more is three.
        assert_eq!(s.slots_spanned(2 * inc), 2, "{label}: exactly two slots");
        assert_eq!(
            s.slots_spanned(2 * inc + 1),
            3,
            "{label}: spills into slot 3"
        );
    }
}

/// A garbage decoded size near `i128::MAX` must not overflow the
/// `size + increment - 1` numerator — the span saturates instead of
/// wrapping (mirroring `offset_of`'s saturation contract).
#[test]
fn stack_args_slots_spanned_saturates_on_overflow() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs {
        base_offset: 8,
        increment: 8,
    };
    // `i128::MAX + 7` would overflow without the saturating add; the result is
    // a large-but-finite slot count, computed without panicking.
    let span = s.slots_spanned(i128::MAX);
    assert_eq!(span, (i128::MAX / 8) as usize);
}

/// Adversarial near-`i128::MAX` offsets (decoded from binary content, not a
/// trusted input) must degrade to `None`/saturation rather than panic in
/// debug or wrap in release.  `index_of`'s `offset + size` is the overflow
/// site; `offset_of`'s `base + n*increment` is the other.
#[test]
fn stack_args_slot_math_degrades_on_overflow_not_panics() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs {
        base_offset: 8,
        increment: 8,
    };
    // index_of: `offset + size` would overflow i128 → None, not a panic.
    assert_eq!(s.index_of(i128::MAX, 8), None);
    assert_eq!(s.index_of(i128::MAX - 1, 8), None);
    // slot_of with a max offset is well-defined (non-negative base) and must
    // not panic.
    assert_eq!(s.slot_of(i128::MAX), Some(((i128::MAX - 8) / 8) as usize));
    // offset_of computes `base + n*increment` saturatingly; with the wider
    // i128 intermediate (1<<62)*8 no longer overflows, so it scales exactly.
    assert_eq!(s.offset_of(1usize << 62), (1i128 << 62) * 8 + 8);
}

/// A negative `base_offset` is rejected at the construction boundary
/// (`try_new`), the same way a non-positive `increment` is.  `base_offset`
/// flows in unvalidated from the Python `CallingConvention.custom`
/// (`stack_arg_base`); without this guard a negative base lets the
/// `offset - base_offset` subtraction in `index_of` / `slot_of` overflow
/// on a garbage offset decoded from a crafted binary.
#[test]
fn try_new_rejects_negative_stack_arg_base_offset() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let sp = regs.name_to_vn("RSP").expect("x86_64 has RSP");
    let result = BuiltCallingConvention::try_new(
        vec![],
        vec![],
        vec![],
        vec![],
        sp,
        Some(StackArgs {
            base_offset: -8,
            increment: 8,
        }),
        0,
        None,
        false,
    );
    assert!(
        result.is_err(),
        "negative stack-arg base_offset must be rejected by try_new",
    );
}
