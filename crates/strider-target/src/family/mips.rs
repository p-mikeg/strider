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
