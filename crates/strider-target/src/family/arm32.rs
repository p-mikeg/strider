use super::{DecodeFacts, Family};
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Prefix, Row};

pub(crate) static ARM32: Family = Family {
    name: "arm32",
    facts: DecodeFacts {
        isa_mode_var: Some("TMode"),
        flow_commit_vars: &["condit", "TEEMode"],
        // `condit` is deliberately absent: it belongs to `flow_commit_vars`, which
        // RE-DERIVES it by re-lifting the owning instruction. These are cleared
        // with no re-lift, including at a region start, which destroys an IT state
        // the predecessor legitimately committed.
        transient_decode_vars: &["LRset", "REToverride", "CALLoverride"],
        has_delay_slots: false,
        callother_output_exact: false,
        internal_registers: &[
            "mult_addr",
            "mult_dat8",
            "mult_dat16",
            "tmpNG",
            "tmpZR",
            "tmpCY",
            "tmpOV",
        ],
        volatile_registers: &[],
        non_volatile_registers: &[],
    },
    rounding_ops: &["fpscrRoundingControl"],
    msr_mode_op: Some("msrRegisterMode"),
    rounding_register: None,
    call_other: CALL_OTHER,
    call_other_prefixes: PREFIXES,
    call_other_fallback: Some(coproc),
};

const CALL_OTHER: &[Row] = &[
    // ARM Linux SVC: r7 = syscall number, r0..r6 = args, r0 = return. ARM's
    // Sleigh spec emits `software_interrupt` for SVC/SWI (the bare `swi`
    // pcodeop is x86's INT, a row in `family/x86.rs`). This is the only row
    // naming `software_interrupt`. All four 32-bit ARM presets share it;
    // split the row if Thumb ever diverges.
    Row {
        names: &["software_interrupt"],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
            implicit_writes: &["r0"],
            // A kernel entry can read or write any user-mode memory, the user
            // stack included, so no stack load forwards across it.
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // The AArch32 half of the same SMCCC (`ARM.sinc:150-151`, raised by
    // `ARMinstructions.sinc:1591`/`:1608` and the Thumb pair at
    // `ARMTHUMBinstructions.sinc:1447`/`:1454`).  The sla declares no output
    // for either op, so without this row nothing writes r0 and a read after
    // an SMC resolves to the value that flowed IN.  SMC32/HVC32 passes
    // arguments in r0..r7 and returns in r0..r3; SMCCC 1.0 leaves r4..r14
    // unpredictable (1.1+ preserves them), so clobber through r12 and leave
    // `sp` / `lr` alone, matching the aarch64 row's x0..x17.
    Row {
        names: &["software_hvc", "software_smc"],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"],
            implicit_writes: &[
                "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12",
            ],
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // ARM32 NEON ops `ARMneon.sinc` names outside its `Vector*` / `Float*`
    // groups.  `vrev` (`:5289-5325`, `Qd = vrev(Qm, esize)`), the saturating
    // pair (`:5081-5082`, `Dd = SatQ(Dd, esize, unsigned)`), and the SHA-256
    // message schedule (`:681`, `Qd = SHA256ScheduleUpdate0(Qd, Qm)`) are all
    // register-to-register with listed operands.  Arch-scoped because `vrev`
    // and `SatQ` are generic enough for another spec to reuse the spelling.
    Row {
        names: &[
            "SHA256ScheduleUpdate0",
            "SHA256ScheduleUpdate1",
            "SatQ",
            "SignedSatQ",
            "vrev",
        ],
        only: None,
        class: PURE,
    },
    // A processor-mode switch re-banks the registers `ARM_MODE_SWITCH`
    // names, so the frame the next instruction sees is not the one that flowed
    // in.  Arch-specific because `r8`..`lr` are ARM-32 register names, and
    // `ARM.sinc` is the only vendored spec declaring these pcodeops.
    Row {
        names: &[
            "setAbortMode",
            "setFIQMode",
            "setIRQMode",
            "setMonitorMode",
            "setProcessorMode",
            "setSupervisorMode",
            "setSystemMode",
            "setUndefinedMode",
            "setUserMode",
        ],
        only: None,
        class: ARM_MODE_SWITCH,
    },
    // Declared by `ARMTHUMBinstructions.sinc` alone, so also ARM-32 only.
    Row {
        names: &["setStackMode"],
        only: None,
        class: ARM_STACK_SWITCH,
    },
];

/// ARM A-profile processor-mode switch: every exception mode banks R13, R14
/// and the SPSR, and FIQ also banks R8-R12 (ARM ARM DDI 0406C B1.3.2), so the
/// values live across one are not the ones that came in.  `ARM.sinc` defines
/// `r13_svc` / `r14_svc` and uses them nowhere, so no banking is modelled and
/// this list is the model for every op that changes mode: `cps` and an `msr`
/// writing the CPSR's control field (`setProcessorMode`). The register form
/// raises it only for a mode
/// [`is_msr_register_mode`](crate::call_other_abi::is_msr_register_mode)'s op
/// yields, which under the `msr_register_keeps_mode` claim is never a new one.
///
/// The FIQ set is applied to every op, not to `setFIQMode` alone: the switch
/// OUT of FIQ un-banks R8-R12 just as the switch in banks them, and the
/// incoming mode is not something the analyser knows.
const ARM_MODE_SWITCH: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["r8", "r9", "r10", "r11", "r12", "sp", "lr", "spsr"],
    clobbers_memory: false,
    no_return: false,
});

/// Thumb `msr control, Rn` selecting MSP or PSP: `sp` alone re-banks.
const ARM_STACK_SWITCH: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["sp"],
    clobbers_memory: false,
    no_return: false,
});

/// Prefix families:
///
/// * `Vector*` (ARM-32 NEON): every member is register-to-register compute,
///   with no `VectorLoad`/`VectorStore` (memory NEON is a separate pcode
///   Load/Store), so the family is `PURE`.  AArch64 declares no `Vector*`
///   user-op and spells its NEON ops `NEON_*`, but PowerPC's `SPE_APU.sinc`
///   declares dozens of unrelated `Vector*` ops that the arch scoping keeps
///   out.
/// * `Float*` (ARM-32 NEON): the float half of the same `ARMneon.sinc` group
///   (`FloatVectorAdd`, `FloatCompareGT`, `FloatToSignedRound`, ...), every
///   member `Xd = op(Xn, Xm, esize)` over listed registers.  `ARMneon.sinc`
///   names no `[ram]` address at all, so the family is `PURE`.  Arch-scoped
///   because PowerPC (`FloatingRoundToIntegerTowardZero1`) and x86
///   (`FloatingReciprocalAprox`) spell unrelated ops with the same stem.
const PREFIXES: &[Prefix] = &[Prefix {
    prefixes: &["Vector", "Float"],
    class: PURE,
}];

/// The coprocessor families, which no stem separates:
///
/// * `coproc_movefrom_*` / `coproc_moveto_*` (ARM cp15): NOT homogeneous by
///   direction, since GHIDRA names cache / TLB / barrier / WFI maintenance ops
///   `coproc_movefrom_X` too (`coproc_movefrom_Data_Memory_Barrier`,
///   `coproc_movefrom_Clean_Data_Cache_by_MVA`).  Classified by OPERATION
///   instead: a cache / barrier / sync / invalidate / flush / wait op is a
///   memory-ordering side effect, a plain system-register move is `PURE`.
/// * `coprocessor_*` (ARM generic cp): `movefrom*` is a register read, while
///   the generic `moveto` / `load` / `store` / `function` ops target an
///   unknown coprocessor and may touch RAM.
fn coproc(name: &str) -> Option<CallOtherClass> {
    // Classified by operation, not direction.  `TLB_Type` is a pure ID
    // read (the real TLB ops are all named `Invalidate_*`); its sibling
    // `Cache_Type` is not, the `Cache` key below sweeping it into
    // MEM_CLOBBER.  Conservative, so sound but imprecise.
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
        // read of the same register is pure.  FCSE_PID remaps every VA
        // below 32MB, and the secure-world writes reselect the address
        // space.  `Peripheral` covers both the port remaps that move a
        // peripheral window (the sla spells that one two ways) and
        // `coproc_moveto_Peripheral_System`, which is `mcr p15,*,*,c15,*`:
        // the whole implementation-defined c15 bank, holding the port
        // remap, cache/TLB debug ops and L2 control on the parts that have
        // them.
        const WRITE_CONTROL_KEYS: &[&str] = &[
            "Translation_table",
            "Control",
            "Context_ID",
            "Domain_Access",
            "FCSE",
            "Peripheral",
            "Remap",
            "Secure",
            "Security",
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
    None
}
