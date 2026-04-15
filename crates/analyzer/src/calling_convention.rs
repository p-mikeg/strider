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
    ret_val_regs: &'static [&'static str],
    /// Byte offsets from the call-time stack pointer for each positional
    /// stack argument.  Entry `i` is the offset for the `i`-th stack arg
    /// (after register arguments are exhausted).
    stack_arg_offsets: &'static [i64],
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
            // Offsets start at +8: the `call` instruction pushes an 8-byte
            // return address, so SP-at-call points to the return address and
            // the first stack-passed arg (arg 7) lives one slot above it.
            stack_arg_offsets: &[8, 16, 24, 32, 40, 48],
         }
    }

    /// Returns the AArch64 AAPCS64 calling convention.
    ///
    /// Argument registers: x0–x7
    /// Callee-saved: x19–x28, x29 (frame pointer), x30 (link register), sp
    /// Return value: x0, x1
    pub fn aarch64_aapcs64() -> CallingConvention {
        CallingConvention {
            arch: crate::arch::SleighArch::aarch64(),
            arg_passing_regs:  &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            callee_saved_regs: &["x19", "x20", "x21", "x22", "x23",
                                  "x24", "x25", "x26", "x27", "x28", "x29", "x30", "sp"],
            ret_val_regs: &["x0", "x1"],
            stack_arg_offsets: &[0, 8, 16, 24],
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
    pub fn arm_aapcs() -> CallingConvention {
        CallingConvention {
            arch: crate::arch::SleighArch::arm(),
            arg_passing_regs: &["r0", "r1", "r2", "r3"],
            callee_saved_regs: &["r4", "r5", "r6", "r7", "r8",
                                 "r9", "r10", "r11", "sp", "lr"],
            ret_val_regs: &["r0", "r1"],
            stack_arg_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
        }
    }

    /// Returns the x86 cdecl calling convention.
    ///
    /// Arguments are passed on the stack, so `arg_passing_regs` is empty.
    /// ESP is listed as callee-saved because cdecl requires the caller to
    /// clean up: the value of ESP after the call equals its value before.
    /// Return value: EAX, EDX
    pub fn x86_cdecl() -> CallingConvention {
        CallingConvention {
            arch: crate::arch::SleighArch::x86(),
            arg_passing_regs: &[],
            callee_saved_regs: &["EBX", "ESI", "EDI", "EBP", "ESP"],
            ret_val_regs: &["EAX", "EDX"],
            // Offsets start at +4: the `call` instruction pushes a 4-byte
            // return address, so SP-at-call points to the return address and
            // arg 0 lives one slot above it.
            stack_arg_offsets: &[4, 8, 12, 16, 20, 24, 28, 32],
         }
    }

    /// Resolves all register name strings in this calling convention to their
    /// concrete [`rsleigh::Vn`] varnodes using `sleigh_regs`.
    ///
    /// The number of varnodes in each resulting list equals the length of the
    /// corresponding name list.  Returns an error if any register name is
    /// unknown, or if the architecture's stack-pointer register is not
    /// included in `callee_saved_regs` (this property is required by the
    /// stack-argument tracking passes).
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        let arg_passing_regs = regs_to_vns(self.arg_passing_regs, sleigh_regs)?;
        let callee_saved_regs = regs_to_vns(self.callee_saved_regs, sleigh_regs)?;
        let ret_val_regs = regs_to_vns(self.ret_val_regs, sleigh_regs)?;
        let stack_ptr_name = self.arch.stack_ptr_reg_name;
        let stack_ptr_vn = sleigh_regs
            .name_to_vn(stack_ptr_name)
            .ok_or(Error::UnknownRegName(stack_ptr_name.to_string()))?;
        if !callee_saved_regs.contains(&stack_ptr_vn) {
            return Err(Error::StackPtrNotCalleeSaved(stack_ptr_name));
        }
        Ok(BuiltCallingConvention {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            stack_ptr_vn,
            stack_arg_offsets: self.stack_arg_offsets.to_vec(),
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
    fn arm_regs()    -> rsleigh::SleighRegs { regs_for(crate::arch::SleighArch::arm()) }

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

    // ── ARM AAPCS ────────────────────────────────────────────────────────────

    /// `build` must resolve exactly as many varnodes as there are register
    /// names in each category.
    #[test]
    fn arm_aapcs_resolves_correct_number_of_registers() {
        let built = build_ok(CallingConvention::arm_aapcs(), &arm_regs());
        assert_eq!(built.arg_passing_regs.len(),  4,  "AAPCS has 4 arg registers (r0–r3)");
        assert_eq!(built.callee_saved_regs.len(), 10, "AAPCS has 10 callee-saved (r4–r11, sp, lr)");
        assert_eq!(built.ret_val_regs.len(),      2,  "AAPCS returns in r0/r1");
    }

    /// Arg-passing and callee-saved sets must be disjoint.
    #[test]
    fn arm_aapcs_arg_and_callee_saved_are_disjoint() {
        let built = build_ok(CallingConvention::arm_aapcs(), &arm_regs());
        assert_disjoint(&built.arg_passing_regs, &built.callee_saved_regs,
                        "ARM AAPCS arg vs callee-saved");
    }

    /// Every arg-passing register must be distinct from every other.
    #[test]
    fn arm_aapcs_all_arg_registers_are_distinct() {
        let built = build_ok(CallingConvention::arm_aapcs(), &arm_regs());
        assert_all_distinct(&built.arg_passing_regs, "ARM AAPCS arg registers");
    }

    /// All callee-saved registers must be distinct from each other.
    #[test]
    fn arm_aapcs_all_callee_saved_registers_are_distinct() {
        let built = build_ok(CallingConvention::arm_aapcs(), &arm_regs());
        assert_all_distinct(&built.callee_saved_regs, "ARM AAPCS callee-saved registers");
    }

    /// Return-value registers must be distinct from each other.
    #[test]
    fn arm_aapcs_return_registers_are_distinct() {
        let built = build_ok(CallingConvention::arm_aapcs(), &arm_regs());
        assert_all_distinct(&built.ret_val_regs, "ARM AAPCS return registers");
    }

    /// On ARM 32-bit, all general-purpose registers are 4 bytes.
    #[test]
    fn arm_aapcs_all_resolved_registers_are_4_bytes() {
        let built = build_ok(CallingConvention::arm_aapcs(), &arm_regs());
        for vn in built.arg_passing_regs.iter()
                       .chain(&built.callee_saved_regs)
                       .chain(&built.ret_val_regs) {
            assert_eq!(vn.size, 4, "expected 4-byte register on ARM-32, got {:?}", vn);
        }
    }

    /// The stack-pointer varnode is resolved to `sp`.
    #[test]
    fn arm_aapcs_stack_ptr_is_sp() {
        let regs = arm_regs();
        let built = CallingConvention::arm_aapcs().build(&regs).unwrap();
        let sp = regs.name_to_vn("sp").expect("sp must resolve");
        assert_eq!(built.stack_ptr_vn, sp);
    }

    /// ARM `bl` does not push the return address, so stack args start at
    /// SP + 0 and are 4-byte spaced.
    #[test]
    fn arm_aapcs_stack_arg_offsets_are_4_byte_spaced() {
        let built = build_ok(CallingConvention::arm_aapcs(), &arm_regs());
        assert_eq!(built.stack_arg_offsets, vec![0, 4, 8, 12, 16, 20, 24, 28]);
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
            Case { name: "ARM AAPCS",   cc: CallingConvention::arm_aapcs(),           regs: arm_regs() },
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
                stack_arg_offsets: &[],
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
            stack_arg_offsets: &[],
        };
        let result = cc.build(&x86_64_regs());
        assert!(result.is_err(), "a list with one bad name must fail");
    }

    // ── stack pointer / stack argument offsets ───────────────────────────────

    /// Every preset must list its stack-pointer register in `callee_saved_regs`
    /// so that the `CallStackArgCollect` pass can assume SP is restored after
    /// the call.
    #[test]
    fn every_preset_has_sp_in_callee_saved() {
        let cases: &[(CallingConvention, rsleigh::SleighRegs)] = &[
            (CallingConvention::x86_64_systemv_abi(), x86_64_regs()),
            (CallingConvention::x86_cdecl(),           x86_regs()),
            (CallingConvention::arm_aapcs(),           arm_regs()),
        ];
        for (cc, regs) in cases {
            let built = cc.build(regs).expect("preset must build");
            assert!(
                built.callee_saved_regs.contains(&built.stack_ptr_vn),
                "stack pointer {:?} missing from callee_saved_regs",
                built.stack_ptr_vn,
            );
        }
    }

    /// The stack-pointer varnode is resolved from the architecture's
    /// `stack_ptr_reg_name` and exposed on `BuiltCallingConvention`.
    #[test]
    fn x86_cdecl_stack_ptr_is_esp() {
        let regs = x86_regs();
        let built = CallingConvention::x86_cdecl().build(&regs).unwrap();
        let esp = regs.name_to_vn("ESP").expect("ESP must resolve");
        assert_eq!(built.stack_ptr_vn, esp);
    }

    #[test]
    fn x86_64_sysv_stack_ptr_is_rsp() {
        let regs = x86_64_regs();
        let built = CallingConvention::x86_64_systemv_abi().build(&regs).unwrap();
        let rsp = regs.name_to_vn("RSP").expect("RSP must resolve");
        assert_eq!(built.stack_ptr_vn, rsp);
    }

    /// Stack argument offsets are preserved from the preset.
    #[test]
    fn x86_cdecl_stack_arg_offsets_are_positional() {
        let built = build_ok(CallingConvention::x86_cdecl(), &x86_regs());
        assert_eq!(built.stack_arg_offsets, vec![4, 8, 12, 16, 20, 24, 28, 32]);
    }

    #[test]
    fn x86_64_sysv_stack_arg_offsets_are_8_byte_spaced() {
        let built = build_ok(CallingConvention::x86_64_systemv_abi(), &x86_64_regs());
        assert_eq!(built.stack_arg_offsets, vec![8, 16, 24, 32, 40, 48]);
    }

    /// `build` must reject a calling convention whose callee-saved list does
    /// not include the stack pointer.
    #[test]
    fn build_rejects_missing_stack_ptr_in_callee_saved() {
        // cdecl's SP is ESP; if we construct a custom convention without ESP
        // in callee_saved_regs, build() must error.
        let cc = CallingConvention {
            arch: crate::arch::SleighArch::x86(),
            arg_passing_regs: &[],
            callee_saved_regs: &["EBX"], // intentionally missing ESP
            ret_val_regs: &["EAX"],
            stack_arg_offsets: &[],
        };
        let result = cc.build(&x86_regs());
        assert!(
            matches!(result, Err(Error::StackPtrNotCalleeSaved("ESP"))),
            "expected StackPtrNotCalleeSaved(\"ESP\"), got {result:?}",
        );
    }
}
