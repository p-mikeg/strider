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
#[must_use]
fn classify_arch_specific(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    match (preset, name) {
        // ARM Linux SVC / SWI ABI: r7 = syscall number, r0..r6 = args
        // (up to 7), r0 = return value.  See `arch/arm/kernel/entry-common.S`
        // and the EABI variant in `arch/arm/include/uapi/asm/unistd.h`.
        // ARM Linux SVC / SWI ABI: r7 = syscall number, r0..r6 = args
        // (up to 7), r0 = return value.  All three 32-bit ARM presets
        // share this ABI; if Thumb ever needs a different one, split the
        // alternation into separate arms.
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

        // ARM SMCCC for HVC and SMC: X0..X7 in, X0..X3 out.  Both
        // little- and big-endian aarch64 share the convention.  Arch-
        // specific because `x0..x7` only resolve on aarch64's Sleigh
        // register table (arm-32 has `r0..r12`).
        (crate::ArchPreset::Aarch64 | crate::ArchPreset::Aarch64Be,
         "CallHyperVisor") => Some(CallOtherClass::Call(CallOtherAbi {
            implicit_reads:  &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            implicit_writes: &["x0", "x1", "x2", "x3"],
            memory_edge:     true,
        })),
        (crate::ArchPreset::Aarch64 | crate::ArchPreset::Aarch64Be,
         "CallSecureMonitor") => Some(CallOtherClass::Call(CallOtherAbi {
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
        "SoftwareBreakpoint"            => NO_RETURN,
        "UndefinedInstructionException" => NO_RETURN,
        "invalidInstructionException"   => NO_RETURN,
        "sysret"                        => NO_RETURN,
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

        // x86 SWAPGS — swaps a synthetic GS_base MSR; no general-reg or
        // RAM effect.
        "swapgs" => PURE,

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
            "swapgs", "UnkSytemRegRead", "software_udf",
        ] {
            let class = classify(crate::ArchPreset::X86_64, n).unwrap_or_else(|| panic!("{n}"));
            let CallOtherClass::Call(abi) = class else { panic!("{n}: expected Call") };
            assert!(abi.implicit_reads.is_empty(), "{n}");
            assert!(abi.implicit_writes.is_empty(), "{n}");
            assert!(!abi.memory_edge, "{n}: must NOT advance mem edge (opt passes need to forward)");
        }
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
    fn empty_abi_ops_use_call_with_empty_abi() {
        for n in [
            "NEON_rev64",
            "NEON_sqshl",
            "NEON_uaddlv",
            "SVE_fnmla",
            "MP_INT_ABS",
            "UnkSytemRegRead",
            "swapgs",
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
}
