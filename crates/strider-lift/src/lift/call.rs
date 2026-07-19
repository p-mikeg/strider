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

            // The ABI carries the footprint and whether control returns, so one
            // path serves side-effecting and terminating ops alike.  A
            // `BUG_ON`-class trap is just the empty-footprint `no_return` case.
            strider_target::call_other_abi::CallOtherClass::Call(abi) => {
                self.handle_call_other_modeled(insn, user_op_id, name, &abi)
            }
        }
    }

    fn handle_call_other_modeled(
        &mut self,
        insn: &rsleigh::Insn,
        user_op_id: u64,
        name: &str,
        abi: &strider_target::call_other_abi::CallOtherAbi,
    ) -> Result<()> {
        // Slot 0 is the user-op id, not an argument.
        let explicit_args = self.read_vns(insn.inputs.get(1..).unwrap_or(&[]))?;
        let output_vn: Option<rsleigh::Vn> = insn.output.as_ref().copied();

        // Resolve the ABI register names to Vns exactly once.
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

    /// Implicit-read registers come FIRST in the arg list, before the explicit
    /// pcode operands.
    fn build_abi_call_other(
        &mut self,
        user_op_id: u64,
        name: &str,
        explicit_args: &[strider_ir::Value],
        abi: &strider_target::BuiltCallOtherAbi,
        output: Option<rsleigh::Vn>,
        terminate: bool,
    ) -> Result<Option<strider_ir::Value>> {
        let mut args: Vec<strider_ir::Value> =
            Vec::with_capacity(abi.implicit_reads.len() + explicit_args.len());
        for vn in &abi.implicit_reads {
            args.push(self.read_vn(vn)?);
        }
        args.extend_from_slice(explicit_args);

        // Output vns are the 0-or-1 result then the clobbers, each canonicalized
        // to its largest tracked container (the builder validates that) and
        // deduplicated: sub-registers of one container collapse to a single
        // slot, and the result wins ties over a clobber.
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

        // Full-container writes: an opaque intrinsic defines the whole
        // container.  Result last, so an aliased clobber cannot re-clobber it.
        //
        // Skipped for a terminating op: `build_call_other` already closed the
        // region, so `write_variable` would insert into a terminated region and
        // no successor could read the bindings anyway.  The node's output slots
        // still exist and simply dangle, which matches `no_return` semantics.
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

    fn x86_64_sleigh_regs() -> rsleigh::SleighRegs {
        let arch = strider_target::SleighArch::x86_64();
        arch.probe_regs()
            .expect("probe_regs must succeed for x86_64")
    }

    /// Confirms `CallOtherAbi::build` resolves against the lifter's own
    /// sleigh_regs.
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
