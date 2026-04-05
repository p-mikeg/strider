use crate::{Error, Result};

/// Converts a slice of register name strings into their corresponding varnode
/// representations using the provided Sleigh register map.
///
/// Iterates over each name in `reg_names`, looks it up in `sleigh_regs`, and
/// returns the list of resolved varnodes in the same order.  Returns an error
/// the moment any name is not found.
fn regs_to_vns(
    reg_names: &[&str],
    sleigh_regs: &rsleigh::SleighRegs,
) -> Result<Vec<rsleigh::Vn>> {
    let sleigh_regs = sleigh_regs;

    reg_names
        .iter()
        .map(|&reg_name| {
            sleigh_regs
                .name_to_vn(reg_name)
                .ok_or(Error::UnknownRegName(reg_name.to_string()))
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
    arch: crate::arch::SleighArch,
    arg_passing_regs:  &'static [&'static str],
    callee_saved_regs:  &'static [&'static str],
    ret_val_regs: &'static [&'static str]
}

/// A calling convention whose register names have been resolved to concrete
/// [`rsleigh::Vn`] varnodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    pub ret_val_regs: Vec<rsleigh::Vn>,
}


impl CallingConvention {
    /// Returns the x86-64 System V ABI calling convention.
    ///
    /// Argument registers: RDI, RSI, RDX, RCX, R8, R9
    /// Callee-saved: RBX, RSP, RBP, R12–R15
    /// Return value: RAX, RDX
    pub fn x86_64_systemv_abi() -> CallingConvention {
        CallingConvention {
            arch: crate::arch::SleighArch::x86_64(),
            arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
            callee_saved_regs: &["RBX", "RSP", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
         }
    }

    /// Returns the x86 cdecl calling convention.
    ///
    /// Arguments are passed on the stack, so `arg_passing_regs` is empty.
    /// Return value: EAX, EDX
    pub fn x86_cdecl() -> CallingConvention {
        CallingConvention {
            arch: crate::arch::SleighArch::x86(),
            arg_passing_regs: &[],
            callee_saved_regs: &[],
            ret_val_regs: &["EAX", "EDX"],
         }
    }

    /// Resolves all register name strings in this calling convention to their
    /// concrete [`rsleigh::Vn`] varnodes using `sleigh_regs`.
    ///
    /// The number of varnodes in each resulting list equals the length of the
    /// corresponding name list.  Returns an error if any register name is
    /// unknown.
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        Ok(BuiltCallingConvention {
            arg_passing_regs: regs_to_vns(self.arg_passing_regs, &sleigh_regs)?,
            callee_saved_regs: regs_to_vns(self.callee_saved_regs, &sleigh_regs)?,
            ret_val_regs: regs_to_vns(self.ret_val_regs, &sleigh_regs)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn regs_for(arch: crate::arch::SleighArch) -> rsleigh::SleighRegs {
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
            .unwrap()
            .regs()
            .unwrap()
    }

    fn x86_64_regs() -> rsleigh::SleighRegs { regs_for(crate::arch::SleighArch::x86_64()) }
    fn x86_regs()    -> rsleigh::SleighRegs { regs_for(crate::arch::SleighArch::x86()) }

    /// Builds `cc` against `regs`, asserts success, and returns the result.
    #[track_caller]
    fn build_ok(cc: CallingConvention, regs: &rsleigh::SleighRegs) -> BuiltCallingConvention {
        cc.build(regs).expect("build should succeed for valid register names")
    }

    /// Asserts that all varnodes in `set` are pairwise distinct.
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

    /// Asserts that `a` and `b` are disjoint sets of varnodes.
    #[track_caller]
    fn assert_disjoint(a: &[rsleigh::Vn], b: &[rsleigh::Vn], label: &str) {
        for vn in a {
            assert!(
                !b.contains(vn),
                "{label}: varnode {vn:?} appears in both sets"
            );
        }
    }

    // ── x86-64 SysV ABI ──────────────────────────────────────────────────────

    /// `build` must resolve exactly as many varnodes as there are register
    /// names in each category.
    #[test]
    fn x86_64_sysv_resolves_correct_number_of_registers() {
        let built = build_ok(CallingConvention::x86_64_systemv_abi(), &x86_64_regs());
        assert_eq!(built.arg_passing_regs.len(), 6,  "SysV has 6 int arg registers");
        assert_eq!(built.callee_saved_regs.len(), 7, "SysV has 7 callee-saved registers");
        assert_eq!(built.ret_val_regs.len(), 2,      "SysV has 2 return-value registers");
    }

    /// Arg-passing and callee-saved sets must be disjoint: a register used to
    /// pass arguments cannot also be callee-saved.
    #[test]
    fn x86_64_sysv_arg_and_callee_saved_are_disjoint() {
        let built = build_ok(CallingConvention::x86_64_systemv_abi(), &x86_64_regs());
        assert_disjoint(&built.arg_passing_regs, &built.callee_saved_regs, "x86-64 SysV arg vs callee-saved");
    }

    /// Every arg-passing register must be distinct from every other.
    #[test]
    fn x86_64_sysv_all_arg_registers_are_distinct() {
        let built = build_ok(CallingConvention::x86_64_systemv_abi(), &x86_64_regs());
        assert_all_distinct(&built.arg_passing_regs, "x86-64 SysV arg registers");
    }

    /// All callee-saved registers must be distinct from each other.
    #[test]
    fn x86_64_sysv_all_callee_saved_registers_are_distinct() {
        let built = build_ok(CallingConvention::x86_64_systemv_abi(), &x86_64_regs());
        assert_all_distinct(&built.callee_saved_regs, "x86-64 SysV callee-saved registers");
    }

    /// Return-value registers must be distinct from each other.
    #[test]
    fn x86_64_sysv_return_registers_are_distinct() {
        let built = build_ok(CallingConvention::x86_64_systemv_abi(), &x86_64_regs());
        assert_all_distinct(&built.ret_val_regs, "x86-64 SysV return registers");
    }

    /// On x86-64, all arg/callee-saved/return registers must have the same
    /// 8-byte size (they are full 64-bit registers).
    #[test]
    fn x86_64_sysv_all_resolved_registers_are_8_bytes() {
        let built = build_ok(CallingConvention::x86_64_systemv_abi(), &x86_64_regs());
        for vn in built.arg_passing_regs.iter()
                       .chain(&built.callee_saved_regs)
                       .chain(&built.ret_val_regs) {
            assert_eq!(vn.size, 8, "expected 8-byte register on x86-64, got {:?}", vn);
        }
    }

    // ── x86 cdecl ABI ────────────────────────────────────────────────────────

    /// cdecl passes arguments on the stack; arg list must be empty after build.
    #[test]
    fn x86_cdecl_has_no_arg_passing_registers() {
        let built = build_ok(CallingConvention::x86_cdecl(), &x86_regs());
        assert!(built.arg_passing_regs.is_empty(), "cdecl must have no arg-passing registers");
    }

    /// cdecl return-value registers must be distinct from each other.
    #[test]
    fn x86_cdecl_return_registers_are_distinct() {
        let built = build_ok(CallingConvention::x86_cdecl(), &x86_regs());
        assert_all_distinct(&built.ret_val_regs, "x86 cdecl return registers");
    }

    /// On x86 (32-bit), registers must be 4 bytes (EAX, EDX, etc.).
    #[test]
    fn x86_cdecl_return_registers_are_4_bytes() {
        let built = build_ok(CallingConvention::x86_cdecl(), &x86_regs());
        for vn in &built.ret_val_regs {
            assert_eq!(vn.size, 4, "expected 4-byte register on x86-32, got {:?}", vn);
        }
    }

    // ── cross-architecture invariants ─────────────────────────────────────────

    /// For any supported architecture, building with valid register names
    /// must succeed and produce the expected counts.
    #[test]
    fn build_succeeds_on_every_supported_arch() {
        struct Case { name: &'static str, cc: CallingConvention, regs: rsleigh::SleighRegs }
        let cases = [
            Case { name: "x86-64 SysV", cc: CallingConvention::x86_64_systemv_abi(), regs: x86_64_regs() },
            Case { name: "x86 cdecl",   cc: CallingConvention::x86_cdecl(),           regs: x86_regs() },
        ];
        for Case { name, cc, regs } in cases {
            let built = cc.build(&regs)
                .unwrap_or_else(|e| panic!("{name}: build failed: {e:?}"));
            // The total number of unique registers must be positive (or zero for
            // architectures that pass everything on the stack).
            let total = built.arg_passing_regs.len()
                + built.callee_saved_regs.len()
                + built.ret_val_regs.len();
            assert!(total > 0, "{name}: expected at least one register in some category");
        }
    }

    // ── error handling ────────────────────────────────────────────────────────

    /// An unknown register name in any category must return an error.
    #[test]
    fn build_returns_error_for_unknown_register_name() {
        for bad_name in &["NOTAREG", "", "rax_FAKE"] {
            let cc = CallingConvention {
                arch: crate::arch::SleighArch::x86_64(),
                arg_passing_regs: std::slice::from_ref(bad_name),
                callee_saved_regs: &[],
                ret_val_regs: &[],
            };
            let result = cc.build(&x86_64_regs());
            assert!(
                matches!(result, Err(Error::UnknownRegName(ref n)) if n == bad_name),
                "expected UnknownRegName({bad_name:?}), got {result:?}"
            );
        }
    }

    /// An error on the first unknown name must not silently succeed; the
    /// iterator short-circuits at the first failure.
    #[test]
    fn build_returns_error_even_when_some_names_are_valid() {
        let cc = CallingConvention {
            arch: crate::arch::SleighArch::x86_64(),
            arg_passing_regs: &["RDI", "NOT_A_REG", "RSI"],
            callee_saved_regs: &[],
            ret_val_regs: &[],
        };
        let result = cc.build(&x86_64_regs());
        assert!(result.is_err(), "a list with one bad name must fail");
    }
}
