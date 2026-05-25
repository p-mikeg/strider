//! Sleigh user-op (CallOther) classification table.  See
//! `docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`
//! (and the predecessor spec `2026-05-05-callother-classification-design.md`
//! for the original cfg/ir consumer split).

use crate::alias_class::{
    AliasClass, MEM_CLOBBER_FULL, MEM_CLOBBER_HEAP_UNKNOWN, MEM_CLOBBER_NONE,
};

/// Per-user-op ABI describing register and memory effects beyond
/// what Sleigh's pcode insn already encodes.  Sleigh emits
/// `CALLOTHER(user_op_id, args…)` with a possible `output` field;
/// the ABI fills in the *implicit* (ISA-fixed, not in pcode)
/// channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallOtherAbi {
    /// Register names this op reads beyond Sleigh's pcode-explicit
    /// `inputs[1..]`.  Resolved to `rsleigh::Vn` by the strider
    /// layer at lift time and appended to the CallOther's value
    /// inputs.  Use the exact Sleigh register name (case-sensitive).
    pub implicit_reads: &'static [&'static str],

    /// Register names this op writes (or scratch-clobbers) beyond
    /// Sleigh's pcode-explicit `output`.  Each becomes one extra
    /// clobber output slot on the CallOther node; the strider layer
    /// rebinds the matching tracked variable to that slot.
    pub implicit_writes: &'static [&'static str],

    /// Set of memory partitions whose memory token this op clobbers.
    /// Replaces the old coarse `memory_edge: bool` — `[]` means "no
    /// memory effect" (pure compute, identical to `memory_edge: false`)
    /// and any non-empty set advances the IR's memory edge and tells
    /// `AliasSplit` which per-partition chains to break across this
    /// op.
    ///
    /// Common choices:
    /// * `MEM_CLOBBER_NONE` (`&[]`) — pure compute (cpuid, rdtsc).
    /// * `MEM_CLOBBER_HEAP_UNKNOWN` — atomic / barrier / port-I/O
    ///   ops that disturb heap and unmapped memory but provably leave
    ///   the user-mode stack frame intact.
    /// * `MEM_CLOBBER_FULL` — SYSCALL and any kernel-entry path that
    ///   may also mutate the user-mode stack frame.
    pub mem_clobbers: &'static [AliasClass],
}

impl CallOtherAbi {
    /// Convenience: does this op advance the IR's memory edge at all?
    /// Equivalent to the old `memory_edge: bool` accessor — true when
    /// at least one partition is clobbered.
    #[must_use]
    #[inline]
    pub fn has_memory_edge(&self) -> bool {
        !self.mem_clobbers.is_empty()
    }
}

/// What `strider::handle_call_other` does for a given user-op name.
/// Single source of truth for all CallOther dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOtherClass {
    /// True no-op.  No IR node emitted; control / memory unchanged;
    /// pcode-explicit output (if any) is ignored.
    NoOp,

    /// Trap — control flow ends here.  cfg terminates the region as
    /// `RegionTerminator::NoReturn`; ir's `build_call_other_terminal`
    /// emits a `[ctrl, mem]` → `[ctrl, mem]` CallOther whose outputs
    /// dangle.
    NoReturn,

    /// Op with a precise ABI describing its register footprint and
    /// memory effect beyond what Sleigh's pcode already encodes.
    Call(CallOtherAbi),
}

/// Look up a user-op name's classification, scoped to the given
/// architecture.  Tries the arch-specific table first (which holds
/// entries whose ABI varies by arch — currently `swi` only), then
/// falls back to the arch-independent table (everything else).
///
/// Strict-on-emission policy: the ir layer (`build_call_other_modeled`'s
/// caller) converts `None` into `UnknownCallOtherError`.  The cfg builder
/// treats `None` as "fall through to today's behaviour" (insn stays in
/// the region) — the ir layer is the single strict gate.
#[must_use]
pub fn classify(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    classify_arch_specific(preset, name).or_else(|| classify_arch_independent(name))
}

/// Arch-specific entries — names whose ABI depends on which arch
/// emitted them.  Currently `swi` (collides between ARM Linux SVC/SWI
/// and x86 INT instruction), Linux syscall ABIs, SMCCC, and the x86
/// MSR / MONITOR-MWAIT / SWAPGS family.  When OS-specific syscall ABI
/// distinctions surface (e.g., Linux vs FreeBSD x86_64 syscall
/// register usage), they slot in here too.
#[must_use]
fn classify_arch_specific(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    ARCH_SPECIFIC_TABLE.iter().find_map(|row| {
        if row.preset_arches.contains(&preset) && row.op_names.contains(&name) {
            Some(row.class)
        } else {
            None
        }
    })
}

/// One row of the arch-specific CallOther dispatch table.  Each row
/// folds a former match arm into pure data: the set of [`ArchPreset`]
/// variants that should match this entry, the set of user-op name
/// strings to match against, and the resulting [`CallOtherClass`].
///
/// `CallOtherClass` is `Copy`, so the row can be returned directly
/// from a static-table walk without cloning.
struct CallOtherRow {
    /// Architectures that this entry applies to.  An entry like
    /// `[Aarch64, Aarch64Be]` lets a single row cover the LE/BE pair;
    /// `[X86, X86_64]` covers both x86 widths.
    preset_arches: &'static [crate::ArchPreset],
    /// User-op name strings this entry matches.  Lets a single row
    /// cover related ops like `mwait` + `mwaitx` or the SMCCC pair
    /// `CallHyperVisor` + `CallSecureMonitor`.
    op_names: &'static [&'static str],
    /// What `classify` returns when both `preset_arches` and `op_names`
    /// hit.
    class: CallOtherClass,
}

/// Arch-specific CallOther dispatch table.  Each row is a former match
/// arm; the dispatch is a linear scan that returns the first hit.
///
/// Adding a new entry is one diffable row — the dispatch loop and the
/// arch-independent fallback do not change.
//
// Three preset-array constants below keep the most common (`X86 +
// X86_64`, all-AArch64, all-32-bit-ARM) row prefixes short.
static ARCH_SPECIFIC_TABLE: &[CallOtherRow] = &[
    // ARM Linux SVC / SWI ABI: r7 = syscall number, r0..r6 = args
    // (up to 7), r0 = return value.  See `arch/arm/kernel/entry-common.S`
    // and the EABI variant in `arch/arm/include/uapi/asm/unistd.h`.
    // All three 32-bit ARM presets share this ABI; if Thumb ever needs
    // a different one, split the row.
    CallOtherRow {
        preset_arches: ARM32_ALL,
        op_names: &["swi"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
            implicit_writes: &["r0"],
            // Linux SVC/SWI is a kernel entry: the kernel can read/write
            // any user-mode memory including the user stack.  Use the
            // full-clobber set so AliasSplit breaks the Stack chain too.
            mem_clobbers:    MEM_CLOBBER_FULL,
        }),
    },
    // x86 INT instruction also lifts to "swi" in some Sleigh contexts.
    // Empty register ABI + full-clobber mem set as a sound stub until a
    // future spec models per-(arch, INT-vector, OS) syscall conventions.
    // Without this entry, any x86 lift containing an INT instruction
    // would error with UnknownCallOtherError (e.g. INT3 padding bytes).
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["swi"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            // Conservatively full-clobber: INT 0x80 (Linux x86 syscall)
            // is a kernel entry that can mutate user stack memory.  Other
            // INT vectors (debugger, page-fault) won't usually but we
            // prefer the sound stub.
            mem_clobbers: MEM_CLOBBER_FULL,
        }),
    },

    // Linux x86_64 syscall ABI: RAX = syscall number, RDI/RSI/RDX/
    // R10/R8/R9 = args, RAX = return.  RCX/R11 are clobbered by the
    // SYSCALL instruction itself (RCX=return rip, R11=rflags).
    // Arch-specific because the register names only resolve on
    // x86_64's Sleigh register table.
    CallOtherRow {
        preset_arches: &[crate::ArchPreset::X86_64],
        op_names: &["syscall"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"],
            implicit_writes: &["RAX", "RCX", "R11"],
            // Kernel entry: can read/write the user stack frame in
            // addition to heap / unknown memory.  Full clobber.
            mem_clobbers:    MEM_CLOBBER_FULL,
        }),
    },

    // ARM SMCCC for HVC (CallHyperVisor) and SMC (CallSecureMonitor):
    // X0..X7 in, X0..X3 out.  Both LE and BE aarch64 share the
    // convention.  Arch-specific because `x0..x7` only resolve on
    // aarch64's Sleigh register table (arm-32 has `r0..r12`).
    CallOtherRow {
        preset_arches: AARCH64_BOTH,
        op_names: &["CallHyperVisor", "CallSecureMonitor"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            implicit_writes: &["x0", "x1", "x2", "x3"],
            // Hypervisor / Secure Monitor calls operate via the SMCCC
            // register-passing channel; the SMCCC spec does not permit
            // them to mutate the caller's stack frame.  Heap+Unknown is
            // the right clobber set.
            mem_clobbers:    MEM_CLOBBER_HEAP_UNKNOWN,
        }),
    },

    // x86 RDPKRU: ECX must be 0 (read by the op), writes EAX, clears
    // EDX.  Arch-specific because ECX/EAX/EDX are x86's 32-bit
    // register names.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["rdpkru_u32"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["ECX"],
            implicit_writes: &["EAX", "EDX"],
            mem_clobbers:    MEM_CLOBBER_NONE,
        }),
    },

    // x86 RDTSC.  Sleigh emits
    //   `tmp:8 = rdtsc(); EDX = tmp(4); EAX = tmp(0);`
    // so the EDX/EAX writes are explicit pcode ops downstream of the
    // CALLOTHER and don't need to be re-declared as implicit clobbers
    // here (double-declaring would over-clobber the call site).  No
    // memory clobber: TSC reads don't observe RAM.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["rdtsc"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_NONE,
        }),
    },

    // x86 RDTSCP: like RDTSC but ALSO writes ECX (= IA32_TSC_AUX MSR's
    // low 32 bits).  Without the ECX clobber, a pattern reading
    // post-RDTSCP ECX would incorrectly see the pre-call value.  No
    // memory clobber: TSC reads don't observe RAM.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["rdtscp"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &["EAX", "EDX", "ECX"],
            mem_clobbers:    MEM_CLOBBER_NONE,
        }),
    },

    // x86 RDMSR — read model-specific register.  Sleigh emits
    //   `tmp:8 = rdmsr(ECX); EDX = tmp(4); EAX = tmp(0);`
    // so ECX is an explicit pcode arg and the EDX/EAX writes are
    // separate downstream pcode ops.  Nothing implicit; no memory
    // clobber (an MSR read doesn't observe RAM).
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["rdmsr"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_NONE,
        }),
    },

    // x86 WRMSR — write model-specific register.  Sleigh emits
    //   `tmp:8 = (zext(EDX)<<32)|zext(EAX); wrmsr(ECX, tmp);`
    // so ECX/tmp (and transitively EDX/EAX) are all explicit pcode
    // operands of upstream ops feeding this CALLOTHER.  Heap+Unknown
    // clobber: a WRMSR can change TSC, FSBASE, etc., so subsequent
    // heap-resident loads must observe the write; the user-mode stack
    // frame is unaffected.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["wrmsr"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_HEAP_UNKNOWN,
        }),
    },

    // x86_64 RDFSBASE / RDGSBASE — read FS/GS segment base into a GPR.
    // Sleigh emits `r32 = readfsbase()` / `r64 = readfsbase()`
    // (destination is the explicit pcode output, no inputs).  Nothing
    // implicit; no memory clobber.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["readfsbase", "readgsbase"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_NONE,
        }),
    },

    // WRFSBASE / WRGSBASE — write FS/GS base from a GPR.  Sleigh emits
    // `writefsbase(r64)` (or `zext(r32)`) with the source register as
    // the explicit pcode arg.  Heap+Unknown clobber: subsequent
    // FS:/GS:-based heap loads depend on the new base; the stack
    // frame is SP-relative and unaffected.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["writefsbase", "writegsbase"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_HEAP_UNKNOWN,
        }),
    },

    // x86_64 MONITOR (0F 01 C8) — sets up address-range monitor.
    // Sleigh emits `monitor()` with zero pcode operands; the implicit
    // register reads are not surfaced as pcode args, so they belong in
    // `implicit_reads`.  Per Intel SDM Vol. 2B §4-39: RAX = linear
    // address to monitor, ECX = extensions (must be 0), EDX = hints
    // (must be 0).  Heap+Unknown clobber: the operation interacts
    // with the cache subsystem and pairs with a subsequent MWAIT; it
    // does not mutate stack-frame contents.  AMD MONITORX (0F 01 FA)
    // shares the same ABI per AMD64 Vol. 3.
    CallOtherRow {
        preset_arches: &[crate::ArchPreset::X86_64],
        op_names: &["monitor", "monitorx"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["RAX", "ECX", "EDX"],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_HEAP_UNKNOWN,
        }),
    },
    // x86 32-bit MONITOR / MONITORX — EAX-relative address.
    CallOtherRow {
        preset_arches: &[crate::ArchPreset::X86],
        op_names: &["monitor", "monitorx"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["EAX", "ECX", "EDX"],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_HEAP_UNKNOWN,
        }),
    },

    // x86 MWAIT (0F 01 C9) / MWAITX (0F 01 FB) — enter a low-power
    // state until the armed cache line is written.  Per Intel SDM
    // Vol. 2B §4-44: EAX = hints, ECX = extensions (must be 0).  No
    // GPR writes.  Heap+Unknown clobber: serialises with the prior
    // MONITOR's cache-line arming and acts as a memory-order point
    // for heap-resident memory; stack frames are unaffected.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["mwait", "mwaitx"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["EAX", "ECX"],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_HEAP_UNKNOWN,
        }),
    },

    // x86_64 SYSRET (0F 07) — fast return from a SYSCALL into ring 3.
    // Sleigh defines `sysret` only on the x86 stack; arch-specific
    // here so a hypothetical non-x86 Sleigh spec that coincidentally
    // names a user-op `sysret` cannot silently inherit NoReturn.  For
    // kernel-internal analysis this terminates the function (the
    // kernel-context control does not return to its kernel-context
    // caller); a future `ReturnToUserMode` classification could
    // differentiate user-mode trampolines.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["sysret"],
        class: CallOtherClass::NoReturn,
    },

    // x86 SWAPGS (0F 01 F8) — exchanges IA32_GS_BASE ↔
    // IA32_KERNEL_GS_BASE.  No GPR or RAM write on its own, but the
    // MSR swap silently changes the virtual base used by every
    // subsequent `%gs:`-relative load/store.  Without a non-empty
    // mem_clobbers, StackLoadForward / LoadReadOnly would forward
    // `%gs:`-loads across the swap.  Analogous to wr{fs,gs}base above —
    // %gs accesses are heap-resident; the stack pointer is not
    // GS-relative, so MEM_CLOBBER_HEAP_UNKNOWN is sufficient.  Arch-
    // specific so it cannot misclassify a non-x86 user-op
    // coincidentally named `swapgs`.
    CallOtherRow {
        preset_arches: X86_BOTH,
        op_names: &["swapgs"],
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &[],
            mem_clobbers:    MEM_CLOBBER_HEAP_UNKNOWN,
        }),
    },

    // x86's INT instruction also lifts to "swi" in some Sleigh
    // contexts.  We don't have a global model (the vector is in the
    // pcode args; INT 0x80 is Linux 32-bit syscall, INT 3 is a
    // debugger trap, INT 0x2E was Windows' legacy syscall, etc).
    // No entry here = arch_independent fallback returns None for
    // (X86, "swi") = lift errors with UnknownCallOtherError, which
    // is the right strict behaviour until a future spec adds a
    // (vector, OS) keyed model.
];

// Shared preset-slice constants — keep the rows above short and make
// the "shared with x86_64" / "shared between LE/BE" intent explicit.
const X86_BOTH: &[crate::ArchPreset] = &[crate::ArchPreset::X86, crate::ArchPreset::X86_64];
const ARM32_ALL: &[crate::ArchPreset] = &[
    crate::ArchPreset::Arm,
    crate::ArchPreset::ArmBe,
    crate::ArchPreset::ArmThumb,
];
const AARCH64_BOTH: &[crate::ArchPreset] =
    &[crate::ArchPreset::Aarch64, crate::ArchPreset::Aarch64Be];

/// Arch-independent entries — names whose meaning is the same on every
/// arch that emits them.  This is the bulk of the table.
///
/// **Invariant: `Call` entries here MUST have empty `implicit_reads`
/// and empty `implicit_writes`.**  Any named register (RAX, x0, r7, …)
/// only resolves on a specific arch's Sleigh register table, which
/// makes the entry arch-specific by definition — put it in
/// `classify_arch_specific` instead.  Memory-edge alone is allowed
/// here (it's purely an IR concept, not arch-specific).  Enforced by
/// the `arch_independent_call_entries_have_empty_register_channels`
/// test — using the `Pure` / `PureMem` table marker exclusively here
/// makes the invariant trivially true at the syntactic level.
///
/// The table is grouped by classification (NoOp / NoReturn / Pure /
/// PureMem) and ASCII-sorted within each group for diffability — one
/// entry per line, identical shape, easy to compare across patches.
/// Lookup is a linear scan; the table is small (~46 entries) and
/// classification fires once per CallOther at lift time, so a hash
/// map's setup cost isn't justified.
#[must_use]
fn classify_arch_independent(name: &str) -> Option<CallOtherClass> {
    use TableClass::{FullClobber, NoOp, NoReturn, Pure, PureMem};
    /// Per-table marker for one of the five pre-canned classifications
    /// — `NoOp` (decoder-context only), `NoReturn` (trap), `Pure`
    /// (visible marker / pure compute, no memory clobber), `PureMem`
    /// (memory-chain marker / external-state effect, clobbers
    /// Heap+Unknown), `FullClobber` (kernel-entry-style ops that can
    /// also mutate the user stack frame).  Resolved to a
    /// `CallOtherClass` by `TableClass::resolve`.
    #[derive(Clone, Copy)]
    enum TableClass {
        NoOp,
        NoReturn,
        Pure,
        PureMem,
        FullClobber,
    }
    impl TableClass {
        fn resolve(self) -> CallOtherClass {
            match self {
                Self::NoOp => CallOtherClass::NoOp,
                Self::NoReturn => CallOtherClass::NoReturn,
                Self::Pure => CallOtherClass::Call(CallOtherAbi {
                    implicit_reads: &[],
                    implicit_writes: &[],
                    mem_clobbers: MEM_CLOBBER_NONE,
                }),
                Self::PureMem => CallOtherClass::Call(CallOtherAbi {
                    implicit_reads: &[],
                    implicit_writes: &[],
                    // Default "memory-edge" classification: barriers,
                    // LOCK/UNLOCK, port I/O, etc. all clobber heap and
                    // unknown memory but leave the SP-relative stack
                    // frame intact.  Ops whose semantics also disturb
                    // the user stack (SYSCALL / SWI) get FullClobber.
                    mem_clobbers: MEM_CLOBBER_HEAP_UNKNOWN,
                }),
                Self::FullClobber => CallOtherClass::Call(CallOtherAbi {
                    implicit_reads: &[],
                    implicit_writes: &[],
                    // Kernel-entry path: kernel can mutate any user
                    // memory including the stack frame.  Used for the
                    // generic `software_interrupt` user-op that ARM's
                    // SVC family lifts to.
                    mem_clobbers: MEM_CLOBBER_FULL,
                }),
            }
        }
    }

    static TABLE: &[(&str, TableClass)] = &[
        // ─── Truly invisible (Sleigh decoder context only) ────────
        ("setEndianState", NoOp),
        ("setISAMode",     NoOp),

        // ─── NoReturn (traps; control flow ends here) ─────────────
        // x86 `sysret` lives in classify_arch_specific so a non-x86
        // user-op of the same name cannot silently inherit NoReturn.
        ("SoftwareBreakpoint",            NoReturn),
        ("UndefinedInstructionException", NoReturn),
        ("invalidInstructionException",   NoReturn),
        ("trap",                          NoReturn),

        // ─── Pure: visible markers / pure compute, no memory edge ──

        // ARM exclusive-monitor primitives — pair with LDREX/STREX
        // which already emit pcode loads/stores.  The monitor flag is
        // synthetic.
        ("ExclusiveMonitorPass",    Pure),
        ("ExclusiveMonitorsStatus", Pure),

        // CPU hints — non-paired, no memory effect.
        ("Hint_Prefetch", Pure),
        ("Yield",         Pure),

        // x86 CPUID family — Sleigh's lift returns a tmpptr; the
        // EAX/EBX/ECX/EDX writes appear as ordinary Loads from
        // tmpptr+{0,4,8,12} in subsequent pcode.  The CallOther itself
        // doesn't touch RAM, so memory edge stays put — opt passes can
        // forward through it.
        ("cpuid",                                           Pure),
        ("cpuid_Architectural_Performance_Monitoring_info", Pure),
        ("cpuid_Deterministic_Cache_Parameters_info",       Pure),
        ("cpuid_Direct_Cache_Access_info",                  Pure),
        ("cpuid_Extended_Feature_Enumeration_info",         Pure),
        ("cpuid_Extended_Topology_info",                    Pure),
        ("cpuid_MONITOR_MWAIT_Features_info",               Pure),
        ("cpuid_Processor_Extended_States_info",            Pure),
        ("cpuid_Quality_of_Service_info",                   Pure),
        ("cpuid_Thermal_Power_Management_info",             Pure),
        ("cpuid_Version_info",                              Pure),
        ("cpuid_basic_info",                                Pure),
        ("cpuid_brand_part1_info",                          Pure),
        ("cpuid_brand_part2_info",                          Pure),
        ("cpuid_brand_part3_info",                          Pure),
        ("cpuid_cache_tlb_info",                            Pure),
        ("cpuid_serial_info",                               Pure),

        // NEON / SVE / multi-precision — Sleigh's pcode carries
        // operand regs; the user-op itself is pure compute.
        ("MP_INT_ABS",  Pure),
        ("NEON_rev64",  Pure),
        ("NEON_sqshl",  Pure),
        ("NEON_uaddlv", Pure),
        ("SVE_fnmla",   Pure),

        // ARM unmodelled sysreg read — pcode-explicit encoding
        // constant and destination; opaque value, no RAM effect.
        ("UnkSytemRegRead", Pure),

        // x86 `swapgs` lives in classify_arch_specific so a non-x86
        // user-op of the same name cannot silently inherit PureMem.

        // ARM permanently-undefined instruction — Sleigh emits
        // CALLOTHER + a branch to the trap handler; the user-op
        // itself doesn't touch state.
        ("software_udf", Pure),

        // ─── PureMem: memory-chain markers + side-effecting ───────

        // x86 port I/O — port + value pcode-explicit; the user-op
        // itself affects external (port) state.
        ("in",  PureMem),
        ("out", PureMem),

        // ─── FullClobber: memory / ordering barriers ──────────────
        //
        // All of these act as serialization or visibility barriers
        // across ALL reachable memory — including the SP-relative stack
        // frame.  A concurrent writer (another CPU, DMA, the kernel
        // after a mode-switch) may have modified the current thread's
        // stack before the barrier completes, so forwarding a
        // Stack-class load across any of them is unsound.
        //
        // Conservative choice: clobber Stack + Unknown.  This prevents
        // StackLoadForward from forwarding a value that a prior barrier
        // has made stale from the point of view of any aliased observer.
        // Precision loss is acceptable — these primitives appear in
        // synchronisation code where forwarding is rarely beneficial.

        // x86 LOCK / UNLOCK — bracket a full hardware-lock prefix.
        // The LOCK prefix implements a full memory barrier (all prior
        // stores made globally visible; all subsequent loads see the
        // latest value from ALL CPUs).  Stack + Unknown are both
        // observable by other CPUs in the presence of shared-stack
        // scenarios, so clobber both.
        ("LOCK",   FullClobber),
        ("UNLOCK", FullClobber),

        // ARM standalone memory / cache barriers.  DSB / DMB are data
        // memory barriers; ISB flushes the instruction pipeline and,
        // conservatively, both instruction and data stream.  On a
        // multicore AArch64/ARM system the stack frame is accessible to
        // other cores if the address escaped (via a pointer argument or
        // a shared data structure), so all three get FullClobber.
        // DC_CVAC (Data Cache operation to Point of Coherency) interacts
        // with the cache subsystem, not with register-side data; it is
        // kept at PureMem (heap+unknown only).
        ("DC_CVAC",                           PureMem),
        ("DataMemoryBarrier",                 FullClobber),
        ("DataSynchronizationBarrier",        FullClobber),
        ("InstructionSynchronizationBarrier", FullClobber),

        // x86/x86_64 standalone memory fences.  Emitted by Sleigh's x86
        // spec as lowercase mnemonics.  All three are ordering barriers:
        // MFENCE serialises all prior / subsequent loads and stores;
        // SFENCE serialises prior stores; LFENCE serialises prior loads.
        // Like LOCK, these can make a remote core's prior writes visible,
        // so Stack + Unknown are both reachable.
        ("lfence", FullClobber),
        ("mfence", FullClobber),
        ("sfence", FullClobber),

        // PowerPC memory barriers.
        // `sync` (SYNC / lwsync / hwsync — the `L` field selects the
        // variant but Sleigh folds all three to the same user-op name).
        // `enforceInOrderExecutionIO` (EIEIO — I/O barrier, also acts
        // as a full data-memory barrier on Power ISA).
        // `instructionSynchronize` (ISYNC — instruction-pipeline flush;
        // treated conservatively as FullClobber for data).
        // Without these entries any PowerPC binary containing a fence
        // would fail with UnknownCallOtherError at the IR layer.
        ("enforceInOrderExecutionIO", FullClobber),
        ("instructionSynchronize",    FullClobber),
        ("sync",                      FullClobber),

        // MIPS memory barriers.
        // `SYNC` — GHIDRA's MIPS32 spec emits this for the SYNC
        //   instruction (instruction-stream sync / data-memory barrier).
        // `synch` — GHIDRA's mips.sinc uses this alternate spelling for
        //   the same SYNC mnemonic in the common include.
        // Without these entries any MIPS binary containing SYNC would
        // fail with UnknownCallOtherError at the IR layer.
        ("SYNC",  FullClobber),
        ("synch", FullClobber),

        // ARM SVC / SWI raised by an immediate — possible syscall
        // path, kernel can do anything to memory including the user
        // stack frame.  Use the full-clobber marker.
        ("software_interrupt", FullClobber),
    ];
    TABLE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| c.resolve())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_abi() -> CallOtherAbi {
        CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            mem_clobbers: MEM_CLOBBER_NONE,
        }
    }

    #[test]
    fn truly_invisible_decoder_context_classifies_as_noop() {
        // Only Sleigh-decoder-context user-ops are NoOp.  Memory
        // markers (LOCK / UNLOCK / barriers) and CPU hints are
        // promoted to Call so patterns can find them.
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
        // All memory / ordering barriers must be on the IR memory chain
        // (so patterns walking mem can find them) and must have empty
        // implicit register channels (arch-independent entries may not
        // carry arch-specific register names).
        for n in [
            "LOCK", "UNLOCK",
            "DataMemoryBarrier", "DataSynchronizationBarrier",
            "InstructionSynchronizationBarrier", "DC_CVAC",
            "lfence", "mfence", "sfence",
            "enforceInOrderExecutionIO", "instructionSynchronize", "sync",
            "SYNC", "synch",
        ] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n}"));
            let CallOtherClass::Call(abi) = class else { panic!("{n}: expected Call") };
            assert!(abi.implicit_reads.is_empty(), "{n}: implicit_reads must be empty");
            assert!(abi.implicit_writes.is_empty(), "{n}: implicit_writes must be empty");
            assert!(abi.has_memory_edge(), "{n}: must advance mem edge for chain visibility");
        }
    }

    /// LOCK, UNLOCK, and all memory / instruction fences must clobber
    /// BOTH Stack and Unknown partitions.  Rationale: these are full
    /// serialization barriers that make all prior stores (including
    /// another CPU's writes to an escaped stack pointer) visible across
    /// the barrier.  Using only MEM_CLOBBER_HEAP_UNKNOWN would allow
    /// StackLoadForward to forward a Stack-class value across a barrier,
    /// which is unsound in the presence of shared-stack / aliased-frame
    /// patterns.
    #[test]
    fn full_memory_barriers_clobber_stack_and_unknown() {
        use crate::alias_class::{AliasClass, MEM_CLOBBER_FULL};
        for n in [
            "LOCK", "UNLOCK",
            "DataMemoryBarrier", "DataSynchronizationBarrier",
            "InstructionSynchronizationBarrier",
            "lfence", "mfence", "sfence",
            "enforceInOrderExecutionIO", "instructionSynchronize", "sync",
            "SYNC", "synch",
        ] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n}"));
            let CallOtherClass::Call(abi) = class else { panic!("{n}: expected Call") };
            assert_eq!(
                abi.mem_clobbers, MEM_CLOBBER_FULL,
                "{n}: must clobber [Stack, Unknown]; got {:?}",
                abi.mem_clobbers,
            );
            assert!(
                abi.mem_clobbers.contains(&AliasClass::Stack),
                "{n}: must clobber Stack partition",
            );
        }
    }

    #[test]
    fn pure_compute_and_hints_classify_as_pure_no_mem_edge() {
        // Pure compute (cpuid, NEON, SVE) and non-paired hints
        // (Hint_Prefetch, Yield) — visible markers but don't advance
        // the memory token (so opt passes can forward through).
        for n in [
            "Hint_Prefetch", "Yield",
            "cpuid", "NEON_rev64", "SVE_fnmla", "MP_INT_ABS",
            "ExclusiveMonitorPass", "ExclusiveMonitorsStatus",
            "UnkSytemRegRead", "software_udf",
        ] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n}"));
            let CallOtherClass::Call(abi) = class else { panic!("{n}: expected Call") };
            assert!(abi.implicit_reads.is_empty(), "{n}");
            assert!(abi.implicit_writes.is_empty(), "{n}");
            assert!(!abi.has_memory_edge(), "{n}: must NOT advance mem edge (opt passes need to forward)");
        }
    }

    #[test]
    fn sysret_and_swapgs_are_x86_only() {
        // Regression: `sysret` and `swapgs` are x86/x86_64-specific
        // user-ops.  They must not silently match on non-x86 arches.
        // Previously they lived in `classify_arch_independent` and
        // would have been classified even on ARM/AArch64/MIPS/PowerPC.
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
            Some(CallOtherClass::NoReturn)
        );
        assert_eq!(
            classify(crate::ArchPreset::X86_64, "sysret"),
            Some(CallOtherClass::NoReturn)
        );
    }

    #[test]
    fn monitor_mwait_implicit_register_channels() {
        // Sleigh emits `monitor()` / `mwait()` with zero pcode operands,
        // so the implicit register reads need to live in `implicit_reads`.
        // Per Intel SDM Vol. 2B §4-39 (MONITOR) and §4-44 (MWAIT).
        let m64 = classify(crate::ArchPreset::X86_64, "monitor").expect("monitor x86_64");
        let CallOtherClass::Call(abi) = m64 else {
            panic!("expected Call(abi) for monitor")
        };
        assert_eq!(abi.implicit_reads, &["RAX", "ECX", "EDX"]);
        assert!(abi.implicit_writes.is_empty());
        assert!(abi.has_memory_edge());

        let m32 = classify(crate::ArchPreset::X86, "monitor").expect("monitor x86");
        let CallOtherClass::Call(abi) = m32 else { panic!() };
        assert_eq!(abi.implicit_reads, &["EAX", "ECX", "EDX"]);

        let mwait = classify(crate::ArchPreset::X86_64, "mwait").expect("mwait classified");
        let CallOtherClass::Call(abi) = mwait else { panic!() };
        assert_eq!(abi.implicit_reads, &["EAX", "ECX"]);
        assert!(abi.implicit_writes.is_empty());
        assert!(abi.has_memory_edge());

        // AMD variants share the same shape.
        assert!(matches!(
            classify(crate::ArchPreset::X86_64, "monitorx"),
            Some(CallOtherClass::Call(_))
        ));
        assert!(matches!(
            classify(crate::ArchPreset::X86_64, "mwaitx"),
            Some(CallOtherClass::Call(_))
        ));

        // Not classified on non-x86 — `monitor` is also an English word
        // and could appear in a future spec; the arch-specific guard
        // prevents misclassification.
        assert_eq!(classify(crate::ArchPreset::Aarch64, "monitor"), None);
        assert_eq!(classify(crate::ArchPreset::Aarch64, "mwait"), None);
    }

    #[test]
    fn swapgs_is_memory_chain_marker() {
        // SWAPGS exchanges IA32_GS_BASE ↔ IA32_KERNEL_GS_BASE.  Subsequent
        // %gs:-relative loads/stores depend on the new base, so swapgs must
        // be on the IR memory chain — analogous to wr{fs,gs}base, which use
        // PURE_WITH_MEM_EDGE.  Without memory_edge=true, StackLoadForward /
        // LoadReadOnly could incorrectly forward across swapgs in kernel
        // entry/exit code.
        let cls = classify(crate::ArchPreset::X86_64, "swapgs").unwrap();
        let CallOtherClass::Call(abi) = cls else { panic!("expected Call(abi)") };
        assert!(abi.implicit_reads.is_empty());
        assert!(abi.implicit_writes.is_empty());
        assert!(abi.has_memory_edge(), "swapgs must advance memory edge (kernel GS base swap)");
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
            assert_eq!(classify(crate::ArchPreset::X86_64, n), Some(CallOtherClass::NoReturn), "{n}");
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
        assert!(abi.has_memory_edge());
    }

    #[test]
    fn cpuid_family_uses_empty_abi_no_memory_edge() {
        // Sleigh's cpuid lift selects one of cpuid / cpuid_* based on
        // EAX, returns a tmpptr, then emits Loads for EAX/EBX/EDX/ECX
        // from the returned pointer.  The user-op itself doesn't touch
        // RAM, so memory_edge stays at false — opt passes can forward
        // through.  (The post-cpuid Loads on the tmpptr advance mem
        // themselves; cpuid doesn't need to.)
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
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n} classified"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert!(abi.implicit_reads.is_empty(), "{n}");
            assert!(abi.implicit_writes.is_empty(), "{n}");
            assert!(!abi.has_memory_edge(), "{n}: cpuid doesn't touch RAM");
        }
    }

    #[test]
    fn rdtsc_has_no_implicit_writes_no_memory_edge() {
        // Sleigh emits EDX/EAX writes as explicit pcode ops after the
        // CALLOTHER (`tmp:8 = rdtsc(); EDX = tmp(4); EAX = tmp(0);`),
        // so re-declaring them as implicit clobbers would double-clobber
        // the call site.  Implicit-write list is empty by design.
        let class = classify(crate::ArchPreset::X86_64, "rdtsc").expect("rdtsc classified");
        let CallOtherClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(abi.implicit_reads, &[] as &[&str]);
        assert_eq!(abi.implicit_writes, &[] as &[&str]);
        assert!(!abi.has_memory_edge());
    }

    #[test]
    fn rdtscp_writes_eax_edx_ecx_no_memory_edge() {
        // RDTSCP differs from RDTSC: writes ECX (= IA32_TSC_AUX MSR low
        // 32 bits) in addition to EAX/EDX.  Pattern queries reading
        // post-RDTSCP ECX must see the clobber.
        let class = classify(crate::ArchPreset::X86_64, "rdtscp").expect("rdtscp classified");
        let CallOtherClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(abi.implicit_reads, &[] as &[&str]);
        assert_eq!(abi.implicit_writes, &["EAX", "EDX", "ECX"]);
        assert!(!abi.has_memory_edge());
    }

    #[test]
    fn empty_abi_ops_use_call_with_empty_abi() {
        // swapgs intentionally excluded — it has memory_edge=true and is
        // covered by `swapgs_is_memory_chain_marker` instead.
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
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n} classified"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{n}: expected Call")
            };
            assert_eq!(abi, empty_abi(), "{n}");
        }
    }

    #[test]
    fn smccc_ops_share_x0_x7_in_x0_x3_out() {
        // SMCCC entries live in classify_arch_specific because their
        // x0..x7 register names only resolve on aarch64.
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
                assert_eq!(abi.implicit_writes, &["x0", "x1", "x2", "x3"], "{preset:?}/{n}");
                assert!(abi.has_memory_edge(), "{preset:?}/{n}");
            }
            // Non-aarch64 presets must NOT resolve these names.
            assert_eq!(classify(crate::ArchPreset::X86_64, "CallHyperVisor"), None);
            assert_eq!(classify(crate::ArchPreset::Arm, "CallHyperVisor"), None);
        }
    }

    #[test]
    fn rdpkru_is_arch_specific_to_x86() {
        // rdpkru_u32 lives in classify_arch_specific because ECX/EAX/EDX
        // only resolve on x86 / x86_64.
        for preset in [crate::ArchPreset::X86, crate::ArchPreset::X86_64] {
            let class = classify(preset, "rdpkru_u32").expect("rdpkru classified");
            let CallOtherClass::Call(abi) = class else { panic!("expected Call") };
            assert_eq!(abi.implicit_reads, &["ECX"]);
            assert_eq!(abi.implicit_writes, &["EAX", "EDX"]);
            assert!(!abi.has_memory_edge());
        }
        // Non-x86 presets must NOT resolve.
        assert_eq!(classify(crate::ArchPreset::Aarch64, "rdpkru_u32"), None);
        assert_eq!(classify(crate::ArchPreset::Arm, "rdpkru_u32"), None);
    }

    #[test]
    fn msr_and_segment_base_ops_classify_pure_or_pure_with_mem_edge() {
        // Sleigh emits these x86 / x86_64 ops with every register
        // operand on the explicit pcode chain — there is no implicit
        // register channel to model.  Reads (`rdmsr`, `readfsbase`,
        // `readgsbase`) are PURE; writes (`wrmsr`, `writefsbase`,
        // `writegsbase`) carry a memory edge so opt passes don't
        // forward across them.
        let pure_ops    = ["rdmsr", "readfsbase", "readgsbase"];
        let edge_ops    = ["wrmsr", "writefsbase", "writegsbase"];
        for preset in [crate::ArchPreset::X86, crate::ArchPreset::X86_64] {
            for n in pure_ops {
                let class = classify(preset, n).unwrap_or_else(|| panic!("{preset:?}/{n}"));
                let CallOtherClass::Call(abi) = class else {
                    panic!("{preset:?}/{n}: expected Call")
                };
                assert_eq!(abi, empty_abi(), "{preset:?}/{n}");
                assert!(!abi.has_memory_edge(), "{preset:?}/{n}");
            }
            for n in edge_ops {
                let class = classify(preset, n).unwrap_or_else(|| panic!("{preset:?}/{n}"));
                let CallOtherClass::Call(abi) = class else {
                    panic!("{preset:?}/{n}: expected Call")
                };
                assert_eq!(abi.implicit_reads, &[] as &[&str], "{preset:?}/{n}");
                assert_eq!(abi.implicit_writes, &[] as &[&str], "{preset:?}/{n}");
                assert!(abi.has_memory_edge(), "{preset:?}/{n}");
            }
        }
        // Non-x86 presets must NOT resolve these names — the encoded
        // instructions only exist on x86/x86_64.
        for n in pure_ops.iter().chain(edge_ops.iter()) {
            assert_eq!(classify(crate::ArchPreset::Aarch64, n), None, "{n} on aarch64");
            assert_eq!(classify(crate::ArchPreset::Arm, n), None, "{n} on arm");
        }
    }

    #[test]
    fn syscall_is_arch_specific_to_x86_64() {
        // The arch-independent fallback must NOT provide "syscall" for
        // non-x86_64 presets (the RAX/RDI/... names wouldn't resolve).
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
        // Invariant: any entry returned by `classify_arch_independent`
        // (via the no-effect Arch::X86_64 fallback path) must have empty
        // implicit_reads and empty implicit_writes — otherwise the named
        // registers tie the entry to a specific arch's Sleigh register
        // table, in which case it belongs in `classify_arch_specific`.
        //
        // We can't iterate the table directly (it's a closed match), so
        // we enumerate every name we expect to be arch-independent and
        // verify the invariant for each.
        let arch_independent_names = [
            // NoOp (also empty by definition; included for completeness)
            "DC_CVAC", "DataMemoryBarrier", "DataSynchronizationBarrier",
            "Hint_Prefetch", "InstructionSynchronizationBarrier",
            "LOCK", "UNLOCK", "Yield", "setEndianState", "setISAMode",
            // NoReturn
            "SoftwareBreakpoint", "UndefinedInstructionException",
            "invalidInstructionException", "sysret", "trap",
            // Call with empty channels
            "ExclusiveMonitorPass", "ExclusiveMonitorsStatus",
            "MP_INT_ABS", "NEON_rev64", "NEON_sqshl", "NEON_uaddlv",
            "SVE_fnmla", "UnkSytemRegRead", "cpuid", "cpuid_basic_info",
            "cpuid_Architectural_Performance_Monitoring_info",
            "cpuid_Deterministic_Cache_Parameters_info",
            "cpuid_Direct_Cache_Access_info",
            "cpuid_Extended_Feature_Enumeration_info",
            "cpuid_Extended_Topology_info",
            "cpuid_MONITOR_MWAIT_Features_info",
            "cpuid_Processor_Extended_States_info",
            "cpuid_Quality_of_Service_info",
            "cpuid_Thermal_Power_Management_info",
            "cpuid_Version_info", "cpuid_brand_part1_info",
            "cpuid_brand_part2_info", "cpuid_brand_part3_info",
            "cpuid_cache_tlb_info", "cpuid_serial_info",
            "in", "out", "software_interrupt", "software_udf",
            "swapgs",
            // PowerPC barriers
            "enforceInOrderExecutionIO", "instructionSynchronize", "sync",
            // MIPS barriers
            "SYNC", "synch",
            // x86 fences
            "lfence", "mfence", "sfence",
        ];
        // Use any preset for the lookup — by definition these resolve
        // identically on every arch.
        for n in arch_independent_names {
            let class = match classify(crate::ArchPreset::X86_64, n) {
                Some(c) => c,
                None => continue,  // not in table
            };
            let abi = match class {
                CallOtherClass::Call(abi) => abi,
                _ => continue,  // NoOp / NoReturn have no ABI
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
            assert!(abi.has_memory_edge(), "{n}");
        }
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(classify(crate::ArchPreset::X86_64, "nonexistent_op_xyzzy_abc"), None);
    }

    #[test]
    fn swi_on_arm_family_returns_linux_arm_abi() {
        // All three 32-bit ARM presets share the Linux SVC/SWI ABI.
        for preset in [
            crate::ArchPreset::Arm,
            crate::ArchPreset::ArmBe,
            crate::ArchPreset::ArmThumb,
        ] {
            let class = classify(preset, "swi").unwrap_or_else(|| panic!("{preset:?}/swi"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{preset:?}/swi: expected Call, got {class:?}")
            };
            assert_eq!(
                abi.implicit_reads,
                &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
                "{preset:?}",
            );
            assert_eq!(abi.implicit_writes, &["r0"], "{preset:?}");
            assert!(abi.has_memory_edge(), "{preset:?}");
        }
    }

    #[test]
    fn swi_on_x86_returns_empty_call_stub() {
        // (X86, "swi") and (X86_64, "swi") use a register-empty Call
        // with a full-clobber memory set as a sound stub until
        // per-INT-vector / per-OS modelling lands — INT 0x80 (Linux
        // x86 syscall) is a kernel entry that can mutate the user
        // stack, so the conservative mem-clobber default is FULL.
        let stub = CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            mem_clobbers: MEM_CLOBBER_FULL,
        });
        assert_eq!(classify(crate::ArchPreset::X86, "swi"), Some(stub));
        assert_eq!(classify(crate::ArchPreset::X86_64, "swi"), Some(stub));
    }

    #[test]
    fn arch_independent_entries_resolve_on_every_arch() {
        // Spot-check that the fallback works regardless of arch.
        for arch in [
            crate::ArchPreset::X86,
            crate::ArchPreset::X86_64,
            crate::ArchPreset::Arm,
            crate::ArchPreset::Aarch64,
        ] {
            // setISAMode is the only true NoOp now (Sleigh decoder bit).
            assert_eq!(
                classify(arch, "setISAMode"),
                Some(CallOtherClass::NoOp),
                "arch={arch:?}",
            );
            // DMB is a memory-chain marker — Call with memory_edge=true.
            let dmb = classify(arch, "DataMemoryBarrier")
                .unwrap_or_else(|| panic!("arch={arch:?}: DataMemoryBarrier"));
            let CallOtherClass::Call(abi) = dmb else {
                panic!("arch={arch:?}: DMB expected Call, got {dmb:?}")
            };
            assert!(abi.has_memory_edge(), "arch={arch:?}: DMB must advance mem edge");
            // Trap is NoReturn on every arch.
            assert_eq!(
                classify(arch, "invalidInstructionException"),
                Some(CallOtherClass::NoReturn),
                "arch={arch:?}",
            );
        }
    }

    #[test]
    fn sleigh_arch_presets_set_distinct_preset_discriminators() {
        // One ArchPreset per preset constructor — full granularity so
        // Arm-32 LE / BE / Thumb are distinguishable.
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
        // Compile-time guard: every variant of CallOtherClass is matched
        // exhaustively here, so adding/removing a variant fails compile.
        for n in ["setISAMode", "invalidInstructionException", "cpuid"] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap();
            match class {
                CallOtherClass::NoOp | CallOtherClass::NoReturn | CallOtherClass::Call(_) => {}
            }
        }
    }

    /// x86/x86_64 memory fences (mfence, sfence, lfence) must classify
    /// as Call with a full-clobber memory set.  Without these entries,
    /// any binary using SSE memory fences would lift to
    /// UnknownCallOtherError at the IR layer.
    #[test]
    fn x86_memory_fences_classify_with_full_clobber() {
        use crate::alias_class::MEM_CLOBBER_FULL;
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
                    abi.has_memory_edge(),
                    "({preset:?}, {name}) must advance memory edge — fences are ordering primitives"
                );
                assert_eq!(
                    abi.mem_clobbers, MEM_CLOBBER_FULL,
                    "({preset:?}, {name}) must clobber [Stack, Unknown]"
                );
            }
        }
    }

    /// PowerPC memory barriers — `sync`, `enforceInOrderExecutionIO`,
    /// `instructionSynchronize` — must classify as Call with full-clobber
    /// memory set so they are visible on the IR memory chain.  Without
    /// these entries any PowerPC binary containing a barrier instruction
    /// would fail with UnknownCallOtherError at the IR layer.
    #[test]
    fn powerpc_barriers_classify_with_full_clobber() {
        use crate::alias_class::MEM_CLOBBER_FULL;
        for preset in [
            crate::ArchPreset::Ppc32Be,
            crate::ArchPreset::Ppc32Le,
            crate::ArchPreset::Ppc64Be,
            crate::ArchPreset::Ppc64Le,
        ] {
            for name in ["sync", "enforceInOrderExecutionIO", "instructionSynchronize"] {
                let cls = classify(preset, name)
                    .unwrap_or_else(|| panic!("({preset:?}, {name}) must classify"));
                let abi = match cls {
                    CallOtherClass::Call(abi) => abi,
                    other => panic!("({preset:?}, {name}) classified as {other:?}, expected Call"),
                };
                assert!(abi.implicit_reads.is_empty(), "({preset:?}, {name}) implicit_reads");
                assert!(abi.implicit_writes.is_empty(), "({preset:?}, {name}) implicit_writes");
                assert!(abi.has_memory_edge(), "({preset:?}, {name}) must advance mem edge");
                assert_eq!(
                    abi.mem_clobbers, MEM_CLOBBER_FULL,
                    "({preset:?}, {name}) must clobber [Stack, Unknown]"
                );
            }
        }
    }

    /// MIPS memory barriers — `SYNC` and `synch` — must classify as
    /// Call with full-clobber memory set.  Without these entries any
    /// MIPS binary containing a SYNC instruction would fail with
    /// UnknownCallOtherError.
    #[test]
    fn mips_barriers_classify_with_full_clobber() {
        use crate::alias_class::MEM_CLOBBER_FULL;
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
                assert!(abi.implicit_reads.is_empty(), "({preset:?}, {name}) implicit_reads");
                assert!(abi.implicit_writes.is_empty(), "({preset:?}, {name}) implicit_writes");
                assert!(abi.has_memory_edge(), "({preset:?}, {name}) must advance mem edge");
                assert_eq!(
                    abi.mem_clobbers, MEM_CLOBBER_FULL,
                    "({preset:?}, {name}) must clobber [Stack, Unknown]"
                );
            }
        }
    }
}
