use crate::error::{ErrorKind, Result};

/// Resolves a single Sleigh register name to its [`rsleigh::Vn`], or returns
/// [`ErrorKind::UnknownRegName`] if the name is not known.  Single source of
/// truth for the name-to-varnode error path.
fn vn_for_name(sleigh_regs: &rsleigh::SleighRegs, name: &str) -> Result<rsleigh::Vn> {
    sleigh_regs
        .name_to_vn(name)
        .ok_or_else(|| ErrorKind::UnknownRegName(name.to_string()).into())
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
    /// own this fact is already passed separately to `Analyzer::new`.
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
            // AArch64 float return regs.  Use d0/d1 (8-byte double-precision
            // view) instead of q0/q1 (16-byte vector view) to avoid U128
            // shift-constant materialisation in write_reg_vn (see
            // analyzer-known-issues BUG-13).  d0/d1 is the natural width for
            // C `float`/`double` return values, which is what user code
            // actually queries.
            ret_val_regs_float: &["d0", "d1"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
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
            // x86 cdecl: floats and doubles return on the x87 stack (`ST0`),
            // 10 bytes wide.  When -mfpmath=sse is used `XMM0` is also a
            // candidate (16-byte SSE reg).  Listing both is harmless: at
            // most one will hold the actual return.
            ret_val_regs_float: &["ST0", "XMM0"],
            // Offsets start at +4: the `call` instruction pushes a 4-byte
            // return address, so SP-at-call points to the return address and
            // arg 0 lives one slot above it.
            stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
            ret_stack_pop: 4,
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
    /// Returns [`ErrorKind::UnknownRegName`] if any register name listed in
    /// this convention (including the stack pointer) does not resolve against
    /// `sleigh_regs`.  The resolution short-circuits on the first failure.
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        let arg_passing_regs = regs_to_vns(sleigh_regs, self.arg_passing_regs)?;
        let callee_saved_regs = regs_to_vns(sleigh_regs, self.callee_saved_regs)?;
        let ret_val_regs = regs_to_vns(sleigh_regs, self.ret_val_regs)?;
        let ret_val_regs_float = regs_to_vns(sleigh_regs, self.ret_val_regs_float)?;
        let stack_ptr_vn = vn_for_name(sleigh_regs, self.stack_ptr_reg_name)?;
        Ok(BuiltCallingConvention {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_ptr_vn,
            stack_arg_offsets: self.stack_arg_offsets.to_vec(),
            ret_stack_pop: self.ret_stack_pop,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs_for(arch: crate::arch::SleighArch) -> rsleigh::SleighRegs {
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
            .unwrap()
            .regs()
            .unwrap()
    }

    /// One row describes a supported calling convention and everything we
    /// expect `build()` to produce for it.  Adding a new convention means
    /// adding one entry here — every invariant test picks it up.
    struct Case {
        name: &'static str,
        cc: fn() -> CallingConvention,
        arch: fn() -> crate::arch::SleighArch,
        arg_count: usize,
        callee_saved_count: usize,
        ret_count: usize,
        reg_size_bytes: u32,
        stack_ptr_name: &'static str,
        stack_arg_offsets: &'static [i64],
        ret_stack_pop: i64,
    }

    fn cases() -> Vec<Case> {
        vec![
            Case {
                name: "x86-64 SysV",
                cc: CallingConvention::x86_64_systemv_abi,
                arch: crate::arch::SleighArch::x86_64,
                arg_count: 6,
                callee_saved_count: 6,
                ret_count: 2,
                reg_size_bytes: 8,
                stack_ptr_name: "RSP",
                stack_arg_offsets: &[8, 16, 24, 32, 40, 48],
                ret_stack_pop: 8,
            },
            Case {
                name: "x86 cdecl",
                cc: CallingConvention::x86_cdecl,
                arch: crate::arch::SleighArch::x86,
                arg_count: 0,
                callee_saved_count: 4,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "ESP",
                stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
                ret_stack_pop: 4,
            },
            Case {
                name: "ARM AAPCS",
                cc: CallingConvention::arm_aapcs,
                arch: crate::arch::SleighArch::arm,
                arg_count: 4,
                callee_saved_count: 9,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
                ret_stack_pop: 0,
            },
            Case {
                name: "AArch64 AAPCS64",
                cc: CallingConvention::aarch64_aapcs64,
                arch: crate::arch::SleighArch::aarch64,
                arg_count: 8,
                callee_saved_count: 12,
                ret_count: 2,
                reg_size_bytes: 8,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[0, 8, 16, 24],
                ret_stack_pop: 0,
            },
            Case {
                name: "MIPS O32 (LE)",
                cc: CallingConvention::mips_o32,
                arch: crate::arch::SleighArch::mipsle32,
                arg_count: 4,
                callee_saved_count: 11,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[16, 20, 24, 28],
                ret_stack_pop: 0,
            },
            Case {
                name: "MIPS O32 (BE)",
                cc: CallingConvention::mips_o32,
                arch: crate::arch::SleighArch::mipsbe32,
                arg_count: 4,
                callee_saved_count: 11,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[16, 20, 24, 28],
                ret_stack_pop: 0,
            },
        ]
    }

    fn build_case(case: &Case) -> (BuiltCallingConvention, rsleigh::SleighRegs) {
        let regs = regs_for((case.arch)());
        let built = (case.cc)()
            .build(&regs)
            .unwrap_or_else(|e| panic!("{}: build failed: {e:?}", case.name));
        (built, regs)
    }

    #[track_caller]
    fn assert_all_distinct(set: &[rsleigh::Vn], label: &str) {
        for i in 0..set.len() {
            for j in (i + 1)..set.len() {
                assert_ne!(
                    set[i], set[j],
                    "{label}: varnodes at positions {i} and {j} are the same"
                );
            }
        }
    }

    #[track_caller]
    fn assert_disjoint(
        a: &[rsleigh::Vn],
        b: &[rsleigh::Vn],
        a_label: &str,
        b_label: &str,
        case_name: &str,
    ) {
        for vn in a {
            assert!(
                !b.contains(vn),
                "{case_name}: {a_label} reg {vn:?} also appears in {b_label}",
            );
        }
    }

    /// Every preset must resolve to the documented number of registers in
    /// each category, with pairwise distinct varnodes and disjoint arg/
    /// callee-saved sets.
    #[test]
    fn presets_resolve_correct_register_sets() {
        for c in cases() {
            let (built, _) = build_case(&c);
            assert_eq!(built.arg_passing_regs.len(), c.arg_count, "{}: args", c.name);
            assert_eq!(
                built.callee_saved_regs.len(),
                c.callee_saved_count,
                "{}: callee-saved",
                c.name
            );
            assert_eq!(
                built.ret_val_regs.len(),
                c.ret_count,
                "{}: return values",
                c.name
            );
            assert_all_distinct(&built.arg_passing_regs, c.name);
            assert_all_distinct(&built.callee_saved_regs, c.name);
            assert_all_distinct(&built.ret_val_regs, c.name);
            assert_disjoint(
                &built.arg_passing_regs,
                &built.callee_saved_regs,
                "arg_passing_regs",
                "callee_saved_regs",
                c.name,
            );
            assert_disjoint(
                &built.ret_val_regs,
                &built.callee_saved_regs,
                "ret_val_regs",
                "callee_saved_regs",
                c.name,
            );
        }
    }

    /// Every register resolved by a preset (including the stack pointer) must
    /// have the architecture's natural word size.  SP is included because
    /// `StackStoreDetect` and the analyzer's stack-arg machinery assume an
    /// SP-sized address — an undersized SP would silently miscompute offsets
    /// downstream and produce no diagnostic from this crate.
    #[test]
    fn presets_resolved_registers_have_expected_size() {
        for c in cases() {
            let (built, _) = build_case(&c);
            for vn in built
                .arg_passing_regs
                .iter()
                .chain(&built.callee_saved_regs)
                .chain(&built.ret_val_regs)
                .chain(std::iter::once(&built.stack_ptr_vn))
            {
                assert_eq!(
                    vn.size, c.reg_size_bytes,
                    "{}: expected {}-byte register, got {vn:?}",
                    c.name, c.reg_size_bytes,
                );
            }
        }
    }

    /// The stack-pointer varnode must resolve to the architecture's SP
    /// register and must NOT appear in any of the three resolved register
    /// lists (`arg_passing_regs`, `callee_saved_regs`, `ret_val_regs`) —
    /// the callee's `ret` pops the return address on stack-push ISAs so
    /// SP is not preserved across a call, and on link-register ISAs the
    /// call doesn't touch SP but SP is still modeled as not callee-saved
    /// for uniformity (with `ret_stack_pop = 0`).  Stack-arg offsets and
    /// `ret_stack_pop` must round-trip unchanged from the preset.
    #[test]
    fn presets_stack_pointer_and_arg_offsets() {
        for c in cases() {
            let (built, regs) = build_case(&c);
            let sp = regs
                .name_to_vn(c.stack_ptr_name)
                .unwrap_or_else(|| panic!("{}: {} must resolve", c.name, c.stack_ptr_name));
            assert_eq!(built.stack_ptr_vn, sp, "{}: stack_ptr_vn", c.name);
            for (label, set) in [
                ("arg_passing_regs", &built.arg_passing_regs),
                ("callee_saved_regs", &built.callee_saved_regs),
                ("ret_val_regs", &built.ret_val_regs),
            ] {
                assert!(
                    !set.contains(&built.stack_ptr_vn),
                    "{}: stack pointer must not appear in {label}",
                    c.name,
                );
            }
            assert_eq!(
                built.stack_arg_offsets,
                c.stack_arg_offsets.to_vec(),
                "{}: stack_arg_offsets",
                c.name,
            );
            assert_eq!(
                built.ret_stack_pop, c.ret_stack_pop,
                "{}: ret_stack_pop",
                c.name,
            );
        }
    }

    /// An unknown register name in any category must return an error,
    /// regardless of architecture.
    #[test]
    fn build_returns_error_for_unknown_register_name() {
        let regs = regs_for(crate::arch::SleighArch::x86_64());
        for bad_name in &["NOTAREG", "", "rax_FAKE"] {
            let cc = CallingConvention {
                stack_ptr_reg_name: "RSP",
                arg_passing_regs: std::slice::from_ref(bad_name),
                callee_saved_regs: &[],
                ret_val_regs: &[],
                ret_val_regs_float: &[],
                stack_arg_offsets: &[],
                ret_stack_pop: 0,
            };
            let result = cc.build(&regs);
            assert!(
                matches!(
                    result.as_ref().map_err(|e| e.kind()),
                    Err(ErrorKind::UnknownRegName(n)) if n == bad_name
                ),
                "expected UnknownRegName({bad_name:?}), got {result:?}"
            );
        }
    }

    /// An error on the first unknown name must short-circuit rather than
    /// silently succeeding for the remaining valid names.
    #[test]
    fn build_returns_error_even_when_some_names_are_valid() {
        let regs = regs_for(crate::arch::SleighArch::x86_64());
        let cc = CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &["RDI", "NOT_A_REG", "RSI"],
            callee_saved_regs: &[],
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        };
        assert!(cc.build(&regs).is_err(), "a list with one bad name must fail");
    }

    #[test]
    #[ignore = "diagnostic — uncomment locally to print MIPS register names"]
    fn dump_mips_register_names() {
        let arch = crate::arch::SleighArch::mipsle32();
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
            .unwrap().regs().unwrap();
        let candidates = ["a0", "a1", "a2", "a3", "v0", "v1", "sp", "ra",
                          "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7",
                          "s8", "fp", "gp",
                          "A0", "V0", "SP", "RA", "S0", "FP", "GP",
                          "r4", "r16", "r28", "r29", "r30", "r31"];
        for n in candidates {
            let v = regs.name_to_vn(n);
            println!("name {n:?} -> {v:?}");
        }
    }

    /// An unknown `stack_ptr_reg_name` must surface as `UnknownRegName`, the
    /// same way an unknown entry in any of the three register lists does.
    /// Guards the open-coded `ok_or_else` in `build()` — the SP name has its
    /// own lookup path separate from `regs_to_vns`.
    #[test]
    fn build_returns_error_for_unknown_stack_pointer_name() {
        let regs = regs_for(crate::arch::SleighArch::x86_64());
        let cc = CallingConvention {
            stack_ptr_reg_name: "NOT_A_SP",
            arg_passing_regs: &[],
            callee_saved_regs: &[],
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        };
        let result = cc.build(&regs);
        assert!(
            matches!(
                result.as_ref().map_err(|e| e.kind()),
                Err(ErrorKind::UnknownRegName(n)) if n == "NOT_A_SP"
            ),
            "expected UnknownRegName(\"NOT_A_SP\"), got {result:?}"
        );
    }
#[test]
#[ignore = "probe"]
fn probe_float_regs() {
    fn try_resolve(arch: crate::arch::SleighArch, names: &[&str]) {
        let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe).unwrap().regs().unwrap();
        for n in names {
            let v = regs.name_to_vn(n);
            println!("  {n:?} -> {v:?}");
        }
    }
    println!("=== aarch64 ===");
    try_resolve(crate::arch::SleighArch::aarch64(), &[
        "q0", "q1", "v0", "v1", "d0", "d1", "s0", "s1", "Q0", "V0", "D0", "S0",
    ]);
    println!("=== x86_64 ===");
    try_resolve(crate::arch::SleighArch::x86_64(), &[
        "XMM0", "XMM1", "xmm0", "xmm1", "ST0", "ST1", "st0",
    ]);
    println!("=== arm ===");
    try_resolve(crate::arch::SleighArch::arm(), &[
        "s0", "s1", "d0", "d1", "S0", "D0",
    ]);
    println!("=== x86 ===");
    try_resolve(crate::arch::SleighArch::x86(), &[
        "XMM0", "ST0", "st0",
    ]);
    println!("=== mips32le ===");
    try_resolve(crate::arch::SleighArch::mipsle32(), &[
        "f0", "f1", "f2", "f3", "f12", "F0", "F12",
    ]);
}
}
