use super::*;

fn regs_for(arch: crate::arch::SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .unwrap()
        .regs()
        .unwrap()
}

/// One row describes a supported calling convention and everything we
/// expect `build()` to produce for it.  Adding a new convention means
/// adding one entry here — every invariant test picks it up.
struct Case {
    name: &'static str,
    cc: fn() -> CallingConvention,
    arch: fn() -> crate::arch::SleighArch,
    arg_count: usize,
    callee_saved_count: usize,
    ret_count: usize,
    reg_size_bytes: u32,
    stack_ptr_name: &'static str,
    stack_arg_offsets: &'static [i64],
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
            stack_arg_offsets: &[8, 16, 24, 32, 40, 48],
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
            stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
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
            stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
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
            stack_arg_offsets: &[0, 8, 16, 24],
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
            stack_arg_offsets: &[16, 20, 24, 28],
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
            stack_arg_offsets: &[16, 20, 24, 28],
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
            stack_arg_offsets: &[0, 8, 16, 24],
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
            stack_arg_offsets: &[0, 8, 16, 24],
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
            stack_arg_offsets: &[8, 12, 16, 20, 24, 28, 32, 36],
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
            stack_arg_offsets: &[8, 12, 16, 20, 24, 28, 32, 36],
            ret_stack_pop: 0,
        },
        Case {
            name: "PowerPC ELFv1 (BE)",
            cc: CallingConvention::powerpc64_elf_v1,
            arch: crate::arch::SleighArch::ppc64be,
            arg_count: 8,
            // r2 + r14..r31 (18) + LR — round 9 wave 24 added LR per
            // CLAUDE.md deliberate-tradeoff (consistent with PPC32).
            callee_saved_count: 20,
            ret_count: 2,
            reg_size_bytes: 8,
            stack_ptr_name: "r1",
            stack_arg_offsets: &[48, 56, 64, 72],
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
            stack_arg_offsets: &[32, 40, 48, 56],
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
            stack_arg_offsets: &[0, 8, 16, 24],
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
            stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
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
            stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
            ret_stack_pop: 0,
        },
        // ── Linux kernel + syscall presets ────────────────────────
        // Kernel-internal CCs that are aliases of their userland
        // counterparts inherit the same register counts; the
        // factories return identical `CallingConvention` values, so
        // a separate Case row would be redundant — covered by the
        // existing arch-specific row above.  Only `x86_linux_kernel`
        // (regparm-3) and the `*_linux_syscall` presets are listed
        // here because they declare distinct register sets.
        Case {
            name: "x86 Linux kernel (regparm-3)",
            cc: CallingConvention::x86_linux_kernel,
            arch: crate::arch::SleighArch::x86,
            arg_count: 3,            // EAX, EDX, ECX
            callee_saved_count: 4,   // EBX, ESI, EDI, EBP
            ret_count: 2,            // EAX, EDX
            reg_size_bytes: 4,
            stack_ptr_name: "ESP",
            stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
            ret_stack_pop: 4,
        },
        Case {
            name: "x86 Linux syscall (int 0x80)",
            cc: CallingConvention::x86_linux_syscall,
            arch: crate::arch::SleighArch::x86,
            arg_count: 6,            // EBX, ECX, EDX, ESI, EDI, EBP
            callee_saved_count: 0,   // every cdecl-callee-saved reg is consumed as an arg
            ret_count: 1,            // EAX
            reg_size_bytes: 4,
            stack_ptr_name: "ESP",
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        },
        Case {
            name: "x86_64 Linux syscall",
            cc: CallingConvention::x86_64_linux_syscall,
            arch: crate::arch::SleighArch::x86_64,
            arg_count: 6,            // RDI, RSI, RDX, R10, R8, R9
            callee_saved_count: 6,   // unchanged from SysV
            ret_count: 1,            // RAX
            reg_size_bytes: 8,
            stack_ptr_name: "RSP",
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        },
        Case {
            name: "AArch64 Linux syscall",
            cc: CallingConvention::aarch64_linux_syscall,
            arch: crate::arch::SleighArch::aarch64,
            arg_count: 6,            // x0..x5
            callee_saved_count: 12,  // unchanged from AAPCS64
            ret_count: 1,            // x0
            reg_size_bytes: 8,
            stack_ptr_name: "sp",
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        },
        Case {
            name: "ARM Linux syscall",
            cc: CallingConvention::arm_linux_syscall,
            arch: crate::arch::SleighArch::arm,
            arg_count: 7,            // r0..r6
            callee_saved_count: 5,   // r8, r9, r10, r11, lr (r4..r7 stripped)
            ret_count: 1,            // r0
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        },
        Case {
            name: "MIPS Linux syscall (O32)",
            cc: CallingConvention::mips_linux_syscall_o32,
            arch: crate::arch::SleighArch::mipsle32,
            arg_count: 4,            // a0..a3
            callee_saved_count: 11,  // unchanged from O32
            ret_count: 1,            // v0
            reg_size_bytes: 4,
            stack_ptr_name: "sp",
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        },
        Case {
            name: "MIPS Linux syscall (N64)",
            cc: CallingConvention::mips_linux_syscall_n64,
            arch: crate::arch::SleighArch::mipsle64,
            arg_count: 6,            // a0..a3, t0..t1 (= $4..$9 in N64)
            callee_saved_count: 11,  // unchanged from N64
            ret_count: 1,            // v0
            reg_size_bytes: 8,
            stack_ptr_name: "sp",
            stack_arg_offsets: &[],
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
        assert_eq!(built.arg_passing_regs().len(), c.arg_count, "{}: args", c.name);
        assert_eq!(
            built.callee_saved_regs().len(),
            c.callee_saved_count,
            "{}: callee-saved",
            c.name
        );
        assert_eq!(
            built.ret_val_regs().len(),
            c.ret_count,
            "{}: return values",
            c.name
        );
        assert_all_distinct(built.arg_passing_regs(), c.name);
        assert_all_distinct(built.callee_saved_regs(), c.name);
        assert_all_distinct(built.ret_val_regs(), c.name);
        assert_disjoint(
            built.arg_passing_regs(),
            built.callee_saved_regs(),
            "arg_passing_regs",
            "callee_saved_regs",
            c.name,
        );
        assert_disjoint(
            built.ret_val_regs(),
            built.callee_saved_regs(),
            "ret_val_regs",
            "callee_saved_regs",
            c.name,
        );
    }
}

/// Every register resolved by a preset (including the stack pointer) must
/// have the architecture's natural word size.  SP is included because
/// `StackStoreDetect` and the analyzer's stack-arg machinery assume an
/// SP-sized address — an undersized SP would silently miscompute offsets
/// downstream and produce no diagnostic from this crate.
#[test]
fn presets_resolved_registers_have_expected_size() {
    for c in cases() {
        let (built, _) = build_case(&c);
        for vn in built
            .arg_passing_regs()
            .iter()
            .chain(built.callee_saved_regs())
            .chain(built.ret_val_regs())
            .chain(std::iter::once(&built.stack_ptr_vn()))
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
        assert_eq!(built.stack_ptr_vn(), sp, "{}: stack_ptr_vn", c.name);
        for (label, set) in [
            ("arg_passing_regs", built.arg_passing_regs()),
            ("callee_saved_regs", built.callee_saved_regs()),
            ("ret_val_regs", built.ret_val_regs()),
        ] {
            assert!(
                !set.contains(&built.stack_ptr_vn()),
                "{}: stack pointer must not appear in {label}",
                c.name,
            );
        }
        assert_eq!(
            built.stack_arg_offsets(),
            c.stack_arg_offsets.to_vec(),
            "{}: stack_arg_offsets",
            c.name,
        );
        assert_eq!(
            built.ret_stack_pop(), c.ret_stack_pop,
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
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            syscall_number_reg_name: None,
            no_memory_clobber: false,
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
        stack_arg_offsets: &[],
        ret_stack_pop: 0,
        link_register_reg_name: None,
        syscall_number_reg_name: None,
        no_memory_clobber: false,
    };
    assert!(cc.build(&regs).is_err(), "a list with one bad name must fail");
}

#[test]
#[ignore = "diagnostic — uncomment locally to print MIPS register names"]
fn dump_mips_register_names() {
    let arch = crate::arch::SleighArch::mipsle32();
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .unwrap().regs().unwrap();
    let candidates = ["a0", "a1", "a2", "a3", "v0", "v1", "sp", "ra",
                      "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7",
                      "s8", "fp", "gp",
                      "A0", "V0", "SP", "RA", "S0", "FP", "GP",
                      "r4", "r16", "r28", "r29", "r30", "r31"];
    for n in candidates {
        let v = regs.name_to_vn(n);
        println!("name {n:?} -> {v:?}");
    }
}

/// One row per calling-convention preset, recording the expected
/// link-register Sleigh name (or `None` for stack-push ISAs that hold
/// the return address on the stack).  Drives every link-register
/// invariant test below; adding a new preset means adding one row here
/// and every test picks it up.
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
            .build(&regs)
            .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", c.name));
        let expected_vn = regs.name_to_vn(expected_name).unwrap_or_else(|| {
            panic!(
                "{}: expected LR name {:?} must resolve in arch's Sleigh regs",
                c.name, expected_name,
            )
        });
        assert_eq!(
            built.link_register_vn(),
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
            .build(&regs)
            .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", c.name));
        assert!(
            built.link_register_vn().is_none(),
            "{}: link_register_vn must be None on stack-push ISAs, got {:?}",
            c.name,
            built.link_register_vn(),
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
/// list) agree for every link-register preset.  /// previously only ARM was pinned; AArch64 / MIPS / PPC could drop
/// their LR from `callee_saved_regs` without triggering this test.
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
            .build(&regs)
            .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", c.name));
        let lr_vn = built
            .link_register_vn()
            .unwrap_or_else(|| panic!("{}: link_register_vn must be Some", c.name));
        assert!(
            built.callee_saved_regs().contains(&lr_vn),
            "{}: link-register varnode must be present in callee_saved_regs \
             (CLAUDE.md deliberate-tradeoff invariant); got callee_saved_regs={:?}",
            c.name,
            built.callee_saved_regs(),
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
        stack_arg_offsets: &[],
        ret_stack_pop: 0,
        link_register_reg_name: None,
        syscall_number_reg_name: None,
        no_memory_clobber: false,
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
#[ignore = "probe float registers to verify names across architectures — uncomment locally to print results"]
fn probe_float_regs() {
fn try_resolve(arch: crate::arch::SleighArch, names: &[&str]) {
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe).unwrap().regs().unwrap();
    for n in names {
        let v = regs.name_to_vn(n);
        println!("  {n:?} -> {v:?}");
    }
}
println!("=== aarch64 ===");
try_resolve(crate::arch::SleighArch::aarch64(), &[
    "q0", "q1", "v0", "v1", "d0", "d1", "s0", "s1", "Q0", "V0", "D0", "S0",
]);
println!("=== x86_64 ===");
try_resolve(crate::arch::SleighArch::x86_64(), &[
    "XMM0", "XMM1", "xmm0", "xmm1", "ST0", "ST1", "st0",
]);
println!("=== arm ===");
try_resolve(crate::arch::SleighArch::arm(), &[
    "s0", "s1", "d0", "d1", "S0", "D0",
]);
println!("=== x86 ===");
try_resolve(crate::arch::SleighArch::x86(), &[
    "XMM0", "ST0", "st0",
]);
println!("=== mips32le ===");
try_resolve(crate::arch::SleighArch::mipsle32(), &[
    "f0", "f1", "f2", "f3", "f12", "F0", "F12",
]);
}

// ── no_memory_clobber field ──────────────────────────────────────────────────

#[test]
fn x86_64_all_preserving_has_no_memory_clobber_true() {
    // The "all-preserving" CC (used for __fentry__ / mcount-style hooks)
    // promises zero observable side-effects.  The Call's memory output must
    // be suppressible at IR-build time so LoadReadOnly / StackLoadForward
    // can forward across these calls.
    assert!(
        CallingConvention::x86_64_all_preserving().no_memory_clobber(),
        "x86_64_all_preserving must declare no_memory_clobber = true"
    );
}

#[test]
fn standard_presets_have_no_memory_clobber_false() {
    // Every standard preset must keep the default no_memory_clobber = false
    // so its Call nodes correctly clobber memory.  Only x86_64_all_preserving
    // opts out.
    let presets: &[(&str, CallingConvention)] = &[
        ("x86_64_systemv", CallingConvention::x86_64_systemv()),
        ("x86_cdecl", CallingConvention::x86_cdecl()),
        ("aarch64_aapcs64", CallingConvention::aarch64_aapcs64()),
        ("arm_aapcs", CallingConvention::arm_aapcs()),
        ("mips_o32", CallingConvention::mips_o32()),
        ("mips_n64", CallingConvention::mips_n64()),
        ("powerpc_sysv32", CallingConvention::powerpc_sysv32()),
        ("powerpc64_elf_v1", CallingConvention::powerpc64_elf_v1()),
        ("powerpc64_elf_v2", CallingConvention::powerpc64_elf_v2()),
    ];
    for (name, cc) in presets {
        assert!(
            !cc.no_memory_clobber(),
            "{name}: standard presets must have no_memory_clobber = false"
        );
    }
}
