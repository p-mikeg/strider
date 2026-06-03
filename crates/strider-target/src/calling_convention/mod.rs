use anyhow::anyhow;

use crate::Result;

/// Resolves a single Sleigh register name to its [`rsleigh::Vn`], or returns
/// an error if the name is not known.  Single source of truth for the
/// name-to-varnode error path.
pub(crate) fn vn_for_name(sleigh_regs: &rsleigh::SleighRegs, name: &str) -> Result<rsleigh::Vn> {
    sleigh_regs
        .name_to_vn(name)
        .ok_or_else(|| anyhow!("unknown sleigh register name {name:?}"))
}

/// Resolves a slice of Sleigh register names to varnodes in the same order.
/// Short-circuits on the first unknown name.
pub(crate) fn regs_to_vns(sleigh_regs: &rsleigh::SleighRegs, reg_names: &[&str]) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|&name| vn_for_name(sleigh_regs, name))
        .collect()
}

/// A calling convention definition expressed as static string slices of
/// register names.
///
/// Call [`CallingConvention::build`] to resolve the names to concrete varnodes
/// using a Sleigh register table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallingConvention {
    /// The Sleigh register name of the hardware stack pointer.  Stored on
    /// the convention because every built convention needs the stack
    /// pointer's `Vn` resolved, and the `SleighArch` that would otherwise
    /// own this fact is already passed separately to `Strider::new`.
    stack_ptr_reg_name: &'static str,
    /// Sleigh register names for the ABI's argument-passing registers, in
    /// positional order.  Resolved into
    /// [`BuiltCallingConvention::arg_passing_regs`] by [`Self::build`].
    arg_passing_regs: &'static [&'static str],
    /// Sleigh register names for registers the callee must preserve across
    /// the call.  Resolved into [`BuiltCallingConvention::callee_saved_regs`]
    /// by [`Self::build`].  Excludes the stack pointer; SP's cross-call
    /// behaviour is expressed through [`Self::ret_stack_pop`].
    callee_saved_regs: &'static [&'static str],
    /// Sleigh register names for return-value registers, in positional order.
    /// Resolved into [`BuiltCallingConvention::ret_val_regs`] by
    /// [`Self::build`].
    ret_val_regs: &'static [&'static str],
    /// Sleigh register names for float return-value registers (e.g. `q0` on
    /// aarch64, `XMM0` on x86_64, `d0` on ARM AAPCS soft-float, `f0` on
    /// MIPS O32).  Listed separately from [`Self::ret_val_regs`] because
    /// these registers have different widths from the integer return regs
    /// (and the size invariant in the test suite checks the integer list).
    /// Resolved into [`BuiltCallingConvention::ret_val_regs_float`] by
    /// [`Self::build`].
    ret_val_regs_float: &'static [&'static str],
    /// Byte offsets from the call-time stack pointer for each positional
    /// stack argument.  Entry `i` is the offset for the `i`-th stack arg
    /// (after register arguments are exhausted).
    stack_arg_offsets: &'static [i64],
    /// Net byte change the callee's `ret` inflicts on the caller's stack
    /// pointer.  On stack-push ISAs (x86, x86_64) `ret` pops the return
    /// address, so this equals the pointer size (4 / 8).  On link-register
    /// ISAs (ARM, AArch64, MIPS, PowerPC) the call does not touch SP, so
    /// this is 0.
    ret_stack_pop: i64,
    /// The Sleigh register name of the calling convention's link register
    /// (the register that holds the return address across a call), or
    /// `None` on stack-push ISAs (x86, x86_64) where the return address
    /// lives on the stack instead.  Resolved into
    /// [`BuiltCallingConvention::link_register_vn`] by [`Self::build`].
    /// Used by the indirect-branch resolver to recognise `bx lr` /
    /// `pop {pc}` / `jr ra` shapes as `Return`.
    link_register_reg_name: Option<&'static str>,
    /// `true` if calls under this convention preserve **all** observable
    /// state, including memory.  When set, `strider_ir::FunctionBuilder::build_call`
    /// skips emitting a Memory output on the resulting Call node and does not
    /// advance the region's memory chain — so passes like `LoadReadOnly` and
    /// `LoadForward` can forward loads across the call.
    ///
    /// `false` for every standard ABI; `true` only on
    /// [`Self::x86_64_all_preserving`] and analogous "transparent hook"
    /// presets (e.g. Linux-kernel `__fentry__` / `mcount` callbacks that
    /// preserve all caller state).
    preserves_memory: bool,
}

/// A calling convention whose register names have been resolved to concrete
/// [`rsleigh::Vn`] varnodes.
///
/// Produced by [`CallingConvention::build`] (canonical path) or
/// [`Self::try_new`] (test/override construction).  Fields are `pub`
/// because the type is immutable post-construction — the validating
/// constructor [`Self::try_new`] checks every ABI invariant, and there
/// is no mutating API.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    /// Argument-passing register varnodes, in positional order.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Callee-saved register varnodes.  Excludes the stack pointer; SP's
    /// callee-side preservation is expressed through [`Self::ret_stack_pop`].
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    /// Integer return-value register varnodes, in positional order.
    pub ret_val_regs: Vec<rsleigh::Vn>,
    /// Float return-value register varnodes (e.g. `[q0, q1]` on AArch64,
    /// `[XMM0, XMM1]` on x86_64).  Tracked separately from
    /// [`Self::ret_val_regs`] because their widths differ.
    pub ret_val_regs_float: Vec<rsleigh::Vn>,
    /// Hardware stack-pointer varnode.  Deliberately absent from the three
    /// register-list fields above — SP's cross-call behaviour is expressed
    /// through [`Self::ret_stack_pop`] instead.
    pub stack_vn: rsleigh::Vn,
    /// Byte offsets from the call-time SP for each positional stack arg.
    pub stack_arg_offsets: Vec<i64>,
    /// Net byte change the callee's `ret` inflicts on the caller's SP.
    /// `8` on x86_64 (pops return address); `0` on link-register ISAs.
    pub ret_stack_pop: i64,
    /// Link-register varnode on link-register ISAs (ARM, AArch64, MIPS,
    /// PowerPC); `None` on stack-push ISAs (x86, x86_64).  Consumed by the
    /// indirect-branch resolver to classify return-shaped indirect branches.
    pub link_register_vn: Option<rsleigh::Vn>,
    /// `true` when calls under this CC preserve memory (zero-side-effect
    /// hooks like `__fentry__` / `mcount`).  Consumed by the IR builder's
    /// `build_call` to suppress the Call's Memory output so
    /// `LoadReadOnly` / `LoadForward` can forward across the call.
    pub preserves_memory: bool,
}

/// The trivial / synthetic calling convention: no real ABI.
///
/// Every [`crate::BuiltCallingConvention`]-bearing type (notably
/// `strider_ir::Function`) requires a convention, but synthetic / mock
/// graphs constructed in tests have no real target ABI.  This `Default`
/// is what they get: empty register lists, no stack arguments,
/// `ret_stack_pop = 0`, `preserves_memory = false`, no link register,
/// and a **synthetic `stack_vn`** that is a real, sized register matching
/// no real machine register.
///
/// The synthetic SP is an 8-byte REGISTER-space varnode at the
/// out-of-range offset [`SYNTHETIC_STACK_VN_OFFSET`].  It is a *real*
/// sized register (unlike the former zero-sized const sentinel) so a
/// `Call` built under the trivial CC can mint a well-typed
/// `InitialVar(stack_vn)` SP anchor — a `Call` always requires a real
/// stack pointer.  The offset is far outside any architecture's register
/// file, so it never collides with a tracked register: stack analyses
/// (`StackOffsetDetect`, `LoadForward`) still find no matches against it
/// on a trivial-CC function (which is the correct "no modelled stack"
/// behaviour), and the SP-exclusion-from-clobbers filter
/// (`*v != stack_vn`) never spuriously drops a real tracked register.
impl Default for BuiltCallingConvention {
    fn default() -> Self {
        Self {
            arg_passing_regs: Vec::new(),
            callee_saved_regs: Vec::new(),
            ret_val_regs: Vec::new(),
            ret_val_regs_float: Vec::new(),
            stack_vn: rsleigh::Vn {
                addr_off: SYNTHETIC_STACK_VN_OFFSET,
                addr_space: rsleigh::VnSpace::REGISTER,
                size: 8,
            },
            stack_arg_offsets: Vec::new(),
            ret_stack_pop: 0,
            link_register_vn: None,
            preserves_memory: false,
        }
    }
}

/// REGISTER-space offset of the synthetic stack-pointer varnode minted by
/// [`BuiltCallingConvention::default`].  Chosen far outside any real
/// architecture's register file so it never aliases a tracked register.
pub const SYNTHETIC_STACK_VN_OFFSET: u64 = 0xFFFF_FFFF_FFFF_0000;

impl BuiltCallingConvention {
    /// Validating constructor.  Builds a
    /// `BuiltCallingConvention` from explicit fields and checks the
    /// canonical ABI invariants:
    ///
    /// - `arg_passing_regs ∩ callee_saved_regs == ∅`
    /// - `ret_val_regs ∩ callee_saved_regs == ∅`
    /// - `ret_val_regs_float ∩ callee_saved_regs == ∅`
    /// - `stack_vn` is not in any of the four register lists
    /// - No duplicates within any single list
    /// - When `link_register_vn` is `Some`, it must be present in
    ///   `callee_saved_regs` (CLAUDE.md "Note (link-register
    ///   handling)" deliberate tradeoff)
    /// - `ret_stack_pop` is non-negative
    ///
    /// # Errors
    ///
    /// Returns `Err` describing the first invariant violation
    /// detected.  The error is intentionally specific so a CC author
    /// debugging a typo (e.g. listing the same Vn in both
    /// `arg_passing_regs` and `callee_saved_regs`) sees the offending
    /// names rather than a downstream miscompile.
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        arg_passing_regs: Vec<rsleigh::Vn>,
        callee_saved_regs: Vec<rsleigh::Vn>,
        ret_val_regs: Vec<rsleigh::Vn>,
        ret_val_regs_float: Vec<rsleigh::Vn>,
        stack_vn: rsleigh::Vn,
        stack_arg_offsets: Vec<i64>,
        ret_stack_pop: i64,
        link_register_vn: Option<rsleigh::Vn>,
        preserves_memory: bool,
    ) -> std::result::Result<Self, anyhow::Error> {
        // Disjointness: arg-passing must not overlap callee-saved.
        for vn in &arg_passing_regs {
            if callee_saved_regs.contains(vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {:?} appears in both \
                     arg_passing_regs and callee_saved_regs (a single varnode \
                     cannot be both caller-supplied and callee-preserved)",
                    vn,
                ));
            }
        }
        // Ret-val regs must not overlap callee-saved (the callee writes
        // them to deliver results — they cannot be required-preserved).
        for vn in ret_val_regs.iter().chain(ret_val_regs_float.iter()) {
            if callee_saved_regs.contains(vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {:?} appears in both \
                     ret_val_regs/ret_val_regs_float and callee_saved_regs",
                    vn,
                ));
            }
        }
        // Per-list checks: SP-not-present + within-list uniqueness.
        // Walked in one pass over the four named lists.
        for (list_name, list) in [
            ("arg_passing_regs", &arg_passing_regs),
            ("callee_saved_regs", &callee_saved_regs),
            ("ret_val_regs", &ret_val_regs),
            ("ret_val_regs_float", &ret_val_regs_float),
        ] {
            if list.contains(&stack_vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack_vn {:?} appears in {} \
                     (the SP is implicit and must not be in any reg list)",
                    stack_vn,
                    list_name,
                ));
            }
            for (i, vn) in list.iter().enumerate() {
                if list[i + 1..].contains(vn) {
                    return Err(anyhow::anyhow!(
                        "BuiltCallingConvention: duplicate varnode {:?} in {}",
                        vn,
                        list_name,
                    ));
                }
            }
        }
        // Link-register-as-callee-saved invariant (CLAUDE.md note).
        if let Some(lr) = link_register_vn
            && !callee_saved_regs.contains(&lr)
        {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: link_register_vn {:?} must also \
                 be present in callee_saved_regs (CLAUDE.md deliberate \
                 tradeoff so InitialVar(lr) propagates through call sites)",
                lr,
            ));
        }
        // ret_stack_pop is non-negative (a negative value would mean the
        // callee's `ret` *grew* the stack, which no real ABI does).
        if ret_stack_pop < 0 {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: ret_stack_pop must be >= 0, got {}",
                ret_stack_pop,
            ));
        }
        Ok(Self {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_vn,
            stack_arg_offsets,
            ret_stack_pop,
            link_register_vn,
            preserves_memory,
        })
    }

    /// Predicate: is `var` clobbered by a call under THIS CC when used
    /// as an override on a function whose stack pointer is
    /// `function_stack_vn`?
    ///
    /// A variable is clobbered iff it's neither in this CC's
    /// `callee_saved_regs` nor the function's stack pointer.  The
    /// stack pointer is treated specially because its cross-call
    /// behaviour is expressed through `ret_stack_pop`, not the
    /// caller-/callee-saved partition.
    ///
    /// This is the single source of truth for the override-clobber
    /// projection — used by both `FunctionBuilder::build_call` and the
    /// orchestrator's in-place tail-call edit (via
    /// `AnchorCallingContext::for_anchor` and `apply_in_place_edit`).
    #[must_use]
    pub fn clobbers_override_var(
        &self,
        var: &rsleigh::Vn,
        function_stack_vn: rsleigh::Vn,
    ) -> bool {
        !self.callee_saved_regs.contains(var) && *var != function_stack_vn
    }
}

/// One positional argument slot in a calling convention, in ABI order.
///
/// `index` is the canonical positional argument index recorded in
/// `Function::arg_index_to_values` (in `strider-ir`).  Register slots
/// come first (indices `0..arg_passing_regs.len()`), followed by
/// stack slots (indices `arg_passing_regs.len()..`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PositionalArg {
    /// Argument passed in `vn` at positional index `index`.
    Register {
        /// Canonical positional argument index.
        index: u32,
        /// The register varnode the caller writes the argument into.
        vn: rsleigh::Vn,
    },
    /// Argument passed at byte `offset` from the call-time SP at
    /// positional index `index`.
    Stack {
        /// Canonical positional argument index.
        index: u32,
        /// Byte offset from the call-time stack pointer.
        offset: i64,
    },
}

/// Positional argument slots of a calling convention, enumerated in
/// ABI order.
///
/// Single source of truth for "what is positional argument `i`?" — the
/// register-arg list and stack-arg-offset list are walked once at
/// construction time and stamped with canonical indices.  Consumers
/// that previously hand-projected `arg_passing_regs` + derived
/// `first_stack_arg = arg_passing_regs.len()` go through
/// [`Self::register_args`] / [`Self::stack_args`] /
/// [`Self::first_stack_index`] instead, so the index numbering can
/// never drift across consumers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PositionalArgLayout {
    /// Positional argument slots in ABI order.  Indices `0..` are
    /// register slots (matching `arg_passing_regs`), followed by stack
    /// slots (matching `stack_arg_offsets`).
    pub(crate) entries: Vec<PositionalArg>,
}

impl PositionalArgLayout {
    /// Walks `cc.arg_passing_regs` and `cc.stack_arg_offsets` once and
    /// stamps each slot with its canonical positional index.
    #[must_use]
    pub fn from_convention(cc: &BuiltCallingConvention) -> Self {
        let mut entries = Vec::with_capacity(cc.arg_passing_regs.len() + cc.stack_arg_offsets.len());
        for (i, vn) in cc.arg_passing_regs.iter().enumerate() {
            entries.push(PositionalArg::Register {
                index: i as u32,
                vn: *vn,
            });
        }
        let first_stack = cc.arg_passing_regs.len();
        for (j, &offset) in cc.stack_arg_offsets.iter().enumerate() {
            entries.push(PositionalArg::Stack {
                index: (first_stack + j) as u32,
                offset,
            });
        }
        Self { entries }
    }

    /// Iterates `(index, vn)` over the register-passed argument slots
    /// in ABI order.  Indices are dense, starting at `0`.
    pub fn register_args(&self) -> impl Iterator<Item = (u32, rsleigh::Vn)> + '_ {
        self.entries.iter().filter_map(|e| match e {
            PositionalArg::Register { index, vn } => Some((*index, *vn)),
            PositionalArg::Stack { .. } => None,
        })
    }

    /// Iterates `(index, offset)` over the stack-passed argument slots
    /// in ABI order.  Indices start at [`Self::first_stack_index`].
    pub fn stack_args(&self) -> impl Iterator<Item = (u32, i64)> + '_ {
        self.entries.iter().filter_map(|e| match e {
            PositionalArg::Stack { index, offset } => Some((*index, *offset)),
            PositionalArg::Register { .. } => None,
        })
    }

    /// First positional index whose source is a stack slot.
    ///
    /// Equals the number of register-passed argument slots in the
    /// underlying convention.  Replaces the hand-derived
    /// `arg_passing_regs.len()` constant in `FunctionArgDetect` so the
    /// register-vs-stack boundary is computed in exactly one place.
    #[must_use]
    pub fn first_stack_index(&self) -> u32 {
        // The first `Stack` entry's index, or the entry count if there
        // are no stack slots (so `i < first_stack_index()` is the
        // register-arg predicate either way).
        self.entries
            .iter()
            .find_map(|e| match e {
                PositionalArg::Stack { index, .. } => Some(*index),
                PositionalArg::Register { .. } => None,
            })
            .unwrap_or(self.entries.len() as u32)
    }

    /// Iterates stack-arg offsets in ABI order.  Callers that need a
    /// `Vec<i64>` can `.collect()`; this returns a borrow-bound iterator
    /// so the common slice-consumer (`CallStackArgCollect`) can avoid
    /// the per-call allocation `Vec`-returning shape forced.
    pub fn stack_arg_offsets(&self) -> impl Iterator<Item = i64> + '_ {
        self.stack_args().map(|(_, o)| o)
    }

}

/// One row of the calling-convention preset table.  Carries the
/// preset's lookup name alongside the `CallingConvention` itself —
/// the entire data surface lives in the `CC_PRESETS` table below, and
/// the named factory functions (`x86_64_systemv`, `x86_cdecl`, ...)
/// are thin wrappers that perform a name lookup.
///
/// `CallingConvention` is `Copy` (all fields are `&'static`), so each
/// row stays on the static heap with no runtime construction cost.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CcPresetRow {
    /// The Rust factory name (also the Python classmethod name).
    pub(crate) name: &'static str,
    /// The convention itself.  `Copy` — every field is `&'static`.
    pub(crate) cc: CallingConvention,
}

/// Static data table of every supported calling convention preset.
///
/// Adding a new preset means appending one [`CcPresetRow`] entry — the
/// dispatch scaffolding (named factory wrappers + `lookup_preset`) does
/// not need changes.
///
/// Linear lookup is fine: ~22 rows, each `name` comparison is a string
/// `eq` that short-circuits on length.
pub(crate) static CC_PRESETS: &[CcPresetRow] = &[
    // ── Userland presets ────────────────────────────────────────────
    //
    // x86-64 System V ABI.
    //   Argument registers: RDI, RSI, RDX, RCX, R8, R9
    //   Callee-saved: RBX, RBP, R12–R15
    //   Return value: RAX, RDX
    //   RSP is the stack pointer and is not listed as callee-saved —
    //   `ret` pops the return address, so the caller observes SP shifted
    //   by `ret_stack_pop` across the call.
    CcPresetRow {
        name: "x86_64_systemv",
        cc: CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
            callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
            // SSE return regs (16-byte XMM); used for `float`/`double` returns.
            ret_val_regs_float: &["XMM0", "XMM1"],
            // Offsets start at +8: the `call` instruction pushes an 8-byte
            // return address, so SP-at-call points to the return address and
            // the first stack-passed arg (arg 7) lives one slot above it.
            stack_arg_offsets: &[8, 16, 24, 32, 40, 48],
            ret_stack_pop: 8,
            // x86-64 `call` pushes the return address on the stack; there
            // is no architectural link register.
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // "All-preserving" x86_64: every userland caller-clobbered register is
    // listed as callee-saved.  Empty arg-passing list, empty ret-val list.
    // Used for sites like Linux-kernel `__fentry__` / `mcount` callbacks
    // that preserve all caller state.  Pair with the per-address override
    // map on `CallingConvention` consumers so the override applies only
    // to specific Call sites; the function-default CC stays SystemV.
    CcPresetRow {
        name: "x86_64_all_preserving",
        cc: CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &[],
            callee_saved_regs: &[
                "RAX", "RBX", "RCX", "RDX", "RSI", "RDI", "RBP",
                "R8", "R9", "R10", "R11", "R12", "R13", "R14", "R15",
                "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5",
                "XMM6", "XMM7", "XMM8", "XMM9", "XMM10", "XMM11",
                "XMM12", "XMM13", "XMM14", "XMM15",
            ],
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            // The defining property of "all-preserving": memory is also
            // preserved.  build_call skips the Memory output so
            // LoadReadOnly / LoadForward forward across the call.
            preserves_memory: true,
        },
    },

    // AArch64 AAPCS64.
    //   Argument registers: x0–x7
    //   Callee-saved: x19–x28, x29 (frame pointer), x30 (link register)
    //   Return value: x0, x1
    //   `sp` is the stack pointer and is not listed as callee-saved —
    //   `ret_stack_pop` is 0 on AAPCS64 because `bl` writes the return
    //   address to `lr` rather than pushing it.
    //   AAPCS64 register conventions are independent of byte order, so
    //   this preset pairs equally with `SleighArch::aarch64` (LE) and
    //   `SleighArch::aarch64be` (BE).
    CcPresetRow {
        name: "aarch64_aapcs64",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            callee_saved_regs: &[
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
            ],
            ret_val_regs: &["x0", "x1"],
            // AArch64 SIMD return regs (16-byte vector; contain s0/d0/q0
            // sub-registers).  Now that vn_mask + build_int_const support
            // I128, the ABI-correct q0/q1 (16-byte) is preferred over d0/d1
            // (which was an earlier workaround for missing I128 support).
            ret_val_regs_float: &["q0", "q1"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
            // AArch64's `lr` is an alias for `x30`; Sleigh's aarch64
            // register table only registers `x30`.
            link_register_reg_name: Some("x30"),
            preserves_memory: false,
        },
    },

    // ARM 32-bit AAPCS.
    //   Argument registers: r0–r3
    //   Callee-saved: r4–r11, lr
    //   Return value: r0, r1  (r0/r1 pair used for 64-bit return values)
    //   `sp` is the stack pointer and is not listed as callee-saved.
    //   Unlike x86, ARM `bl` stores the return address in `lr` rather
    //   than pushing it, so the first stack-passed arg sits at SP + 0 and
    //   `ret_stack_pop` is 0.
    //   AAPCS register conventions are independent of byte order, so this
    //   preset pairs equally with `SleighArch::arm` (LE), `arm_be` (BE),
    //   and `arm_thumb` (Thumb-2).
    CcPresetRow {
        name: "arm_aapcs",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["r0", "r1", "r2", "r3"],
            callee_saved_regs: &["r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "lr"],
            ret_val_regs: &["r0", "r1"],
            // VFP return regs (8-byte d0/d1, also accessed as 4-byte s0/s1).
            // For VFP-disabled (-mfloat-abi=soft) builds the float result still
            // flows through r0/r1 — listing d0/d1 doesn't hurt because they're
            // simply unused in that case.
            ret_val_regs_float: &["d0", "d1"],
            stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
            ret_stack_pop: 0,
            // ARM's `bl` writes the return address to `lr` (= `r14`);
            // Sleigh registers it under the lowercase `lr` name.
            link_register_reg_name: Some("lr"),
            preserves_memory: false,
        },
    },

    // MIPS O32.
    //   Used by 32-bit MIPS Linux binaries on both LE and BE targets —
    //   the ABI is identical regardless of byte order.  Pairs equally with
    //   `SleighArch::mipsle32` and `mipsbe32`.
    //   Argument registers: a0, a1, a2, a3 (= r4–r7)
    //   Callee-saved:       s0–s7, s8 (= fp), gp, ra
    //   Return value:       v0, v1 (= r2, r3)
    //   `sp` (= r29) is the stack pointer.  `ret_stack_pop` is 0 because
    //   MIPS `jal`/`jalr` writes the return address to `$ra` rather than
    //   pushing it.  The first 16 bytes of stack-arg space (offsets 0..16)
    //   are MIPS's reserved "shadow space" for the four register args;
    //   positional stack args start at offset 16.
    //   Note: Sleigh's MIPS spec uses lowercase names (`a0`, `s0`, `sp`,
    //   `ra`, `gp`) and `s8` for the frame pointer register (not `fp`,
    //   which does not resolve in the Sleigh register table).
    CcPresetRow {
        name: "mips_o32",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["a0", "a1", "a2", "a3"],
            callee_saved_regs: &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra"],
            ret_val_regs: &["v0", "v1"],
            // FPU return regs (4-byte single-precision; doubles use the
            // f0/f1 pair).  Even on soft-float builds the listing is harmless
            // — these regs are simply unused.
            ret_val_regs_float: &["f0", "f2"],
            stack_arg_offsets: &[16, 20, 24, 28],
            ret_stack_pop: 0,
            // MIPS `jal`/`jalr` writes the return address to `$ra` (`$31`);
            // Sleigh's mips32 register table uses lowercase `ra`.
            link_register_reg_name: Some("ra"),
            preserves_memory: false,
        },
    },

    // MIPS N64 (used by 64-bit MIPS Linux binaries on both LE and BE —
    // `mips64-linux-gnuabi64-gcc`).
    //   The N64 ABI extends O32's 4 register args to 8 register args
    //   (`$4`–`$11`).  Sleigh's `mips64` spec uses the older naming where
    //   `$4`–`$7` are `a0`–`a3` and `$8`–`$11` are `t0`–`t3`, so the
    //   arg-passing list lists the latter under their Sleigh names.
    //   Argument registers: a0–a3 (`$4`–`$7`), t0–t3 (`$8`–`$11`)
    //   Callee-saved:       s0–s7, s8 (= fp), gp, ra
    //   Return value:       v0, v1
    //   Float return:       f0, f2
    //   Stack args start at offset 0 from SP (no O32-style shadow space).
    CcPresetRow {
        name: "mips_n64",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3"],
            callee_saved_regs: &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra"],
            ret_val_regs: &["v0", "v1"],
            ret_val_regs_float: &["f0", "f2"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
            // Same as O32: the return address lives in `$ra`.
            link_register_reg_name: Some("ra"),
            preserves_memory: false,
        },
    },

    // PowerPC 32-bit System V ABI.  Used by `powerpc-linux-gnu-gcc` (with
    // both `-mbig-endian` and `-mlittle-endian` — the ABI is byte-order
    // independent).
    //   Argument registers: r3–r10 (8 GPRs)
    //   Callee-saved:       r14–r31, LR
    //   Return value:       r3, r4 (r3:r4 pair for 64-bit returns)
    //   Float return:       f1
    //   Stack args start at offset 8 (4-byte back-chain + 4-byte LR save).
    //   `r1` is the stack pointer in PowerPC convention.
    CcPresetRow {
        name: "powerpc_sysv32",
        cc: CallingConvention {
            stack_ptr_reg_name: "r1",
            arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            callee_saved_regs: &[
                "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21",
                "r22", "r23", "r24", "r25", "r26", "r27", "r28", "r29",
                "r30", "r31", "LR",
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1"],
            stack_arg_offsets: &[8, 12, 16, 20, 24, 28, 32, 36],
            ret_stack_pop: 0,
            // PowerPC `bl` writes the return address to the `LR` SPR;
            // Sleigh's PPC register table uses uppercase `LR`.
            link_register_reg_name: Some("LR"),
            preserves_memory: false,
        },
    },

    // PowerPC 64-bit ELFv1 calling convention (BE — used by
    // `powerpc64-linux-gnu-gcc`).
    //   ELFv1 has function descriptors: an external function symbol
    //   resolves to a 3-pointer descriptor (entry, TOC, env) rather than
    //   the entry directly.  The analyzer treats indirect calls in ELFv1
    //   binaries as pointer-to-descriptor; pattern queries that need the
    //   entry address must follow the descriptor convention.  For now we
    //   register the register-level ABI; descriptor-aware lifting is a
    //   follow-up.
    //   Argument registers: r3–r10 (8 GPRs)
    //   Callee-saved:       r2 (TOC), r14–r31
    //   Return value:       r3
    //   Float return:       f1
    //   Stack args start at offset 48 (ELFv1 linkage area is 48 bytes).
    CcPresetRow {
        name: "powerpc64_elf_v1",
        cc: CallingConvention {
            stack_ptr_reg_name: "r1",
            arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            callee_saved_regs: &[
                "r2",
                "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21",
                "r22", "r23", "r24", "r25", "r26", "r27", "r28", "r29",
                "r30", "r31",
                // include `LR` per the CLAUDE.md
                // "Note (link-register handling)" deliberate tradeoff
                // (consistent with `powerpc_sysv32`).  PPC64 ELFv1
                // §3.4 marks LR as volatile/caller-saved, but listing
                // it here makes `InitialVar(lr)` propagate through
                // call sites so the indirect-branch resolver's
                // `LinkRegister` arm fires for functions returning
                // via the entry LR.
                "LR",
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1"],
            stack_arg_offsets: &[48, 56, 64, 72],
            ret_stack_pop: 0,
            // Same as 32-bit PPC SysV: the return address lives in `LR`.
            link_register_reg_name: Some("LR"),
            preserves_memory: false,
        },
    },

    // PowerPC 64-bit ELFv2 calling convention (LE — used by
    // `powerpc64le-linux-gnu-gcc`).
    //   ELFv2 drops function descriptors — symbols point directly to the
    //   entry point.  Linkage area shrinks from 48 to 32 bytes.  Otherwise
    //   register usage matches ELFv1.
    //   Argument registers: r3–r10 (8 GPRs)
    //   Callee-saved:       r2 (TOC), r14–r31
    //   Return value:       r3
    //   Float return:       f1
    //   Stack args start at offset 32 (ELFv2 linkage area is 32 bytes).
    CcPresetRow {
        name: "powerpc64_elf_v2",
        cc: CallingConvention {
            stack_ptr_reg_name: "r1",
            arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            callee_saved_regs: &[
                "r2",
                "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21",
                "r22", "r23", "r24", "r25", "r26", "r27", "r28", "r29",
                "r30", "r31",
                // see powerpc64_elf_v1 above for the CLAUDE.md
                // deliberate-tradeoff rationale.
                "LR",
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1"],
            stack_arg_offsets: &[32, 40, 48, 56],
            ret_stack_pop: 0,
            // Same as ELFv1: the return address lives in `LR`.
            link_register_reg_name: Some("LR"),
            preserves_memory: false,
        },
    },

    // x86 cdecl.  Arguments passed on the stack, so `arg_passing_regs` is
    // empty.
    //   Callee-saved: EBX, ESI, EDI, EBP
    //   Return value: EAX, EDX
    //   ESP is the stack pointer and is not listed as callee-saved —
    //   `ret` pops the 4-byte return address, so the caller observes SP
    //   shifted by `ret_stack_pop` across the call.
    CcPresetRow {
        name: "x86_cdecl",
        cc: CallingConvention {
            stack_ptr_reg_name: "ESP",
            arg_passing_regs: &[],
            callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
            ret_val_regs: &["EAX", "EDX"],
            // x86 cdecl returns floats in `ST0` (the x87 FPU's 80-bit
            // top-of-stack).  GCC's i686 default lowers floats through
            // x87 even when arithmetic is via SSE.  Listing ST0 here
            // (now that the IR has F80 / I80 support) keeps the Return
            // node connected to the float chain.
            //
            // XMM0 is also listed as a fallback for SSE-default builds
            // (`-mfpmath=sse2`).  When neither is referenced by the
            // function, `FunctionBuilder::new_raw`'s upgrade-to-container
            // logic skips them harmlessly.
            ret_val_regs_float: &["ST0", "XMM0"],
            // Offsets start at +4: the `call` instruction pushes a 4-byte
            // return address, so SP-at-call points to the return address
            // and arg 0 lives one slot above it.
            stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
            ret_stack_pop: 4,
            // x86 `call` pushes the return address on the stack; there is
            // no architectural link register.
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // ── Linux kernel-internal presets ───────────────────────────────
    //
    // One row per (arch, role) pair.  Where the kernel-internal CC is
    // identical to the userland one (every supported arch except x86
    // 32-bit), the kernel row carries the same fields rather than
    // delegating at runtime — encoding it as data keeps the dispatch
    // table flat and makes "kernel CC = userland CC" obvious in one
    // place (see the comment block per row).
    //
    // For details see
    // docs/superpowers/specs/2026-05-01-linux-kernel-cc-design.md.

    // x86 32-bit Linux kernel-internal CC (`-mregparm=3`): the first
    // three integer args go in EAX, EDX, ECX; remaining args sit on the
    // stack at the same cdecl offsets.  Differs from `x86_cdecl` only in
    // `arg_passing_regs`.
    CcPresetRow {
        name: "x86_linux_kernel",
        cc: CallingConvention {
            stack_ptr_reg_name: "ESP",
            arg_passing_regs: &["EAX", "EDX", "ECX"],
            callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
            ret_val_regs: &["EAX", "EDX"],
            ret_val_regs_float: &["ST0", "XMM0"],
            stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
            ret_stack_pop: 4,
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // x86_64 Linux kernel-internal CC.  Identical to x86_64_systemv —
    // the kernel writes its C in SystemV (the syscall-entry assembly
    // does the `r10`→`rcx` shuffle before calling C handlers, so by the
    // time any kernel function is entered its args are already in their
    // SystemV slots).  Provided as a self-documenting alias so "this is
    // kernel code" is explicit at the call site.
    CcPresetRow {
        name: "x86_64_linux_kernel",
        cc: CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
            callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
            ret_val_regs_float: &["XMM0", "XMM1"],
            stack_arg_offsets: &[8, 16, 24, 32, 40, 48],
            ret_stack_pop: 8,
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // AArch64 Linux kernel-internal CC.  Identical to aarch64_aapcs64.
    CcPresetRow {
        name: "aarch64_linux_kernel",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            callee_saved_regs: &[
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
            ],
            ret_val_regs: &["x0", "x1"],
            ret_val_regs_float: &["q0", "q1"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
            link_register_reg_name: Some("x30"),
            preserves_memory: false,
        },
    },

    // ARM Linux kernel-internal CC.  Identical to arm_aapcs.
    CcPresetRow {
        name: "arm_linux_kernel",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["r0", "r1", "r2", "r3"],
            callee_saved_regs: &["r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "lr"],
            ret_val_regs: &["r0", "r1"],
            ret_val_regs_float: &["d0", "d1"],
            stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
            ret_stack_pop: 0,
            link_register_reg_name: Some("lr"),
            preserves_memory: false,
        },
    },

    // MIPS O32 Linux kernel-internal CC.  Identical to mips_o32.
    CcPresetRow {
        name: "mips_linux_kernel_o32",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["a0", "a1", "a2", "a3"],
            callee_saved_regs: &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra"],
            ret_val_regs: &["v0", "v1"],
            ret_val_regs_float: &["f0", "f2"],
            stack_arg_offsets: &[16, 20, 24, 28],
            ret_stack_pop: 0,
            link_register_reg_name: Some("ra"),
            preserves_memory: false,
        },
    },

    // MIPS N64 Linux kernel-internal CC.  Identical to mips_n64.
    CcPresetRow {
        name: "mips_linux_kernel_n64",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3"],
            callee_saved_regs: &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra"],
            ret_val_regs: &["v0", "v1"],
            ret_val_regs_float: &["f0", "f2"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
            link_register_reg_name: Some("ra"),
            preserves_memory: false,
        },
    },

    // ── Linux syscall presets ───────────────────────────────────────
    //
    // One row per arch.  The trap ABI strips most callee-saved state
    // (the kernel preserves only what the trap path actually saves), so
    // each row is encoded directly rather than derived from the userland
    // preset.

    // x86 32-bit Linux syscall ABI (`int 0x80`).  Args in EBX, ECX, EDX,
    // ESI, EDI, EBP; syscall number in EAX; return in EAX.  No link
    // register; no stack args (the `int 0x80` ABI is register-only).
    // `callee_saved_regs` is empty: every cdecl-callee-saved register
    // (EBX, ESI, EDI, EBP) is consumed as an argument here, so none of
    // them remain in the callee-saved set.
    CcPresetRow {
        name: "x86_linux_syscall",
        cc: CallingConvention {
            stack_ptr_reg_name: "ESP",
            arg_passing_regs: &["EBX", "ECX", "EDX", "ESI", "EDI", "EBP"],
            callee_saved_regs: &[],
            ret_val_regs: &["EAX"],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // x86_64 Linux syscall ABI (`syscall`).  Args in RDI, RSI, RDX, R10,
    // R8, R9 — note R10 not RCX because the `syscall` instruction
    // clobbers RCX with the return RIP.  Syscall number in RAX; return
    // in RAX.
    CcPresetRow {
        name: "x86_64_linux_syscall",
        cc: CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &["RDI", "RSI", "RDX", "R10", "R8", "R9"],
            callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX"],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // AArch64 Linux syscall ABI (`svc #0`).  Args in x0..x5; syscall
    // number in x8; return in x0.  No link register: `svc` returns via
    // `eret` reading `ELR_EL1`, not `lr`.
    CcPresetRow {
        name: "aarch64_linux_syscall",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5"],
            callee_saved_regs: &[
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
            ],
            ret_val_regs: &["x0"],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // ARM 32-bit Linux syscall ABI (`svc 0`).  Args in r0..r6; syscall
    // number in r7; return in r0.  Same on Thumb.  `callee_saved_regs`
    // strips r4..r7 (consumed as args plus the syscall number) from the
    // AAPCS callee-saved set; r8..r11 and lr remain — the kernel
    // preserves them across the trap.
    CcPresetRow {
        name: "arm_linux_syscall",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["r0", "r1", "r2", "r3", "r4", "r5", "r6"],
            callee_saved_regs: &["r8", "r9", "r10", "r11", "lr"],
            ret_val_regs: &["r0"],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // MIPS O32 Linux syscall ABI (`syscall`).  Args in a0..a3; syscall
    // number in v0; return in v0.
    CcPresetRow {
        name: "mips_linux_syscall_o32",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["a0", "a1", "a2", "a3"],
            callee_saved_regs: &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra"],
            ret_val_regs: &["v0"],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },

    // MIPS N64 Linux syscall ABI (`syscall`).  Args in a0..a5; syscall
    // number in v0; return in v0.
    CcPresetRow {
        name: "mips_linux_syscall_n64",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["a0", "a1", "a2", "a3", "t0", "t1"],
            callee_saved_regs: &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra"],
            ret_val_regs: &["v0"],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },
];

/// Look up a calling-convention preset by its factory name.
///
/// Linear scan over `CC_PRESETS` — the table holds ~22 rows, and
/// each name comparison short-circuits on length, so the lookup is
/// cheap enough to skip a hash map.
#[must_use]
pub(crate) fn lookup_preset(name: &str) -> Option<&'static CcPresetRow> {
    CC_PRESETS.iter().find(|row| row.name == name)
}

/// Error returned by [`CallingConvention`]'s named factory wrappers
/// when their lookup key is missing from the `CC_PRESETS` table.
///
/// Indicates an internal inconsistency in this source file: every named
/// factory must have a matching row in `CC_PRESETS`.  Surfaced as a
/// typed error rather than a panic / silent fallback so callers can
/// propagate the failure into their own error chain (every factory's
/// `Err` is automatically convertible into `anyhow::Error`).
#[derive(Debug, thiserror::Error)]
#[error("calling-convention preset not registered: {0}")]
pub struct MissingPresetError(pub &'static str);

/// Internal helper used by every named factory wrapper below — looks
/// up the row and returns its `CallingConvention`, or a typed
/// [`MissingPresetError`] if no row matches `name`.
fn cc_from_table(name: &'static str) -> std::result::Result<CallingConvention, MissingPresetError> {
    lookup_preset(name)
        .map(|row| row.cc)
        .ok_or(MissingPresetError(name))
}

/// Emits a named factory wrapper around [`cc_from_table`] with the
/// canonical `# Errors` doc block.  `$desc` is the per-preset
/// description that becomes the first paragraph of the rustdoc.
///
/// `#[doc = concat!(...)]` is used because rustdoc's `///` form
/// doesn't accept macro variables — the macro emits `#[doc = "..."]`
/// directly with the computed string.
macro_rules! cc_factory {
    ($name:ident, $desc:expr) => {
        #[doc = concat!($desc, "  See `CC_PRESETS` for the full field table.")]
        ///
        /// # Errors
        ///
        /// Returns [`MissingPresetError`] if this factory's preset name is
        /// not registered in `CC_PRESETS` (an internal-consistency failure).
        pub fn $name() -> std::result::Result<CallingConvention, MissingPresetError> {
            cc_from_table(stringify!($name))
        }
    };
}

impl CallingConvention {
    /// Returns `true` if calls under this convention preserve memory
    /// across the call (i.e. the IR's Call node should NOT advance the
    /// memory chain).  See the `Self::preserves_memory` field docs.
    #[must_use]
    pub fn preserves_memory(&self) -> bool {
        self.preserves_memory
    }

    cc_factory!(x86_64_systemv, "Returns the x86-64 System V ABI calling convention.");
    cc_factory!(
        x86_64_all_preserving,
        "\"All-preserving\" x86_64 calling convention: every userland \
         caller-clobbered register is listed as callee-saved.  Used for \
         sites like Linux-kernel `__fentry__` / `mcount` callbacks that \
         preserve all caller state.  Pair with the per-address override \
         map on [`crate::CallingConvention`] consumers (e.g. \
         `strider::Config::per_address_ccs`) so the override applies only \
         to specific Call sites; the function-default CC stays SystemV."
    );
    cc_factory!(aarch64_aapcs64, "Returns the AArch64 AAPCS64 calling convention.");
    cc_factory!(arm_aapcs, "Returns the ARM 32-bit AAPCS calling convention.");
    cc_factory!(mips_o32, "Returns the MIPS O32 calling convention.");
    cc_factory!(mips_n64, "Returns the MIPS N64 calling convention.");
    cc_factory!(powerpc_sysv32, "Returns the PowerPC 32-bit System V ABI calling convention.");
    cc_factory!(powerpc64_elf_v1, "Returns the PowerPC 64-bit ELFv1 calling convention.");
    cc_factory!(powerpc64_elf_v2, "Returns the PowerPC 64-bit ELFv2 calling convention.");
    cc_factory!(x86_cdecl, "Returns the x86 cdecl calling convention.");

    /// Resolves all register name strings in this calling convention to their
    /// concrete [`rsleigh::Vn`] varnodes using `sleigh_regs`.
    ///
    /// The number of varnodes in each resulting list equals the length of the
    /// corresponding name list.
    ///
    /// # Errors
    ///
    /// Returns an error if any register name listed in this convention
    /// (including the stack pointer) does not resolve against `sleigh_regs`.
    /// The resolution short-circuits on the first failure.
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        let arg_passing_regs = regs_to_vns(sleigh_regs, self.arg_passing_regs)?;
        let callee_saved_regs = regs_to_vns(sleigh_regs, self.callee_saved_regs)?;
        let ret_val_regs = regs_to_vns(sleigh_regs, self.ret_val_regs)?;
        let ret_val_regs_float = regs_to_vns(sleigh_regs, self.ret_val_regs_float)?;
        let stack_vn = vn_for_name(sleigh_regs, self.stack_ptr_reg_name)?;
        // Resolve the link-register name when one is declared; propagate
        // any `UnknownRegName` from `vn_for_name` so a typo in the preset
        // surfaces at build time rather than later in the indirect-branch
        // resolver.
        let link_register_vn = match self.link_register_reg_name {
            Some(name) => Some(vn_for_name(sleigh_regs, name)?),
            None => None,
        };
        // Route through `try_new` so the disjointness invariants
        // (SP not in any reg list, arg/callee-saved disjoint, no
        // duplicates within a list, link-reg in callee-saved when set,
        // non-negative ret_stack_pop) are enforced at build time.  The
        // documented presets all satisfy them; routing here means a
        // future preset with a typo (SP in arg_passing_regs, missing
        // link-reg, etc.) fails at construction rather than producing
        // a downstream miscompile.
        BuiltCallingConvention::try_new(
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_vn,
            self.stack_arg_offsets.to_vec(),
            self.ret_stack_pop,
            link_register_vn,
            self.preserves_memory,
        )
    }
}

// ── Linux kernel + syscall preset wrappers ───────────────────────────────────
//
// Each named factory is a one-line lookup into `CC_PRESETS`.  The
// per-row table above carries the full ABI data; documentation comments
// on each row describe the convention.
//
// Adding a new (arch, role) preset is a single-source edit: append one
// `CcPresetRow` to `CC_PRESETS` and a matching wrapper here.  For details
// on the kernel + syscall ABIs see
// docs/superpowers/specs/2026-05-01-linux-kernel-cc-design.md.
impl CallingConvention {
    cc_factory!(
        x86_linux_kernel,
        "Returns the Linux kernel-internal CC for x86 32-bit (`-mregparm=3`)."
    );
    cc_factory!(
        x86_64_linux_kernel,
        "Returns the Linux kernel-internal CC for x86_64.  Identical to \
         [`Self::x86_64_systemv`] \u{2014} provided as a self-documenting alias."
    );
    cc_factory!(
        aarch64_linux_kernel,
        "Returns the Linux kernel-internal CC for AArch64.  Identical to \
         [`Self::aarch64_aapcs64`]."
    );
    cc_factory!(
        arm_linux_kernel,
        "Returns the Linux kernel-internal CC for ARM.  Identical to \
         [`Self::arm_aapcs`]."
    );
    cc_factory!(
        mips_linux_kernel_o32,
        "Returns the Linux kernel-internal CC for MIPS O32.  Identical to \
         [`Self::mips_o32`]."
    );
    cc_factory!(
        mips_linux_kernel_n64,
        "Returns the Linux kernel-internal CC for MIPS N64.  Identical to \
         [`Self::mips_n64`]."
    );
    cc_factory!(x86_linux_syscall, "Returns the Linux syscall ABI for x86 32-bit (`int 0x80`).");
    cc_factory!(x86_64_linux_syscall, "Returns the Linux syscall ABI for x86_64 (`syscall`).");
    cc_factory!(
        aarch64_linux_syscall,
        "Returns the Linux syscall ABI for AArch64 (`svc #0`)."
    );
    cc_factory!(arm_linux_syscall, "Returns the Linux syscall ABI for ARM 32-bit (`svc 0`).");
    cc_factory!(
        mips_linux_syscall_o32,
        "Returns the Linux syscall ABI for MIPS O32 (`syscall`)."
    );
    cc_factory!(
        mips_linux_syscall_n64,
        "Returns the Linux syscall ABI for MIPS N64 (`syscall`)."
    );
}


#[cfg(test)]
mod tests;
