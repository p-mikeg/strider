use crate::error::{ErrorKind, Result};

/// Converts a slice of register name strings into their corresponding varnode
/// representations using the provided Sleigh register map.
///
/// Iterates over each name in `reg_names`, looks it up in `sleigh_regs`, and
/// returns the list of resolved varnodes in the same order.  Returns an error
/// the moment any name is not found.
fn regs_to_vns(reg_names: &[&str], sleigh_regs: &rsleigh::SleighRegs) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|&reg_name| {
            sleigh_regs
                .name_to_vn(reg_name)
                .ok_or_else(|| ErrorKind::UnknownRegName(reg_name.to_string()).into())
        })
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
    arg_passing_regs: &'static [&'static str],
    callee_saved_regs: &'static [&'static str],
    ret_val_regs: &'static [&'static str],
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    pub ret_val_regs: Vec<rsleigh::Vn>,
    pub stack_ptr_vn: rsleigh::Vn,
    pub stack_arg_offsets: Vec<i64>,
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
    #[must_use]
    pub fn aarch64_aapcs64() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            callee_saved_regs: &[
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
            ],
            ret_val_regs: &["x0", "x1"],
            stack_arg_offsets: &[0, 8, 16, 24],
            ret_stack_pop: 0,
        }
    }

    /// Returns the ARM 32-bit AAPCS calling convention.
    ///
    /// Argument registers: r0–r3
    /// Callee-saved: r4–r11, sp, lr
    /// Return value: r0, r1  (r0/r1 pair is used for 64-bit return values)
    ///
    /// Unlike x86, the ARM `bl` instruction stores the return address in the
    /// link register `lr` rather than pushing it on the stack, so the first
    /// stack-passed arg sits at SP + 0.
    #[must_use]
    pub fn arm_aapcs() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["r0", "r1", "r2", "r3"],
            callee_saved_regs: &["r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "lr"],
            ret_val_regs: &["r0", "r1"],
            stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
            ret_stack_pop: 0,
        }
    }

    /// Returns the x86 cdecl calling convention.
    ///
    /// Arguments are passed on the stack, so `arg_passing_regs` is empty.
    /// Return value: EAX, EDX
    #[must_use]
    pub fn x86_cdecl() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "ESP",
            arg_passing_regs: &[],
            callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
            ret_val_regs: &["EAX", "EDX"],
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
        let arg_passing_regs = regs_to_vns(self.arg_passing_regs, sleigh_regs)?;
        let callee_saved_regs = regs_to_vns(self.callee_saved_regs, sleigh_regs)?;
        let ret_val_regs = regs_to_vns(self.ret_val_regs, sleigh_regs)?;
        let stack_ptr_name = self.stack_ptr_reg_name;
        let stack_ptr_vn = sleigh_regs
            .name_to_vn(stack_ptr_name)
            .ok_or(ErrorKind::UnknownRegName(stack_ptr_name.to_string()))?;
        Ok(BuiltCallingConvention {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
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
            for vn in &built.arg_passing_regs {
                assert!(
                    !built.callee_saved_regs.contains(vn),
                    "{}: arg reg {vn:?} is also callee-saved",
                    c.name,
                );
            }
        }
    }

    /// Every register resolved by a preset must have the architecture's
    /// natural word size.
    #[test]
    fn presets_resolved_registers_have_expected_size() {
        for c in cases() {
            let (built, _) = build_case(&c);
            for vn in built
                .arg_passing_regs
                .iter()
                .chain(&built.callee_saved_regs)
                .chain(&built.ret_val_regs)
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
    /// register and must NOT appear in `callee_saved_regs` (the callee's
    /// `ret` pops the return address, so SP is not preserved across a call
    /// on stack-push ISAs; on link-register ISAs the call doesn't touch SP
    /// but SP is still modeled as not callee-saved for uniformity, with
    /// `ret_stack_pop = 0`).  Stack-arg offsets and `ret_stack_pop` must
    /// round-trip unchanged from the preset.
    #[test]
    fn presets_stack_pointer_and_arg_offsets() {
        for c in cases() {
            let (built, regs) = build_case(&c);
            let sp = regs
                .name_to_vn(c.stack_ptr_name)
                .unwrap_or_else(|| panic!("{}: {} must resolve", c.name, c.stack_ptr_name));
            assert_eq!(built.stack_ptr_vn, sp, "{}: stack_ptr_vn", c.name);
            assert!(
                !built.callee_saved_regs.contains(&built.stack_ptr_vn),
                "{}: stack pointer must not be listed as callee-saved",
                c.name,
            );
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
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
        };
        assert!(cc.build(&regs).is_err(), "a list with one bad name must fail");
    }
}
