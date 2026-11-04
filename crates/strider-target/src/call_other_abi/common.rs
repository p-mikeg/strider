use super::{CallOtherClass, MEM_CLOBBER, PURE};
use CallOtherClass::NoOp;

/// Names meaning the same on every arch that emits them.
///
/// **Invariant: `Call` entries here MUST have empty `implicit_reads` and
/// `implicit_writes`.**  A named register (RAX, x0, r7) only resolves on one
/// arch's Sleigh register table, which makes the entry arch-specific by
/// definition.
pub(super) static ROWS: &[(&str, CallOtherClass)] = &[
    // ARM `SETEND` writes CPSR.E, byte-reversing every later DATA access, and
    // the sla says it cannot model that ("we can not change the effective
    // endianness of the language", ARMinstructions.sinc).  A load after one
    // does not read what a store before it wrote, so the memory edge has to
    // break.
    ("setEndianState", MEM_CLOBBER),
    // Pure Sleigh decoder context, invisible to the IR. The interworking
    // ISA mode `setISAMode` commits is captured from the `ISAModeSwitch`
    // write and carried on the `IndirectBranch` (see strider-lift `write_vn`),
    // so it needs no IR modeling here.
    ("setISAMode", NoOp),
    // AArch64 ERET returns from an exception, reloading PC and PSTATE
    // from ELR/SPSR, so control leaves the function like a return.
    // (x86 `sysret` is an x86 family row so a non-x86 user-op of
    // the same name cannot inherit NoReturn.)
    ("ExceptionReturn", CallOtherClass::NO_RETURN),
    ("UndefinedInstructionException", CallOtherClass::NO_RETURN),
    // `<op>(); goto [target]` in their slas, naming a handler the decoder
    // cannot see.  Linux spells BOTH `BUG()` and `WARN_ON()` with these (arm64
    // `brk #0x800`, x86 `ud2`), split only by a `__bug_table` flag on the
    // trap's address, so no instruction-keyed classification is right for both:
    // `BUG` does not return, `WARN` resumes at the next instruction.
    //
    // NoReturn is the deliberate choice: it costs WARN its resume edge, which
    // the branch skipping the trap normally reaches anyway, while seating that
    // edge at `inst_next` would give every BUG site an edge that never
    // executes, widening ranges at the join.  Exactness needs `__bug_table`,
    // per-address data a caller parses rather than a property of the opcode.
    ("SoftwareBreakpoint", CallOtherClass::NO_RETURN),
    ("invalidInstructionException", CallOtherClass::NO_RETURN),
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
    // so the register footprint is empty.  A memory source operand
    // (`m128`) is a separate `Load` ahead of the CallOther, so the op
    // itself touches no RAM.
    ("pblendw", PURE),
    ("pshufb", PURE),
    ("sha256rnds2_sha", PURE),
    ("sha256msg1_sha", PURE),
    ("sha256msg2_sha", PURE),
    // SVE / multi-precision: Sleigh's pcode carries the operand registers, so
    // the user-op is pure compute.  The `NEON_` names are the aarch64 prefix
    // family's, not rows here.
    ("MP_INT_ABS", PURE),
    ("SVE_fnmla", PURE),
    // ARM unmodelled sysreg read: pcode-explicit encoding constant and
    // destination, opaque value, no RAM effect.
    ("UnkSytemRegRead", PURE),
    // ARM permanently-undefined instruction: `BUG()` / `WARN()` in the Linux
    // kernel share ONE encoding, split only by a `__bug_table` flag.  Same
    // trade as `SoftwareBreakpoint` above: NoReturn costs the WARN half its
    // resume edge at `pc + 4`, and keeps every BUG site from contributing an
    // edge that never executes.
    ("software_udf", CallOtherClass::NO_RETURN),
    // The LDREX/STREX pair, like ExclusiveMonitorPass/Status above: the
    // monitor flag is synthetic, no RAM effect.  CLREX drops the monitor and
    // is emitted as a bare `ClearExclusiveLocal();` with no operands and no
    // output, so it has no footprint at all.
    ("ClearExclusiveLocal", PURE),
    ("ExclusiveAccess", PURE),
    ("hasExclusiveAccess", PURE),
    // ARM PLD / PLDW / PLI / YIELD are architectural no-ops, a prefetch being
    // a cache HINT rather than an observable access, but they stay visible
    // pure markers so patterns can find them.  All three preload forms take
    // the address as a VALUE (`addrmode2` exports `rn + offset`), not a
    // dynamic memory varnode, so none emits a p-code Load.
    ("HintPreloadData", PURE),
    ("HintPreloadDataForWrite", PURE),
    ("HintPreloadInstruction", PURE),
    ("HintYield", PURE),
    // SEV / SEVL set the event register on other cores and observe no
    // memory on this one.
    ("SendEvent", PURE),
    ("SendEventLocally", PURE),
    // SSAT/USAT and the Q-flag helpers: pure compute, pcode-explicit
    // operands.
    ("SignedSaturate", PURE),
    ("UnsignedSaturate", PURE),
    // NEON scalar conversions and polynomial multiply: pure register compute
    // with pcode-explicit operands, companions to the `Vector*` prefix family.
    // The float NEON ops are the `Float*` family, not rows here.
    ("FPToFixed", PURE),
    ("FixedToFP", PURE),
    ("PolynomialMultiply", PURE),
    // Interrupt-mask / processor-mode writes (CPSR I/F/A bits, mode
    // field) change privileged CPU state strider does not model, with no
    // data or RAM effect for single-threaded dataflow.  `setISAMode` is a
    // Sleigh DECODER-context switch and stays NoOp above, not here.
    ("disableDataAbortInterrupts", PURE),
    ("disableFIQinterrupts", PURE),
    ("disableIRQinterrupts", PURE),
    ("enableDataAbortInterrupts", PURE),
    ("enableFIQinterrupts", PURE),
    ("enableIRQinterrupts", PURE),
    // Queries of that same privileged state, reading into a pcode-explicit
    // destination: the CPSR I / F mask bits (`mrs Rd, primask` /
    // `mrs Rd, faultmask`) and the Thumb `msr control, Rn` / `mrs Rd, control`
    // pair between them read the lot.
    ("isCurrentModePrivileged", PURE),
    ("isFIQinterruptsEnabled", PURE),
    ("isIRQinterruptsEnabled", PURE),
    ("isThreadMode", PURE),
    ("isThreadModePrivileged", PURE),
    ("isUsingMainStack", PURE),
    // Setting the privilege bit re-banks nothing; the mode switches that do
    // name `sp` / `lr` and live in the family rows.
    ("setThreadModePrivileged", PURE),
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
    // MIPS PREF.  Every hint but one is architecturally a no-op; hint 25
    // (`PrepareForStore`) leaves the target cache line UNPREDICTABLE, so it is
    // a memory write this `PURE` does not model.  The sla passes the hint as a
    // plain operand (`mips32Instructions.sinc:501`
    // `prefetch(OFF_BASE, hint:1)`), so no row can separate the two.  `PURE`
    // is the deliberate choice: a program reading the line a `pref 25` armed
    // without writing it first is already unpredictable, and `MEM_CLOBBER`
    // would break the memory edge at every prefetch hint in every MIPS binary.
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
    // x86 has no such coverage: MFENCE / LFENCE / SFENCE have EMPTY
    // constructor bodies in `x86/data/languages/ia.sinc`, so no `CallOther`
    // node exists to stop a stack load forwarding across one.  These two rows
    // are the LOCK prefix, which is a real pcodeop.
    ("LOCK", MEM_CLOBBER),
    ("UNLOCK", MEM_CLOBBER),
    // DSB / DMB are data memory barriers; ISB flushes the instruction
    // pipeline and, conservatively, both instruction and data streams.  On
    // a multicore ARM system the stack frame is reachable from other cores
    // once its address escapes via a pointer argument or shared structure.
    ("DataMemoryBarrier", MEM_CLOBBER),
    ("DataSynchronizationBarrier", MEM_CLOBBER),
    ("InstructionSynchronizationBarrier", MEM_CLOBBER),
    // AArch64 LDAR (load-acquire) / STLR (store-release), Sleigh's LOAcquire
    // / LORelease.  Like the barriers above they clobber memory conservatively
    // (no load-forwarding across the barrier).  Both userops are bare void
    // markers taking no arguments: the access itself is separate real p-code
    // (`LOAcquire(); Wt = *Rn` / `*Rn = Wt; LORelease()`), so the loaded
    // value is on a `Load` node, not on this op's output.
    ("LOAcquire", MEM_CLOBBER),
    ("LORelease", MEM_CLOBBER),
    // PowerPC barriers.  `sync` covers SYNC / lwsync / hwsync: the `L`
    // field selects the variant, but Sleigh folds all three to one name.
    // `enforceInOrderExecutionIO` is EIEIO, an I/O barrier that also acts
    // as a full data-memory barrier on Power ISA.  `instructionSynchronize`
    // is ISYNC, an instruction-pipeline flush treated conservatively as a
    // data clobber.  Without these, any PowerPC binary containing a fence
    // fails the lift as an unknown CallOther user-op.
    ("enforceInOrderExecutionIO", MEM_CLOBBER),
    ("instructionSynchronize", MEM_CLOBBER),
    ("sync", MEM_CLOBBER),
    // Two spellings of the one MIPS SYNC mnemonic: GHIDRA's MIPS32 spec
    // emits `SYNC`, its mips.sinc common include emits `synch`.  Without
    // both, a MIPS binary containing SYNC fails the lift as an unknown
    // CallOther user-op.
    ("SYNC", MEM_CLOBBER),
    ("synch", MEM_CLOBBER),
    // WFE/WFI are synchronisation / low-power wait points: a remote agent
    // may modify shared memory, an escaped stack frame included, while the
    // core waits.
    ("WaitForEvent", MEM_CLOBBER),
    ("WaitForInterrupt", MEM_CLOBBER),
    // BKPT / HLT may return via a WARN-style handler, so they are returning
    // side-effecting ops rather than NoReturn: over-terminating would
    // truncate the function.  `software_hvc` / `software_smc` do the same but
    // carry a register footprint, so they live in the family rows.
    ("software_bkpt", MEM_CLOBBER),
    ("software_hlt", MEM_CLOBBER),
    // AArch64 privileged-state writes: an unmodeled sysreg write, the
    // generic SYS write, and address translation (AT_S1E1R writes PAR and
    // pokes the MMU).  Conservatively memory-affecting.
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
    // LGDT/SGDT, LIDT/SIDT, LLDT/SLDT, LTR/STR read from or write to a
    // memory operand describing the table.
    ("GlobalDescriptorTableRegister", MEM_CLOBBER),
    ("InterruptDescriptorTableRegister", MEM_CLOBBER),
    ("LocalDescriptorTableRegister", MEM_CLOBBER),
    ("TaskRegister", MEM_CLOBBER),
    // FXSAVE / FXRSTOR and the XSAVE / XRSTOR family carry a register
    // footprint -- the state they move IS the x87 / SSE / AVX registers -- so
    // they live in the family rows.
    // CLFLUSH, INVLPG, INVPCID: cache / TLB maintenance, relevant to
    // memory ordering.
    ("clflush", MEM_CLOBBER),
    ("invlpg", MEM_CLOBBER),
    ("invpcid", MEM_CLOBBER),
];
