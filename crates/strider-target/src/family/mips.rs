use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};

pub(crate) static MIPS: Family = Family {
    name: "mips",
    ghidra_ldefs: "MIPS/data/languages/mips.ldefs",
    facts: DecodeFacts {
        // The low bit selects the alternate ISA, which `RELP` fixes: every MIPS
        // pspec here pins it to 1, so that ISA is MIPS16e. rsleigh ships
        // `PSPEC_MIPS32MICRO` / `PSPEC_MIPS64MICRO` for `RELP=0` (microMIPS) and no
        // preset uses them, so a microMIPS image decodes its odd-addressed
        // functions against MIPS16e tables; Python's `load_elf` refuses one.
        isa_mode_var: Some("ISA_MODE"),
        flow_commit_vars: &[],
        // `ext_delay` is written by the five MIPS16e branch/call constructors
        // (`mips16.sinc:520,528,542,550,557`), reachable only at `ISA_MODE=1`.
        transient_decode_vars: &["PAIR_INSTRUCTION_FLAG", "ext_delay"],
        has_delay_slots: true,
        callother_output_exact: false,
        internal_registers: &["tsp"],
        // CP0 `Count` is a cycle counter, `Random` a TLB index that steps every
        // cycle, `Cause` holds the pending-interrupt bits the hardware sets,
        // `PerfCnt.1`/`.3`/`.5`/`.7` are the performance counters (the even
        // selects are their controls) and `cop0_reg9.6` Octeon's `CvmCount`.
        // No MIPS pspec marks any of them.
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
    // MIPS `break` is `{ tmp=breakcode; trap(tmp); }` and the conditional
    // `teq`/`tne`/`tge`/`tgeu`/`tlt`/`tltu` are the same `trap(tmp)` behind an
    // `if (!cond) goto <done>` guard.  Both terminate: Linux MIPS spells
    // `BUG()` as `break BRK_BUG` and `BUG_ON(c)` as `tne $0,c,BRK_BUG`, and
    // `BUG()` is `__noreturn`.  The guarded form keeps its non-trapping path
    // through the sla's own branch to `<done>`, so terminating at the
    // CallOther costs it nothing.
    //
    // Unlike the other arches emitting `trap`, MIPS does not route `WARN_ON`
    // through it: no `__bug_table` accompanies these traps, and `__warn` /
    // `warn_slowpath_fmt` are called, so nothing that resumes reaches `trap`.
    Row {
        names: &["trap"],
        only: None,
        class: CallOtherClass::NO_RETURN,
    },
    // Linux MIPS o32 SYSCALL: v0 = number, a0..a3 = args, v0 = return value,
    // a3 = error flag, v1 = the second half of a two-value return (`pipe`).
    // Sleigh's `:syscall is ... breakcode { tmp:4=breakcode; syscall(tmp); }`
    // passes only the encoded code field, so the whole register ABI is
    // implicit here. The kernel's partial exit restores neither the
    // temporaries nor `at`, `hi`, `lo` (glibc's `__SYSCALL_CLOBBERS`).
    Row {
        names: &["syscall"],
        only: Some(MIPS32_BOTH),
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["v0", "a0", "a1", "a2", "a3"],
            implicit_writes: &[
                "at", "v0", "v1", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9",
                "hi", "lo",
            ],
            // A kernel entry can read or write any user memory, the user stack
            // frame included.
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // The same op under n64, which passes eight argument registers.  GHIDRA's
    // MIPS spec has no `a4`..`a7` names: those are `t0`..`t3` in `mips.sinc`'s
    // one register list, so the extra four arguments are read from there.
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

/// Base for `mips_o32` and `mips_n64`; n64 widens both argument banks,
/// narrows the callee-saved float set, and drops the shadow space.
const MIPS_O32_BASE: CallingConvention = CallingConvention {
    stack_ptr: "sp",
    arg_passing_regs: &["a0", "a1", "a2", "a3"],
    // O32 passes at most two float arguments, in $f12 and $f14; $f13 / $f15
    // are the odd halves of those double pairs, never argument slots of their
    // own.
    //
    // Positional here, which o32 is NOT: it uses the FP registers only while
    // every preceding argument is also FP, so `g(int, double)` passes the
    // double in a2/a3 and `g(float, int, float)` passes the second float in
    // a2. Which rule applies depends on the callee's signature, and the IR does
    // not carry one, so a function whose arguments do not lead with floats can
    // report an $f12 temporary as float argument 0.
    arg_passing_regs_float: &["f12", "f14"],
    callee_saved_regs: &[
        // `gp` is preserved by the callee only in non-PIC o32.  Under
        // `-mabicalls` the callee recomputes it from `t9` and the CALLER
        // restores it (`.cprestore`), so o32 PIC gets the same deliberate
        // over-preservation as `LR` on PowerPC: `InitialVar(gp)` reaches GOT
        // loads, and a PIC caller needing `gp` after a call reloads it.
        "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra",
        // o32 preserves $f20-$f31; n64 narrows it to $f24-$f31 and overrides
        // this whole list.
        "f20", "f21", "f22", "f23", "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31",
    ],
    ret_val_regs: &["v0", "v1"],
    // O32 returns a `double` in the $f0/$f1 pair, which the mips32 sla
    // (`FREGSIZE 4`) declares as the 8-byte `f0_1` over each even/odd FPR
    // pair; every double-format instruction writes it.  Naming the 4-byte
    // `f0` instead TRUNCATES the return: `Function::ret_val_regs` hands the
    // list to the lifter verbatim, with no tracked-container projection, so
    // the read slices I32 out of `f0_1` and the high half is discarded.
    // n64 overrides this back to `f0` / `f2`, which are 8 bytes there.
    // Unused on soft-float builds.
    ret_val_regs_float: &["f0_1", "f2_3"],
    stack_args: Some(StackArgs {
        base_offset: 16,
        increment: 4,
    }),
    ret_stack_pop: 0,
    // `jal`/`jalr` writes the return address to `$ra` (`$31`); Sleigh's mips32
    // register table names it lowercase `ra`.
    link_register: Some("ra"),
    preserves_memory: false,
    preserves_all_registers: false,
    // The abicalls (PIC) contract: a call through `jalr t9` leaves the callee's
    // own address in t9, and the prologue adds it to a link-time constant to
    // form `gp`.  `-mno-abicalls` code never reads t9 at entry, so the binding
    // is inert rather than wrong there.
    entry_address: Some("t9"),
    reg_args_adjoin_stack_args: true,
};

impl CallingConvention {
    /// Returns the MIPS O32 calling convention.
    #[must_use]
    pub const fn mips_o32() -> Self {
        // MIPS O32, byte-order independent: pairs with both `mipsle32` and
        // `mipsbe32`.  `ret_stack_pop` is 0 because `jal`/`jalr` writes the return
        // address to `$ra` rather than pushing it.  Offsets 0..16 are MIPS's
        // reserved shadow space for the four register args, so positional stack
        // args start at 16.
        //
        // Sleigh's MIPS spec uses lowercase names and calls the frame pointer
        // `s8`; `fp` does not resolve in its register table.
        MIPS_O32_BASE
    }

    /// Returns the MIPS N64 calling convention.
    #[must_use]
    pub const fn mips_n64() -> Self {
        // MIPS N64 (`mips64-linux-gnuabi64-gcc`, LE and BE).  Extends O32's 4
        // register args to 8 (`$4`..`$11`) and drops the shadow space, so stack
        // args start at SP+0.  Sleigh's `mips64` spec uses the older naming where
        // `$8`..`$11` are `t0`..`t3`, so they are listed under those names.
        CallingConvention {
            arg_passing_regs: &["a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3"],
            // N64 widens the float argument bank to eight, $f12..$f19.
            arg_passing_regs_float: &["f12", "f13", "f14", "f15", "f16", "f17", "f18", "f19"],
            // `FREGSIZE 8`: the FPRs are already 8 bytes and the sla declares
            // no `f0_1` pair register, so n64 undoes o32's pair naming.
            ret_val_regs_float: &["f0", "f2"],
            // n64's float argument bank is $f12-$f19; the callee-saved set
            // narrows from o32's $f20-$f31 to $f24-$f31.
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
