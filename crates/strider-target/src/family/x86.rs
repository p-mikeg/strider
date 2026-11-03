use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};

pub(crate) static X86: Family = Family {
    name: "x86",
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
    // * `vp*_avx` / `*_avx2` / `*_avx512*` (x86): NOT homogeneous.  The same
    //   prefixes cover gather / scatter / compress / expand / maskmov / MXCSR ops
    //   that reach memory the p-code does not spell out, so the SIMD members are
    //   individual rows in `CALL_OTHER`.
    call_other_prefixes: &[],
    call_other_fallback: None,
    calling_conventions: CALLING_CONVENTIONS,
};

const CALL_OTHER: &[Row] = &[
    // Sleigh's `swi` covers EVERY x86 INT regardless of vector immediate (INT
    // 0x80 Linux syscall, INT3 debugger trap / padding byte, legacy DOS
    // services, page-fault triggers), and carries no per-call operand info at
    // the CallOther layer, so this one row must accept all of them.
    //
    // Hence the empty register ABI: INT 0x80 really does read
    // EAX/EBX/ECX/EDX/ESI/EDI/EBP and write EAX, but modelling that here
    // would be wrong for INT3 padding and the other vectors, which touch none
    // of those registers at the user-visible level.
    //
    // The cost: with no argument register listed, an `int 0x80` handed a stack
    // address in EBX gives that value no consumer node, so the frame-escape
    // scans (`frame_address_escapes`, read by `assumptions.escape_analysis`
    // and the listed-allocator relaxation, and `incoming_area_escapes`) do
    // not see the escape.  ARM `software_interrupt` and x86-64 `syscall` list
    // theirs and are covered.  The call itself clobbers memory and blocks
    // every walk, so only a pointer the kernel keeps and writes during a
    // later call matters.
    Row {
        names: &["swi"],
        only: None,
        class: MEM_CLOBBER,
    },
    // Linux x86_64 syscall: RAX = number, RDI/RSI/RDX/R10/R8/R9 = args, RAX =
    // return.  RCX and R11 are clobbered by the SYSCALL instruction itself
    // (RCX = return rip, R11 = rflags).  Arch-specific because these names
    // only resolve on x86_64's Sleigh register table.
    Row {
        names: &["syscall"],
        only: Some(&[crate::ArchPreset::X86_64]),
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"],
            implicit_writes: &["RAX", "RCX", "R11"],
            // A kernel entry can touch the user stack frame as well as heap
            // and unknown memory.
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // x86 RDPKRU: reads ECX (which must be 0), zeroes EDX; EAX is the op's own
    // pcode output (not declared here; the result-wins-ties dedup would drop
    // it, but listing it is a latent double-clobber trap).
    // Arch-specific because ECX/EAX/EDX are x86 register names.
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
    // x86 RDTSC.  Sleigh emits `tmp:8 = rdtsc(); EDX = tmp(4); EAX = tmp(0);`,
    // so the EDX/EAX writes are explicit pcode ops downstream of the CALLOTHER
    // and re-declaring them here would over-clobber the call site.  A TSC read
    // does not observe RAM.
    Row {
        names: &["rdtsc"],
        only: None,
        class: PURE,
    },
    // `ia.sinc` gives RDTSC explicit p-code writes and RDTSCP none, so RDTSCP
    // declares the whole set here: EDX:EAX plus ECX (IA32_TSC_AUX's low 32).
    // Without that clobber, a pattern reading post-RDTSCP ECX would see the
    // pre-call value.  A TSC read does not observe RAM.
    Row {
        names: &["rdtscp"],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &["EAX", "EDX", "ECX"],
            clobbers_memory: false,
            no_return: false,
        }),
    },
    // x86 RDMSR.  Sleigh emits `tmp:8 = rdmsr(ECX); EDX = tmp(4); EAX =
    // tmp(0);`, so ECX is an explicit pcode arg and the EDX/EAX writes are
    // separate downstream ops.  Nothing implicit, and an MSR read does not
    // observe RAM.
    Row {
        names: &["rdmsr"],
        only: None,
        class: PURE,
    },
    // x86 WRMSR.  Sleigh emits `tmp:8 = (zext(EDX)<<32)|zext(EAX); wrmsr(ECX,
    // tmp);`, so ECX and tmp (and transitively EDX/EAX) are explicit operands
    // of upstream ops feeding the CALLOTHER.  Clobbers memory because a WRMSR
    // can change TSC, FSBASE and the like, which subsequent loads must
    // observe; the user-mode stack frame is unaffected.
    Row {
        names: &["wrmsr"],
        only: None,
        class: MEM_CLOBBER,
    },
    // RDFSBASE / RDGSBASE read the FS/GS segment base into a GPR.  Sleigh
    // emits `r32 = readfsbase()` / `r64 = readfsbase()` with the destination
    // as the explicit pcode output and no inputs, so nothing is implicit.
    Row {
        names: &["readfsbase", "readgsbase"],
        only: None,
        class: PURE,
    },
    // WRFSBASE / WRGSBASE write the FS/GS base from a GPR, emitted as
    // `writefsbase(r64)` (or `zext(r32)`) with the source as the explicit
    // pcode arg.  Clobbers memory because subsequent FS:/GS:-based loads
    // depend on the new base; the stack frame is SP-relative and unaffected.
    Row {
        names: &["writefsbase", "writegsbase"],
        only: None,
        class: MEM_CLOBBER,
    },
    // MONITOR (0F 01 C8) sets up an address-range monitor.  Sleigh emits
    // `monitor()` with zero pcode operands, so the register reads belong in
    // `implicit_reads`.  Per the Intel SDM `MONITOR` entry: RAX = linear
    // address to monitor, ECX = extensions (must be 0), EDX = hints (must
    // be 0).
    // Clobbers memory because it interacts with the cache subsystem and pairs
    // with a later MWAIT, though it does not mutate stack-frame contents.  AMD
    // MONITORX (0F 01 FA) shares the ABI per AMD64 Vol. 3.
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
    // MWAIT (0F 01 C9) / MWAITX (0F 01 FB) enter a low-power state until the
    // armed cache line is written.  Per the Intel SDM `MWAIT` entry:
    // EAX = hints, ECX = extensions (must be 0), no GPR writes.  Clobbers
    // memory because it serialises with the prior MONITOR's cache-line arming
    // and is a memory-order point; stack frames are unaffected.
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
    // HLT (F4) waits for an interrupt and resumes at the next instruction;
    // the handler may write any memory, and restores every register.
    Row {
        names: &["halt"],
        only: None,
        class: MEM_CLOBBER,
    },
    // SYSRET (0F 07) is a fast return from SYSCALL into ring 3.  Kept
    // arch-specific so a non-x86 spec that coincidentally names a user-op
    // `sysret` cannot inherit NoReturn.  For kernel-internal analysis it
    // terminates the function: kernel-context control does not return to its
    // kernel-context caller.
    Row {
        names: &["sysret", "sysexit"],
        only: None,
        class: CallOtherClass::NO_RETURN,
    },
    // SWAPGS (0F 01 F8) exchanges IA32_GS_BASE with IA32_KERNEL_GS_BASE.  It
    // writes no GPR or RAM itself, but the MSR swap silently changes the
    // virtual base of every subsequent `%gs:`-relative access, so without the
    // memory clobber LoadForward / LoadReadOnly would forward `%gs:` loads
    // across it.  Arch-specific so a non-x86 user-op named `swapgs` cannot
    // inherit the classification.
    Row {
        names: &["swapgs"],
        only: None,
        class: MEM_CLOBBER,
    },
    // x86 SIMD / crypto / bit-manipulation / x87 intrinsics GHIDRA leaves as
    // named `pcodeop`s.  Every one is register-to-register: the constructors in
    // `x86/data/languages/{ia,sha,avx,avx2,avx512}.sinc` list each operand,
    // and a memory operand arrives through `m128` / `m256` / `m512`, which
    // `ia.sinc:1159-1161` export as the DYNAMIC varnode `*:N Mem`, so reading
    // one emits a p-code `LOAD` ahead of the CALLOTHER and assigning one emits
    // a p-code `STORE` after it.  Neither is the user-op's own effect.
    //
    // Arch-scoped because the plainer names here (`crc32`, `psraw`,
    // `pblendvb`) could be spelled by another processor's spec meaning
    // something else.
    //
    // NOT a `vp*` / `_avx*` prefix family: those prefixes are NOT homogeneous.
    // `avx512.sinc` names gather / scatter / compress / expand ops
    // `vpgatherdd_avx512f`, `vpscatterqq_avx512f`, `vpcompressd_avx512f`,
    // and `avx.sinc` names `vmaskmovdqu_avx` / `vldmxcsr_avx`, all of which
    // reach memory the p-code does NOT spell out (`vpgatherdd_avx512f(m32)`
    // models ONE element of a 16-element gather).  Those must never inherit
    // `PURE`, so these stay individual rows.
    Row {
        names: &[
            // AES-NI (`ia.sinc:10364-10416`), the whole block:
            // `XmmReg1 = <op>(XmmReg1, XmmReg2_m128)`.
            "aesdec",
            "aesdeclast",
            "aesenc",
            "aesenclast",
            "aesimc",
            "aeskeygenassist",
            // `Reg32 = crc32(Reg32, rm8|rm16|rm32)` (`ia.sinc:10315-10321`).
            "crc32",
            // x87 FYL2X / FYL2XP1: `log2st0:10 = log2(ST0)` or
            // `log2(ST0 f+ 1)`, the logarithm the following multiply scales.
            "log2",
            // x87 FPREM / FPREM1: `ST0 = x87Prem(ST0, ST1)` and the status
            // word `FPUStatusWord = x87PremStatus(ST0, ST1, FPUStatusWord)`,
            // and their `x87Prem1` pair.
            "x87Prem",
            "x87Prem1",
            "x87Prem1Status",
            "x87PremStatus",
            // `XmmReg1 = pblendvb(XmmReg1, XmmReg2_m128, XMM0)`
            // (`ia.sinc:9996-9997`): the otherwise-implicit XMM0 is a listed
            // operand, like SHA256RNDS2.
            "pblendvb",
            // MMX `psraw` (`ia.sinc:8939-8941`) and its SSE form (`:8970-8972`).
            "psraw",
            // MOVNTDQA (`ia.sinc:10308`) is `XmmReg = movntdqa(XmmReg, m128)`:
            // a non-temporal LOAD whose access is the explicit `m128` p-code
            // Load, leaving the user-op itself pure.
            "movntdqa",
            // The rest of `sha.sinc`, whose seven pcodeops all have the shape
            // `XmmReg1 = <op>(XmmReg1, XmmReg2_m128[, imm8|XMM0])`; the
            // `sha256*` three are arch-independent rows (`call_other_abi/common.rs`).
            "sha1msg1_sha",
            "sha1msg2_sha",
            "sha1nexte_sha",
            "sha1rnds4_sha",
            // AVX (`avx.sinc`), VEX-encoded register compute.
            "vpaddq_avx",
            "vpshufb_avx",
            "vpshufd_avx",
            "vpsrldq_avx",
            // VMOVNTDQ (`avx.sinc:1267`, `:1273`) is
            // `m128 = vmovntdq_avx(XmmReg1)`:
            // the non-temporal STORE is the explicit `m128` p-code Store, so
            // the user-op computes a value and touches no RAM itself.
            "vmovntdq_avx",
            // AVX2 (`avx2.sinc`).
            "vpaddd_avx2",
            "vpblendd_avx2",
            "vpcmpgtb_avx2",
            "vpermd_avx2",
            "vpshufb_avx2",
            "vpsraw_avx2",
            // AVX-512 (`avx512.sinc`).  Masking is explicit p-code around the
            // call (`ZmmResult = <op>(...); ZmmMask = ZmmReg1; build
            // ZmmOpMask32; ZmmReg1 = ZmmResult;`), and both VMOVDQA64
            // directions go through `ZmmReg2_m512`, the dynamic-varnode
            // alternation of `ia.sinc:1308-1309`.
            //
            // `vpbroadcastb_avx512bw` has a zero-operand form
            // (`avx512.sinc:10171`) where GHIDRA's own comment records it could
            // not model the ModRM GPR source, so the op's value is opaque and
            // its GPR read unmodelled.  That is a REGISTER imprecision in the
            // sla, not a memory effect: the register is instruction-encoded, so
            // no fixed name could be listed here anyway.
            "vbroadcasti32x4_avx512f",
            "vmovdqa64_avx512f",
            "vpbroadcastb_avx512bw",
        ],
        only: None,
        class: PURE,
    },
    // x86 ops that DO reach memory without the p-code saying so.
    //
    // MOVDIR64B (`ia.sinc:7304-7316`) is `movdir64b(Reg, m512)`: the 64-byte
    // SOURCE read is the explicit `m512` Load, but the 64-byte destination
    // store to the address in `Reg` is modelled nowhere.
    //
    // VIA PadLock XSHA256 (`ia.sinc:9946`) is `xsha256(ECX,ESI,EDI)`: it
    // streams ECX blocks from [ESI] and writes the digest at [EDI], with
    // neither access spelled in p-code.  ECX/ESI/EDI are listed operands, so
    // the register channel stays empty.
    Row {
        names: &["movdir64b", "xsha256"],
        only: None,
        class: MEM_CLOBBER,
    },
    // FXSAVE / FXRSTOR and the XSAVE / XSAVEC / XSAVEOPT / XSAVES / XRSTOR /
    // XRSTORS family move the x87, SSE and AVX register state to and from a
    // memory area (SDM Vol 2A).  The sla emits each as a void pcodeop with no
    // output (`ia.sinc:6166`, `:5029`), so this table is the only model of the
    // registers they touch.  A restore declaring none left the INCOMING value
    // of every one of them live across it, so `fxrstor (%rdi); movaps
    // %xmm0,(%rsi)` stored `InitialVar(XMM0)` rather than what was restored.
    //
    // `ST0`-`ST7` cover `MM0`-`MM7`, which the sla overlays at the same
    // offsets and the lifter folds into their largest container.  The AVX
    // halves are `YMMn_H` (`ia.sinc:155`), `XMMn` being the low overlay.
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
            // XRSTOR takes the component bitmap in EDX:EAX; FXRSTOR does not,
            // and reading two more registers than it does costs a fold, not an
            // answer.
            implicit_reads: &["EAX", "EDX"],
            implicit_writes: X86_EXTENDED_STATE,
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // The save direction: the same state read rather than written, so no pass
    // can take one of these registers dead across it.
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

/// The x87 / SSE / AVX register state the FXSAVE and XSAVE families move.
/// The sla declares all of it for both x86 widths in one block
/// (`ia.sinc:93`, `:140`, `:155`), so one list serves both widths.
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
    // XSAVE state component 5 (opmask) and 7 (Hi16_ZMM). ia.sinc declares
    // both in the block it shares between the widths, and both are disjoint
    // from every name above, so no container write reaches them.
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
    // Component 3/4 (MPX bounds + config) and 12 (CET supervisor shadow
    // stacks). `BND0`-`BND3` are read back by the sla itself (`mpx.sinc:53`,
    // `:119`) and `IA32_PL0_SSP` by `SETSSBSY` (`cet.sinc:73`). `SSP` is NOT
    // here: it is loaded from `IA32_PLx_SSP` on a privilege transition and
    // sits in no XSAVE area.
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
    // Intel386 psABI: scalar floats go on the stack like every other cdecl
    // argument.  Its SSE class (`__m128`) is NOT modelled, though the psABI
    // passes the first three such arguments in XMM0-XMM2:
    // naming them here would renumber `arg(n)` on an arch whose arguments are
    // otherwise all stack-passed.  A callee still reads its own XMM0-2
    // correctly; only a caller's vector argument setup goes unrooted.
    arg_passing_regs_float: &[],
    callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
    ret_val_regs: &["EAX", "EDX"],
    // cdecl returns scalar float / double / long double in ST0, the x87 80-bit
    // top-of-stack, and a COMPLEX_X87 value in ST0/ST1 (Intel386 psABI).  XMM0
    // is the return register only for the `__m128` / vector classes.
    // `FunctionBuilder::new` seeds all three on EVERY function, so ST0 (10
    // bytes at register offset 0x1100) subsumes MM0 (8 bytes, same offset) in
    // `dedup_overlapping_largest` and MMX accesses slice out of ST0.
    ret_val_regs_float: &["ST0", "ST1", "XMM0"],
    // +4 because `call` pushes a 4-byte return address: SP-at-call points at
    // that address, so arg 0 sits one slot above it.
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
        // x86-64 System V.  RSP is not listed callee-saved: `ret` pops the return
        // address, so the caller observes SP shifted by `ret_stack_pop`.
        CallingConvention {
            stack_ptr: "RSP",
            arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
            // psABI 3.2.3: SSE-class arguments in XMM0..XMM7.  X87-class
            // (`long double`) arguments go on the stack, so ST0/ST1 are
            // return-only here.
            arg_passing_regs_float: &[
                "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5", "XMM6", "XMM7",
            ],
            callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
            // SSE class in XMM0/XMM1; X87 class (`long double`) in ST0; the
            // COMPLEX_X87 class puts the real part in ST0 and the imaginary
            // part in ST1 (psABI 3.2.3).  Without ST1 nothing roots the
            // imaginary half's cone and DCE deletes it.
            //
            // `FunctionBuilder::new` tracks every `ret_val_regs_float` entry on
            // EVERY function, so listing ST0 (10 bytes at register offset
            // 0x1100) makes it subsume MM0 (8 bytes, same offset) in
            // `dedup_overlapping_largest`, and MMX accesses slice out of ST0.
            ret_val_regs_float: &["XMM0", "XMM1", "ST0", "ST1"],
            // +8 because `call` pushes an 8-byte return address: SP-at-call
            // points at it, so the first stack arg (arg 7) is one slot above.
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

    /// Returns the x86 cdecl calling convention.
    ///
    /// Its `ret_stack_pop` of 4 is a plain `ret`. A callee that also pops
    /// its caller's stack (`ret $imm16`: a struct return's hidden pointer,
    /// stdcall, fastcall, thiscall) needs a per-address convention with
    /// `ret_stack_pop` of `4 + imm16`; without one, SP-relative accesses after
    /// the call are off by `imm16`. A PC thunk (`__x86.get_pc_thunk.bx`) needs
    /// one that does not preserve its register; without one, the register
    /// keeps the caller's value across the call.
    #[must_use]
    pub const fn x86_cdecl() -> Self {
        // x86 cdecl: all arguments on the stack, so `arg_passing_regs` is empty.
        // ESP is not listed callee-saved: `ret` pops the 4-byte return address, so
        // the caller observes SP shifted by `ret_stack_pop`.
        X86_CDECL_BASE
    }

    /// Returns the Linux kernel-internal CC for x86 32-bit (`-mregparm=3`).
    ///
    /// A variadic callee (`printk`) and an `asmlinkage` one (`regparm(0)`)
    /// take every argument on the stack, yet a call to one under this
    /// convention still lists EAX, EDX and ECX as arguments 0-2, so its first
    /// argument reads as `arg(3)`.  Map such callees to [`Self::x86_cdecl`] in
    /// `AnalyzeOptions::per_address_ccs`.  To analyse one of them, pass
    /// `x86_cdecl` as its own convention and map its regparm callees to this
    /// one.
    #[must_use]
    pub const fn x86_linux_kernel() -> Self {
        // Kernel-internal presets exist for x86 32-bit and PowerPC; every
        // other supported arch's kernel CC has the same register SET as its
        // userland preset, so callers pick that directly.  A role can differ
        // without the set doing so, which is part of why PowerPC has its own:
        // ppc64 r13 is the per-CPU `paca` pointer there, not the userland TLS
        // pointer (`POWERPC64_ELF_BASE`).
        //
        // A register-level syscall footprint lives in `call_other_abi` only
        // where the sla leaves the trap opaque (`syscall`, `svc`, `sc`).  x86
        // `int 0x80` is NOT one: its `swi` row carries no registers, and the
        // sla's trailing `call [intloc]` (`ia.sinc:3694`) is what clobbers
        // EAX/ECX/EDX, as an ordinary `Call` under the function's default
        // convention.
        //
        // x86 32-bit `-mregparm=3`: first three integer args in EAX, EDX, ECX, the
        // rest on the stack at the same cdecl offsets.
        CallingConvention {
            arg_passing_regs: &["EAX", "EDX", "ECX"],
            ..X86_CDECL_BASE
        }
    }
}
