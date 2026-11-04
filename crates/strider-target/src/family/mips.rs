use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};

pub(crate) static MIPS: Family = Family {
    name: "mips",
    ghidra_ldefs: "MIPS/data/languages/mips.ldefs",
    facts: DecodeFacts {
        // The pspecs pin `RELP=1`, so the alternate ISA is MIPS16e, never
        // microMIPS.
        isa_mode_var: Some("ISA_MODE"),
        flow_commit_vars: &[],
        // `ext_delay` is set by the MIPS16e branch and call constructors.
        transient_decode_vars: &["PAIR_INSTRUCTION_FLAG", "ext_delay"],
        has_delay_slots: true,
        callother_output_exact: false,
        internal_registers: &["tsp"],
        // CP0 counters and hardware-set state, which no MIPS pspec marks:
        // `PerfCnt.<odd>` are the counters, `cop0_reg9.6` Octeon's `CvmCount`.
        volatile_registers: &[
            "Count",
            "Random",
            "Cause",
            "PerfCnt.1",
            "PerfCnt.3",
            "PerfCnt.5",
            "PerfCnt.7",
            "cop0_reg9.6",
        ],
        non_volatile_registers: &[],
    },
    rounding_ops: &["fcsrRoundingControl"],
    msr_mode_op: None,
    rounding_register: None,
    call_other: CALL_OTHER,
    call_other_prefixes: &[],
    call_other_fallback: None,
    calling_conventions: CALLING_CONVENTIONS,
};

const CALL_OTHER: &[Row] = &[
    // `break` and the conditional traps (behind the sla's own branch): Linux
    // spells `BUG()` with them, and `WARN_ON` as a call.
    Row {
        names: &["trap"],
        only: None,
        class: CallOtherClass::NO_RETURN,
    },
    // Linux o32 SYSCALL: v0 = number, a0..a3 = args, v0/v1 = return, a3 =
    // error flag. The kernel's partial exit restores neither the temporaries
    // nor `at`, `hi`, `lo` (glibc's `__SYSCALL_CLOBBERS`).
    Row {
        names: &["syscall"],
        only: Some(MIPS32_BOTH),
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["v0", "a0", "a1", "a2", "a3"],
            implicit_writes: &[
                "at", "v0", "v1", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9",
                "hi", "lo",
            ],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // n64: eight arguments; the sla names a4..a7 `t0`..`t3`.
    Row {
        names: &["syscall"],
        only: Some(MIPS64_BOTH),
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["v0", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3"],
            implicit_writes: &[
                "at", "v0", "v1", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9",
                "hi", "lo",
            ],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // `mtc0` / `dmtc0` name their CP0 destination as an operand
    // (`setCopReg(0, RD0, rt, sel)`) while `mfc0` reads it directly, so the
    // op writes the whole file. COP2's `setCopReg(2, imm, rt)` shares the name.
    Row {
        names: &["setCopReg"],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &[],
            implicit_writes: CP0_REGISTERS,
            clobbers_memory: true,
            no_return: false,
        }),
    },
];

/// The CP0 file in `mips.sinc` order: 32 registers at each select 0..=7.
#[rustfmt::skip]
const CP0_REGISTERS: &[&str] = &[
    "Index",         "Random",        "EntryLo0",      "EntryLo1",
    "Context",       "PageMask",      "Wired",         "HWREna",
    "BadVAddr",      "Count",         "EntryHi",       "Compare",
    "Status",        "Cause",         "EPC",           "PRId",
    "Config",        "LLAddr",        "WatchLo",       "WatchHi",
    "XContext",      "cop0_reg21",    "cop0_reg22",    "Debug",
    "DEPC",          "PerfCnt",       "ErrCtl",        "CacheErr",
    "TagLo",         "TagHi",         "ErrorEPC",      "DESAVE",
    "MVPControl",    "VPEControl",    "TCStatus",      "cop0_reg3.1",
    "ContextConfig", "PageGrain",     "SRSConf0",      "cop0_reg7.1",
    "cop0_reg8.1",   "cop0_reg9.1",   "cop0_reg10.1",  "cop0_reg11.1",
    "IntCtl",        "cop0_reg13.1",  "cop0_reg14.1",  "EBase",
    "Config1",       "cop0_reg17.1",  "WatchLo.1",     "WatchHi.1",
    "cop0_reg20.1",  "cop0_reg21.1",  "cop0_reg22.1",  "TraceControl",
    "cop0_reg24.1",  "PerfCnt.1",     "cop0_reg26.1",  "CacheErr.1",
    "DataLo.1",      "DataHi.1",      "cop0_reg30.1",  "cop0_reg31.1",
    "MVPConf0",      "VPEConf0",      "TCBind",        "cop0_reg3.2",
    "cop0_reg4.2",   "cop0_reg5.2",   "SRSConf1",      "cop0_reg7.2",
    "cop0_reg8.2",   "cop0_reg9.2",   "cop0_reg10.2",  "cop0_reg11.2",
    "SRSCtl",        "cop0_reg13.2",  "cop0_reg14.2",  "cop0_reg15.2",
    "Config2",       "cop0_reg17.2",  "WatchLo.2",     "WatchHi.2",
    "cop0_reg20.2",  "cop0_reg21.2",  "cop0_reg22.2",  "TraceControl2",
    "cop0_reg24.2",  "PerfCnt.2",     "cop0_reg26.2",  "CacheErr.2",
    "TagLo.2",       "TagHi.2",       "cop0_reg30.2",  "cop0_reg31.2",
    "MVPConf1",      "VPEConf1",      "TCRestart",     "cop0_reg3.3",
    "cop0_reg4.3",   "cop0_reg5.3",   "SRSConf2",      "cop0_reg7.3",
    "cop0_reg8.3",   "cop0_reg9.3",   "cop0_reg10.3",  "cop0_reg11.3",
    "SRSMap",        "cop0_reg13.3",  "cop0_reg14.3",  "cop0_reg15.3",
    "Config3",       "cop0_reg17.3",  "WatchLo.3",     "WatchHi.3",
    "cop0_reg20.3",  "cop0_reg21.3",  "cop0_reg22.3",  "UserTraceData",
    "cop0_reg24.3",  "PerfCnt.3",     "cop0_reg26.3",  "CacheErr.3",
    "DataLo.3",      "DataHi.3",      "cop0_reg30.3",  "cop0_reg31.3",
    "cop0_reg0.4",   "YQMask",        "TCHalt",        "cop0_reg3.4",
    "cop0_reg4.4",   "cop0_reg5.4",   "SRSConf3",      "cop0_reg7.4",
    "cop0_reg8.4",   "cop0_reg9.4",   "cop0_reg10.4",  "cop0_reg11.4",
    "cop0_reg12.4",  "cop0_reg13.4",  "cop0_reg14.4",  "cop0_reg15.4",
    "cop0_reg16.4",  "cop0_reg17.4",  "WatchLo.4",     "WatchHi.4",
    "cop0_reg20.4",  "cop0_reg21.4",  "cop0_reg22.4",  "TraceBPC",
    "cop0_reg24.4",  "PerfCnt.4",     "cop0_reg26.4",  "CacheErr.4",
    "TagLo.4",       "TagHi.4",       "cop0_reg30.4",  "cop0_reg31.4",
    "cop0_reg0.5",   "VPESchedule",   "TCContext",     "cop0_reg3.5",
    "cop0_reg4.5",   "cop0_reg5.5",   "SRSConf4",      "cop0_reg7.5",
    "cop0_reg8.5",   "cop0_reg9.5",   "cop0_reg10.5",  "cop0_reg11.5",
    "cop0_reg12.5",  "cop0_reg13.5",  "cop0_reg14.5",  "cop0_reg15.5",
    "cop0_reg16.5",  "cop0_reg17.5",  "WatchLo.5",     "WatchHi.5",
    "cop0_reg20.5",  "cop0_reg21.5",  "cop0_reg22.5",  "cop0_reg23.5",
    "cop0_reg24.5",  "PerfCnt.5",     "cop0_reg26.5",  "CacheErr.5",
    "DataLo.5",      "DataHi.5",      "cop0_reg30.5",  "cop0_reg31.5",
    "cop0_reg0.6",   "VPEScheFBack",  "TCSchedule",    "cop0_reg3.6",
    "cop0_reg4.6",   "cop0_reg5.6",   "cop0_reg6.6",   "cop0_reg7.6",
    "cop0_reg8.6",   "cop0_reg9.6",   "cop0_reg10.6",  "cop0_reg11.6",
    "cop0_reg12.6",  "cop0_reg13.6",  "cop0_reg14.6",  "cop0_reg15.6",
    "cop0_reg16.6",  "cop0_reg17.6",  "WatchLo.6",     "WatchHi.6",
    "cop0_reg20.6",  "cop0_reg21.6",  "cop0_reg22.6",  "cop0_reg23.6",
    "cop0_reg24.6",  "PerfCnt.6",     "cop0_reg26.6",  "CacheErr.6",
    "TagLo.6",       "TagHi.6",       "cop0_reg30.6",  "cop0_reg31.6",
    "cop0_reg0.7",   "VPEOpt",        "TCScheFBack",   "cop0_reg3.7",
    "cop0_reg4.7",   "cop0_reg5.7",   "cop0_reg6.7",   "cop0_reg7.7",
    "cop0_reg8.7",   "cop0_reg9.7",   "cop0_reg10.7",  "cop0_reg11.7",
    "cop0_reg12.7",  "cop0_reg13.7",  "cop0_reg14.7",  "cop0_reg15.7",
    "cop0_reg16.7",  "cop0_reg17.7",  "WatchLo.7",     "WatchHi.7",
    "cop0_reg20.7",  "cop0_reg21.7",  "cop0_reg22.7",  "cop0_reg23.7",
    "cop0_reg24.7",  "PerfCnt.7",     "cop0_reg26.7",  "CacheErr.7",
    "DataLo.7",      "DataHi.7",      "cop0_reg30.7",  "cop0_reg31.7",
];

const MIPS32_BOTH: &[crate::ArchPreset] =
    &[crate::ArchPreset::MipsBe32, crate::ArchPreset::MipsLe32];
const MIPS64_BOTH: &[crate::ArchPreset] =
    &[crate::ArchPreset::MipsBe64, crate::ArchPreset::MipsLe64];

const CALLING_CONVENTIONS: &[CcPreset] = &[
    CcPreset {
        name: "mips_o32",
        cc: CallingConvention::mips_o32(),
        arches: &[ArchPreset::MipsBe32, ArchPreset::MipsLe32],
        ghidra: &[
            (
                "MIPS/data/languages/mips32be.cspec",
                None,
                &[ArchPreset::MipsBe32],
            ),
            (
                "MIPS/data/languages/mips32le.cspec",
                None,
                &[ArchPreset::MipsLe32],
            ),
        ],
    },
    CcPreset {
        name: "mips_n64",
        cc: CallingConvention::mips_n64(),
        arches: &[ArchPreset::MipsBe64, ArchPreset::MipsLe64],
        ghidra: &[
            (
                "MIPS/data/languages/mips64be.cspec",
                None,
                &[ArchPreset::MipsBe64],
            ),
            (
                "MIPS/data/languages/mips64le.cspec",
                None,
                &[ArchPreset::MipsLe64],
            ),
        ],
    },
];

/// Base for `mips_o32` and `mips_n64`.
const MIPS_O32_BASE: CallingConvention = CallingConvention {
    stack_ptr: "sp",
    arg_passing_regs: &["a0", "a1", "a2", "a3"],
    // Positional, which o32 is not: it uses $f12 / $f14 only while every
    // earlier argument is FP, so a function not leading with floats can report
    // an $f12 temporary as float argument 0.
    arg_passing_regs_float: &["f12", "f14"],
    callee_saved_regs: &[
        // Under `-mabicalls` the caller restores `gp`; listed so
        // `InitialVar(gp)` reaches GOT loads, as a PIC caller reloads it.
        "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra", "f20", "f21", "f22",
        "f23", "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31",
    ],
    ret_val_regs: &["v0", "v1"],
    // The 8-byte pair registers (`FREGSIZE 4`); the 4-byte `f0` would
    // truncate a `double` return.
    ret_val_regs_float: &["f0_1", "f2_3"],
    // Above the 16-byte home area of a0..a3.
    stack_args: Some(StackArgs {
        base_offset: 16,
        increment: 4,
    }),
    ret_stack_pop: 0,
    link_register: Some("ra"),
    preserves_memory: false,
    preserves_all_registers: false,
    // abicalls: a callee is entered through `jalr t9`.
    entry_address: Some("t9"),
    reg_args_adjoin_stack_args: true,
};

impl CallingConvention {
    /// Returns the MIPS O32 calling convention.
    #[must_use]
    pub const fn mips_o32() -> Self {
        MIPS_O32_BASE
    }

    /// Returns the MIPS N64 calling convention.
    #[must_use]
    pub const fn mips_n64() -> Self {
        // `$8`..`$11` are `t0`..`t3` in the sla.
        CallingConvention {
            arg_passing_regs: &["a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3"],
            arg_passing_regs_float: &["f12", "f13", "f14", "f15", "f16", "f17", "f18", "f19"],
            // `FREGSIZE 8`: no pair registers.
            ret_val_regs_float: &["f0", "f2"],
            callee_saved_regs: &[
                "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra", "f24", "f25",
                "f26", "f27", "f28", "f29", "f30", "f31",
            ],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ..MIPS_O32_BASE
        }
    }
}
