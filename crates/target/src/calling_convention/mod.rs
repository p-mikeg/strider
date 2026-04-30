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
}

/// A calling convention whose register names have been resolved to concrete
/// [`rsleigh::Vn`] varnodes.
///
/// Produced by [`CallingConvention::build`].  The field semantics mirror
/// [`CallingConvention`]; see that type's field docs for details.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    /// Varnodes for the ABI's argument-passing registers, in positional order.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Varnodes the callee must preserve across the call.  Excludes the
    /// stack pointer; SP's callee-side preservation is expressed through
    /// [`Self::ret_stack_pop`] instead.
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    /// Varnodes used to return a value to the caller, in positional order.
    pub ret_val_regs: Vec<rsleigh::Vn>,
    /// Varnodes used to return a *float* value to the caller, in positional
    /// order (e.g. `[q0, q1]` on aarch64, `[XMM0, XMM1]` on x86_64).  These
    /// have different widths from [`Self::ret_val_regs`] and are tracked
    /// separately so the analyzer can include both in `Return`'s input list
    /// without polluting integer-only patterns.
    pub ret_val_regs_float: Vec<rsleigh::Vn>,
    /// The hardware stack-pointer varnode (e.g. `RSP` on x86-64, `sp` on
    /// AArch64).  Deliberately absent from all three resolved register lists
    /// ([`Self::arg_passing_regs`], [`Self::callee_saved_regs`],
    /// [`Self::ret_val_regs`]) — SP's cross-call behaviour is expressed
    /// through [`Self::ret_stack_pop`] instead.  This invariant is pinned by
    /// the `presets_stack_pointer_and_arg_offsets` unit test.
    pub stack_ptr_vn: rsleigh::Vn,
    /// Byte offsets from the call-time stack pointer for each positional
    /// stack argument.  Entry `i` is the offset for the `i`-th stack arg
    /// (after register arguments are exhausted).
    pub stack_arg_offsets: Vec<i64>,
    /// Net byte change the callee's `ret` inflicts on the caller's stack
    /// pointer.  On stack-push ISAs (x86, x86_64) `ret` pops the return
    /// address, so this equals the pointer size (4 / 8).  On link-register
    /// ISAs (ARM, AArch64, MIPS, PowerPC) the call does not touch SP, so
    /// this is 0.
    pub ret_stack_pop: i64,
    /// The varnode that holds the return address across a call on
    /// link-register ISAs (ARM, AArch64, MIPS, PowerPC), or `None` on
    /// stack-push ISAs (x86, x86_64) where the return address lives on
    /// the stack.  Resolved from
    /// [`CallingConvention::link_register_reg_name`] by
    /// [`CallingConvention::build`].  Consumed by the indirect-branch
    /// resolver to classify `BranchIndirect` whose target is the
    /// function-entry value of this varnode as a `Return`.
    pub link_register_vn: Option<rsleigh::Vn>,
}

impl CallingConvention {
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
    pub fn x86_64_systemv_abi() -> CallingConvention {
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
            // (which was a workaround for the U128 panic — BUG-13).
            ret_val_regs_float: &["q0", "q1"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
            // AArch64's `lr` is an alias for `x30`; Sleigh's aarch64
            // register table only registers `x30`.
            link_register_reg_name: Some("x30"),
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
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1", "f2"],
            stack_arg_offsets: &[48, 56, 64, 72],
            ret_stack_pop: 0,
            // Same as 32-bit PPC SysV: the return address lives in `LR`.
            link_register_reg_name: Some("LR"),
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
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1", "f2"],
            stack_arg_offsets: &[32, 40, 48, 56],
            ret_stack_pop: 0,
            // Same as ELFv1: the return address lives in `LR`.
            link_register_reg_name: Some("LR"),
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
        Ok(BuiltCallingConvention {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_ptr_vn,
            stack_arg_offsets: self.stack_arg_offsets.to_vec(),
            ret_stack_pop: self.ret_stack_pop,
            link_register_vn,
        })
    }
}


#[cfg(test)]
mod tests;
