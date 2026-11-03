use super::{DecodeFacts, Family};
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Prefix, Row};

pub(crate) static AARCH64: Family = Family {
    name: "aarch64",
    facts: DecodeFacts {
        isa_mode_var: None,
        flow_commit_vars: &[],
        transient_decode_vars: &[],
        has_delay_slots: false,
        callother_output_exact: false,
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
        // The kernel's `current`: only a context switch writes it, and a task
        // resumes with its own, so two reads in one function are one value.
        non_volatile_registers: &["sp_el0"],
    },
    rounding_ops: &["fpcrRoundingControl"],
    msr_mode_op: None,
    rounding_register: Some(("fpcrRoundingControl", "fpcr")),
    call_other: CALL_OTHER,
    call_other_prefixes: PREFIXES,
    call_other_fallback: None,
};

const CALL_OTHER: &[Row] = &[
    // Linux AArch64 SVC: w8 = syscall number, x0..x5 = args, x0 = return.  The
    // number is read as the whole `x8` container, which is what the lifter's
    // tracked-varnode set carries.  `AARCH64base.sinc:6489` raises
    // `CallSupervisor(imm16:2)` with the vector immediate as its only operand,
    // so the register ABI is entirely implicit here.  Unlike x86-64 SYSCALL,
    // the SVC instruction itself clobbers no register.
    Row {
        names: &["CallSupervisor"],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["x8", "x0", "x1", "x2", "x3", "x4", "x5"],
            implicit_writes: &["x0"],
            // A kernel entry can read or write any user memory, the user stack
            // frame included.
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // ARM SMCCC for HVC (CallHyperVisor) and SMC (CallSecureMonitor): x0..x7
    // in, x0..x3 out, shared by LE and BE aarch64.  Arch-specific because
    // `x0..x7` only resolve on aarch64's register table; arm-32 has `r0..r12`.
    Row {
        names: &["CallHyperVisor", "CallSecureMonitor"],
        only: None,
        class: CallOtherClass::Call(CallOtherAbi {
            implicit_reads: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            // x0..x3 carry the SMCCC result. Under SMCCC 1.0 the callee may
            // leave x4..x17 in an unpredictable state (1.1+ preserves them);
            // the version isn't statically known, so clobber them conservatively
            // rather than forward a stale value across the SMC/HVC.
            implicit_writes: &[
                "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12",
                "x13", "x14", "x15", "x16", "x17",
            ],
            // Everything passes through the register channel; the spec forbids
            // mutating the caller's stack frame, but a firing SMC/HVC may touch
            // memory, so keep it on the memory chain.
            clobbers_memory: true,
            no_return: false,
        }),
    },
    // AArch64 TBL / TBX (`AARCH64neon.sinc:24646-24757`): a table lookup
    // across up to four vector REGISTERS, every one a listed operand
    // (`Rd_VPR128.16B = a64_TBL(tblx, Rn_VPR128.16B, ..., Rm_VPR128.16B)`).
    // The name is aarch64's alone, so it is scoped rather than sharing the
    // `NEON_` family below.
    Row {
        names: &["a64_TBL"],
        only: None,
        class: PURE,
    },
    // AArch64 SVE LDR / STR of a Z or P register (`AARCH64sve.sinc:4553`,
    // `:4562`, `:6593`, `:6602`).  The sla passes the BASE REGISTER as a
    // value (`Zd = SVE_ldr(Zd, Rn_GPR64xsp, imm)`, `SVE_str(Zd,
    // Rn_GPR64xsp, imm)`), NOT a dynamic memory varnode, so no p-code Load or
    // Store is emitted and the access is entirely implicit.  The `SVE_ld*`,
    // `SVE_st*` and `SVE_prf*` user-ops have that shape (`SVE_ld1b` at
    // `:2702` is structurally identical to `SVE_ldr`), which is why `SVE_` is
    // not a prefix family: the rest are register compute.  These two are
    // listed because they are the ones seen;
    // an unlisted name fails the lift rather than forwarding across a hidden
    // access.
    Row {
        names: &["SVE_ldr", "SVE_str"],
        only: None,
        class: MEM_CLOBBER,
    },
];

/// Prefix families:
///
/// * `NEON_*` (AArch64): all 159 `AARCH64instructions.sinc` declares, used
///   throughout `AARCH64neon.sinc`, are arithmetic / crypto / permute compute
///   with listed register operands; not one is a load, store, prefetch, or
///   barrier (NEON `LD1` / `ST1` lift to real p-code Load / Store), so the
///   family is `PURE`.
/// * `TLBI_*` / `DC_*` / `IC_*` (AArch64): every member is a TLB-invalidate or
///   cache-maintenance system op, so `MEM_CLOBBER`.
/// * `SVE_*` (AArch64): NOT homogeneous.  74 of the 315 declares (`SVE_ld*`,
///   `SVE_st*`, `SVE_prf*`, `SVE_ldr` / `SVE_str` among them) take a base
///   register rather than a dynamic memory varnode, so their access is
///   implicit; a `PURE` family here would make every SVE load and store
///   forwardable.  Memory-shaped names get individual `MEM_CLOBBER` rows and
///   the rest stay individual too.
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
