use super::{DecodeFacts, Family};
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, Row};

pub(crate) static MIPS: Family = Family {
    name: "mips",
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
    // implicit here.  The caller-saved temporaries are listed as written
    // because the ABI does not preserve them across the kernel entry.
    Row {
        names: &["syscall"],
        only: Some(MIPS32_BOTH),
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["v0", "a0", "a1", "a2", "a3"],
            implicit_writes: &[
                "v0", "v1", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9",
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
                "v0", "v1", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9",
            ],
            clobbers_memory: true,
            no_return: false,
        }),
    },
];

const MIPS32_BOTH: &[crate::ArchPreset] =
    &[crate::ArchPreset::MipsBe32, crate::ArchPreset::MipsLe32];
const MIPS64_BOTH: &[crate::ArchPreset] =
    &[crate::ArchPreset::MipsBe64, crate::ArchPreset::MipsLe64];
