use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Prefix, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};

pub(crate) static ARM32: Family = Family {
    #[cfg(any(test, feature = "test-util"))]
    name: "arm32",
    #[cfg(any(test, feature = "test-util"))]
    ghidra_ldefs: "ARM/data/languages/ARM.ldefs",
    facts: DecodeFacts {
        isa_mode_var: Some("TMode"),
        flow_commit_vars: &["condit", "TEEMode"],
        // Not `condit`: `flow_commit_vars` re-derives it.
        transient_decode_vars: &["LRset", "REToverride", "CALLoverride"],
        has_delay_slots: false,
        callother_output_exact: false,
        // Every `*[register]` access is NEON: `&Dreg` (d0..d31, 0x300..0x400)
        // plus or minus `regInc * 24` (regInc <= 2), plus a lane offset under 8.
        register_space_reach: Some(0x2d0..0x430),
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
    calling_conventions: CALLING_CONVENTIONS,
};

const CALL_OTHER: &[Row] = &[
    // Linux SVC: r7 = number, r0..r6 = args, r0 = return.
    Row::new(
        &["software_interrupt"],
        CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
            implicit_writes: &["r0"],
            clobbers_memory: true,
            no_return: false,
        }),
    ),
    // SMCCC HVC / SMC (the sla's `software_hvc` / `software_smc`, no output):
    // r0..r7 in and out (r4..r7 carry results from SMCCC 1.2); r8..r14 are
    // preserved.
    Row::new(
        &["software_hvc", "software_smc"],
        CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"],
            implicit_writes: &["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"],
            clobbers_memory: true,
            no_return: false,
        }),
    ),
    // NEON register compute outside the `Vector*` / `Float*` families.
    Row::new(
        &["SHA256ScheduleUpdate0", "SHA256ScheduleUpdate1", "vrev"],
        PURE,
    ),
    // NEON saturation, which sets FPSCR.QC.
    Row::new(
        &["SatQ", "SignedSatQ"],
        CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["fpscr"],
            implicit_writes: &["fpscr"],
            clobbers_memory: false,
            no_return: false,
        }),
    ),
    Row::new(
        &[
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
        ARM_MODE_SWITCH,
    ),
    Row::new(&["setStackMode"], ARM_STACK_SWITCH),
];

/// A processor-mode switch (`cps`, `msr` to the CPSR control field): every
/// exception mode banks R13, R14 and the SPSR, FIQ also R8-R12, and the sla
/// models no banking. The FIQ set applies to every switch, since leaving FIQ
/// un-banks it and the incoming mode is unknown.
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

/// NEON `Vector*` / `Float*`: register compute over listed operands
/// (`ARMneon.sinc` names no RAM; NEON loads and stores are p-code).
const PREFIXES: &[Prefix] = &[Prefix {
    prefixes: &["Vector", "Float"],
    class: PURE,
}];

/// cp15 `coproc_movefrom_*` / `coproc_moveto_*` by operation, not direction
/// (maintenance ops use both stems): maintenance and translation-control
/// writes clobber memory, other register moves are pure. Generic
/// `coprocessor_*` ops other than reads may touch RAM.
fn coproc(name: &str) -> Option<CallOtherClass> {
    if name.starts_with("coproc_movefrom_") || name.starts_with("coproc_moveto_") {
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
        // Writes that change the translation view: TTBR, SCTLR, CONTEXTIDR,
        // DACR, FCSE_PID, secure-world selection, the peripheral remaps and
        // the implementation-defined c15 bank.
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

/// Every ARM32 preset; AAPCS is byte-order independent.
const ARM32_PRESETS: &[ArchPreset] = &[
    ArchPreset::Arm,
    ArchPreset::ArmBe,
    ArchPreset::ArmBeKernel,
    ArchPreset::ArmThumb,
];

const CALLING_CONVENTIONS: &[CcPreset] = &[
    CcPreset {
        name: "arm_aapcs",
        cc: CallingConvention::arm_aapcs(),
        arches: ARM32_PRESETS,
        ghidra: &[("ARM/data/languages/ARM.cspec", None, ARM32_PRESETS)],
    },
    // Soft-float is Ghidra's `__stdcall_softfp`.
    CcPreset {
        name: "arm_aapcs_soft",
        cc: CallingConvention::arm_aapcs_soft(),
        arches: ARM32_PRESETS,
        ghidra: &[(
            "ARM/data/languages/ARM.cspec",
            Some("__stdcall_softfp"),
            &[ArchPreset::Arm],
        )],
    },
];

impl CallingConvention {
    /// The ARM 32-bit AAPCS hard-float (VFP) calling convention.
    #[must_use]
    pub const fn arm_aapcs() -> Self {
        CallingConvention {
            stack_ptr: "sp",
            arg_passing_regs: &["r0", "r1", "r2", "r3"],
            // The double carriers. A `float` argument n is in `s_n` (inside
            // `d_{n/2}`), which the IR cannot tell without the callee's signature, so
            // past position 0 a float slot is a candidate, not an answer.
            arg_passing_regs_float: &["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"],
            callee_saved_regs: &[
                "r4",
                "r5",
                "r6",
                "r7",
                "r8",
                "r9",
                "r10",
                "r11",
                "lr",
                "d8",
                "d9",
                "d10",
                "d11",
                "d12",
                "d13",
                "d14",
                "d15",
                // CPSR.M (a fork register): `sp` is banked per mode and preserved,
                // so a callee returns in its caller's mode.
                "cpsr_mode",
            ],
            ret_val_regs: &["r0", "r1"],
            // A homogeneous aggregate has up to four members, q0-q3 for vectors.
            ret_val_regs_float: &["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
            link_register: Some("lr"),
            preserves_memory: false,
            preserves_all_registers: false,
            entry_address: None,
            reg_args_adjoin_stack_args: true,
        }
    }

    /// The ARM 32-bit AAPCS calling convention for a
    /// `-mfloat-abi=soft` / `softfp` binary.
    #[must_use]
    pub const fn arm_aapcs_soft() -> Self {
        // Floats travel in core registers; d8-d15 stay callee-saved. A
        // 16-byte vector returns in r0..r3; only r0:r1 are listed, as a
        // soft-float kernel would otherwise return r2:r3 everywhere.
        CallingConvention {
            arg_passing_regs_float: &[],
            ret_val_regs_float: &[],
            ..Self::arm_aapcs()
        }
    }
}
