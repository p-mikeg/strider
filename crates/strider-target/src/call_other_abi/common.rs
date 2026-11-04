use super::{CallOtherClass, MEM_CLOBBER, PURE};
use CallOtherClass::NoOp;

/// Names meaning the same on every arch that emits them.
///
/// Invariant: no entry names a register, which would resolve on one arch only.
pub(super) static ROWS: &[(&str, CallOtherClass)] = &[
    // ARM `SETEND` byte-reverses later data accesses, which the sla cannot
    // model.
    ("setEndianState", MEM_CLOBBER),
    // Decoder context; the lift reads the mode off `ISAModeSwitch`.
    ("setISAMode", NoOp),
    // AArch64 ERET.
    ("ExceptionReturn", CallOtherClass::NO_RETURN),
    ("UndefinedInstructionException", CallOtherClass::NO_RETURN),
    // AArch64 `brk`. Linux spells both `BUG()` and `WARN_ON()` with it, told
    // apart only by `__bug_table`. NoReturn costs WARN its resume edge, which
    // the branch around the trap usually reaches anyway.
    ("SoftwareBreakpoint", CallOtherClass::NO_RETURN),
    // x86 `ud0` / `ud1` / `ud2`; `ud2` is Linux's `BUG()` and `WARN_ON()`, the
    // same tradeoff.
    ("invalidInstructionException", CallOtherClass::NO_RETURN),
    // The exclusive monitor; LDREX / STREX accesses are p-code.
    ("ExclusiveMonitorPass", PURE),
    ("ExclusiveMonitorsStatus", PURE),
    ("Hint_Prefetch", PURE),
    ("Yield", PURE),
    // CPUID serializes, a full memory barrier; its register writes are p-code.
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
    // SIMD ops with every operand listed (SHA256RNDS2 lists XMM0).
    ("pblendw", PURE),
    ("pshufb", PURE),
    ("sha256rnds2_sha", PURE),
    ("sha256msg1_sha", PURE),
    ("sha256msg2_sha", PURE),
    ("MP_INT_ABS", PURE),
    ("SVE_fnmla", PURE),
    // ARM unmodelled sysreg read.
    ("UnkSytemRegRead", PURE),
    // ARM UDF: `BUG()` / `WARN()`, as `SoftwareBreakpoint`.
    ("software_udf", CallOtherClass::NO_RETURN),
    ("ClearExclusiveLocal", PURE),
    ("ExclusiveAccess", PURE),
    ("hasExclusiveAccess", PURE),
    // ARM preload hints and YIELD, kept as markers patterns can find.
    ("HintPreloadData", PURE),
    ("HintPreloadDataForWrite", PURE),
    ("HintPreloadInstruction", PURE),
    ("HintYield", PURE),
    ("SendEvent", PURE),
    ("SendEventLocally", PURE),
    ("SignedSaturate", PURE),
    ("UnsignedSaturate", PURE),
    ("FPToFixed", PURE),
    ("FixedToFP", PURE),
    ("PolynomialMultiply", PURE),
    // CPSR interrupt-mask writes and reads.
    ("disableDataAbortInterrupts", PURE),
    ("disableFIQinterrupts", PURE),
    ("disableIRQinterrupts", PURE),
    ("enableDataAbortInterrupts", PURE),
    ("enableFIQinterrupts", PURE),
    ("enableIRQinterrupts", PURE),
    ("isCurrentModePrivileged", PURE),
    ("isFIQinterruptsEnabled", PURE),
    ("isIRQinterruptsEnabled", PURE),
    ("isThreadMode", PURE),
    ("isThreadModePrivileged", PURE),
    ("isUsingMainStack", PURE),
    // Re-banks nothing; the mode switches are ARM family rows.
    ("setThreadModePrivileged", PURE),
    ("SignedDoesSaturate", PURE),
    ("UnsignedDoesSaturate", PURE),
    // MIPS RDHWR / MFTR / TLBP / TLBR.
    ("TLB_probe_for_matching_entry", PURE),
    ("TLB_read_indexed_entryHi", PURE),
    ("TLB_read_indexed_entryLo0", PURE),
    ("TLB_read_indexed_entryLo1", PURE),
    ("TLB_read_indexed_entryPageMask", PURE),
    ("getHWRegister", PURE),
    ("move_from_thread_cp0", PURE),
    // MIPS PREF. Hint 25 (`PrepareForStore`) leaves the line unpredictable,
    // but reading it before writing it is already undefined.
    ("prefetch", PURE),
    // x86 VERW, RDPMC, RDRAND / RDSEED: outputs are p-code.
    ("rdpmc", PURE),
    ("rdrand", PURE),
    ("rdrandIsValid", PURE),
    ("rdseed", PURE),
    ("rdseedIsValid", PURE),
    ("verw", PURE),
    // x86 port I/O.
    ("in", MEM_CLOBBER),
    ("out", MEM_CLOBBER),
    // Barriers order all memory, the stack frame included. x86's
    // MFENCE / LFENCE / SFENCE have empty p-code; LOCK is a pcodeop.
    ("LOCK", MEM_CLOBBER),
    ("UNLOCK", MEM_CLOBBER),
    ("DataMemoryBarrier", MEM_CLOBBER),
    ("DataSynchronizationBarrier", MEM_CLOBBER),
    ("InstructionSynchronizationBarrier", MEM_CLOBBER),
    // AArch64 LDAR / STLR markers around a p-code access.
    ("LOAcquire", MEM_CLOBBER),
    ("LORelease", MEM_CLOBBER),
    // PowerPC EIEIO, ISYNC and every SYNC variant.
    ("enforceInOrderExecutionIO", MEM_CLOBBER),
    ("instructionSynchronize", MEM_CLOBBER),
    ("sync", MEM_CLOBBER),
    // MIPS SYNC, spelled two ways.
    ("SYNC", MEM_CLOBBER),
    ("synch", MEM_CLOBBER),
    // WFE / WFI: another agent may write memory while the core waits.
    ("WaitForEvent", MEM_CLOBBER),
    ("WaitForInterrupt", MEM_CLOBBER),
    // BKPT / HLT may resume through their handler.
    ("software_bkpt", MEM_CLOBBER),
    ("software_hlt", MEM_CLOBBER),
    // AArch64 system-register writes and address translation.
    ("AT_S1E1R", MEM_CLOBBER),
    ("SysOp_W", MEM_CLOBBER),
    ("UnkSytemRegWrite", MEM_CLOBBER),
    // MIPS cache maintenance (CACHE), low-power WAIT, another thread's CP0
    // write (MTTR), and TLB writes / invalidation: external system state.
    // `setCopReg` (MTC0) is a MIPS family row.
    ("TLB_invalidate", MEM_CLOBBER),
    ("TLB_invalidate_flush", MEM_CLOBBER),
    ("TLB_write_indexed_entry", MEM_CLOBBER),
    ("TLB_write_random_entry", MEM_CLOBBER),
    ("cacheOp", MEM_CLOBBER),
    ("move_to_thread_cp0", MEM_CLOBBER),
    ("wait", MEM_CLOBBER),
    // x86 descriptor-table loads and stores.
    ("GlobalDescriptorTableRegister", MEM_CLOBBER),
    ("InterruptDescriptorTableRegister", MEM_CLOBBER),
    ("LocalDescriptorTableRegister", MEM_CLOBBER),
    ("TaskRegister", MEM_CLOBBER),
    // x86 cache and TLB maintenance.
    ("clflush", MEM_CLOBBER),
    ("invlpg", MEM_CLOBBER),
    ("invpcid", MEM_CLOBBER),
];
