use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Prefix, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};

pub(crate) static AARCH64: Family = Family {
    name: "aarch64",
    ghidra_ldefs: "AARCH64/data/languages/AARCH64.ldefs",
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
    calling_conventions: CALLING_CONVENTIONS,
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
    /// Returns the AArch64 AAPCS64 calling convention.
    #[must_use]
    pub const fn aarch64_aapcs64() -> Self {
        // AArch64 AAPCS64.  `ret_stack_pop` is 0 because `bl` writes the return
        // address to `lr` rather than pushing it.  Register conventions are
        // byte-order independent, so this pairs with both `aarch64` and
        // `aarch64be`.
        //
        // AAPCS64 6.9's indirect result location register `x8` is NOT listed: it
        // carries the destination address for a large aggregate return, not an
        // argument, and listing it would renumber every `arg(n)`.  The cost is that
        // a caller passing a stack slot there has an escape `frame_escape` cannot
        // see, so `assumptions.escape_analysis` (off by default) could forward a
        // spill across such a call.  `x8` is still a `Call` clobber, so the
        // register itself is never stale.
        CallingConvention {
            stack_ptr: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            // AAPCS64 6.4.1: SIMD/FP arguments in v0..v7, listed in their
            // widest form so an s/d/q access slices out of one container.
            arg_passing_regs_float: &["q0", "q1", "q2", "q3", "q4", "q5", "q6", "q7"],
            callee_saved_regs: &[
                // AAPCS64 6.1.1 makes x30 (LR) volatile, not callee-saved. It
                // is listed because a conforming callee restores it before
                // returning, which is what a caller's view needs: treating it
                // as clobbered would lose the return address across every
                // call. The same choice is documented at the PPC and MIPS
                // presets.
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
                // AAPCS64 6.1.2 preserves only the LOW 64 bits of v8-v15, so
                // these name the `d` views; naming `q8`-`q15` would claim the
                // upper halves preserved too.
                "d8", "d9", "d10", "d11", "d12", "d13", "d14", "d15",
            ],
            ret_val_regs: &["x0", "x1"],
            // v0..v3, the widest form of the s/d/q sub-registers: a
            // homogeneous float aggregate returns up to four members.
            ret_val_regs_float: &["q0", "q1", "q2", "q3"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
            // `lr` is an alias for `x30`, and Sleigh's aarch64 table only
            // registers `x30`.
            link_register: Some("x30"),
            preserves_memory: false,
            preserves_all_registers: false,
            entry_address: None,
            reg_args_adjoin_stack_args: false,
        }
    }
}
