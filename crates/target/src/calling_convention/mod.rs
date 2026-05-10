use anyhow::anyhow;

use crate::Result;

/// Resolves a single Sleigh register name to its [`rsleigh::Vn`], or returns
/// an error if the name is not known.  Single source of truth for the
/// name-to-varnode error path.
fn vn_for_name(sleigh_regs: &rsleigh::SleighRegs, name: &str) -> Result<rsleigh::Vn> {
    sleigh_regs
        .name_to_vn(name)
        .ok_or_else(|| anyhow!("unknown sleigh register name {name:?}"))
}

/// Resolves a slice of Sleigh register names to varnodes in the same order.
/// Short-circuits on the first unknown name.
fn regs_to_vns(sleigh_regs: &rsleigh::SleighRegs, reg_names: &[&str]) -> Result<Vec<rsleigh::Vn>> {
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
    /// The Sleigh register name of the convention's syscall-number
    /// register (the register that carries the syscall index on entry
    /// to a kernel from a user-mode `syscall` / `svc` / `int 0x80`
    /// instruction), or `None` on userland and kernel-internal CCs.
    /// Resolved into [`BuiltCallingConvention::syscall_number_vn`] by
    /// [`Self::build`].  Set on the `*_linux_syscall` presets only.
    syscall_number_reg_name: Option<&'static str>,
    /// `true` if calls under this convention preserve **all** observable
    /// state, including memory.  When set, [`build_call_with_cc`](
    /// `ir::FunctionBuilder::build_call_with_cc`) skips emitting a Memory
    /// output on the resulting Call node and does not advance the region's
    /// memory chain — so passes like `LoadReadOnly` and `StackLoadForward`
    /// can forward loads across the call.
    ///
    /// `false` for every standard ABI; `true` only on
    /// [`Self::x86_64_all_preserving`] and analogous "transparent hook"
    /// presets (e.g. Linux-kernel `__fentry__` / `mcount` callbacks that
    /// preserve all caller state).
    no_memory_clobber: bool,
}

/// A calling convention whose register names have been resolved to concrete
/// [`rsleigh::Vn`] varnodes.
///
/// Produced by [`CallingConvention::build`] (canonical path) or
/// [`Self::try_from_parts`] / [`Self::from_parts_unchecked`]
/// (test/override construction).  Fields are
/// `pub(crate)`: callers read them through the typed accessors below
/// rather than touching the storage directly.  This keeps the type
/// immutable post-construction (no `.callee_saved_regs.push(x)` after
/// `build` returned) and gives the accessor return types — `&[Vn]` for
/// slices, `Vn` / `i64` / `bool` for `Copy` scalars — a single source
/// of truth as the storage shape evolves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    pub(crate) arg_passing_regs: Vec<rsleigh::Vn>,
    pub(crate) callee_saved_regs: Vec<rsleigh::Vn>,
    pub(crate) ret_val_regs: Vec<rsleigh::Vn>,
    pub(crate) ret_val_regs_float: Vec<rsleigh::Vn>,
    pub(crate) stack_ptr_vn: rsleigh::Vn,
    pub(crate) stack_arg_offsets: Vec<i64>,
    pub(crate) ret_stack_pop: i64,
    pub(crate) link_register_vn: Option<rsleigh::Vn>,
    pub(crate) syscall_number_vn: Option<rsleigh::Vn>,
    pub(crate) no_memory_clobber: bool,
}

/// Owned-field bag for [`BuiltCallingConvention::from_parts_unchecked`].  Used by
/// callers (typically tests building one-off override CCs) that need to
/// construct a `BuiltCallingConvention` without going through
/// [`CallingConvention::build`].  Field names mirror the
/// `BuiltCallingConvention`'s storage one-to-one; the field-by-field
/// docs live on the accessors of [`BuiltCallingConvention`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConventionParts {
    /// Argument-passing register varnodes, in positional order.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Callee-saved register varnodes (excludes SP).
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    /// Integer return-value register varnodes, in positional order.
    pub ret_val_regs: Vec<rsleigh::Vn>,
    /// Float return-value register varnodes, in positional order.
    pub ret_val_regs_float: Vec<rsleigh::Vn>,
    /// Hardware stack-pointer varnode.
    pub stack_ptr_vn: rsleigh::Vn,
    /// Per-positional-arg call-time stack offsets.
    pub stack_arg_offsets: Vec<i64>,
    /// Net SP delta the callee's `ret` inflicts (0 on link-register ISAs).
    pub ret_stack_pop: i64,
    /// Link-register varnode on link-register ISAs, `None` on stack-push ISAs.
    pub link_register_vn: Option<rsleigh::Vn>,
    /// Syscall-number register varnode for `*_linux_syscall` CCs.
    pub syscall_number_vn: Option<rsleigh::Vn>,
    /// `true` when calls under this CC preserve memory (zero-side-effect hooks).
    pub no_memory_clobber: bool,
}

impl BuiltCallingConvention {
    /// Constructs a `BuiltCallingConvention` from an explicit
    /// [`BuiltCallingConventionParts`] bag, **without validation**.
    /// Use this only in tests building override / synthesised CCs
    /// where the inputs are known well-formed; production code
    /// should use [`Self::try_from_parts`] (validates ABI invariants)
    /// or [`CallingConvention::build`] (resolves register names
    /// against a `SleighRegs` table and feeds [`Self::try_from_parts`]).
    ///
    /// **Test-only escape hatch.**  A typo overlapping `arg_passing_regs`
    /// with `callee_saved_regs` (or any other invariant violation listed
    /// on [`Self::try_from_parts`]) silently miscompiles downstream
    /// pattern queries.  Use the validated path in any production code.
    #[doc(hidden)]
    #[must_use]
    pub fn from_parts_unchecked(parts: BuiltCallingConventionParts) -> Self {
        let BuiltCallingConventionParts {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_ptr_vn,
            stack_arg_offsets,
            ret_stack_pop,
            link_register_vn,
            syscall_number_vn,
            no_memory_clobber,
        } = parts;
        Self {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_ptr_vn,
            stack_arg_offsets,
            ret_stack_pop,
            link_register_vn,
            syscall_number_vn,
            no_memory_clobber,
        }
    }

    /// Validating constructor .  Builds a
    /// `BuiltCallingConvention` from explicit parts and checks the
    /// canonical ABI invariants:
    ///
    /// - `arg_passing_regs ∩ callee_saved_regs == ∅`
    /// - `ret_val_regs ∩ callee_saved_regs == ∅`
    /// - `ret_val_regs_float ∩ callee_saved_regs == ∅`
    /// - `stack_ptr_vn` is not in any of the four register lists
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
    pub fn try_from_parts(
        parts: BuiltCallingConventionParts,
    ) -> std::result::Result<Self, anyhow::Error> {
        // Disjointness: arg-passing must not overlap callee-saved.
        for vn in &parts.arg_passing_regs {
            if parts.callee_saved_regs.contains(vn) {
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
        for vn in parts.ret_val_regs.iter().chain(parts.ret_val_regs_float.iter()) {
            if parts.callee_saved_regs.contains(vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {:?} appears in both \
                     ret_val_regs/ret_val_regs_float and callee_saved_regs",
                    vn,
                ));
            }
        }
        // Stack-pointer must not be in any reg-list.
        for (list_name, list) in [
            ("arg_passing_regs", &parts.arg_passing_regs),
            ("callee_saved_regs", &parts.callee_saved_regs),
            ("ret_val_regs", &parts.ret_val_regs),
            ("ret_val_regs_float", &parts.ret_val_regs_float),
        ] {
            if list.contains(&parts.stack_ptr_vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack_ptr_vn {:?} appears in {} \
                     (the SP is implicit and must not be in any reg list)",
                    parts.stack_ptr_vn,
                    list_name,
                ));
            }
        }
        // No duplicates within a list.
        for (list_name, list) in [
            ("arg_passing_regs", &parts.arg_passing_regs),
            ("callee_saved_regs", &parts.callee_saved_regs),
            ("ret_val_regs", &parts.ret_val_regs),
            ("ret_val_regs_float", &parts.ret_val_regs_float),
        ] {
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
        if let Some(lr) = parts.link_register_vn
            && !parts.callee_saved_regs.contains(&lr)
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
        if parts.ret_stack_pop < 0 {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: ret_stack_pop must be >= 0, got {}",
                parts.ret_stack_pop,
            ));
        }
        Ok(Self::from_parts_unchecked(parts))
    }

    /// Argument-passing register varnodes, in positional order.
    #[must_use]
    pub fn arg_passing_regs(&self) -> &[rsleigh::Vn] {
        &self.arg_passing_regs
    }

    /// Callee-saved register varnodes.  Excludes the stack pointer;
    /// SP's callee-side preservation is expressed through
    /// [`Self::ret_stack_pop`].
    #[must_use]
    pub fn callee_saved_regs(&self) -> &[rsleigh::Vn] {
        &self.callee_saved_regs
    }

    /// Integer return-value register varnodes, in positional order.
    #[must_use]
    pub fn ret_val_regs(&self) -> &[rsleigh::Vn] {
        &self.ret_val_regs
    }

    /// Float return-value register varnodes (e.g. `[q0, q1]` on
    /// AArch64, `[XMM0, XMM1]` on x86_64).  Tracked separately from
    /// [`Self::ret_val_regs`] because their widths differ.
    #[must_use]
    pub fn ret_val_regs_float(&self) -> &[rsleigh::Vn] {
        &self.ret_val_regs_float
    }

    /// Hardware stack-pointer varnode.  Deliberately absent from the
    /// three register-list accessors above — SP's cross-call behaviour
    /// is expressed through [`Self::ret_stack_pop`] instead.
    #[must_use]
    pub fn stack_ptr_vn(&self) -> rsleigh::Vn {
        self.stack_ptr_vn
    }

    /// Byte offsets from the call-time SP for each positional stack arg.
    #[must_use]
    pub fn stack_arg_offsets(&self) -> &[i64] {
        &self.stack_arg_offsets
    }

    /// Net byte change the callee's `ret` inflicts on the caller's SP.
    /// `8` on x86_64 (pops return address); `0` on link-register ISAs.
    #[must_use]
    pub fn ret_stack_pop(&self) -> i64 {
        self.ret_stack_pop
    }

    /// Link-register varnode on link-register ISAs (ARM, AArch64, MIPS,
    /// PowerPC); `None` on stack-push ISAs (x86, x86_64).  Consumed by
    /// the indirect-branch resolver to classify return-shaped indirect
    /// branches.
    #[must_use]
    pub fn link_register_vn(&self) -> Option<rsleigh::Vn> {
        self.link_register_vn
    }

    /// Syscall-number register varnode for `*_linux_syscall` CCs;
    /// `None` on userland and kernel-internal CCs.
    #[must_use]
    pub fn syscall_number_vn(&self) -> Option<rsleigh::Vn> {
        self.syscall_number_vn
    }

    /// `true` when calls under this CC preserve memory (zero-side-effect
    /// hooks like `__fentry__` / `mcount`).  Consumed by the IR builder's
    /// `build_call_with_cc` to suppress the Call's Memory output so
    /// `LoadReadOnly` / `StackLoadForward` can forward across the call.
    #[must_use]
    pub fn no_memory_clobber(&self) -> bool {
        self.no_memory_clobber
    }
}

impl CallingConvention {
    /// Returns `true` if calls under this convention preserve memory
    /// across the call (i.e. the IR's Call node should NOT advance the
    /// memory chain).  See the [`Self::no_memory_clobber`](field) docs.
    #[must_use]
    pub fn no_memory_clobber(&self) -> bool {
        self.no_memory_clobber
    }

    /// Returns the x86-64 System V ABI calling convention.
    ///
    /// Argument registers: RDI, RSI, RDX, RCX, R8, R9
    /// Callee-saved: RBX, RBP, R12–R15
    /// Return value: RAX, RDX
    ///
    /// RSP is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved — `ret` pops the return address, so the caller observes
    /// SP shifted by `ret_stack_pop` across the call.
    #[must_use]
    pub fn x86_64_systemv() -> CallingConvention {
        CallingConvention {
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
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

    /// "All-preserving" x86_64 calling convention: every userland
    /// caller-clobbered register is listed as callee-saved.  Empty
    /// arg-passing list, empty ret-val list.  Used for sites like
    /// Linux-kernel `__fentry__` / `mcount` callbacks that preserve
    /// all caller state.
    ///
    /// Pair with the per-address override map on
    /// [`crate::CallingConvention`] consumers (e.g.
    /// `strider::RunConfig::per_address_ccs`) so the override applies
    /// only to specific Call sites; the function-default CC stays
    /// SystemV.
    #[must_use]
    pub fn x86_64_all_preserving() -> CallingConvention {
        CallingConvention {
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
            syscall_number_reg_name: None,
            // The defining property of "all-preserving": memory is also
            // preserved.  build_call_with_cc skips the Memory output so
            // LoadReadOnly / StackLoadForward forward across the call.
            no_memory_clobber: true,
        }
    }

    /// Returns the AArch64 AAPCS64 calling convention.
    ///
    /// Argument registers: x0–x7
    /// Callee-saved: x19–x28, x29 (frame pointer), x30 (link register)
    /// Return value: x0, x1
    ///
    /// `sp` is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved — `ret_stack_pop` is `0` on AAPCS64 because `bl` writes
    /// the return address to `lr` rather than pushing it.
    ///
    /// AAPCS64 register conventions are independent of byte order, so this
    /// preset pairs equally with [`crate::SleighArch::aarch64`] (LE) and
    /// [`crate::SleighArch::aarch64be`] (BE).
    #[must_use]
    pub fn aarch64_aapcs64() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            callee_saved_regs: &[
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
            ],
            ret_val_regs: &["x0", "x1"],
            // AArch64 SIMD return regs (16-byte vector; contain s0/d0/q0
            // sub-registers).  Now that vn_mask + build_int_const support
            // U128, the ABI-correct q0/q1 (16-byte) is preferred over d0/d1
            // (which was an earlier workaround for missing U128 support).
            ret_val_regs_float: &["q0", "q1"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
            // AArch64's `lr` is an alias for `x30`; Sleigh's aarch64
            // register table only registers `x30`.
            link_register_reg_name: Some("x30"),
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

    /// Returns the ARM 32-bit AAPCS calling convention.
    ///
    /// Argument registers: r0–r3
    /// Callee-saved: r4–r11, lr
    /// Return value: r0, r1  (r0/r1 pair is used for 64-bit return values)
    ///
    /// `sp` is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved.  Unlike x86, the ARM `bl` instruction stores the
    /// return address in the link register `lr` rather than pushing it on the
    /// stack, so the first stack-passed arg sits at SP + 0 and `ret_stack_pop`
    /// is `0`.
    ///
    /// AAPCS register conventions are independent of byte order, so this
    /// preset pairs equally with [`crate::SleighArch::arm`] (LE),
    /// [`crate::SleighArch::arm_be`] (BE), and
    /// [`crate::SleighArch::arm_thumb`] (Thumb-2).
    #[must_use]
    pub fn arm_aapcs() -> CallingConvention {
        CallingConvention {
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
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

    /// Returns the MIPS O32 calling convention.
    ///
    /// Used by 32-bit MIPS Linux binaries on both LE and BE targets — the ABI
    /// is identical regardless of byte order.  Pairs equally with
    /// [`crate::SleighArch::mipsle32`] and [`crate::SleighArch::mipsbe32`].
    ///
    /// Argument registers: a0, a1, a2, a3 (= r4–r7)
    /// Callee-saved:       s0–s7, s8 (= fp), gp, ra (= r16–r23, r30, r28, r31)
    /// Return value:       v0, v1 (= r2, r3)
    ///
    /// `sp` (= r29) is the stack pointer.  `ret_stack_pop` is `0` because
    /// MIPS `jal`/`jalr` writes the return address to `$ra` rather than
    /// pushing it on the stack.  The first 16 bytes of stack-arg space
    /// (offsets 0..16) are MIPS's reserved "shadow space" for the four
    /// register args; positional stack args start at offset 16.
    ///
    /// Note: Sleigh's MIPS spec uses lowercase names (`a0`, `s0`, `sp`, `ra`,
    /// `gp`) and `s8` for the frame pointer register (not `fp`, which does not
    /// resolve in the Sleigh register table).
    #[must_use]
    pub fn mips_o32() -> CallingConvention {
        CallingConvention {
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
            // MIPS `jal`/`jalr` writes the return address to `$ra`
            // (`$31`); Sleigh's mips32 register table uses lowercase `ra`.
            link_register_reg_name: Some("ra"),
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

    /// Returns the MIPS N64 calling convention (used by 64-bit MIPS Linux
    /// binaries on both LE and BE — `mips64-linux-gnuabi64-gcc`).
    ///
    /// The N64 ABI extends O32's 4 register args to 8 register args
    /// (`$4`–`$11`).  Sleigh's `mips64` spec uses the older naming where
    /// `$4`–`$7` are `a0`–`a3` and `$8`–`$11` are `t0`–`t3`, so the arg-
    /// passing list lists the latter under their Sleigh names.
    ///
    /// Argument registers: a0–a3 (`$4`–`$7`), t0–t3 (`$8`–`$11`)
    /// Callee-saved:       s0–s7, s8 (= fp), gp, ra
    /// Return value:       v0, v1
    /// Float return:       f0, f2
    /// Stack args start at offset 0 from SP (no O32-style shadow space).
    #[must_use]
    pub fn mips_n64() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3"],
            callee_saved_regs: &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra"],
            ret_val_regs: &["v0", "v1"],
            ret_val_regs_float: &["f0", "f2"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
            // Same as O32: the return address lives in `$ra`.
            link_register_reg_name: Some("ra"),
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

    /// Returns the PowerPC 32-bit System V ABI calling convention.
    /// Used by `powerpc-linux-gnu-gcc` (with both `-mbig-endian` and
    /// `-mlittle-endian` — the ABI is byte-order independent).
    ///
    /// Argument registers: r3–r10 (8 GPRs)
    /// Callee-saved:       r14–r31, LR
    /// Return value:       r3, r4 (r3:r4 pair for 64-bit returns)
    /// Float return:       f1
    /// Stack args start at offset 8 (4-byte back-chain + 4-byte LR save).
    /// `r1` is the stack pointer in PowerPC convention.
    #[must_use]
    pub fn powerpc_sysv32() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "r1",
            arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            callee_saved_regs: &[
                "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21",
                "r22", "r23", "r24", "r25", "r26", "r27", "r28", "r29",
                "r30", "r31", "LR",
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1", "f2"],
            stack_arg_offsets: &[8, 12, 16, 20, 24, 28, 32, 36],
            ret_stack_pop: 0,
            // PowerPC `bl` writes the return address to the `LR` SPR;
            // Sleigh's PPC register table uses uppercase `LR`.
            link_register_reg_name: Some("LR"),
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

    /// Returns the PowerPC 64-bit ELFv1 calling convention (BE — used by
    /// `powerpc64-linux-gnu-gcc`).
    ///
    /// ELFv1 has function descriptors: an external function symbol resolves
    /// to a 3-pointer descriptor (entry, TOC, env) rather than the entry
    /// directly.  The analyzer treats indirect calls in ELFv1 binaries as
    /// pointer-to-descriptor; pattern queries that need the entry address
    /// must follow the descriptor convention.  For now we register the
    /// register-level ABI; descriptor-aware lifting is a follow-up.
    ///
    /// Argument registers: r3–r10 (8 GPRs)
    /// Callee-saved:       r2 (TOC), r14–r31
    /// Return value:       r3
    /// Float return:       f1
    /// Stack args start at offset 48 (ELFv1 linkage area is 48 bytes).
    #[must_use]
    pub fn powerpc64_elf_v1() -> CallingConvention {
        CallingConvention {
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
            ret_val_regs_float: &["f1", "f2"],
            stack_arg_offsets: &[48, 56, 64, 72],
            ret_stack_pop: 0,
            // Same as 32-bit PPC SysV: the return address lives in `LR`.
            link_register_reg_name: Some("LR"),
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

    /// Returns the PowerPC 64-bit ELFv2 calling convention (LE — used by
    /// `powerpc64le-linux-gnu-gcc`).
    ///
    /// ELFv2 drops function descriptors — symbols point directly to the
    /// entry point.  Linkage area shrinks from 48 to 32 bytes.  Otherwise
    /// register usage matches ELFv1.
    ///
    /// Argument registers: r3–r10 (8 GPRs)
    /// Callee-saved:       r2 (TOC), r14–r31
    /// Return value:       r3
    /// Float return:       f1
    /// Stack args start at offset 32 (ELFv2 linkage area is 32 bytes).
    #[must_use]
    pub fn powerpc64_elf_v2() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "r1",
            arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            callee_saved_regs: &[
                "r2",
                "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21",
                "r22", "r23", "r24", "r25", "r26", "r27", "r28", "r29",
                "r30", "r31",
                // see powerpc64_elf_v1 above for
                // the CLAUDE.md deliberate-tradeoff rationale.
                "LR",
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1", "f2"],
            stack_arg_offsets: &[32, 40, 48, 56],
            ret_stack_pop: 0,
            // Same as ELFv1: the return address lives in `LR`.
            link_register_reg_name: Some("LR"),
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

    /// Returns the x86 cdecl calling convention.
    ///
    /// Arguments are passed on the stack, so `arg_passing_regs` is empty.
    /// Callee-saved: EBX, ESI, EDI, EBP
    /// Return value: EAX, EDX
    ///
    /// ESP is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved — `ret` pops the 4-byte return address, so the caller
    /// observes SP shifted by `ret_stack_pop` across the call.
    #[must_use]
    pub fn x86_cdecl() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "ESP",
            arg_passing_regs: &[],
            callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
            ret_val_regs: &["EAX", "EDX"],
            // x86 cdecl returns floats in `ST0` (the x87 FPU's 80-bit
            // top-of-stack).  GCC's i686 default lowers floats through
            // x87 even when arithmetic is via SSE.  Listing ST0 here
            // (now that the IR has F80 / U80 support) keeps the Return
            // node connected to the float chain.
            //
            // XMM0 is also listed as a fallback for SSE-default builds
            // (`-mfpmath=sse2`).  When neither is referenced by the
            // function, `FunctionBuilder::new_raw`'s upgrade-to-
            // container logic skips them harmlessly.
            ret_val_regs_float: &["ST0", "XMM0"],
            // Offsets start at +4: the `call` instruction pushes a 4-byte
            // return address, so SP-at-call points to the return address and
            // arg 0 lives one slot above it.
            stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
            ret_stack_pop: 4,
            // x86 `call` pushes the return address on the stack; there
            // is no architectural link register.
            link_register_reg_name: None,
            syscall_number_reg_name: None,
            no_memory_clobber: false,
        }
    }

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
        let stack_ptr_vn = vn_for_name(sleigh_regs, self.stack_ptr_reg_name)?;
        // Resolve the link-register name when one is declared; propagate
        // any `UnknownRegName` from `vn_for_name` so a typo in the preset
        // surfaces at build time rather than later in the indirect-branch
        // resolver.
        let link_register_vn = match self.link_register_reg_name {
            Some(name) => Some(vn_for_name(sleigh_regs, name)?),
            None => None,
        };
        // Same propagation rule for the syscall-number register: a
        // typo in a `*_linux_syscall` preset surfaces here rather than
        // silently dropping the field at the analysis layer.
        let syscall_number_vn = match self.syscall_number_reg_name {
            Some(name) => Some(vn_for_name(sleigh_regs, name)?),
            None => None,
        };
        // Route through `try_from_parts` so the disjointness invariants
        // (SP not in any reg list, arg/callee-saved disjoint, no
        // duplicates within a list, link-reg in callee-saved when set,
        // non-negative ret_stack_pop) are enforced at build time.  The
        // documented presets all satisfy them; routing here means a
        // future preset with a typo (SP in arg_passing_regs, missing
        // link-reg, etc.) fails at construction rather than producing
        // a downstream miscompile.
        BuiltCallingConvention::try_from_parts(BuiltCallingConventionParts {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_ptr_vn,
            stack_arg_offsets: self.stack_arg_offsets.to_vec(),
            ret_stack_pop: self.ret_stack_pop,
            link_register_vn,
            syscall_number_vn,
            no_memory_clobber: self.no_memory_clobber,
        })
    }
}

// ── Linux kernel + syscall presets ───────────────────────────────────────────
//
// One factory per (arch, role) pair.  Where the kernel-internal CC is
// identical to the userland one (every supported arch except x86 32-bit),
// the kernel factory delegates to the userland preset rather than
// duplicating the field list — keeps a single source of truth and makes
// "kernel CC = userland CC" obvious by inspection.  For details see
// docs/superpowers/specs/2026-05-01-linux-kernel-cc-design.md.
impl CallingConvention {
    /// Returns the Linux kernel-internal CC for x86 32-bit
    /// (`-mregparm=3`): the first three integer args go in
    /// `EAX, EDX, ECX`; remaining args sit on the stack at the same
    /// cdecl offsets.  Differs from [`Self::x86_cdecl`] only in
    /// `arg_passing_regs`.
    #[must_use]
    pub fn x86_linux_kernel() -> CallingConvention {
        let mut cc = Self::x86_cdecl();
        cc.arg_passing_regs = &["EAX", "EDX", "ECX"];
        cc
    }

    /// Returns the Linux kernel-internal CC for x86_64.  Identical to
    /// [`Self::x86_64_systemv`] — the kernel writes its C in
    /// SystemV (the syscall-entry assembly does the
    /// `r10`→`rcx` shuffle before calling C handlers, so by the time
    /// any kernel function is entered its args are already in their
    /// SystemV slots).  Provided as a self-documenting alias so
    /// "this is kernel code" is explicit at the call site.
    #[must_use]
    pub fn x86_64_linux_kernel() -> CallingConvention {
        Self::x86_64_systemv()
    }

    /// Returns the Linux kernel-internal CC for AArch64.  Identical
    /// to [`Self::aarch64_aapcs64`].  See [`Self::x86_64_linux_kernel`]
    /// for the rationale on aliases.
    #[must_use]
    pub fn aarch64_linux_kernel() -> CallingConvention {
        Self::aarch64_aapcs64()
    }

    /// Returns the Linux kernel-internal CC for ARM.  Identical to
    /// [`Self::arm_aapcs`].
    #[must_use]
    pub fn arm_linux_kernel() -> CallingConvention {
        Self::arm_aapcs()
    }

    /// Returns the Linux kernel-internal CC for MIPS O32.  Identical
    /// to [`Self::mips_o32`].
    #[must_use]
    pub fn mips_linux_kernel_o32() -> CallingConvention {
        Self::mips_o32()
    }

    /// Returns the Linux kernel-internal CC for MIPS N64.  Identical
    /// to [`Self::mips_n64`].
    #[must_use]
    pub fn mips_linux_kernel_n64() -> CallingConvention {
        Self::mips_n64()
    }

    /// Returns the Linux syscall ABI for x86 32-bit (`int 0x80`).
    /// Args in `EBX, ECX, EDX, ESI, EDI, EBP`; syscall number in
    /// `EAX`; return in `EAX`.  No link register; no stack args (the
    /// `int 0x80` ABI is register-only).  `callee_saved_regs` is
    /// empty: every cdecl-callee-saved register (`EBX, ESI, EDI,
    /// EBP`) is consumed as an argument here, so none of them
    /// remain in the callee-saved set.  Disjointness between
    /// `arg_passing_regs` and `callee_saved_regs` is an architectural
    /// invariant of `BuiltCallingConvention` — see the
    /// `assert_disjoint` checks in the unit tests.
    #[must_use]
    pub fn x86_linux_syscall() -> CallingConvention {
        let mut cc = Self::x86_cdecl();
        cc.arg_passing_regs = &["EBX", "ECX", "EDX", "ESI", "EDI", "EBP"];
        cc.callee_saved_regs = &[];
        cc.ret_val_regs = &["EAX"];
        cc.ret_val_regs_float = &[];
        cc.stack_arg_offsets = &[];
        cc.ret_stack_pop = 0;
        cc.link_register_reg_name = None;
        cc.syscall_number_reg_name = Some("EAX");
        cc
    }

    /// Returns the Linux syscall ABI for x86_64 (`syscall`).  Args in
    /// `RDI, RSI, RDX, R10, R8, R9` — note `R10` not `RCX` because
    /// the `syscall` instruction clobbers `RCX` with the return RIP.
    /// Syscall number in `RAX`; return in `RAX`.
    #[must_use]
    pub fn x86_64_linux_syscall() -> CallingConvention {
        let mut cc = Self::x86_64_systemv();
        cc.arg_passing_regs = &["RDI", "RSI", "RDX", "R10", "R8", "R9"];
        cc.ret_val_regs = &["RAX"];
        cc.ret_val_regs_float = &[];
        cc.stack_arg_offsets = &[];
        cc.ret_stack_pop = 0;
        cc.link_register_reg_name = None;
        cc.syscall_number_reg_name = Some("RAX");
        cc
    }

    /// Returns the Linux syscall ABI for AArch64 (`svc #0`).  Args in
    /// `x0..x5`; syscall number in `x8`; return in `x0`.  No link
    /// register: `svc` returns via `eret` reading `ELR_EL1`, not `lr`.
    #[must_use]
    pub fn aarch64_linux_syscall() -> CallingConvention {
        let mut cc = Self::aarch64_aapcs64();
        cc.arg_passing_regs = &["x0", "x1", "x2", "x3", "x4", "x5"];
        cc.ret_val_regs = &["x0"];
        cc.ret_val_regs_float = &[];
        cc.stack_arg_offsets = &[];
        cc.ret_stack_pop = 0;
        cc.link_register_reg_name = None;
        cc.syscall_number_reg_name = Some("x8");
        cc
    }

    /// Returns the Linux syscall ABI for ARM 32-bit (`svc 0`).  Args
    /// in `r0..r6`; syscall number in `r7`; return in `r0`.  Same on
    /// Thumb.  `callee_saved_regs` strips `r4..r7` (consumed as args
    /// plus the syscall number) from the AAPCS callee-saved set;
    /// `r8..r11` and `lr` remain — the kernel preserves them across
    /// the trap.  Disjointness between `arg_passing_regs`, the
    /// syscall-number register, and `callee_saved_regs` is an
    /// architectural invariant of `BuiltCallingConvention`.
    #[must_use]
    pub fn arm_linux_syscall() -> CallingConvention {
        let mut cc = Self::arm_aapcs();
        cc.arg_passing_regs = &["r0", "r1", "r2", "r3", "r4", "r5", "r6"];
        cc.callee_saved_regs = &["r8", "r9", "r10", "r11", "lr"];
        cc.ret_val_regs = &["r0"];
        cc.ret_val_regs_float = &[];
        cc.stack_arg_offsets = &[];
        cc.ret_stack_pop = 0;
        cc.link_register_reg_name = None;
        cc.syscall_number_reg_name = Some("r7");
        cc
    }

    /// Returns the Linux syscall ABI for MIPS O32 (`syscall`).  Args
    /// in `a0..a3`; syscall number in `v0`; return in `v0`.
    #[must_use]
    pub fn mips_linux_syscall_o32() -> CallingConvention {
        let mut cc = Self::mips_o32();
        cc.arg_passing_regs = &["a0", "a1", "a2", "a3"];
        cc.ret_val_regs = &["v0"];
        cc.ret_val_regs_float = &[];
        cc.stack_arg_offsets = &[];
        cc.ret_stack_pop = 0;
        cc.link_register_reg_name = None;
        cc.syscall_number_reg_name = Some("v0");
        cc
    }

    /// Returns the Linux syscall ABI for MIPS N64 (`syscall`).  Args
    /// in `a0..a5`; syscall number in `v0`; return in `v0`.
    #[must_use]
    pub fn mips_linux_syscall_n64() -> CallingConvention {
        let mut cc = Self::mips_n64();
        cc.arg_passing_regs = &["a0", "a1", "a2", "a3", "t0", "t1"];
        cc.ret_val_regs = &["v0"];
        cc.ret_val_regs_float = &[];
        cc.stack_arg_offsets = &[];
        cc.ret_stack_pop = 0;
        cc.link_register_reg_name = None;
        cc.syscall_number_reg_name = Some("v0");
        cc
    }
}


#[cfg(test)]
mod tests;
