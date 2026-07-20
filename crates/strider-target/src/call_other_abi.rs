use crate::calling_convention::regs_to_vns;
use CallOtherClass::NoOp;

/// Vn-resolved [`CallOtherAbi`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltCallOtherAbi {
    pub implicit_reads: Vec<rsleigh::Vn>,
    pub implicit_writes: Vec<rsleigh::Vn>,
    pub clobbers_memory: bool,
    pub no_return: bool,
}

/// Per-user-op ABI covering the ISA-fixed effects Sleigh's pcode does NOT
/// encode.  Sleigh emits `CALLOTHER(user_op_id, args)` with an optional
/// `output`; this fills in the implicit channel around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallOtherAbi {
    /// Read beyond Sleigh's pcode-explicit `inputs[1..]`.  Exact Sleigh
    /// register names, case-sensitive.
    pub implicit_reads: &'static [&'static str],

    /// Written or scratch-clobbered beyond Sleigh's pcode-explicit `output`.
    pub implicit_writes: &'static [&'static str],

    /// Whether the op advances the IR's memory edge: `false` for pure compute
    /// (rdtsc, NEON/SVE), `true` for anything touching memory (atomics,
    /// barriers, port I/O, syscalls, kernel entries).
    pub clobbers_memory: bool,

    /// `true` when control does not pass this op: a `BUG_ON`-class trap,
    /// `sysret`, or a modeled call known never to return.  A no-return op
    /// still carries its full register / memory footprint; a bare trap is the
    /// empty-footprint case.
    pub no_return: bool,
}

impl CallOtherAbi {
    /// Resolves the name-based footprint against `sleigh_regs`.
    ///
    /// # Errors
    ///
    /// Short-circuits on the first name in `implicit_reads` or
    /// `implicit_writes` that does not resolve.
    pub fn build(&self, sleigh_regs: &rsleigh::SleighRegs) -> crate::Result<BuiltCallOtherAbi> {
        Ok(BuiltCallOtherAbi {
            implicit_reads: regs_to_vns(sleigh_regs, self.implicit_reads)?,
            implicit_writes: regs_to_vns(sleigh_regs, self.implicit_writes)?,
            clobbers_memory: self.clobbers_memory,
            no_return: self.no_return,
        })
    }
}

/// How a user-op name is lifted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOtherClass {
    /// No IR node emitted, control and memory unchanged, and any
    /// pcode-explicit output ignored.
    NoOp,

    /// Register footprint, memory effect, and whether control returns, beyond
    /// what Sleigh's pcode encodes.
    Call(CallOtherAbi),
}

impl CallOtherClass {
    /// Bare trap: an empty-footprint, non-returning `Call` (`BUG_ON`,
    /// `sysret`, `UndefinedInstruction`).
    pub const NO_RETURN: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: true,
    });

    #[must_use]
    pub fn is_no_return(&self) -> bool {
        matches!(self, CallOtherClass::Call(abi) if abi.no_return)
    }
}

/// Classifies a user-op name for the given architecture: arch-specific table
/// first, then the arch-independent one, then the prefix families and the PPC
/// table.
///
/// A missing entry is intentional: entries are added on demand as real
/// binaries surface them.
pub fn classify(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    classify_arch_specific(preset, name)
        .or_else(|| classify_arch_independent(name))
        .or_else(|| classify_prefix_family(preset, name))
        .or_else(|| classify_ppc(preset, name))
}

/// GHIDRA's PowerPC `.sinc` lifts atomic store-conditional, cache / TLB / SLB
/// management, Altivec/VSX vector ops, traps, and system-register moves to
/// named `pcodeop` CallOthers.  Scoped to the four PPC presets so the generic
/// names among them (`random`, `message`) cannot match on another arch.
fn classify_ppc(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    use crate::ArchPreset::{Ppc32Be, Ppc32Le, Ppc64Be, Ppc64Le};
    if !matches!(preset, Ppc32Be | Ppc32Le | Ppc64Be | Ppc64Le) {
        return None;
    }
    // Mirrors the arch-independent PURE / MEM_CLOBBER split.
    const PURE: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: false,
    });
    const MEM_CLOBBER: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: true,
        no_return: false,
    });
    // Linux PowerPC `sc`: r0 = syscall number, r3..r8 = args, return in r3.
    // Sleigh's `syscall()` has no pcode output, so model r3 here or a later read
    // forwards the pre-call value.
    const SYSCALL: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &["r0", "r3", "r4", "r5", "r6", "r7", "r8"],
        implicit_writes: &["r3"],
        clobbers_memory: true,
        no_return: false,
    });

    static TABLE: &[(&str, CallOtherClass)] = &[
        // `rfi`/`rfid` return from an exception handler and end the function.
        ("returnFromInterrupt", CallOtherClass::NO_RETURN),
        // `tw`/`td` conditional traps: GHIDRA emits the pcodeop unconditionally
        // with the trap-on condition as a plain operand (NO branch), so control
        // FALLS THROUGH. They must be RETURNING, not NO_RETURN, or every
        // function with a compiler bounds/overflow trap loses its tail. A
        // firing trap may enter a memory-touching handler, so MEM_CLOBBER.
        ("trapDoubleWordImmediate", MEM_CLOBBER),
        ("trapWord", MEM_CLOBBER),
        // Store-conditional and byte-reverse stores write RAM; dcbz zeroes a
        // block; dcbf/dcbst/icbi and TLB/SLB management are ordering barriers;
        // icswx/copy-paste and doorbell messages have external memory effects.
        ("MessageClear", MEM_CLOBBER),
        ("MessageSend", MEM_CLOBBER),
        ("StoreDoublewordByteReverseIndexed", MEM_CLOBBER),
        ("TLBInvalidateEntry", MEM_CLOBBER),
        ("TLBInvalidateEntryLocal", MEM_CLOBBER),
        ("TLBSynchronize", MEM_CLOBBER),
        ("TLBWrite", MEM_CLOBBER),
        ("copytrans", MEM_CLOBBER),
        ("dataCacheBlockClearToZero", MEM_CLOBBER),
        ("dataCacheBlockFlush", MEM_CLOBBER),
        ("dataCacheBlockInvalidate", MEM_CLOBBER),
        ("dataCacheBlockStore", MEM_CLOBBER),
        ("icswxDotOp", MEM_CLOBBER),
        ("instructionCacheBlockInvalidate", MEM_CLOBBER),
        ("instructionCacheCongruenceClassInvalidate", MEM_CLOBBER),
        ("message", MEM_CLOBBER),
        ("pastetrans", MEM_CLOBBER),
        ("slbInvalidateAll", MEM_CLOBBER),
        ("slbMoveToEntry", MEM_CLOBBER),
        ("storeDoubleWordConditionalIndexed", MEM_CLOBBER),
        ("storeWordConditionalIndexed", MEM_CLOBBER),
        ("syscall", SYSCALL),
        // `wait` is a low-power wait like ARM WFI: a remote agent may modify
        // shared memory while the core is parked, so it must not let a load
        // forward across it.
        ("waitT", MEM_CLOBBER),
        // Byte-reverse load, the vector shift-control generator (lvsl/lvsr),
        // SLB reads, and the hardware RNG produce a pcode-explicit output and
        // touch no RAM.  Altivec/VSX/vector compute is covered by the
        // `altv`/`vsx`/`vector` prefix below.
        ("LoadDoublewordByteReverseIndexed", PURE),
        ("loadVectorForShiftLeft", PURE),
        ("random", PURE),
        ("slbMoveFromEntryESID", PURE),
        ("slbMoveFromEntryVSID", PURE),
        ("slbfeeDotOp", PURE),
        // Cache-touch and stream-stop are prefetch hints;
        // wrtee/clearHistory/mtfsf change MSR, branch-history, or FPSCR state
        // strider does not track.  No memory or value effect to model.
        ("MoveToFPSCRFields", NoOp),
        ("WriteExternalEnable", NoOp),
        ("WriteExternalEnableImmediate", NoOp),
        ("clearHistory", NoOp),
        ("dataCacheBlockTouch", NoOp),
        ("dataCacheBlockTouchForStore", NoOp),
        ("dataStreamStopAll", NoOp),
    ];
    if let Some(c) = TABLE.iter().find_map(|(n, c)| (*n == name).then_some(*c)) {
        return Some(c);
    }
    // GHIDRA emits Altivec / VSX / named vector intrinsics as `altv207_<n>`,
    // `vsx<ver>_<n>`, and `vector<Op>`.  All are pure SIMD compute with
    // pcode-explicit operands; vector LOADS and STORES lift to ordinary
    // Load/Store pcode, not these user-ops.
    if name.starts_with("altv") || name.starts_with("vsx") || name.starts_with("vector") {
        return Some(PURE);
    }
    None
}

/// Names whose ABI depends on the emitting arch: `swi` (which collides
/// between ARM's Linux SVC/SWI and x86's INT), Linux syscall ABIs, SMCCC, and
/// the x86 MSR / MONITOR-MWAIT / SWAPGS family.
fn classify_arch_specific(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    ARCH_SPECIFIC_TABLE.iter().find_map(|row| {
        (row.preset_arches.contains(&preset) && row.op_names.contains(&name)).then_some(row.class)
    })
}

struct CallOtherRow {
    /// A slice so one row covers a family: `[Aarch64, Aarch64Be]` for the
    /// LE/BE pair, `[X86, X86_64]` for both x86 widths.
    preset_arches: &'static [crate::ArchPreset],
    /// A slice so one row covers related ops: `mwait` + `mwaitx`, or the
    /// SMCCC pair `CallHyperVisor` + `CallSecureMonitor`.
    op_names: &'static [&'static str],
    class: CallOtherClass,
}

static ARCH_SPECIFIC_TABLE: &[CallOtherRow] = &[
    // ARM Linux SVC: r7 = syscall number, r0..r6 = args, r0 = return. ARM's
    // Sleigh spec emits `software_interrupt` for SVC/SWI (the bare `swi`
    // pcodeop is x86's INT, handled separately below). Arch-specific so it wins
    // over the generic `software_interrupt` = MEM_CLOBBER fallback. All four
    // 32-bit ARM presets share it; split the row if Thumb ever diverges.
    CallOtherRow {
        preset_arches: ARM32_ALL,
        op_names: &["software_interrupt"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
            implicit_writes: &["r0"],
            // A kernel entry can read or write any user-mode memory, the user
            // stack included, so StackOffsetDetect must break the Stack chain.
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // Sleigh's `swi` covers EVERY x86 INT regardless of vector immediate (INT
    // 0x80 Linux syscall, INT3 debugger trap / padding byte, legacy DOS
    // services, page-fault triggers), and carries no per-call operand info at
    // the CallOther layer, so this one row must accept all of them.
    //
    // Hence the empty register ABI: INT 0x80 really does read
    // EAX/EBX/ECX/EDX/ESI/EDI/EBP and write EAX, but modelling that here
    // would be wrong for INT3 padding and the other vectors, which touch none
    // of those registers at the user-visible level.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["swi"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // Linux x86_64 syscall: RAX = number, RDI/RSI/RDX/R10/R8/R9 = args, RAX =
    // return.  RCX and R11 are clobbered by the SYSCALL instruction itself
    // (RCX = return rip, R11 = rflags).  Arch-specific because these names
    // only resolve on x86_64's Sleigh register table.
    CallOtherRow {
        preset_arches: &[crate::ArchPreset::X86_64],
        op_names: &["syscall"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"],
            implicit_writes: &["RAX", "RCX", "R11"],
            // A kernel entry can touch the user stack frame as well as heap
            // and unknown memory.
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // ARM SMCCC for HVC (CallHyperVisor) and SMC (CallSecureMonitor): x0..x7
    // in, x0..x3 out, shared by LE and BE aarch64.  Arch-specific because
    // `x0..x7` only resolve on aarch64's register table; arm-32 has `r0..r12`.
    CallOtherRow {
        preset_arches: AARCH64_BOTH,
        op_names: &["CallHyperVisor", "CallSecureMonitor"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            // x0..x3 carry the SMCCC result. Under SMCCC 1.0 the callee may
            // leave x4..x17 in an unpredictable state (1.1+ preserves them);
            // the version isn't statically known, so clobber them conservatively
            // rather than forward a stale value across the SMC/HVC.
            implicit_writes: &[
                "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12",
                "x13", "x14", "x15", "x16", "x17",
            ],
            // Everything passes through the register channel; the spec forbids
            // mutating the caller's stack frame, but a firing SMC/HVC may touch
            // memory, so keep it on the memory chain.
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // x86 RDPKRU: reads ECX (which must be 0), zeroes EDX; EAX is the op's own
    // pcode output (not declared here — the result-wins-ties dedup would drop
    // it, but listing it is a latent double-clobber trap).
    // Arch-specific because ECX/EAX/EDX are x86 register names.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["rdpkru_u32"],
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
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["rdtsc"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: false,
            no_return: false,
        }),
    },
    // Like RDTSC but ALSO writes ECX (the IA32_TSC_AUX MSR's low 32 bits).
    // Without that clobber, a pattern reading post-RDTSCP ECX would see the
    // pre-call value.  A TSC read does not observe RAM.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["rdtscp"],
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
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["rdmsr"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: false,
            no_return: false,
        }),
    },
    // x86 WRMSR.  Sleigh emits `tmp:8 = (zext(EDX)<<32)|zext(EAX); wrmsr(ECX,
    // tmp);`, so ECX and tmp (and transitively EDX/EAX) are explicit operands
    // of upstream ops feeding the CALLOTHER.  Clobbers memory because a WRMSR
    // can change TSC, FSBASE and the like, which subsequent loads must
    // observe; the user-mode stack frame is unaffected.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["wrmsr"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // RDFSBASE / RDGSBASE read the FS/GS segment base into a GPR.  Sleigh
    // emits `r32 = readfsbase()` / `r64 = readfsbase()` with the destination
    // as the explicit pcode output and no inputs, so nothing is implicit.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["readfsbase", "readgsbase"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: false,
            no_return: false,
        }),
    },
    // WRFSBASE / WRGSBASE write the FS/GS base from a GPR, emitted as
    // `writefsbase(r64)` (or `zext(r32)`) with the source as the explicit
    // pcode arg.  Clobbers memory because subsequent FS:/GS:-based loads
    // depend on the new base; the stack frame is SP-relative and unaffected.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["writefsbase", "writegsbase"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // MONITOR (0F 01 C8) sets up an address-range monitor.  Sleigh emits
    // `monitor()` with zero pcode operands, so the register reads belong in
    // `implicit_reads`.  Per Intel SDM Vol. 2B 4-39: RAX = linear address to
    // monitor, ECX = extensions (must be 0), EDX = hints (must be 0).
    // Clobbers memory because it interacts with the cache subsystem and pairs
    // with a later MWAIT, though it does not mutate stack-frame contents.  AMD
    // MONITORX (0F 01 FA) shares the ABI per AMD64 Vol. 3.
    CallOtherRow {
        preset_arches: &[crate::ArchPreset::X86_64],
        op_names: &["monitor", "monitorx"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["RAX", "ECX", "EDX"],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // 32-bit MONITOR / MONITORX take an EAX-relative address.
    CallOtherRow {
        preset_arches: &[crate::ArchPreset::X86],
        op_names: &["monitor", "monitorx"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["EAX", "ECX", "EDX"],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // MWAIT (0F 01 C9) / MWAITX (0F 01 FB) enter a low-power state until the
    // armed cache line is written.  Per Intel SDM Vol. 2B 4-44: EAX = hints,
    // ECX = extensions (must be 0), no GPR writes.  Clobbers memory because it
    // serialises with the prior MONITOR's cache-line arming and is a
    // memory-order point; stack frames are unaffected.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["mwait", "mwaitx"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["EAX", "ECX"],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // SYSRET (0F 07) is a fast return from SYSCALL into ring 3.  Kept
    // arch-specific so a non-x86 spec that coincidentally names a user-op
    // `sysret` cannot inherit NoReturn.  For kernel-internal analysis it
    // terminates the function: kernel-context control does not return to its
    // kernel-context caller.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["sysret", "sysexit"],
        class: CallOtherClass::NO_RETURN,
    },
    // SWAPGS (0F 01 F8) exchanges IA32_GS_BASE with IA32_KERNEL_GS_BASE.  It
    // writes no GPR or RAM itself, but the MSR swap silently changes the
    // virtual base of every subsequent `%gs:`-relative access, so without the
    // memory clobber LoadForward / LoadReadOnly would forward `%gs:` loads
    // across it.  Arch-specific so a non-x86 user-op named `swapgs` cannot
    // inherit the classification.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["swapgs"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        }),
    },
];

const X86_BOTH: &[crate::ArchPreset] = &[crate::ArchPreset::X86, crate::ArchPreset::X86_64];
const ARM32_ALL: &[crate::ArchPreset] = &[
    crate::ArchPreset::Arm,
    crate::ArchPreset::ArmBe,
    crate::ArchPreset::ArmBeKernel,
    crate::ArchPreset::ArmThumb,
];
const AARCH64_BOTH: &[crate::ArchPreset] =
    &[crate::ArchPreset::Aarch64, crate::ArchPreset::Aarch64Be];

/// Names meaning the same on every arch that emits them.
///
/// **Invariant: `Call` entries here MUST have empty `implicit_reads` and
/// `implicit_writes`.**  A named register (RAX, x0, r7) only resolves on one
/// arch's Sleigh register table, which makes the entry arch-specific by
/// definition.
fn classify_arch_independent(name: &str) -> Option<CallOtherClass> {
    const PURE: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: false,
    });
    const MEM_CLOBBER: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: true,
        no_return: false,
    });

    static TABLE: &[(&str, CallOtherClass)] = &[
        // Sleigh decoder context only, invisible to the IR.
        ("setEndianState", NoOp),
        ("setISAMode", NoOp),
        // AArch64 ERET returns from an exception, reloading PC and PSTATE
        // from ELR/SPSR, so control leaves the function like a return.
        // (x86 `sysret` is in classify_arch_specific so a non-x86 user-op of
        // the same name cannot inherit NoReturn.)
        ("ExceptionReturn", CallOtherClass::NO_RETURN),
        ("SoftwareBreakpoint", CallOtherClass::NO_RETURN),
        ("UndefinedInstructionException", CallOtherClass::NO_RETURN),
        ("invalidInstructionException", CallOtherClass::NO_RETURN),
        ("trap", CallOtherClass::NO_RETURN),
        // ARM exclusive-monitor primitives pair with LDREX/STREX, which
        // already emit pcode loads/stores.  The monitor flag is synthetic.
        ("ExclusiveMonitorPass", PURE),
        ("ExclusiveMonitorsStatus", PURE),
        // Non-paired CPU hints, no memory effect.
        ("Hint_Prefetch", PURE),
        ("Yield", PURE),
        // CPUID is a SERIALIZING instruction (Intel SDM Vol. 3 8.3): it
        // drains the store buffer and forces prior memory operations globally
        // visible, making it a full ordering barrier stronger than MFENCE.  So
        // a load after CPUID may observe a concurrent write that CPUID is the
        // barrier for and must not be forwarded from a store before it.  The
        // EAX/EBX/ECX/EDX writes are pcode-explicit Loads from a scratch
        // tmpptr, so `clobbers_memory` here is about ORDERING, not them.
        ("cpuid", MEM_CLOBBER),
        (
            "cpuid_Architectural_Performance_Monitoring_info",
            MEM_CLOBBER,
        ),
        ("cpuid_Deterministic_Cache_Parameters_info", MEM_CLOBBER),
        ("cpuid_Direct_Cache_Access_info", MEM_CLOBBER),
        ("cpuid_Extended_Feature_Enumeration_info", MEM_CLOBBER),
        ("cpuid_Extended_Topology_info", MEM_CLOBBER),
        ("cpuid_MONITOR_MWAIT_Features_info", MEM_CLOBBER),
        ("cpuid_Processor_Extended_States_info", MEM_CLOBBER),
        ("cpuid_Quality_of_Service_info", MEM_CLOBBER),
        ("cpuid_Thermal_Power_Management_info", MEM_CLOBBER),
        ("cpuid_Version_info", MEM_CLOBBER),
        ("cpuid_basic_info", MEM_CLOBBER),
        ("cpuid_brand_part1_info", MEM_CLOBBER),
        ("cpuid_brand_part2_info", MEM_CLOBBER),
        ("cpuid_brand_part3_info", MEM_CLOBBER),
        ("cpuid_cache_tlb_info", MEM_CLOBBER),
        ("cpuid_serial_info", MEM_CLOBBER),
        // SSE4.1 / SSSE3 / SHA-NI SIMD intrinsics: Sleigh carries EVERY
        // operand register as a pcode operand (the `:SHA256RNDS2 XmmReg1,
        // XmmReg2_m128, XMM0` constructor lists the otherwise-implicit XMM0),
        // so there is no implicit register footprint.  A memory source operand
        // (`m128`) is a separate `Load` ahead of the CallOther, so the op
        // itself touches no RAM.
        ("pblendw", PURE),
        ("pshufb", PURE),
        ("sha256rnds2_sha", PURE),
        ("sha256msg1_sha", PURE),
        ("sha256msg2_sha", PURE),
        // NEON / SVE / multi-precision: Sleigh's pcode carries the operand
        // registers, so the user-op is pure compute.
        ("MP_INT_ABS", PURE),
        ("NEON_rev64", PURE),
        ("NEON_sqshl", PURE),
        ("NEON_uaddlv", PURE),
        ("SVE_fnmla", PURE),
        // ARM unmodelled sysreg read: pcode-explicit encoding constant and
        // destination, opaque value, no RAM effect.
        ("UnkSytemRegRead", PURE),
        // ARM permanently-undefined instruction.  Sleigh emits CALLOTHER plus
        // a branch to the trap handler; the user-op itself touches no state.
        // (x86 `swapgs` is in classify_arch_specific so a non-x86 user-op of
        // the same name cannot inherit MemClobber.)
        ("software_udf", PURE),
        // The LDREX/STREX pair, like ExclusiveMonitorPass/Status above: the
        // monitor flag is synthetic, no RAM effect.
        ("ExclusiveAccess", PURE),
        ("hasExclusiveAccess", PURE),
        // ARM PLD / PLDW / YIELD are architectural no-ops, a prefetch being a
        // cache HINT rather than an observable access, but they stay visible
        // pure markers so patterns can find them.
        ("HintPreloadData", PURE),
        ("HintPreloadDataForWrite", PURE),
        ("HintYield", PURE),
        // SEV / SEVL set the event register on other cores and observe no
        // memory on this one.
        ("SendEvent", PURE),
        ("SendEventLocally", PURE),
        // SSAT/USAT and the Q-flag helpers: pure compute, pcode-explicit
        // operands.
        ("SignedSaturate", PURE),
        ("UnsignedSaturate", PURE),
        // NEON scalar conversions, polynomial multiply, and the vector float
        // compare: pure register compute with pcode-explicit operands,
        // companions to the `Vector*` prefix family.
        ("FPToFixed", PURE),
        ("FixedToFP", PURE),
        ("FloatCompareGE", PURE),
        ("PolynomialMultiply", PURE),
        // Interrupt-mask / processor-mode writes (CPSR I/F/A bits, mode
        // field) change privileged CPU state strider does not model, with no
        // data or RAM effect for single-threaded dataflow.  `setISAMode` is a
        // Sleigh DECODER-context switch and stays NoOp above, not here.
        ("disableFIQinterrupts", PURE),
        ("disableIRQinterrupts", PURE),
        ("enableDataAbortInterrupts", PURE),
        ("enableFIQinterrupts", PURE),
        ("enableIRQinterrupts", PURE),
        ("setAbortMode", PURE),
        ("setFIQMode", PURE),
        ("setIRQMode", PURE),
        ("setMonitorMode", PURE),
        ("setStackMode", PURE),
        ("setSupervisorMode", PURE),
        ("setSystemMode", PURE),
        ("setThreadMode", PURE),
        ("setUndefinedMode", PURE),
        ("setUserMode", PURE),
        // Saturation-occurred query: sets the Q flag / returns a bool, pure
        // compute with pcode-explicit operands.
        ("SignedDoesSaturate", PURE),
        ("UnsignedDoesSaturate", PURE),
        // MIPS RDHWR / MFTR / TLBP / TLBR read opaque values into a register
        // with no RAM effect.
        ("TLB_probe_for_matching_entry", PURE),
        ("TLB_read_indexed_entryHi", PURE),
        ("TLB_read_indexed_entryLo0", PURE),
        ("TLB_read_indexed_entryLo1", PURE),
        ("TLB_read_indexed_entryPageMask", PURE),
        ("getHWRegister", PURE),
        ("move_from_thread_cp0", PURE),
        // MIPS PREF, an architectural no-op prefetch hint.
        ("prefetch", PURE),
        // x86 reads into a register with no RAM effect: VERW (writes ZF),
        // RDPMC (perf counter into EDX:EAX, pcode-explicit like RDTSC), and
        // RDRAND / RDSEED (random into a reg plus CF).
        ("rdpmc", PURE),
        ("rdrand", PURE),
        ("rdrandIsValid", PURE),
        ("rdseed", PURE),
        ("rdseedIsValid", PURE),
        ("verw", PURE),
        // x86 port I/O: port and value are pcode-explicit, but the op
        // affects external port state.
        ("in", MEM_CLOBBER),
        ("out", MEM_CLOBBER),
        // The barriers below serialize across ALL reachable memory, the
        // SP-relative stack frame included.  A concurrent writer (another CPU,
        // DMA, the kernel after a mode switch) may have modified this thread's
        // stack before the barrier completes, so forwarding a stack load
        // across one is unsound.  The precision loss is acceptable: these
        // appear in synchronisation code where forwarding rarely helps.
        //
        // The LOCK prefix is a full memory barrier: prior stores become
        // globally visible and subsequent loads see the latest value from ALL
        // CPUs.
        ("LOCK", MEM_CLOBBER),
        ("UNLOCK", MEM_CLOBBER),
        // DSB / DMB are data memory barriers; ISB flushes the instruction
        // pipeline and, conservatively, both instruction and data streams.  On
        // a multicore ARM system the stack frame is reachable from other cores
        // once its address escapes via a pointer argument or shared structure.
        // DC_CVAC (Data Cache to Point of Coherency) interacts with the cache
        // subsystem rather than register-side data.
        ("DC_CVAC", MEM_CLOBBER),
        ("DataMemoryBarrier", MEM_CLOBBER),
        ("DataSynchronizationBarrier", MEM_CLOBBER),
        ("InstructionSynchronizationBarrier", MEM_CLOBBER),
        // Sleigh's x86 spec emits the fences as lowercase mnemonics.  MFENCE
        // serialises all prior and subsequent loads and stores, SFENCE prior
        // stores, LFENCE prior loads.  Like LOCK, each can make a remote
        // core's prior writes visible.
        ("lfence", MEM_CLOBBER),
        ("mfence", MEM_CLOBBER),
        ("sfence", MEM_CLOBBER),
        // PowerPC barriers.  `sync` covers SYNC / lwsync / hwsync: the `L`
        // field selects the variant, but Sleigh folds all three to one name.
        // `enforceInOrderExecutionIO` is EIEIO, an I/O barrier that also acts
        // as a full data-memory barrier on Power ISA.  `instructionSynchronize`
        // is ISYNC, an instruction-pipeline flush treated conservatively as a
        // data clobber.  Without these, any PowerPC binary containing a fence
        // fails with UnknownCallOtherError at the IR layer.
        ("enforceInOrderExecutionIO", MEM_CLOBBER),
        ("instructionSynchronize", MEM_CLOBBER),
        ("sync", MEM_CLOBBER),
        // Two spellings of the one MIPS SYNC mnemonic: GHIDRA's MIPS32 spec
        // emits `SYNC`, its mips.sinc common include emits `synch`.  Without
        // both, a MIPS binary containing SYNC fails with
        // UnknownCallOtherError.
        ("SYNC", MEM_CLOBBER),
        ("synch", MEM_CLOBBER),
        // ARM SVC / SWI raised by an immediate: a possible syscall path, and
        // the kernel can do anything to memory including the user stack frame.
        ("software_interrupt", MEM_CLOBBER),
        // WFE/WFI are synchronisation / low-power wait points: a remote agent
        // may modify shared memory, an escaped stack frame included, while the
        // core waits.
        ("WaitForEvent", MEM_CLOBBER),
        ("WaitForInterrupt", MEM_CLOBBER),
        // BKPT / HLT may return via a WARN-style handler, and HVC / SMC DO
        // return and may mutate memory, so all four are returning
        // side-effecting ops rather than NoReturn: over-terminating would
        // truncate the function.  The SMCCC register footprint is
        // arch-specific and omitted here, leaving these memory-safe but
        // register-imprecise.
        ("software_bkpt", MEM_CLOBBER),
        ("software_hlt", MEM_CLOBBER),
        ("software_hvc", MEM_CLOBBER),
        ("software_smc", MEM_CLOBBER),
        // AArch64 privileged-state writes: an unmodeled sysreg write, the
        // generic SYS write, and address translation (AT_S1E1R writes PAR and
        // pokes the MMU).  Conservatively memory-affecting.
        ("AT_S1E1R", MEM_CLOBBER),
        ("SysOp_W", MEM_CLOBBER),
        ("UnkSytemRegWrite", MEM_CLOBBER),
        // MIPS cache maintenance (CACHE), low-power WAIT, coprocessor
        // register write (MTC0 via setCopReg), cp0-thread write (MTTR), and
        // TLB writes / invalidation (TLBWI/TLBWR/TLBINV): external system
        // state.  TLB READS are PURE, above.
        ("TLB_invalidate", MEM_CLOBBER),
        ("TLB_invalidate_flush", MEM_CLOBBER),
        ("TLB_write_indexed_entry", MEM_CLOBBER),
        ("TLB_write_random_entry", MEM_CLOBBER),
        ("cacheOp", MEM_CLOBBER),
        ("move_to_thread_cp0", MEM_CLOBBER),
        ("setCopReg", MEM_CLOBBER),
        ("wait", MEM_CLOBBER),
        // LGDT/SGDT, LIDT/SIDT, LLDT/SLDT, LTR/STR read from or write to a
        // memory operand describing the table.
        ("GlobalDescriptorTableRegister", MEM_CLOBBER),
        ("InterruptDescriptorTableRegister", MEM_CLOBBER),
        ("LocalDescriptorTableRegister", MEM_CLOBBER),
        ("TaskRegister", MEM_CLOBBER),
        // FXSAVE/FXRSTOR and the XSAVE / XSAVEC / XSAVEOPT / XSAVES / XRSTOR
        // / XRSTORS family (plus 64-bit forms) read or write a large in-memory
        // state area.
        ("_fxrstor", MEM_CLOBBER),
        ("_fxrstor64", MEM_CLOBBER),
        ("_fxsave", MEM_CLOBBER),
        ("_fxsave64", MEM_CLOBBER),
        ("xrstor", MEM_CLOBBER),
        ("xrstor64", MEM_CLOBBER),
        ("xrstors", MEM_CLOBBER),
        ("xrstors64", MEM_CLOBBER),
        ("xsave", MEM_CLOBBER),
        ("xsave64", MEM_CLOBBER),
        ("xsavec", MEM_CLOBBER),
        ("xsavec64", MEM_CLOBBER),
        ("xsaveopt", MEM_CLOBBER),
        ("xsaveopt64", MEM_CLOBBER),
        ("xsaves", MEM_CLOBBER),
        ("xsaves64", MEM_CLOBBER),
        // CLFLUSH, INVLPG, INVPCID: cache / TLB maintenance, relevant to
        // memory ordering.
        ("clflush", MEM_CLOBBER),
        ("invlpg", MEM_CLOBBER),
        ("invpcid", MEM_CLOBBER),
    ];
    TABLE.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// Arch-scoped prefix families, each covering dozens of GHIDRA user-ops (one
/// per named system register, cache-maintenance target, or SIMD op).  Scoped
/// to the arches that define the family so a same-named user-op elsewhere
/// cannot inherit the classification; anything outside a known family returns
/// `None`.
///
/// **Homogeneity was verified against the GHIDRA `.sinc` `define pcodeop`
/// lists**, since a substring rule is only sound if EVERY member shares the
/// classification:
///
/// * `Vector*` (ARM + AArch64 NEON): every member is register-to-register
///   compute, with no `VectorLoad`/`VectorStore` (memory NEON is a separate
///   pcode Load/Store), so the family is `PURE`.
/// * `TLBI_*` / `DC_*` / `IC_*` (AArch64): every member is a TLB-invalidate or
///   cache-maintenance system op, so `MEM_CLOBBER`.
/// * `coproc_movefrom_*` / `coproc_moveto_*` (ARM cp15): NOT homogeneous by
///   direction, since GHIDRA names cache / TLB / barrier / WFI maintenance ops
///   `coproc_movefrom_X` too (`coproc_movefrom_Data_Memory_Barrier`,
///   `coproc_movefrom_Clean_Data_Cache_by_MVA`).  Classified by OPERATION
///   instead: a cache / barrier / sync / invalidate / flush / wait op is a
///   memory-ordering side effect, a plain system-register move is `PURE`.
/// * `coprocessor_*` (ARM generic cp): `movefrom*` is a register read, while
///   the generic `moveto` / `load` / `store` / `function` ops target an
///   unknown coprocessor and may touch RAM.
fn classify_prefix_family(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    use crate::ArchPreset::*;
    const PURE: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: false,
    });
    const MEM_CLOBBER: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: true,
        no_return: false,
    });
    let is_arm32 = matches!(preset, Arm | ArmBe | ArmBeKernel | ArmThumb);
    let is_aarch64 = matches!(preset, Aarch64 | Aarch64Be);

    if (is_arm32 || is_aarch64) && name.starts_with("Vector") {
        return Some(PURE);
    }
    if is_aarch64
        && (name.starts_with("TLBI_") || name.starts_with("DC_") || name.starts_with("IC_"))
    {
        return Some(MEM_CLOBBER);
    }
    if is_arm32 {
        // Classified by operation, not direction.  `TLB_Type` and friends are
        // pure ID reads; the real TLB ops are all named `Invalidate_*`.
        if name.starts_with("coproc_movefrom_") || name.starts_with("coproc_moveto_") {
            // Cache/TLB/barrier maintenance clobbers memory in either direction.
            const SIDE_EFFECT_KEYS: &[&str] = &[
                "Cache",
                "cache",
                "Barrier",
                "Synchron",
                "Invalidate",
                "Clean",
                "Flush",
                "Wait_for",
            ];
            // MMU / translation control: only WRITING TTBR / SCTLR (Control) /
            // CONTEXTIDR (Context_ID) / DACR (Domain_Access) changes the
            // memory-translation view, so a load must not forward across it; a
            // read of the same register is pure.
            const WRITE_CONTROL_KEYS: &[&str] = &[
                "Translation_table",
                "Control",
                "Context_ID",
                "Domain_Access",
            ];
            let clobbers = SIDE_EFFECT_KEYS.iter().any(|k| name.contains(k))
                || (name.starts_with("coproc_moveto_")
                    && WRITE_CONTROL_KEYS.iter().any(|k| name.contains(k)));
            return Some(if clobbers { MEM_CLOBBER } else { PURE });
        }
        if name.starts_with("coprocessor_movefrom") {
            return Some(PURE);
        }
        if name.starts_with("coprocessor_") {
            return Some(MEM_CLOBBER);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_abi() -> CallOtherAbi {
        CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: false,
            no_return: false,
        }
    }

    #[test]
    fn truly_invisible_decoder_context_classifies_as_noop() {
        // On x86/x86_64 the only NoOp user-ops are the Sleigh decoder-context
        // ops (setEndianState / setISAMode).  Memory markers (LOCK / UNLOCK /
        // barriers) and CPU hints are promoted to Call so patterns can find
        // them.  (PowerPC additionally treats some prefetch / MSR-state hints
        // as NoOp; see `classify_ppc`.)
        for n in ["setEndianState", "setISAMode"] {
            assert_eq!(
                classify(crate::ArchPreset::X86_64, n),
                Some(CallOtherClass::NoOp),
                "{n}",
            );
        }
    }

    #[test]
    fn memory_chain_markers_have_mem_edge_and_empty_register_channels() {
        // Barriers must sit on the IR memory chain so patterns walking mem
        // find them, and must have empty register channels since
        // arch-independent entries may not name arch-specific registers.
        for n in [
            "LOCK",
            "UNLOCK",
            "DataMemoryBarrier",
            "DataSynchronizationBarrier",
            "InstructionSynchronizationBarrier",
            "DC_CVAC",
            "lfence",
            "mfence",
            "sfence",
            "enforceInOrderExecutionIO",
            "instructionSynchronize",
            "sync",
            "SYNC",
            "synch",
        ] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n}"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert!(
                abi.implicit_reads.is_empty(),
                "{n}: implicit_reads must be empty"
            );
            assert!(
                abi.implicit_writes.is_empty(),
                "{n}: implicit_writes must be empty"
            );
            assert!(
                abi.clobbers_memory,
                "{n}: must advance mem edge for chain visibility"
            );
        }
    }

    /// LOCK, UNLOCK, and the fences are full serialization barriers that make
    /// all prior stores visible across them, another CPU's writes to an
    /// escaped stack pointer included.  Sparing the stack would let
    /// LoadForward carry a stack value across a barrier, unsound under
    /// shared-stack / aliased-frame patterns.
    #[test]
    fn full_memory_barriers_clobber_memory() {
        for n in [
            "LOCK",
            "UNLOCK",
            "DataMemoryBarrier",
            "DataSynchronizationBarrier",
            "InstructionSynchronizationBarrier",
            "lfence",
            "mfence",
            "sfence",
            "enforceInOrderExecutionIO",
            "instructionSynchronize",
            "sync",
            "SYNC",
            "synch",
        ] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n}"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert!(abi.clobbers_memory, "{n}: barrier ops must clobber memory",);
        }
    }

    #[test]
    fn pure_compute_and_hints_classify_as_pure_no_mem_edge() {
        // Pure compute (NEON, SVE) and non-paired hints (Hint_Prefetch,
        // Yield) stay visible markers but must not advance the memory token,
        // so opt passes can forward through them.  cpuid is NOT here: it is
        // serializing, per
        // `cpuid_family_has_empty_register_abi_but_clobbers_memory`.
        for n in [
            "Hint_Prefetch",
            "Yield",
            "NEON_rev64",
            "SVE_fnmla",
            "MP_INT_ABS",
            "ExclusiveMonitorPass",
            "ExclusiveMonitorsStatus",
            "UnkSytemRegRead",
            "software_udf",
        ] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n}"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert!(abi.implicit_reads.is_empty(), "{n}");
            assert!(abi.implicit_writes.is_empty(), "{n}");
            assert!(
                !abi.clobbers_memory,
                "{n}: must NOT advance mem edge (opt passes need to forward)"
            );
        }
    }

    #[test]
    fn sysret_and_swapgs_are_x86_only() {
        // These once lived in `classify_arch_independent` and matched on
        // ARM/AArch64/MIPS/PowerPC too.
        for arch in [
            crate::ArchPreset::Arm,
            crate::ArchPreset::ArmBe,
            crate::ArchPreset::ArmThumb,
            crate::ArchPreset::Aarch64,
            crate::ArchPreset::Aarch64Be,
            crate::ArchPreset::MipsLe32,
            crate::ArchPreset::MipsBe32,
            crate::ArchPreset::MipsLe64,
            crate::ArchPreset::MipsBe64,
            crate::ArchPreset::Ppc32Le,
            crate::ArchPreset::Ppc32Be,
            crate::ArchPreset::Ppc64Le,
            crate::ArchPreset::Ppc64Be,
        ] {
            assert_eq!(classify(arch, "sysret"), None, "sysret on {arch:?}");
            assert_eq!(classify(arch, "swapgs"), None, "swapgs on {arch:?}");
        }
        // Still classified on x86 / x86_64.
        assert_eq!(
            classify(crate::ArchPreset::X86, "sysret"),
            Some(CallOtherClass::NO_RETURN)
        );
        assert_eq!(
            classify(crate::ArchPreset::X86_64, "sysret"),
            Some(CallOtherClass::NO_RETURN)
        );
    }

    #[test]
    fn monitor_mwait_implicit_register_channels() {
        // Sleigh emits `monitor()` / `mwait()` with zero pcode operands, so
        // the register reads have to live in `implicit_reads`.  Per Intel SDM
        // Vol. 2B 4-39 (MONITOR) and 4-44 (MWAIT).
        let m64 = classify(crate::ArchPreset::X86_64, "monitor").expect("monitor x86_64");
        let CallOtherClass::Call(abi) = m64 else {
            panic!("expected Call(abi) for monitor")
        };
        assert_eq!(abi.implicit_reads, &["RAX", "ECX", "EDX"]);
        assert!(abi.implicit_writes.is_empty());
        assert!(abi.clobbers_memory);

        let m32 = classify(crate::ArchPreset::X86, "monitor").expect("monitor x86");
        let CallOtherClass::Call(abi) = m32 else {
            panic!()
        };
        assert_eq!(abi.implicit_reads, &["EAX", "ECX", "EDX"]);

        let mwait = classify(crate::ArchPreset::X86_64, "mwait").expect("mwait classified");
        let CallOtherClass::Call(abi) = mwait else {
            panic!()
        };
        assert_eq!(abi.implicit_reads, &["EAX", "ECX"]);
        assert!(abi.implicit_writes.is_empty());
        assert!(abi.clobbers_memory);

        // AMD variants share the same shape.
        assert!(matches!(
            classify(crate::ArchPreset::X86_64, "monitorx"),
            Some(CallOtherClass::Call(_))
        ));
        assert!(matches!(
            classify(crate::ArchPreset::X86_64, "mwaitx"),
            Some(CallOtherClass::Call(_))
        ));

        // `monitor` is an English word and could appear in a future spec, so
        // the arch-specific guard has to keep it from matching elsewhere.
        assert_eq!(classify(crate::ArchPreset::Aarch64, "monitor"), None);
        assert_eq!(classify(crate::ArchPreset::Aarch64, "mwait"), None);
    }

    #[test]
    fn swapgs_is_memory_chain_marker() {
        // SWAPGS exchanges IA32_GS_BASE with IA32_KERNEL_GS_BASE, and
        // subsequent %gs:-relative accesses depend on the new base.  Without
        // the memory edge, LoadForward / LoadReadOnly would forward across
        // swapgs in kernel entry/exit code.
        let cls = classify(crate::ArchPreset::X86_64, "swapgs").unwrap();
        let CallOtherClass::Call(abi) = cls else {
            panic!("expected Call(abi)")
        };
        assert!(abi.implicit_reads.is_empty());
        assert!(abi.implicit_writes.is_empty());
        assert!(
            abi.clobbers_memory,
            "swapgs must advance memory edge (kernel GS base swap)"
        );
    }

    #[test]
    fn known_trap_classifies_as_noreturn() {
        for n in [
            "invalidInstructionException",
            "SoftwareBreakpoint",
            "UndefinedInstructionException",
            "sysret",
            "trap",
        ] {
            assert_eq!(
                classify(crate::ArchPreset::X86_64, n),
                Some(CallOtherClass::NO_RETURN),
                "{n}"
            );
        }
    }

    #[test]
    fn ppc_call_others_classify_by_effect() {
        use crate::ArchPreset::{Ppc32Be, Ppc64Be};
        let mem =
            |n| matches!(classify(Ppc64Be, n), Some(CallOtherClass::Call(a)) if a.clobbers_memory);
        let pure = |n| matches!(classify(Ppc64Be, n), Some(CallOtherClass::Call(a)) if !a.clobbers_memory && !a.no_return);

        // Stores, atomics, and cache/TLB/SLB modification advance the memory
        // edge.
        for n in [
            "storeWordConditionalIndexed",
            "storeDoubleWordConditionalIndexed",
            "dataCacheBlockClearToZero",
            "dataCacheBlockInvalidate",
            "TLBInvalidateEntry",
            "slbMoveToEntry",
        ] {
            assert!(mem(n), "{n} should clobber memory");
        }
        // Loads, SLB reads, the RNG, and every Altivec/VSX/vector compute op
        // are pure.
        for n in [
            "LoadDoublewordByteReverseIndexed",
            "slbMoveFromEntryVSID",
            "random",
            "altv207_45",              // prefix
            "vsx300_20",               // prefix
            "vectorConditionalSelect", // prefix
        ] {
            assert!(pure(n), "{n} should be pure");
        }
        // `returnFromInterrupt` ends the function.
        assert_eq!(
            classify(Ppc64Be, "returnFromInterrupt"),
            Some(CallOtherClass::NO_RETURN)
        );
        // Conditional traps FALL THROUGH in GHIDRA's model, so they must be
        // returning (memory-clobbering), never NO_RETURN, or code after a
        // compiler bounds/overflow trap is dropped from the IR.
        for n in ["trapWord", "trapDoubleWordImmediate"] {
            assert!(mem(n), "{n} must clobber memory");
            assert!(
                !classify(Ppc64Be, n).unwrap().is_no_return(),
                "{n} must return (fall-through)"
            );
        }
        // Prefetch hints are no-ops; `wait` parks the core so a remote agent
        // may touch memory, hence memory-clobbering.
        assert_eq!(
            classify(Ppc32Be, "dataCacheBlockTouch"),
            Some(CallOtherClass::NoOp)
        );
        assert!(mem("waitT"), "waitT must clobber memory");

        // The generic-word ops must not leak to another arch.
        for n in ["random", "message", "trapWord", "vectorConditionalSelect"] {
            assert_eq!(
                classify(crate::ArchPreset::Aarch64, n),
                None,
                "{n} leaked to AArch64"
            );
        }
    }

    #[test]
    fn syscall_has_linux_x86_64_abi() {
        let class = classify(crate::ArchPreset::X86_64, "syscall").expect("syscall classified");
        let CallOtherClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(
            abi.implicit_reads,
            &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"]
        );
        assert_eq!(abi.implicit_writes, &["RAX", "RCX", "R11"]);
        assert!(abi.clobbers_memory);
    }

    #[test]
    fn cpuid_family_has_empty_register_abi_but_clobbers_memory() {
        // Sleigh's cpuid lift picks one of cpuid / cpuid_* by EAX, returns a
        // tmpptr, then emits Loads for EAX/EBX/EDX/ECX from it, so the register
        // channels are pcode-explicit and the ABI's stay empty.  But cpuid is
        // SERIALIZING (SDM Vol. 3 8.3), a full ordering barrier stronger than
        // MFENCE, so it must advance the memory edge: a load after cpuid may
        // observe a concurrent write cpuid is the barrier for.
        for n in [
            "cpuid",
            "cpuid_basic_info",
            "cpuid_Version_info",
            "cpuid_cache_tlb_info",
            "cpuid_serial_info",
            "cpuid_Deterministic_Cache_Parameters_info",
            "cpuid_MONITOR_MWAIT_Features_info",
            "cpuid_Thermal_Power_Management_info",
            "cpuid_Extended_Feature_Enumeration_info",
            "cpuid_Direct_Cache_Access_info",
            "cpuid_Architectural_Performance_Monitoring_info",
            "cpuid_Extended_Topology_info",
            "cpuid_Processor_Extended_States_info",
            "cpuid_Quality_of_Service_info",
            "cpuid_brand_part1_info",
            "cpuid_brand_part2_info",
            "cpuid_brand_part3_info",
        ] {
            let class =
                classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n} classified"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert!(abi.implicit_reads.is_empty(), "{n}");
            assert!(abi.implicit_writes.is_empty(), "{n}");
            assert!(
                abi.clobbers_memory,
                "{n}: cpuid is serializing — must advance the memory edge",
            );
            assert!(!abi.no_return, "{n}: cpuid returns");
        }
    }

    #[test]
    fn rdtsc_has_no_implicit_writes_no_memory_edge() {
        // Sleigh emits the EDX/EAX writes as explicit pcode ops after the
        // CALLOTHER (`tmp:8 = rdtsc(); EDX = tmp(4); EAX = tmp(0);`), so
        // re-declaring them implicit would double-clobber the call site.
        let class = classify(crate::ArchPreset::X86_64, "rdtsc").expect("rdtsc classified");
        let CallOtherClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(abi.implicit_reads, &[] as &[&str]);
        assert_eq!(abi.implicit_writes, &[] as &[&str]);
        assert!(!abi.clobbers_memory);
    }

    #[test]
    fn rdtscp_writes_eax_edx_ecx_no_memory_edge() {
        // Unlike RDTSC, RDTSCP also writes ECX (the IA32_TSC_AUX MSR's low 32
        // bits), and a query reading post-RDTSCP ECX must see the clobber.
        let class = classify(crate::ArchPreset::X86_64, "rdtscp").expect("rdtscp classified");
        let CallOtherClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(abi.implicit_reads, &[] as &[&str]);
        assert_eq!(abi.implicit_writes, &["EAX", "EDX", "ECX"]);
        assert!(!abi.clobbers_memory);
    }

    #[test]
    fn empty_abi_ops_use_call_with_empty_abi() {
        // swapgs is excluded on purpose: it clobbers memory and is covered by
        // `swapgs_is_memory_chain_marker`.
        for n in [
            "NEON_rev64",
            "NEON_sqshl",
            "NEON_uaddlv",
            "SVE_fnmla",
            "MP_INT_ABS",
            "UnkSytemRegRead",
            "ExclusiveMonitorPass",
            "ExclusiveMonitorsStatus",
        ] {
            let class =
                classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n} classified"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert_eq!(abi, empty_abi(), "{n}");
        }
    }

    #[test]
    fn smccc_ops_read_x0_x7_and_clobber_x0_x17() {
        // SMCCC lives in classify_arch_specific because x0..x17 only resolve
        // on aarch64.
        for preset in [crate::ArchPreset::Aarch64, crate::ArchPreset::Aarch64Be] {
            for n in ["CallHyperVisor", "CallSecureMonitor"] {
                let class = classify(preset, n).unwrap_or_else(|| panic!("{preset:?}/{n}"));
                let CallOtherClass::Call(abi) = class else {
                    panic!("{preset:?}/{n}: expected Call")
                };
                assert_eq!(
                    abi.implicit_reads,
                    &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
                    "{preset:?}/{n}",
                );
                // x0..x3 are the result; x4..x17 are conservatively clobbered
                // (unpredictable under SMCCC 1.0).
                assert_eq!(
                    abi.implicit_writes,
                    &[
                        "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11",
                        "x12", "x13", "x14", "x15", "x16", "x17",
                    ],
                    "{preset:?}/{n}"
                );
                assert!(abi.clobbers_memory, "{preset:?}/{n}");
            }
            assert_eq!(classify(crate::ArchPreset::X86_64, "CallHyperVisor"), None);
            assert_eq!(classify(crate::ArchPreset::Arm, "CallHyperVisor"), None);
        }
    }

    #[test]
    fn rdpkru_is_arch_specific_to_x86() {
        // rdpkru_u32 lives in classify_arch_specific because ECX/EAX/EDX only
        // resolve on x86 / x86_64.
        for preset in [crate::ArchPreset::X86, crate::ArchPreset::X86_64] {
            let class = classify(preset, "rdpkru_u32").expect("rdpkru classified");
            let CallOtherClass::Call(abi) = class else {
                panic!("expected Call")
            };
            assert_eq!(abi.implicit_reads, &["ECX"]);
            // EAX is the op's explicit pcode output, not an implicit write.
            assert_eq!(abi.implicit_writes, &["EDX"]);
            assert!(!abi.clobbers_memory);
        }
        assert_eq!(classify(crate::ArchPreset::Aarch64, "rdpkru_u32"), None);
        assert_eq!(classify(crate::ArchPreset::Arm, "rdpkru_u32"), None);
    }

    #[test]
    fn msr_and_segment_base_ops_classify_pure_or_pure_with_mem_edge() {
        // Sleigh puts every register operand of these ops on the explicit
        // pcode chain, so there is no implicit channel to model.  The writes
        // carry a memory edge so opt passes cannot forward across them.
        let pure_ops = ["rdmsr", "readfsbase", "readgsbase"];
        let edge_ops = ["wrmsr", "writefsbase", "writegsbase"];
        for preset in [crate::ArchPreset::X86, crate::ArchPreset::X86_64] {
            for n in pure_ops {
                let class = classify(preset, n).unwrap_or_else(|| panic!("{preset:?}/{n}"));
                let CallOtherClass::Call(abi) = class else {
                    panic!("{preset:?}/{n}: expected Call")
                };
                assert_eq!(abi, empty_abi(), "{preset:?}/{n}");
                assert!(!abi.clobbers_memory, "{preset:?}/{n}");
            }
            for n in edge_ops {
                let class = classify(preset, n).unwrap_or_else(|| panic!("{preset:?}/{n}"));
                let CallOtherClass::Call(abi) = class else {
                    panic!("{preset:?}/{n}: expected Call")
                };
                assert_eq!(abi.implicit_reads, &[] as &[&str], "{preset:?}/{n}");
                assert_eq!(abi.implicit_writes, &[] as &[&str], "{preset:?}/{n}");
                assert!(abi.clobbers_memory, "{preset:?}/{n}");
            }
        }
        // The encoded instructions only exist on x86/x86_64.
        for n in pure_ops.iter().chain(edge_ops.iter()) {
            assert_eq!(
                classify(crate::ArchPreset::Aarch64, n),
                None,
                "{n} on aarch64"
            );
            assert_eq!(classify(crate::ArchPreset::Arm, n), None, "{n} on arm");
        }
    }

    #[test]
    fn syscall_is_arch_specific_to_x86_64() {
        // The arch-independent fallback must not supply "syscall" for
        // non-x86_64 presets, whose RAX/RDI names would not resolve.
        assert!(matches!(
            classify(crate::ArchPreset::X86_64, "syscall"),
            Some(CallOtherClass::Call(_)),
        ));
        assert_eq!(classify(crate::ArchPreset::X86, "syscall"), None);
        assert_eq!(classify(crate::ArchPreset::Aarch64, "syscall"), None);
        assert_eq!(classify(crate::ArchPreset::Arm, "syscall"), None);
    }

    #[test]
    fn arch_independent_call_entries_have_empty_register_channels() {
        // Named registers would tie an entry to one arch's Sleigh register
        // table, putting it in `classify_arch_specific` instead.
        //
        // The table is not iterable from here, so this enumerates every name
        // expected to be arch-independent and checks each.
        let arch_independent_names = [
            // NoOp, empty by definition; listed for completeness.
            "DC_CVAC",
            "DataMemoryBarrier",
            "DataSynchronizationBarrier",
            "Hint_Prefetch",
            "InstructionSynchronizationBarrier",
            "LOCK",
            "UNLOCK",
            "Yield",
            "setEndianState",
            "setISAMode",
            // NoReturn
            "SoftwareBreakpoint",
            "UndefinedInstructionException",
            "invalidInstructionException",
            "sysret",
            "trap",
            // Call with empty channels
            "ExclusiveMonitorPass",
            "ExclusiveMonitorsStatus",
            "MP_INT_ABS",
            "NEON_rev64",
            "NEON_sqshl",
            "NEON_uaddlv",
            "SVE_fnmla",
            "UnkSytemRegRead",
            "cpuid",
            "cpuid_basic_info",
            "cpuid_Architectural_Performance_Monitoring_info",
            "cpuid_Deterministic_Cache_Parameters_info",
            "cpuid_Direct_Cache_Access_info",
            "cpuid_Extended_Feature_Enumeration_info",
            "cpuid_Extended_Topology_info",
            "cpuid_MONITOR_MWAIT_Features_info",
            "cpuid_Processor_Extended_States_info",
            "cpuid_Quality_of_Service_info",
            "cpuid_Thermal_Power_Management_info",
            "cpuid_Version_info",
            "cpuid_brand_part1_info",
            "cpuid_brand_part2_info",
            "cpuid_brand_part3_info",
            "cpuid_cache_tlb_info",
            "cpuid_serial_info",
            "in",
            "out",
            "software_interrupt",
            "software_udf",
            "swapgs",
            // PowerPC barriers
            "enforceInOrderExecutionIO",
            "instructionSynchronize",
            "sync",
            // MIPS barriers
            "SYNC",
            "synch",
            // x86 fences
            "lfence",
            "mfence",
            "sfence",
        ];
        // Any preset works: these resolve identically on every arch.
        for n in arch_independent_names {
            let class = match classify(crate::ArchPreset::X86_64, n) {
                Some(c) => c,
                None => continue, // not in table
            };
            let abi = match class {
                CallOtherClass::Call(abi) => abi,
                CallOtherClass::NoOp => continue, // NoOp emits no node
            };
            assert!(
                abi.implicit_reads.is_empty(),
                "arch-independent entry {n:?} has non-empty implicit_reads \
                 ({:?}); move it to classify_arch_specific",
                abi.implicit_reads,
            );
            assert!(
                abi.implicit_writes.is_empty(),
                "arch-independent entry {n:?} has non-empty implicit_writes \
                 ({:?}); move it to classify_arch_specific",
                abi.implicit_writes,
            );
        }
    }

    #[test]
    fn port_io_has_memory_edge_no_implicit_regs() {
        for n in ["in", "out"] {
            let class = classify(crate::ArchPreset::X86_64, n).expect(n);
            let CallOtherClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert_eq!(abi.implicit_reads, &[] as &[&str], "{n}");
            assert_eq!(abi.implicit_writes, &[] as &[&str], "{n}");
            assert!(abi.clobbers_memory, "{n}");
        }
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(
            classify(crate::ArchPreset::X86_64, "nonexistent_op_xyzzy_abc"),
            None
        );
    }

    #[test]
    fn software_interrupt_on_arm_family_returns_linux_arm_abi() {
        // ARM emits `software_interrupt` (not `swi`) for SVC; the arch-specific
        // row wins over the generic MEM_CLOBBER fallback for all four presets.
        for preset in [
            crate::ArchPreset::Arm,
            crate::ArchPreset::ArmBe,
            crate::ArchPreset::ArmBeKernel,
            crate::ArchPreset::ArmThumb,
        ] {
            let class = classify(preset, "software_interrupt")
                .unwrap_or_else(|| panic!("{preset:?}/software_interrupt"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{preset:?}: expected Call, got {class:?}")
            };
            assert_eq!(
                abi.implicit_reads,
                &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
                "{preset:?}",
            );
            assert_eq!(abi.implicit_writes, &["r0"], "{preset:?}");
            assert!(abi.clobbers_memory, "{preset:?}");
        }
    }

    #[test]
    fn swi_on_x86_returns_empty_call_stub() {
        // A register-empty Call with a full memory clobber is the sound stub
        // until per-INT-vector / per-OS modelling lands: INT 0x80 is a kernel
        // entry that can mutate the user stack.
        let stub = CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            clobbers_memory: true,
            no_return: false,
        });
        assert_eq!(classify(crate::ArchPreset::X86, "swi"), Some(stub));
        assert_eq!(classify(crate::ArchPreset::X86_64, "swi"), Some(stub));
    }

    #[test]
    fn arch_independent_entries_resolve_on_every_arch() {
        for arch in [
            crate::ArchPreset::X86,
            crate::ArchPreset::X86_64,
            crate::ArchPreset::Arm,
            crate::ArchPreset::Aarch64,
        ] {
            // The Sleigh decoder bit is the only true NoOp.
            assert_eq!(
                classify(arch, "setISAMode"),
                Some(CallOtherClass::NoOp),
                "arch={arch:?}",
            );
            let dmb = classify(arch, "DataMemoryBarrier")
                .unwrap_or_else(|| panic!("arch={arch:?}: DataMemoryBarrier"));
            let CallOtherClass::Call(abi) = dmb else {
                panic!("arch={arch:?}: DMB expected Call, got {dmb:?}")
            };
            assert!(
                abi.clobbers_memory,
                "arch={arch:?}: DMB must advance mem edge"
            );
            assert_eq!(
                classify(arch, "invalidInstructionException"),
                Some(CallOtherClass::NO_RETURN),
                "arch={arch:?}",
            );
        }
    }

    #[test]
    fn sleigh_arch_presets_set_distinct_preset_discriminators() {
        // One ArchPreset per constructor, so Arm-32 LE / BE / Thumb stay
        // distinguishable.
        use crate::SleighArch;
        assert_eq!(SleighArch::x86_64().preset, crate::ArchPreset::X86_64);
        assert_eq!(SleighArch::x86().preset, crate::ArchPreset::X86);
        assert_eq!(SleighArch::arm().preset, crate::ArchPreset::Arm);
        assert_eq!(SleighArch::arm_be().preset, crate::ArchPreset::ArmBe);
        assert_eq!(SleighArch::arm_thumb().preset, crate::ArchPreset::ArmThumb);
        assert_eq!(SleighArch::aarch64().preset, crate::ArchPreset::Aarch64);
        assert_eq!(SleighArch::aarch64be().preset, crate::ArchPreset::Aarch64Be);
        assert_eq!(SleighArch::mipsbe32().preset, crate::ArchPreset::MipsBe32);
        assert_eq!(SleighArch::mipsle32().preset, crate::ArchPreset::MipsLe32);
        assert_eq!(SleighArch::mipsbe64().preset, crate::ArchPreset::MipsBe64);
        assert_eq!(SleighArch::mipsle64().preset, crate::ArchPreset::MipsLe64);
        assert_eq!(SleighArch::ppc32be().preset, crate::ArchPreset::Ppc32Be);
        assert_eq!(SleighArch::ppc32le().preset, crate::ArchPreset::Ppc32Le);
        assert_eq!(SleighArch::ppc64be().preset, crate::ArchPreset::Ppc64Be);
        assert_eq!(SleighArch::ppc64le().preset, crate::ArchPreset::Ppc64Le);
    }

    #[test]
    fn opaque_variant_does_not_exist() {
        // The exhaustive match below is a compile-time guard: adding or
        // removing a `CallOtherClass` variant breaks the build here.
        for n in ["setISAMode", "invalidInstructionException", "cpuid"] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap();
            match class {
                CallOtherClass::NoOp | CallOtherClass::Call(_) => {}
            }
        }
    }

    /// Without these entries any binary using SSE memory fences would lift to
    /// UnknownCallOtherError at the IR layer.
    #[test]
    fn x86_memory_fences_classify_with_full_clobber() {
        for preset in [crate::ArchPreset::X86, crate::ArchPreset::X86_64] {
            for name in ["mfence", "sfence", "lfence"] {
                let cls = classify(preset, name)
                    .unwrap_or_else(|| panic!("({preset:?}, {name}) must classify"));
                let abi = match cls {
                    CallOtherClass::Call(abi) => abi,
                    other => panic!("({preset:?}, {name}) classified as {other:?}, expected Call"),
                };
                assert!(
                    abi.implicit_reads.is_empty(),
                    "({preset:?}, {name}) must have empty implicit_reads"
                );
                assert!(
                    abi.implicit_writes.is_empty(),
                    "({preset:?}, {name}) must have empty implicit_writes"
                );
                assert!(
                    abi.clobbers_memory,
                    "({preset:?}, {name}) must advance memory edge — fences are ordering primitives"
                );
            }
        }
    }

    /// `sync`, `enforceInOrderExecutionIO`, and `instructionSynchronize` must
    /// clobber memory so they stay visible on the IR memory chain.  Without
    /// them any PowerPC binary containing a barrier would fail with
    /// UnknownCallOtherError.
    #[test]
    fn powerpc_barriers_classify_with_full_clobber() {
        for preset in [
            crate::ArchPreset::Ppc32Be,
            crate::ArchPreset::Ppc32Le,
            crate::ArchPreset::Ppc64Be,
            crate::ArchPreset::Ppc64Le,
        ] {
            for name in [
                "sync",
                "enforceInOrderExecutionIO",
                "instructionSynchronize",
            ] {
                let cls = classify(preset, name)
                    .unwrap_or_else(|| panic!("({preset:?}, {name}) must classify"));
                let abi = match cls {
                    CallOtherClass::Call(abi) => abi,
                    other => panic!("({preset:?}, {name}) classified as {other:?}, expected Call"),
                };
                assert!(
                    abi.implicit_reads.is_empty(),
                    "({preset:?}, {name}) implicit_reads"
                );
                assert!(
                    abi.implicit_writes.is_empty(),
                    "({preset:?}, {name}) implicit_writes"
                );
                assert!(
                    abi.clobbers_memory,
                    "({preset:?}, {name}) must advance mem edge"
                );
            }
        }
    }

    /// Without `SYNC` and `synch` any MIPS binary containing a SYNC would
    /// fail with UnknownCallOtherError.
    #[test]
    fn mips_barriers_classify_with_full_clobber() {
        for preset in [
            crate::ArchPreset::MipsBe32,
            crate::ArchPreset::MipsLe32,
            crate::ArchPreset::MipsBe64,
            crate::ArchPreset::MipsLe64,
        ] {
            for name in ["SYNC", "synch"] {
                let cls = classify(preset, name)
                    .unwrap_or_else(|| panic!("({preset:?}, {name}) must classify"));
                let abi = match cls {
                    CallOtherClass::Call(abi) => abi,
                    other => panic!("({preset:?}, {name}) classified as {other:?}, expected Call"),
                };
                assert!(
                    abi.implicit_reads.is_empty(),
                    "({preset:?}, {name}) implicit_reads"
                );
                assert!(
                    abi.implicit_writes.is_empty(),
                    "({preset:?}, {name}) implicit_writes"
                );
                assert!(
                    abi.clobbers_memory,
                    "({preset:?}, {name}) must advance mem edge"
                );
            }
        }
    }

    fn x86_64_sleigh_regs() -> rsleigh::SleighRegs {
        let arch = crate::SleighArch::x86_64();
        arch.probe_regs()
            .expect("probe_regs must succeed for x86_64")
    }

    #[test]
    fn build_syscall_x86_64_resolves_correct_vns() {
        let regs = x86_64_sleigh_regs();
        let abi =
            match classify(crate::ArchPreset::X86_64, "syscall").expect("syscall must classify") {
                CallOtherClass::Call(abi) => abi,
                other => panic!("expected Call(abi), got {other:?}"),
            };

        let built = abi.build(&regs).expect("syscall ABI must build on x86_64");

        let rax = regs.name_to_vn("RAX").expect("RAX must exist");
        assert!(
            built.implicit_reads.contains(&rax),
            "RAX must be in implicit_reads"
        );
        assert!(
            built.implicit_writes.contains(&rax),
            "RAX must be in implicit_writes"
        );
        assert!(built.clobbers_memory, "syscall must clobber memory");

        // Full channel comparison against a per-name lookup.
        let expected_reads: Vec<rsleigh::Vn> = abi
            .implicit_reads
            .iter()
            .map(|n| {
                regs.name_to_vn(n)
                    .unwrap_or_else(|| panic!("reg {n:?} not found"))
            })
            .collect();
        let expected_writes: Vec<rsleigh::Vn> = abi
            .implicit_writes
            .iter()
            .map(|n| {
                regs.name_to_vn(n)
                    .unwrap_or_else(|| panic!("reg {n:?} not found"))
            })
            .collect();
        assert_eq!(
            built.implicit_reads, expected_reads,
            "implicit_reads mismatch"
        );
        assert_eq!(
            built.implicit_writes, expected_writes,
            "implicit_writes mismatch"
        );
        assert_eq!(built.clobbers_memory, abi.clobbers_memory);
    }

    #[test]
    fn build_empty_channels_produces_empty_vecs() {
        let regs = x86_64_sleigh_regs();
        let abi = match classify(crate::ArchPreset::X86_64, "rdtsc").expect("rdtsc must classify") {
            CallOtherClass::Call(abi) => abi,
            other => panic!("expected Call(abi), got {other:?}"),
        };

        let built = abi.build(&regs).expect("rdtsc ABI must build");
        assert!(
            built.implicit_reads.is_empty(),
            "rdtsc has no implicit reads"
        );
        assert!(
            built.implicit_writes.is_empty(),
            "rdtsc has no implicit writes"
        );
        assert!(!built.clobbers_memory, "rdtsc does not clobber memory");
    }

    #[test]
    fn build_unknown_register_name_errors() {
        let regs = x86_64_sleigh_regs();
        let abi = CallOtherAbi {
            implicit_reads: &["NONEXISTENT_REG_XYZZY"],
            implicit_writes: &[],
            clobbers_memory: false,
            no_return: false,
        };
        let result = abi.build(&regs);
        assert!(result.is_err(), "unknown register must produce an error");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("NONEXISTENT_REG_XYZZY"),
            "error must name the bad register; got: {msg}",
        );
    }

    fn mem_clobbers(c: Option<CallOtherClass>) -> bool {
        matches!(c, Some(CallOtherClass::Call(abi)) if abi.clobbers_memory)
    }
    fn is_pure(c: Option<CallOtherClass>) -> bool {
        matches!(c, Some(CallOtherClass::Call(abi)) if !abi.clobbers_memory && !abi.no_return)
    }

    #[test]
    fn coproc_family_classifies_by_operation_not_direction() {
        use crate::ArchPreset::Arm;
        // The MRC/MCR direction does NOT indicate a side effect: cache / TLB
        // / barrier / WFI maintenance ops are named `coproc_movefrom_*` too and
        // must clobber memory, while a plain system-register move must not.
        for op in [
            "coproc_movefrom_Data_Memory_Barrier",
            "coproc_movefrom_Clean_Data_Cache_by_MVA",
            "coproc_movefrom_Invalidate_unified_TLB_unlocked",
            "coproc_movefrom_Data_Synchronization",
            "coproc_movefrom_Flush_Prefetch_Buffer",
            "coproc_movefrom_Wait_for_interrupt",
            "coproc_moveto_Clean_Entire_Data_Cache",
            "coproc_moveto_Invalidate_Entire_Instruction",
            // Writing MMU / translation-control registers changes the memory
            // view, so a load must not forward across the write.
            "coproc_moveto_Control",
            "coproc_moveto_Context_ID",
            "coproc_moveto_Translation_table_base_0",
            "coproc_moveto_Domain_Access_Control",
        ] {
            assert!(
                mem_clobbers(classify(Arm, op)),
                "{op} is a cache/TLB/barrier or MMU-control write — must MEM_CLOBBER",
            );
        }
        for op in [
            "coproc_movefrom_Main_ID",
            "coproc_movefrom_User_R_Thread_and_Process_ID",
            // Reading a control register is pure; only the write clobbers.
            "coproc_movefrom_Control",
            "coproc_movefrom_Translation_table_base_0",
        ] {
            assert!(
                is_pure(classify(Arm, op)),
                "{op} is a plain system-register read — must be PURE",
            );
        }
    }

    #[test]
    fn prefix_families_are_arch_scoped() {
        use crate::ArchPreset::{Aarch64, Arm, X86_64};
        // NEON compute is ARM/AArch64 only.
        assert!(is_pure(classify(Arm, "VectorMultiply")));
        assert!(is_pure(classify(Aarch64, "VectorMultiply")));
        assert_eq!(classify(X86_64, "VectorMultiply"), None);
        // The AArch64 cache/TLB families have no 32-bit ARM or x86 user-op.
        assert!(mem_clobbers(classify(Aarch64, "TLBI_ALLE1")));
        assert!(mem_clobbers(classify(Aarch64, "DC_ZVA")));
        assert!(mem_clobbers(classify(Aarch64, "IC_IALLU")));
        assert_eq!(classify(Arm, "TLBI_ALLE1"), None);
        assert_eq!(classify(X86_64, "DC_ZVA"), None);
        assert_eq!(classify(X86_64, "coproc_movefrom_Main_ID"), None);
    }

    #[test]
    fn generic_coprocessor_ops_split_read_vs_opaque() {
        use crate::ArchPreset::ArmThumb;
        // Reads are pure; opaque moveto/load/store/function clobber memory.
        assert!(is_pure(classify(ArmThumb, "coprocessor_movefromRt")));
        assert!(is_pure(classify(ArmThumb, "coprocessor_movefrom2")));
        for op in [
            "coprocessor_moveto",
            "coprocessor_moveto2",
            "coprocessor_load",
            "coprocessor_storelong",
            "coprocessor_function",
        ] {
            assert!(mem_clobbers(classify(ArmThumb, op)), "{op}");
        }
    }
}
