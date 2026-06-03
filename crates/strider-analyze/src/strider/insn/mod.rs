use anyhow::{anyhow, bail, Result};
use strider_lift::pcode_lift::nth_input_or_err;
use rsleigh::Opcode;

use super::PerRegionDriver;

mod control;

impl<'a, R: rsleigh::MemReader> PerRegionDriver<'a, R> {
    /// Translates a single p-code instruction `insn` from `region_id` into
    /// one or more IR nodes.
    ///
    /// Matches on the opcode and delegates to the appropriate `process_*`
    /// helper or inline logic.  `region_lookup` resolves a CFG region id to its
    /// IR counterpart; it is called only for branch and conditional-branch
    /// opcodes.  Unimplemented opcodes return an error.
    pub(super) fn process_insn<F>(
        &mut self,
        region_id: strider_lift::cfg::RegionId,
        insn: &rsleigh::Insn,
        addr: strider_lift::cfg::PcodeInsnAddr,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId>,
    {
        // Funnel: every IR node born from this pcode insn picks up the
        // parent machine-instruction address in its asm-fingerprint
        // side-table.  The set_lift_addr(Some)/set_lift_addr(None)
        // bracket is the funnel.  A closure API would force a `&mut
        // self` plus a `&mut self.builder` split the borrow checker
        // rejects, so we use open-call brackets instead.
        let machine_addr = addr.machine_addr.addr;
        self.builder.set_lift_addr(Some(machine_addr));
        let res = self.process_insn_inner(region_id, insn, region_lookup);
        self.builder.set_lift_addr(None);
        res
    }

    fn process_insn_inner<F>(
        &mut self,
        region_id: strider_lift::cfg::RegionId,
        insn: &rsleigh::Insn,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId>,
    {
        // Try the pcode-lift value lifter first.  It returns `Ok(true)` for
        // value-producing opcodes (`Add`, `Load`, casts, …) and `Ok(false)`
        // for control-flow / call / store ops the match arm below handles.
        if self.value_lifter().lift(insn)? {
            return Ok(());
        }
        // `handle_branch` / `handle_cond_branch` take `&dyn Fn(...)`;
        // `&region_lookup` (generic `F: Fn(...)`) coerces to the trait
        // object at the call boundary, so no explicit cast is needed.
        match insn.opcode {
            Opcode::Nop => {}
            Opcode::Branch => self.handle_branch(region_id, &region_lookup)?,
            Opcode::CondBranch => self.handle_cond_branch(region_id, insn, &region_lookup)?,
            Opcode::Store => self.handle_store(insn)?,
            // `Return` and `BranchIndirect` share a handler that emits a
            // calling-convention `Return`.  This is correct for the
            // link-register-return case (e.g. ARM `bx lr`); the cfg
            // builder's cfg-time mini-graph resolver detects tail
            // calls / jump tables / computed gotos and routes them via
            // dedicated terminators (`Switch`, `UnresolvedIndirectBranch`),
            // both handled in the special-terminator post-pass.
            Opcode::Return | Opcode::BranchIndirect => self.handle_return(insn)?,
            Opcode::Call => self.handle_call(insn)?,
            Opcode::CallIndirect => self.handle_call_indirect(insn)?,
            // GHIDRA's MULTIEQUAL is a decompiler-internal phi that
            // `rsleigh::Sleigh::lift_one` does not emit.  Surfacing it
            // here means rsleigh's contract changed; surface as an
            // error rather than guessing semantics.
            Opcode::MultiEqual => {
                bail!(
                    "opcode {:?} is a decompiler-internal phi; rsleigh::lift_one is contracted not to emit it",
                    insn.opcode
                );
            }
            // CallOther: user-defined CPU intrinsic (cpuid, rdtsc, syscall, …).
            // inputs[0] is a CONST user-op id; remaining inputs are arguments.
            // Clobbers memory.  The instruction's output varnode, if present,
            // receives the intrinsic's result value.
            Opcode::CallOther => self.handle_call_other(insn)?,
            _ => bail!("unimplemented p-code opcode {:?}", insn.opcode),
        }
        Ok(())
    }

    fn handle_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = strider_lift::pcode_lift::decode_space_id(insn)?;
        let addr = self.read_vn(nth_input_or_err(insn, 1)?)?;
        let data = self.read_vn(nth_input_or_err(insn, 2)?)?;
        self.builder.build_store(addr, data, space)?;
        Ok(())
    }

    fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let (user_op_id, name) = decode_user_op(insn, self.sleigh)?;
        let class = strider_target::call_other_abi::classify(self.strider.arch.preset(), name)
            .ok_or_else(|| anyhow::anyhow!(
                "unknown CallOther user-op {name:?}; \
                 add an entry to strider_target::call_other_abi::classify"
            ))?;

        match class {
            strider_target::call_other_abi::CallOtherClass::NoOp => Ok(()),

            strider_target::call_other_abi::CallOtherClass::NoReturn => {
                // A NoReturn trap (Linux `BUG_ON`-class) emits a
                // CallOther with only ctrl + mem (no args / clobbers /
                // value).  terminate=true closes the region as part of
                // the build_call_other call — no separate
                // mark_cur_region_terminated needed.  The empty footprint
                // carries no implicit reads/writes and does not advance
                // memory.
                let empty_abi = strider_target::BuiltCallOtherAbi {
                    implicit_reads: Vec::new(),
                    implicit_writes: Vec::new(),
                    clobbers_memory: false,
                };
                let _ = self.builder.build_call_other(
                    user_op_id,
                    name,
                    None,
                    &[],
                    &empty_abi,
                    None,
                    true,
                )?;
                Ok(())
            }

            strider_target::call_other_abi::CallOtherClass::Call(abi) => {
                self.handle_call_other_modeled(insn, user_op_id, name, &abi)
            }
        }
    }

    /// Handle the `CallOtherClass::Call(abi)` arm of
    /// [`Self::handle_call_other`] — the only modeled form.  Resolves the
    /// pcode-explicit operands + the ABI register names, hands the
    /// vn-resolved [`strider_target::BuiltCallOtherAbi`] to the builder
    /// (which owns the implicit-footprint resolution: reading implicit
    /// reads, emitting + tagging clobbers, advancing memory, writing the
    /// clobbers back, writing the result back to `output`, and recording
    /// the `CallDescriptor`).
    fn handle_call_other_modeled(
        &mut self,
        insn: &rsleigh::Insn,
        user_op_id: u64,
        name: &str,
        abi: &strider_target::call_other_abi::CallOtherAbi,
    ) -> Result<()> {
        // Resolve pcode-explicit inputs (args) via the aliasing-aware
        // value lifter.  The result destination (if any) is now written by
        // the builder via `write_reg_vn`, so it must name a register /
        // unique varnode (the builder enforces this).
        let explicit_args = self.read_call_other_args(insn)?;
        let output_vn: Option<rsleigh::Vn> = insn.output.as_ref().copied();

        // Resolve the ABI register names → Vns exactly once, building the
        // vn-resolved footprint the builder consumes.
        let built_abi = abi.build(&self.strider.sleigh_regs)?;

        // The builder reads the implicit reads, emits + tags the clobbers,
        // advances memory per `clobbers_memory`, writes each clobber back,
        // writes the result back to `output`, and records the
        // `CallDescriptor::CallOther` footprint.  The result writeback now
        // lives in the builder — the lifter no longer touches it.
        let _ = self.builder.build_call_other(
            user_op_id,
            name,
            None,
            &explicit_args,
            &built_abi,
            output_vn,
            false,
        )?;

        Ok(())
    }

    /// Read every p-code-explicit input past slot 0 (the user-op id)
    /// as a value via the aliasing-aware value lifter.  Slot 0 is
    /// excluded because it carries the user-op id, not a real argument.
    /// Called by [`Self::handle_call_other_modeled`].
    fn read_call_other_args(&mut self, insn: &rsleigh::Insn) -> Result<Vec<strider_ir::Value>> {
        if insn.inputs.len() > 1 {
            insn.inputs
                .get(1..)
                .unwrap_or(&[])
                .iter()
                .map(|vn| self.read_vn(vn))
                .collect()
        } else {
            Ok(Vec::new())
        }
    }

}

/// Decode the user-op id + look up its name from a `CallOther` insn.
/// Extracted from [`PerRegionDriver::handle_call_other`]'s preamble.
fn decode_user_op<'a, R: rsleigh::MemReader>(
    insn: &rsleigh::Insn,
    sleigh: &'a rsleigh::Sleigh<R>,
) -> Result<(u64, &'a str)> {
    let id_vn = strider_lift::pcode_lift::first_input_or_err(insn)?;
    if id_vn.addr_space != rsleigh::VnSpace::CONST {
        bail!(
            "opcode {:?} expects a CONST input at position 0",
            insn.opcode
        );
    }
    let user_op_id = id_vn.addr_off;
    let user_op_id_u32 = u32::try_from(user_op_id)
        .map_err(|_| anyhow!("CallOther user-op id {user_op_id:#x} exceeds u32"))?;
    let name = sleigh.user_op_name(user_op_id_u32).ok_or_else(|| {
        anyhow!("CallOther user-op id {user_op_id_u32} not in Sleigh's user_op table")
    })?;
    Ok((user_op_id, name))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use strider_target::call_other_abi::{CallOtherAbi, classify, CallOtherClass};

    /// Helper: build an x86_64 SleighRegs table for use in unit tests.
    fn x86_64_sleigh_regs() -> rsleigh::SleighRegs {
        let arch = strider_target::SleighArch::x86_64();
        arch.probe_regs().expect("probe_regs must succeed for x86_64")
    }

    /// Thin integration test: `CallOtherAbi::build` (defined in strider-target)
    /// resolves the x86_64 syscall ABI to the correct vns via the lifter's
    /// sleigh_regs.  The build-level tests live in strider-target; this test
    /// confirms that the same `build` call works end-to-end from the analyze
    /// crate's perspective.
    #[test]
    fn call_other_abi_build_syscall_x86_64() {
        let regs = x86_64_sleigh_regs();
        let abi = match classify(strider_target::ArchPreset::X86_64, "syscall")
            .expect("syscall must classify")
        {
            CallOtherClass::Call(abi) => abi,
            other => panic!("expected Call(abi), got {other:?}"),
        };

        let built = abi.build(&regs).expect("syscall ABI must build on x86_64");

        let rax = regs.name_to_vn("RAX").expect("RAX must exist");
        assert!(built.implicit_reads.contains(&rax), "RAX must be in implicit_reads");
        assert!(built.implicit_writes.contains(&rax), "RAX must be in implicit_writes");
        assert!(built.clobbers_memory, "syscall must clobber memory");
    }

    /// `CallOtherAbi::build` returns an error for an unknown register name,
    /// and the error message names the bad register.
    #[test]
    fn call_other_abi_build_unknown_register_errors() {
        let regs = x86_64_sleigh_regs();
        let abi = CallOtherAbi {
            implicit_reads: &["NONEXISTENT_REG_XYZZY"],
            implicit_writes: &[],
            clobbers_memory: false,
        };
        let result = abi.build(&regs);
        assert!(result.is_err(), "unknown register must produce an error");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("NONEXISTENT_REG_XYZZY"), "error must name the bad register");
    }
}
