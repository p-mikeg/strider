//! Sleigh user-op (CallOther) classification table.  See
//! `docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`
//! (and the v1 spec `2026-05-05-callother-classification-design.md`
//! for the original cfg/ir consumer split).

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

    /// Whether this op advances the IR's memory edge (token).  True
    /// for ops whose effect on memory is observable to subsequent
    /// loads / stores (syscall, port I/O, cache writeback).  False
    /// for pure register-level computation (cpuid, rdtsc, NEON math).
    pub memory_edge: bool,
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
/// emitted them.  Currently just `swi` (collides between ARM Linux
/// SVC/SWI and x86 INT instruction).  When OS-specific syscall ABI
/// distinctions surface (e.g., Linux vs FreeBSD x86_64 syscall
/// register usage), they slot in here too.
//
// `match_same_arms`: each (preset, name) pair is a separate diffable
// entry with its own justification comment — combining via `|` would
// defeat the table's per-line property.
#[allow(clippy::match_same_arms)]
#[must_use]
fn classify_arch_specific(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    match (preset, name) {
        // ARM Linux SVC / SWI ABI: r7 = syscall number, r0..r6 = args
        // (up to 7), r0 = return value.  See `arch/arm/kernel/entry-common.S`
        // and the EABI variant in `arch/arm/include/uapi/asm/unistd.h`.
        // All three 32-bit ARM presets share this ABI; if Thumb ever
        // needs a different one, split the alternation into separate arms.
        (crate::ArchPreset::Arm | crate::ArchPreset::ArmBe | crate::ArchPreset::ArmThumb,
         "swi") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
            implicit_writes: &["r0"],
            memory_edge:     true,
        })),
        // x86 INT instruction also lifts to "swi" in some Sleigh contexts.
        // Empty ABI + memory_edge for now: sound stub until a future spec
        // models per-(arch, INT-vector, OS) syscall conventions.  Without
        // this entry, any x86 lift containing an INT instruction would
        // error with UnknownCallOtherError (e.g. INT3 padding bytes).
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "swi") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: true,
        })),

        // Linux x86_64 syscall ABI: RAX = syscall number, RDI/RSI/RDX/
        // R10/R8/R9 = args, RAX = return.  RCX/R11 are clobbered by
        // the SYSCALL instruction itself (RCX=return rip, R11=rflags).
        // Arch-specific because the register names only resolve on
        // x86_64's Sleigh register table.
        (crate::ArchPreset::X86_64, "syscall") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"],
            implicit_writes: &["RAX", "RCX", "R11"],
            memory_edge:     true,
        })),

        // ARM SMCCC for HVC (CallHyperVisor) and SMC (CallSecureMonitor):
        // X0..X7 in, X0..X3 out.  Both little- and big-endian aarch64
        // share the convention.  Arch-specific because `x0..x7` only
        // resolve on aarch64's Sleigh register table (arm-32 has
        // `r0..r12`).
        (crate::ArchPreset::Aarch64 | crate::ArchPreset::Aarch64Be,
         "CallHyperVisor" | "CallSecureMonitor") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            implicit_writes: &["x0", "x1", "x2", "x3"],
            memory_edge:     true,
        })),

        // x86 RDPKRU: ECX must be 0 (read by the op), writes EAX,
        // clears EDX.  Arch-specific because ECX/EAX/EDX are x86's
        // 32-bit register names.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "rdpkru_u32") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["ECX"],
            implicit_writes: &["EAX", "EDX"],
            memory_edge:     false,
        })),

        // x86 RDTSC: no inputs, writes EDX:EAX.  Arch-specific
        // because EAX/EDX are x86 32-bit register names.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "rdtsc") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &["EAX", "EDX"],
            memory_edge:     false,
        })),

        // x86 RDTSCP: like RDTSC but ALSO writes ECX (= IA32_TSC_AUX
        // MSR's low 32 bits).  Without the ECX clobber, a pattern
        // reading post-RDTSCP ECX would incorrectly see the pre-call
        // value.  No memory edge: TSC reads don't observe RAM.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "rdtscp") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &[],
            implicit_writes: &["EAX", "EDX", "ECX"],
            memory_edge:     false,
        })),

        // x86 RDMSR — read model-specific register.  Sleigh emits
        //   `tmp:8 = rdmsr(ECX); EDX = tmp(4); EAX = tmp(0);`
        // so ECX is an explicit pcode arg and the EDX/EAX writes are
        // separate downstream pcode ops.  Nothing implicit; no memory
        // edge (an MSR read doesn't observe RAM).
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "rdmsr") => PURE,

        // x86 WRMSR — write model-specific register.  Sleigh emits
        //   `tmp:8 = (zext(EDX)<<32)|zext(EAX); wrmsr(ECX, tmp);`
        // so ECX/tmp (and transitively EDX/EAX) are all explicit pcode
        // operands of upstream ops feeding this CALLOTHER.  Memory
        // edge: a WRMSR can change TSC, FSBASE, etc., so subsequent
        // loads must observe the write.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "wrmsr") => PURE_WITH_MEM_EDGE,

        // x86_64 RDFSBASE / RDGSBASE — read FS/GS segment base into a
        // GPR.  Sleigh emits `r32 = readfsbase()` / `r64 = readfsbase()`
        // (destination is the explicit pcode output, no inputs).
        // Nothing implicit; no memory edge.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "readfsbase" | "readgsbase") => PURE,

        // WRFSBASE / WRGSBASE — write FS/GS base from a GPR.  Sleigh
        // emits `writefsbase(r64)` (or `zext(r32)`) with the source
        // register as the explicit pcode arg.  Memory edge: subsequent
        // FS:/GS:-based loads depend on the new base.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "writefsbase" | "writegsbase") => PURE_WITH_MEM_EDGE,

        // x86_64 MONITOR (0F 01 C8) — sets up address-range monitor.
        // Sleigh emits `monitor()` with zero pcode operands; the
        // implicit register reads are not surfaced as pcode args, so
        // they belong in `implicit_reads`.  Per Intel SDM Vol. 2B §4-39:
        // RAX = linear address to monitor, ECX = extensions (must be 0),
        // EDX = hints (must be 0).  Memory edge: the operation interacts
        // with the cache subsystem and pairs with a subsequent MWAIT.
        (crate::ArchPreset::X86_64,
         "monitor") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["RAX", "ECX", "EDX"],
            implicit_writes: &[],
            memory_edge:     true,
        })),
        // x86 32-bit MONITOR — same operation, EAX-relative address.
        (crate::ArchPreset::X86,
         "monitor") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["EAX", "ECX", "EDX"],
            implicit_writes: &[],
            memory_edge:     true,
        })),

        // AMD MONITORX (0F 01 FA) — like MONITOR but available outside
        // CPL 0 with vendor-specific cache hints.  Implicit reads match
        // MONITOR per AMD64 Vol. 3.
        (crate::ArchPreset::X86_64,
         "monitorx") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["RAX", "ECX", "EDX"],
            implicit_writes: &[],
            memory_edge:     true,
        })),
        (crate::ArchPreset::X86,
         "monitorx") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["EAX", "ECX", "EDX"],
            implicit_writes: &[],
            memory_edge:     true,
        })),

        // x86 MWAIT (0F 01 C9) / MWAITX (0F 01 FB) — entries a low-power
        // state until the armed cache line is written.  Per Intel SDM
        // Vol. 2B §4-44: EAX = hints, ECX = extensions (must be 0).
        // No GPR writes.  Memory edge: serialises with the prior
        // MONITOR's cache-line arming and acts as a memory-order point.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "mwait" | "mwaitx") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["EAX", "ECX"],
            implicit_writes: &[],
            memory_edge:     true,
        })),

        // x86_64 SYSRET (0F 07) — fast return from a SYSCALL into ring 3.
        // Sleigh defines `sysret` only on the x86 stack; arch-specific
        // here so a hypothetical non-x86 Sleigh spec that coincidentally
        // names a user-op `sysret` cannot silently inherit NoReturn.
        // For kernel-internal analysis this terminates the function (the
        // kernel-context control does not return to its kernel-context
        // caller); a future `ReturnToUserMode` classification could
        // differentiate user-mode trampolines.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "sysret") => NO_RETURN,

        // x86 SWAPGS (0F 01 F8) — exchanges IA32_GS_BASE ↔
        // IA32_KERNEL_GS_BASE.  No GPR or RAM write on its own, but
        // the MSR swap silently changes the virtual base used by
        // every subsequent `%gs:`-relative load/store.  Without
        // memory_edge = true, StackLoadForward / LoadReadOnly would
        // forward `%gs:`-loads across the swap.  Analogous to
        // wr{fs,gs}base above.  Arch-specific so it cannot misclassify
        // a non-x86 user-op coincidentally named `swapgs`.
        (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
         "swapgs") => PURE_WITH_MEM_EDGE,

        // x86's INT instruction also lifts to "swi" in some Sleigh
        // contexts.  We don't have a global model (the vector is in
        // the pcode args; INT 0x80 is Linux 32-bit syscall, INT 3 is
        // a debugger trap, INT 0x2E was Windows' legacy syscall, etc).
        // No entry here = arch_independent fallback returns None for
        // (X86, "swi") = lift errors with UnknownCallOtherError, which
        // is the right strict behaviour until a future spec adds a
        // (vector, OS) keyed model.
        _ => None,
    }
}

// ── Shared classification constants ──────────────────────────────────
//
// `Option<CallOtherClass>` shorthand for the three repeated shapes
// every entry below uses.  Lets each table arm be a single line.

/// Empty-ABI Call, **does not** advance the IR memory edge.  Use for
/// pure compute (cpuid, NEON, SVE, ...) and standalone hints
/// (Hint_Prefetch, Yield, ExclusiveMonitor*) that don't touch RAM
/// or pair with a Store.
const PURE: Option<CallOtherClass> = Some(CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &[],
    memory_edge: false,
}));

/// Empty-ABI Call that **does** advance the IR memory edge — for ops
/// that act as memory-chain markers (LOCK / UNLOCK brackets, standalone
/// barriers DMB / DSB / ISB / DC_CVAC) so patterns walking the memory
/// chain can find them, plus ops that may actually write external
/// state (port I/O, syscall, software_interrupt).
///
/// Tradeoff: opt passes that walk the memory chain (e.g.
/// `StackLoadForward`) cannot forward across these, since the IR
/// can't prove they don't write RAM.  In practice the affected
/// patterns target prologue/epilogue field reads, far from atomics.
const PURE_WITH_MEM_EDGE: Option<CallOtherClass> = Some(CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &[],
    memory_edge: true,
}));

const NO_OP: Option<CallOtherClass> = Some(CallOtherClass::NoOp);
const NO_RETURN: Option<CallOtherClass> = Some(CallOtherClass::NoReturn);

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
/// test — using `PURE` / `PURE_WITH_MEM_EDGE` exclusively here makes
/// the invariant trivially true at the syntactic level.
//
// `match_same_arms`: each name is a separate diffable entry — combining
// arms via `|` would defeat the table's per-line diff property.
#[allow(clippy::match_same_arms)]
#[must_use]
fn classify_arch_independent(name: &str) -> Option<CallOtherClass> {
    // ASCII-sorted within each group for diffability.
    match name {
        // ─── Truly invisible (Sleigh decoder context only) ────────
        "setEndianState" => NO_OP,
        "setISAMode"     => NO_OP,

        // ─── NoReturn (traps; control flow ends here) ─────────────
        // x86 `sysret` lives in classify_arch_specific so a non-x86
        // user-op of the same name cannot silently inherit NO_RETURN.
        "SoftwareBreakpoint"            => NO_RETURN,
        "UndefinedInstructionException" => NO_RETURN,
        "invalidInstructionException"   => NO_RETURN,
        "trap"                          => NO_RETURN,

        // ─── PURE: visible markers / pure compute, no memory edge ─

        // ARM exclusive-monitor primitives — pair with LDREX/STREX which
        // already emit pcode loads/stores.  The monitor flag is synthetic.
        "ExclusiveMonitorPass"     => PURE,
        "ExclusiveMonitorsStatus"  => PURE,

        // CPU hints — non-paired, no memory effect.
        "Hint_Prefetch" => PURE,
        "Yield"         => PURE,

        // x86 CPUID family — Sleigh's lift returns a tmpptr; the
        // EAX/EBX/ECX/EDX writes appear as ordinary Loads from
        // tmpptr+{0,4,8,12} in subsequent pcode.  The CallOther itself
        // doesn't touch RAM, so memory edge stays put — opt passes can
        // forward through it.
        "cpuid"                                           => PURE,
        "cpuid_Architectural_Performance_Monitoring_info" => PURE,
        "cpuid_Deterministic_Cache_Parameters_info"       => PURE,
        "cpuid_Direct_Cache_Access_info"                  => PURE,
        "cpuid_Extended_Feature_Enumeration_info"         => PURE,
        "cpuid_Extended_Topology_info"                    => PURE,
        "cpuid_MONITOR_MWAIT_Features_info"               => PURE,
        "cpuid_Processor_Extended_States_info"            => PURE,
        "cpuid_Quality_of_Service_info"                   => PURE,
        "cpuid_Thermal_Power_Management_info"             => PURE,
        "cpuid_Version_info"                              => PURE,
        "cpuid_basic_info"                                => PURE,
        "cpuid_brand_part1_info"                          => PURE,
        "cpuid_brand_part2_info"                          => PURE,
        "cpuid_brand_part3_info"                          => PURE,
        "cpuid_cache_tlb_info"                            => PURE,
        "cpuid_serial_info"                               => PURE,

        // NEON / SVE / multi-precision — Sleigh's pcode carries operand
        // regs; the user-op itself is pure compute.
        "MP_INT_ABS"  => PURE,
        "NEON_rev64"  => PURE,
        "NEON_sqshl"  => PURE,
        "NEON_uaddlv" => PURE,
        "SVE_fnmla"   => PURE,

        // ARM unmodelled sysreg read — pcode-explicit encoding constant
        // and destination; opaque value, no RAM effect.
        "UnkSytemRegRead" => PURE,

        // x86 `swapgs` lives in classify_arch_specific so a non-x86
        // user-op of the same name cannot silently inherit
        // PURE_WITH_MEM_EDGE.

        // ARM permanently-undefined instruction — Sleigh emits
        // CALLOTHER + a branch to the trap handler; the user-op itself
        // doesn't touch state.
        "software_udf" => PURE,

        // ─── PURE_WITH_MEM_EDGE: memory-chain markers + side-effecting

        // x86 LOCK / UNLOCK — bracket an atomic memory operation.  On
        // the memory chain so patterns walking mem from a Store inside
        // the bracket can find LOCK / UNLOCK as predecessors /
        // successors.
        "LOCK"   => PURE_WITH_MEM_EDGE,
        "UNLOCK" => PURE_WITH_MEM_EDGE,

        // ARM standalone memory / cache barriers — explicit ordering
        // markers with no accompanying Store; the only way they're
        // visible to the IR is by being on the memory chain.
        "DC_CVAC"                           => PURE_WITH_MEM_EDGE,
        "DataMemoryBarrier"                 => PURE_WITH_MEM_EDGE,
        "DataSynchronizationBarrier"        => PURE_WITH_MEM_EDGE,
        "InstructionSynchronizationBarrier" => PURE_WITH_MEM_EDGE,

        // x86/x86_64 standalone memory fences — explicit ordering
        // primitives with no register channel.  Emitted by Sleigh's
        // x86 spec as the lowercase mnemonic.  Memory-edge so opt
        // passes that walk the memory chain (StackLoadForward,
        // LoadReadOnly) cannot forward across them — matches the
        // semantic that subsequent loads must observe prior stores
        // in program order.  Without this entry, any binary using
        // SSE memory fences would lift to UnknownCallOtherError.
        "lfence" => PURE_WITH_MEM_EDGE,
        "mfence" => PURE_WITH_MEM_EDGE,
        "sfence" => PURE_WITH_MEM_EDGE,

        // x86 port I/O — port + value pcode-explicit; the user-op
        // itself affects external (port) state.
        "in"  => PURE_WITH_MEM_EDGE,
        "out" => PURE_WITH_MEM_EDGE,

        // ARM SVC / SWI raised by an immediate — possible syscall path,
        // kernel can do anything to memory.
        "software_interrupt" => PURE_WITH_MEM_EDGE,

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_abi() -> CallOtherAbi {
        CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: &[],
            memory_edge: false,
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
    fn memory_chain_markers_classify_as_pure_with_mem_edge() {
        // LOCK / UNLOCK and standalone barriers must be on the IR
        // memory chain so patterns walking mem can find them.  Tradeoff:
        // StackLoadForward stops at these (acceptable — they appear in
        // sync code, not in bsdfinder's offset patterns).
        for n in [
            "LOCK", "UNLOCK",
            "DataMemoryBarrier", "DataSynchronizationBarrier",
            "InstructionSynchronizationBarrier", "DC_CVAC",
        ] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n}"));
            let CallOtherClass::Call(abi) = class else { panic!("{n}: expected Call") };
            assert!(abi.implicit_reads.is_empty(), "{n}");
            assert!(abi.implicit_writes.is_empty(), "{n}");
            assert!(abi.memory_edge, "{n}: must advance mem edge for chain visibility");
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
            assert!(!abi.memory_edge, "{n}: must NOT advance mem edge (opt passes need to forward)");
        }
    }

    #[test]
    fn sysret_and_swapgs_are_x86_only() {
        // Regression: round-12 CA-2 — `sysret` and `swapgs` are
        // x86/x86_64-specific user-ops.  They must not silently match
        // on non-x86 arches.  Previously they lived in
        // `classify_arch_independent` and would have been classified
        // even on ARM/AArch64/MIPS/PowerPC.
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
        assert!(abi.memory_edge);

        let m32 = classify(crate::ArchPreset::X86, "monitor").expect("monitor x86");
        let CallOtherClass::Call(abi) = m32 else { panic!() };
        assert_eq!(abi.implicit_reads, &["EAX", "ECX", "EDX"]);

        let mwait = classify(crate::ArchPreset::X86_64, "mwait").expect("mwait classified");
        let CallOtherClass::Call(abi) = mwait else { panic!() };
        assert_eq!(abi.implicit_reads, &["EAX", "ECX"]);
        assert!(abi.implicit_writes.is_empty());
        assert!(abi.memory_edge);

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
        assert!(abi.memory_edge, "swapgs must advance memory edge (kernel GS base swap)");
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
        assert!(abi.memory_edge);
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
            assert!(!abi.memory_edge, "{n}: cpuid doesn't touch RAM");
        }
    }

    #[test]
    fn rdtsc_writes_edx_eax_no_memory_edge() {
        let class = classify(crate::ArchPreset::X86_64, "rdtsc").expect("rdtsc classified");
        let CallOtherClass::Call(abi) = class else {
            panic!("expected Call, got {class:?}")
        };
        assert_eq!(abi.implicit_reads, &[] as &[&str]);
        assert_eq!(abi.implicit_writes, &["EAX", "EDX"]);
        assert!(!abi.memory_edge);
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
        assert!(!abi.memory_edge);
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
                assert!(abi.memory_edge, "{preset:?}/{n}");
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
            assert!(!abi.memory_edge);
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
                assert!(!abi.memory_edge, "{preset:?}/{n}");
            }
            for n in edge_ops {
                let class = classify(preset, n).unwrap_or_else(|| panic!("{preset:?}/{n}"));
                let CallOtherClass::Call(abi) = class else {
                    panic!("{preset:?}/{n}: expected Call")
                };
                assert_eq!(abi.implicit_reads, &[] as &[&str], "{preset:?}/{n}");
                assert_eq!(abi.implicit_writes, &[] as &[&str], "{preset:?}/{n}");
                assert!(abi.memory_edge, "{preset:?}/{n}");
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
            assert!(abi.memory_edge, "{n}");
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
            assert!(abi.memory_edge, "{preset:?}");
        }
    }

    #[test]
    fn swi_on_x86_returns_empty_call_stub() {
        // (X86, "swi") and (X86_64, "swi") use an empty-ABI Call as a
        // sound stub until per-INT-vector / per-OS modelling lands.
        let empty = CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: true,
        });
        assert_eq!(classify(crate::ArchPreset::X86, "swi"), Some(empty));
        assert_eq!(classify(crate::ArchPreset::X86_64, "swi"), Some(empty));
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
            assert!(abi.memory_edge, "arch={arch:?}: DMB must advance mem edge");
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

    /// Regression: x86/x86_64 memory fences (mfence,
    /// sfence, lfence) MUST classify as PURE_WITH_MEM_EDGE so any binary
    /// using SSE memory fences lifts cleanly.  Without this entry, the
    /// CallOther emitted by Sleigh would raise UnknownCallOtherError at
    /// the IR layer.
    #[test]
    fn x86_memory_fences_classify_as_pure_with_mem_edge() {
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
                    abi.memory_edge,
                    "({preset:?}, {name}) must advance memory edge — fences are ordering primitives"
                );
            }
        }
    }
}
