use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Prefix, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};

pub(crate) static AARCH64: Family = Family {
    #[cfg(any(test, feature = "test-util"))]
    name: "aarch64",
    #[cfg(any(test, feature = "test-util"))]
    ghidra_ldefs: "AARCH64/data/languages/AARCH64.ldefs",
    facts: DecodeFacts {
        isa_mode_var: None,
        flow_commit_vars: &[],
        transient_decode_vars: &[],
        has_delay_slots: false,
        callother_output_exact: false,
        register_space_reach: None,
        internal_registers: &[
            "tmp_ldWn",
            "tmp_ldXn",
            "tmp_stWn",
            "tmp_stXn",
            "VecMemAddr",
            "VectorSelem",
            "VecRegAddr",
            "glob_mask32",
            "glob_mask64",
            "TMPZ1",
            "TMPZ2",
            "TMPZ3",
            "TMPZ4",
            "TMPZ5",
            "TMPZ6",
            "tmpCY",
            "tmpOV",
            "tmpNG",
            "tmpZR",
        ],
        volatile_registers: &[],
        // The kernel's `current`: every writer a call can reach restores the
        // caller's task before returning, so two reads in one function are
        // one value.
        non_volatile_registers: &["sp_el0"],
    },
    rounding_ops: &["fpcrRoundingControl"],
    msr_mode_op: None,
    rounding_register: Some(("fpcrRoundingControl", "fpcr")),
    call_other: CALL_OTHER,
    call_other_prefixes: PREFIXES,
    call_other_fallback: None,
    calling_conventions: CALLING_CONVENTIONS,
};

const CALL_OTHER: &[Row] = &[
    // Linux SVC: w8 = number (read as `x8`), x0..x5 = args, x0 = return.
    // `CallSupervisor(imm16)` takes only the immediate.
    Row::new(
        &["CallSupervisor"],
        CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["x8", "x0", "x1", "x2", "x3", "x4", "x5"],
            implicit_writes: &["x0"],
            clobbers_memory: true,
            no_return: false,
        }),
    ),
    // SMCCC HVC / SMC: x0..x7 in, x0..x3 out.
    Row::new(
        &["CallHyperVisor", "CallSecureMonitor"],
        CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            // SMCCC 1.0 may also leave x4..x17 unpredictable.
            implicit_writes: &[
                "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12",
                "x13", "x14", "x15", "x16", "x17",
            ],
            clobbers_memory: true,
            no_return: false,
        }),
    ),
    // TBL / TBX (constructors `tbl` / `tbx`): every table register is a listed
    // operand.
    Row::new(&["a64_TBL"], PURE),
    // `SIMD_PIECE(reg, lane)` reads one lane of a listed operand.
    Row::new(&["SIMD_PIECE"], PURE),
    // SVE LDR / STR take the base register as a value, so their access has no
    // p-code. Other `SVE_ld*` / `SVE_st*` / `SVE_prf*` ops share the shape and
    // stay unclassified, failing the lift.
    Row::new(&["SVE_ldr", "SVE_str"], MEM_CLOBBER),
];

/// `TLBI_` / `DC_` / `IC_` are TLB and cache maintenance; every `NEON_` op is
/// register compute (NEON loads and stores are p-code). `SVE_` is not a
/// family: 74 of its 315 ops access memory the p-code leaves out.
const PREFIXES: &[Prefix] = &[
    Prefix {
        prefixes: &["TLBI_", "DC_", "IC_"],
        class: MEM_CLOBBER,
    },
    Prefix {
        prefixes: &["NEON_"],
        class: PURE,
    },
];

const CALLING_CONVENTIONS: &[CcPreset] = &[CcPreset {
    name: "aarch64_aapcs64",
    cc: CallingConvention::aarch64_aapcs64(),
    arches: &[ArchPreset::Aarch64, ArchPreset::Aarch64Be],
    ghidra: &[(
        "AARCH64/data/languages/AARCH64.cspec",
        None,
        &[ArchPreset::Aarch64, ArchPreset::Aarch64Be],
    )],
}];

impl CallingConvention {
    /// The AArch64 AAPCS64 calling convention.
    #[must_use]
    pub const fn aarch64_aapcs64() -> Self {
        // `x8` (indirect result) is not an argument; a stack address in it
        // escapes as a clobbered register.
        CallingConvention {
            stack_ptr: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            // v0..v7 in their widest form.
            arg_passing_regs_float: &["q0", "q1", "q2", "q3", "q4", "q5", "q6", "q7"],
            callee_saved_regs: &[
                // x30 (LR) is volatile, but a callee restores it before
                // returning, so it is listed to keep the return address.
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
                // sp_el0 is the kernel's `current`: a callee returns to the same
                // task. Only the low 64 bits of v8-v15 are preserved.
                "sp_el0", "d8", "d9", "d10", "d11", "d12", "d13", "d14", "d15",
            ],
            ret_val_regs: &["x0", "x1"],
            // v0..v3: a homogeneous float aggregate has up to four members.
            ret_val_regs_float: &["q0", "q1", "q2", "q3"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
            // The sla names `lr` only as `x30`.
            link_register: Some("x30"),
            preserves_memory: false,
            preserves_all_registers: false,
            entry_address: None,
            reg_args_adjoin_stack_args: false,
        }
    }
}
