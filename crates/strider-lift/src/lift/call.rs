//! The `CallOther` CPU-intrinsic family.
//!
//! `handle_call_other` classifies the user-op (NoOp / NoReturn / modeled
//! Call) and delegates the modeled form to `handle_call_other_modeled`,
//! which resolves the pcode-explicit args + ABI register footprint.  The
//! `decode_user_op` free helper extracts the user-op id + name.
//!
//! Direct / indirect calls (`handle_call`, `handle_call_indirect`) live in
//! the sibling `control` module alongside the other terminator handlers.

use anyhow::{Result, anyhow};

use super::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    pub(super) fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let (user_op_id, name) = decode_user_op(insn, self.lifter.user_op_names())?;
        let class = strider_target::call_other_abi::classify(self.lifter.arch.preset(), name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown CallOther user-op {name:?}; \
                 add an entry to strider_target::call_other_abi::classify"
                )
            })?;

        match class {
            strider_target::call_other_abi::CallOtherClass::NoOp => Ok(()),

            // A modeled user-op.  Its register / memory footprint (possibly
            // empty — a `BUG_ON`-class trap is just the empty-footprint,
            // `no_return: true` case) and whether control returns are all
            // carried by the ABI, so one path handles side-effecting and
            // terminating ops alike.
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
    /// the per-Call CC override map).
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
        // Slot 0 is the user-op id, not a real argument — skip it.
        let explicit_args = self.read_vns(insn.inputs.get(1..).unwrap_or(&[]))?;
        let output_vn: Option<rsleigh::Vn> = insn.output.as_ref().copied();

        // Resolve the ABI register names → Vns exactly once, building the
        // vn-resolved footprint the builder consumes.
        let built_abi = abi.build(&self.lifter.sleigh_regs)?;

        self.build_abi_call_other(
            user_op_id,
            name,
            &explicit_args,
            &built_abi,
            output_vn,
            abi.no_return,
        )?;
        Ok(())
    }

    /// Build a `CallOther` from a vn-resolved ABI footprint — the prod
    /// CallOther orchestration (strider-ir's `build_call_other` is a dumb
    /// node emitter).  Reads the implicit-read registers FIRST (before the
    /// explicit pcode operands), emits the node with the result + implicit-write
    /// clobber output vns, then writes the clobbers and the result back through
    /// the shared vn write path.  Returns the result value, if any.
    fn build_abi_call_other(
        &mut self,
        user_op_id: u64,
        name: &str,
        explicit_args: &[strider_ir::Value],
        abi: &strider_target::BuiltCallOtherAbi,
        output: Option<rsleigh::Vn>,
        terminate: bool,
    ) -> Result<Option<strider_ir::Value>> {
        // Args: implicit-read register values FIRST, then the explicit operands.
        let mut args: Vec<strider_ir::Value> =
            Vec::with_capacity(abi.implicit_reads.len() + explicit_args.len());
        for vn in &abi.implicit_reads {
            args.push(self.read_vn(vn)?);
        }
        args.extend_from_slice(explicit_args);

        // Output vns: the 0-or-1 result, then one per implicit-write clobber —
        // each canonicalized to its largest tracked container (read_reg /
        // write_reg operate on containers, and the builder validates output vns
        // are containers) and deduplicated, since sub-registers of one container
        // collapse to a single slot and the result wins ties over a clobber.
        let result_vn = output.map(|vn| self.container_of(&vn));
        let mut clobber_vns: Vec<rsleigh::Vn> = Vec::new();
        for vn in &abi.implicit_writes {
            let c = self.container_of(vn);
            if Some(c) == result_vn || clobber_vns.contains(&c) {
                continue;
            }
            clobber_vns.push(c);
        }
        let mut output_vns: Vec<rsleigh::Vn> = result_vn.into_iter().collect();
        output_vns.extend_from_slice(&clobber_vns);

        let (node, outputs) = self.builder.build_call_other(
            user_op_id,
            &args,
            &output_vns,
            abi.clobbers_memory,
            terminate,
        )?;
        self.builder
            .function_mut()
            .side_tables_mut()
            .set_call_other_name(node, name);
        let (ret_vals, clobbers) = outputs.split_at(result_vn.iter().count());

        // Writeback: clobbers then the result — both full-container writes via
        // `write_variable` (an opaque intrinsic defines the whole container; an
        // aliased clobber must not re-clobber the result, hence result last).
        //
        // Skipped entirely for a `terminate`ing op: `build_call_other` has
        // already closed the region, so there is no successor to read these
        // bindings and `write_variable` would insert into a terminated region.
        // The node's output slots (result + clobbers) still exist on the node
        // itself and simply dangle — matching a `no_return` op's semantics
        // (control ends; the footprint is recorded but never consumed).
        let result = ret_vals.first().copied();
        if !terminate {
            for (vn, v) in core::iter::zip(&clobber_vns, clobbers) {
                self.builder.write_variable(vn, *v)?;
            }
            if let (Some(c), Some(v)) = (result_vn, result) {
                self.builder.write_variable(&c, v)?;
            }
        }
        Ok(result)
    }
}

/// Decode the user-op id + look up its name from a `CallOther` insn.
/// Extracted from [`FunctionLifter::handle_call_other`]'s preamble.
pub(super) fn decode_user_op<'a>(
    insn: &rsleigh::Insn,
    user_op_names: &'a [String],
) -> Result<(u64, &'a str)> {
    let id_vn = crate::lift::pcode_util::nth_input_or_err(insn, 0)?;
    crate::lift::pcode_util::ensure_const_space(id_vn, insn.opcode, "input 0")?;
    let user_op_id = id_vn.addr_off;
    let user_op_id_u32 = u32::try_from(user_op_id)
        .map_err(|_| anyhow!("CallOther user-op id {user_op_id:#x} exceeds u32"))?;
    let name = user_op_names
        .get(user_op_id_u32 as usize)
        .map(String::as_str)
        .ok_or_else(|| {
            anyhow!("CallOther user-op id {user_op_id_u32} not in Sleigh's user_op table")
        })?;
    Ok((user_op_id, name))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use strider_target::call_other_abi::{CallOtherAbi, CallOtherClass, classify};

    /// Helper: build an x86_64 SleighRegs table for use in unit tests.
    fn x86_64_sleigh_regs() -> rsleigh::SleighRegs {
        let arch = strider_target::SleighArch::x86_64();
        arch.probe_regs()
            .expect("probe_regs must succeed for x86_64")
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
        assert!(
            built.implicit_reads.contains(&rax),
            "RAX must be in implicit_reads"
        );
        assert!(
            built.implicit_writes.contains(&rax),
            "RAX must be in implicit_writes"
        );
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
            no_return: false,
        };
        let result = abi.build(&regs);
        assert!(result.is_err(), "unknown register must produce an error");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("NONEXISTENT_REG_XYZZY"),
            "error must name the bad register"
        );
    }
}
