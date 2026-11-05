use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};

pub(crate) static X86: Family = Family {
    name: "x86",
    ghidra_ldefs: "x86/data/languages/x86.ldefs",
    facts: DecodeFacts {
        isa_mode_var: None,
        flow_commit_vars: &[],
        transient_decode_vars: &[],
        has_delay_slots: false,
        callother_output_exact: true,
        internal_registers: &["xmmTmp1", "xmmTmp2"],
        volatile_registers: &[],
        non_volatile_registers: &[],
    },
    rounding_ops: &["mxcsrRoundingControl", "x87RoundingControl"],
    msr_mode_op: None,
    rounding_register: None,
    call_other: CALL_OTHER,
    // No prefixes: `vp*_avx*` also names gather / scatter / maskmov ops that
    // reach memory the p-code does not spell out.
    call_other_prefixes: &[],
    call_other_fallback: None,
    calling_conventions: CALLING_CONVENTIONS,
};

/// Writes a segment base; memory is clobbered so no access forwards across it.
const fn base_write(base: &'static [&'static str]) -> CallOtherClass {
    CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: base,
        clobbers_memory: true,
        no_return: false,
    })
}

const CALL_OTHER: &[Row] = &[
    // `swi` is every INT vector (0x80, INT3, ...), so it names no registers:
    // a stack address passed to `int 0x80` in EBX escapes unseen.
    Row {
        names: &["swi"],
        only: None,
        class: MEM_CLOBBER,
    },
    // Linux x86_64 syscall: RAX = number, RDI/RSI/RDX/R10/R8/R9 = args, RAX =
    // return; SYSCALL itself writes RCX (rip) and R11 (rflags).
    Row {
        names: &["syscall"],
        only: Some(&[crate::ArchPreset::X86_64]),
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"],
            implicit_writes: &["RAX", "RCX", "R11"],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // RDPKRU reads ECX and zeroes EDX; EAX is the op's p-code output.
    Row {
        names: &["rdpkru_u32"],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["ECX"],
            implicit_writes: &["EDX"],
            clobbers_memory: false,
            no_return: false,
        }),
    },
    // RDTSC: `tmp:8 = rdtsc(); EDX = tmp(4); EAX = tmp(0);`; RDTSCP also
    // `ECX = tmp(8)` of its 12 bytes. All explicit.
    Row {
        names: &["rdtsc", "rdtscp"],
        only: None,
        class: PURE,
    },
    // RDMSR: `tmp:8 = rdmsr(ECX); EDX = tmp(4); EAX = tmp(0);`, all explicit.
    Row {
        names: &["rdmsr"],
        only: None,
        class: PURE,
    },
    // WRMSR: operands explicit; an MSR (FSBASE, ...) can change what later
    // loads read.
    Row {
        names: &["wrmsr"],
        only: None,
        class: MEM_CLOBBER,
    },
    // RDFSBASE / RDGSBASE: the destination is the p-code output.
    Row {
        names: &["readfsbase", "readgsbase"],
        only: None,
        class: PURE,
    },
    // WRFSBASE / WRGSBASE: `FS:` / `GS:` addresses read FS_OFFSET / GS_OFFSET.
    Row {
        names: &["writefsbase"],
        only: None,
        class: base_write(&["FS_OFFSET"]),
    },
    Row {
        names: &["writegsbase"],
        only: None,
        class: base_write(&["GS_OFFSET"]),
    },
    // MONITOR / MONITORX: `monitor()` has no operands; RAX = address,
    // ECX = extensions, EDX = hints.
    Row {
        names: &["monitor", "monitorx"],
        only: Some(&[crate::ArchPreset::X86_64]),
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["RAX", "ECX", "EDX"],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // 32-bit MONITOR / MONITORX take the address in EAX.
    Row {
        names: &["monitor", "monitorx"],
        only: Some(&[crate::ArchPreset::X86]),
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["EAX", "ECX", "EDX"],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // MWAIT / MWAITX wait for the monitored line to be written: EAX = hints,
    // ECX = extensions.
    Row {
        names: &["mwait", "mwaitx"],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["EAX", "ECX"],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // HLT resumes after an interrupt, whose handler may write any memory.
    Row {
        names: &["halt"],
        only: None,
        class: MEM_CLOBBER,
    },
    // SYSRET / SYSEXIT leave the kernel for user mode.
    Row {
        names: &["sysret", "sysexit"],
        only: None,
        class: CallOtherClass::NO_RETURN,
    },
    // SWAPGS changes the base of every later `%gs:` access.
    Row {
        names: &["swapgs"],
        only: None,
        class: base_write(&["GS_OFFSET"]),
    },
    // Register-to-register SIMD / crypto / x87 ops: every operand is listed,
    // and an `m128` / `m256` / `m512` operand is a dynamic varnode, so its
    // access is a p-code LOAD / STORE beside the op.
    Row {
        names: &[
            // AES-NI: `XmmReg1 = <op>(XmmReg1, XmmReg2_m128)`.
            "aesdec",
            "aesdeclast",
            "aesenc",
            "aesenclast",
            "aesimc",
            "aeskeygenassist",
            // `Reg32 = crc32(Reg32, rm8|rm16|rm32)`.
            "crc32",
            // x87 FYL2X / FYL2XP1: `log2st0:10 = log2(ST0)`.
            "log2",
            // x87 FPREM / FPREM1 and their status-word ops.
            "x87Prem",
            "x87Prem1",
            "x87Prem1Status",
            "x87PremStatus",
            // `XmmReg1 = pblendvb(XmmReg1, XmmReg2_m128, XMM0)`.
            "pblendvb",
            // MMX and SSE `psraw`.
            "psraw",
            // `XmmReg = movntdqa(XmmReg, m128)`.
            "movntdqa",
            // `sha.sinc`; the `sha256*` three are arch-independent rows.
            "sha1msg1_sha",
            "sha1msg2_sha",
            "sha1nexte_sha",
            "sha1rnds4_sha",
            // AVX.
            "vpaddq_avx",
            "vpshufb_avx",
            "vpshufd_avx",
            "vpsrldq_avx",
            // `m128 = vmovntdq_avx(XmmReg1)`.
            "vmovntdq_avx",
            // AVX2.
            "vpaddd_avx2",
            "vpblendd_avx2",
            "vpcmpgtb_avx2",
            "vpermd_avx2",
            "vpshufb_avx2",
            "vpsraw_avx2",
            // AVX-512; masking is p-code around the op. One
            // `vpbroadcastb_avx512bw` form leaves its ModRM GPR source
            // unmodelled in the sla: a register imprecision, not a memory one.
            "vbroadcasti32x4_avx512f",
            "vmovdqa64_avx512f",
            "vpbroadcastb_avx512bw",
        ],
        only: None,
        class: PURE,
    },
    // Memory the p-code leaves out: `movdir64b(Reg, m512)`'s store to [Reg],
    // and `xsha256(ECX, ESI, EDI)`'s reads from [ESI] and digest at [EDI].
    Row {
        names: &["movdir64b", "xsha256"],
        only: None,
        class: MEM_CLOBBER,
    },
    // FXRSTOR / XRSTOR*: void pcodeops (constructors `FXRSTOR`, `XRSTOR`), so
    // this row is the only model of the state they load.
    Row {
        names: &[
            "_fxrstor",
            "_fxrstor64",
            "xrstor",
            "xrstor64",
            "xrstors",
            "xrstors64",
        ],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            // XRSTOR's component bitmap; FXRSTOR shares the over-approximation.
            implicit_reads: &["EAX", "EDX"],
            implicit_writes: X86_EXTENDED_STATE,
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // FXSAVE / XSAVE*: the same state, read.
    Row {
        names: &[
            "_fxsave",
            "_fxsave64",
            "xsave",
            "xsave64",
            "xsavec",
            "xsavec64",
            "xsaveopt",
            "xsaveopt64",
            "xsaves",
            "xsaves64",
        ],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: X86_EXTENDED_STATE,
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
];

/// The x87 / SSE / AVX / AVX-512 / MPX / CET state the FXSAVE and XSAVE
/// families move, declared by the sla for both widths. `ST0`-`ST7` contain
/// `MM0`-`MM7`; `YMMn_H` are the AVX upper halves.
const X86_EXTENDED_STATE: &[&str] = &[
    "ST0",
    "ST1",
    "ST2",
    "ST3",
    "ST4",
    "ST5",
    "ST6",
    "ST7",
    "XMM0",
    "XMM1",
    "XMM2",
    "XMM3",
    "XMM4",
    "XMM5",
    "XMM6",
    "XMM7",
    "XMM8",
    "XMM9",
    "XMM10",
    "XMM11",
    "XMM12",
    "XMM13",
    "XMM14",
    "XMM15",
    "YMM0_H",
    "YMM1_H",
    "YMM2_H",
    "YMM3_H",
    "YMM4_H",
    "YMM5_H",
    "YMM6_H",
    "YMM7_H",
    "YMM8_H",
    "YMM9_H",
    "YMM10_H",
    "YMM11_H",
    "YMM12_H",
    "YMM13_H",
    "YMM14_H",
    "YMM15_H",
    // Components 5 (opmask) and 7 (Hi16_ZMM).
    "K0",
    "K1",
    "K2",
    "K3",
    "K4",
    "K5",
    "K6",
    "K7",
    "XMM16",
    "XMM17",
    "XMM18",
    "XMM19",
    "XMM20",
    "XMM21",
    "XMM22",
    "XMM23",
    "XMM24",
    "XMM25",
    "XMM26",
    "XMM27",
    "XMM28",
    "XMM29",
    "XMM30",
    "XMM31",
    "YMM16_H",
    "YMM17_H",
    "YMM18_H",
    "YMM19_H",
    "YMM20_H",
    "YMM21_H",
    "YMM22_H",
    "YMM23_H",
    "YMM24_H",
    "YMM25_H",
    "YMM26_H",
    "YMM27_H",
    "YMM28_H",
    "YMM29_H",
    "YMM30_H",
    "YMM31_H",
    // Components 3/4 (MPX) and 12 (CET supervisor shadow stacks); `SSP` is in
    // no XSAVE area.
    "BND0",
    "BND1",
    "BND2",
    "BND3",
    "BNDCFGU",
    "BNDSTATUS",
    "IA32_PL0_SSP",
    "IA32_PL1_SSP",
    "IA32_PL2_SSP",
    "MXCSR",
    "FPUControlWord",
    "FPUStatusWord",
    "FPUTagWord",
    "FPULastInstructionOpcode",
    "FPUDataPointer",
    "FPUInstructionPointer",
    "FPUPointerSelector",
    "FPUDataSelector",
];

const CALLING_CONVENTIONS: &[CcPreset] = &[
    CcPreset {
        name: "x86_64_systemv",
        cc: CallingConvention::x86_64_systemv(),
        arches: &[ArchPreset::X86_64],
        ghidra: &[(
            "x86/data/languages/x86-64-gcc.cspec",
            None,
            &[ArchPreset::X86_64],
        )],
    },
    CcPreset {
        name: "x86_cdecl",
        cc: CallingConvention::x86_cdecl(),
        arches: &[ArchPreset::X86],
        ghidra: &[("x86/data/languages/x86gcc.cspec", None, &[ArchPreset::X86])],
    },
    // `-mregparm=3` is Ghidra's `__regparm3`.
    CcPreset {
        name: "x86_linux_kernel",
        cc: CallingConvention::x86_linux_kernel(),
        arches: &[ArchPreset::X86],
        ghidra: &[(
            "x86/data/languages/x86gcc.cspec",
            Some("__regparm3"),
            &[ArchPreset::X86],
        )],
    },
];

/// Base for `x86_cdecl` and `x86_linux_kernel`, which differ only in
/// `arg_passing_regs`.
const X86_CDECL_BASE: CallingConvention = CallingConvention {
    stack_ptr: "ESP",
    arg_passing_regs: &[],
    // Scalar floats go on the stack. The psABI's `__m128` arguments in
    // XMM0-XMM2 are left out so `arg(n)` stays stack-only; a caller's vector
    // argument setup goes unrooted.
    arg_passing_regs_float: &[],
    callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
    ret_val_regs: &["EAX", "EDX"],
    // Scalar floats in ST0, COMPLEX_X87 in ST0/ST1, `__m128` in XMM0.
    ret_val_regs_float: &["ST0", "ST1", "XMM0"],
    // Above the 4-byte return address.
    stack_args: Some(StackArgs {
        base_offset: 4,
        increment: 4,
    }),
    // The plain `ret`; see `x86_cdecl` for callees that pop more.
    ret_stack_pop: 4,
    // `call` pushes the return address.
    link_register: None,
    preserves_memory: false,
    preserves_all_registers: false,
    entry_address: None,
    reg_args_adjoin_stack_args: false,
};

impl CallingConvention {
    /// Returns the x86-64 System V ABI calling convention.
    #[must_use]
    pub const fn x86_64_systemv() -> Self {
        CallingConvention {
            stack_ptr: "RSP",
            arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
            // X87-class (`long double`) arguments go on the stack.
            arg_passing_regs_float: &[
                "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5", "XMM6", "XMM7",
            ],
            callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
            // SSE in XMM0/XMM1, X87 in ST0, COMPLEX_X87 in ST0/ST1.
            ret_val_regs_float: &["XMM0", "XMM1", "ST0", "ST1"],
            // Above the 8-byte return address.
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 8,
            }),
            ret_stack_pop: 8,
            // `call` pushes the return address.
            link_register: None,
            preserves_memory: false,
            preserves_all_registers: false,
            entry_address: None,
            reg_args_adjoin_stack_args: false,
        }
    }

    /// Returns the x86 cdecl calling convention. A callee ending in
    /// `ret $imm16` (stdcall, a hidden struct-return pointer) needs a
    /// per-address convention popping `4 + imm16`, and a PC thunk one that
    /// does not preserve its register.
    #[must_use]
    pub const fn x86_cdecl() -> Self {
        X86_CDECL_BASE
    }

    /// Returns the Linux kernel-internal CC for x86 32-bit (`-mregparm=3`).
    /// Map a variadic (`printk`) or `asmlinkage` callee, which takes every
    /// argument on the stack, to [`Self::x86_cdecl`] in `per_address_ccs`.
    #[must_use]
    pub const fn x86_linux_kernel() -> Self {
        // `int 0x80` clobbers EAX/ECX/EDX through the sla's trailing
        // `call [intloc]` (constructor `INT`), an ordinary `Call` under the
        // function's default convention.
        CallingConvention {
            arg_passing_regs: &["EAX", "EDX", "ECX"],
            ..X86_CDECL_BASE
        }
    }
}
