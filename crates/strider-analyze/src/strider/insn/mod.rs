use anyhow::{anyhow, bail, Result};
use strider_ir::node::ValueType;
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
                // mark_cur_region_terminated needed.
                let _ = self.builder.build_call_other(
                    user_op_id,
                    name,
                    None,
                    &[],
                    &[],
                    &[],
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
    /// [`Self::handle_call_other`] — the only modeled form.  The body
    /// is structured as seven small helpers below (read implicit-reads,
    /// resolve per-instruction clobber set, advance current region's
    /// control/memory, emit the CallOther node, write back implicit-
    /// writes, etc.).  Extracted to keep the parent dispatch terse.
    fn handle_call_other_modeled(
        &mut self,
        insn: &rsleigh::Insn,
        user_op_id: u64,
        name: &str,
        abi: &strider_target::call_other_abi::CallOtherAbi,
    ) -> Result<()> {
        // 1. Resolve pcode-explicit inputs (args) via the aliasing-aware
        //    value lifter.
        let args = self.read_call_other_args(insn)?;
        let output_ty: Option<ValueType> = match insn.output.as_ref() {
            Some(out_vn) => Some(strider_ir::ValueType::int_for_byte_size(out_vn.size)?),
            None => None,
        };

        // 2+3. Resolve ABI register names → Vns exactly once per list.
        // implicit_writes_vns is resolved first (immutable); implicit_reads_vns
        // is resolved next before any &mut self call so the borrow checker is
        // satisfied.  Read values are derived from the already-resolved vns.
        let implicit_writes_vns = self.resolve_abi_regs(name, abi.implicit_writes)?;
        let implicit_reads_vns = self.resolve_abi_regs(name, abi.implicit_reads)?;
        // `implicit_reads_vns` is an owned Vec, so iterating over it while
        // calling `&mut self` methods poses no borrow-checker conflict.
        let implicit_read_values: Vec<strider_ir::Value> = implicit_reads_vns
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;

        // 4. Derive the slot kind for each implicit-write from the
        //    Vn's size (clobber slots match the written register's
        //    exact width — strider's write_vn below inserts any
        //    necessary insert/extract for aliasing).
        let implicit_write_kinds: Vec<strider_ir::node::ValueKind> = implicit_writes_vns
            .iter()
            .map(|vn| -> Result<strider_ir::node::ValueKind> {
                Ok(strider_ir::node::ValueKind::Typed(strider_ir::ValueType::int_for_byte_size(vn.size)?))
            })
            .collect::<Result<_>>()?;

        // 5. Build the precise CallOther node.  `build_call_other` takes
        //    a single `arg_values` channel, so the pcode-explicit args
        //    and the ABI implicit-reads are concatenated here (args
        //    first, then implicit-reads) to preserve the old input
        //    layout `[ctrl, mem, *args, *implicit_reads]`.
        let arg_values: Vec<strider_ir::Value> = args
            .iter()
            .copied()
            .chain(implicit_read_values.iter().copied())
            .collect();
        let (node, value, clobber_outs) = self.builder.build_call_other(
            user_op_id,
            name,
            None,
            &arg_values,
            &implicit_writes_vns,
            &implicit_write_kinds,
            output_ty,
            false,
        )?;

        // 6. Memory edge: strider decides whether to advance.  Any
        //    non-empty mem-clobber set advances the unified memory
        //    token so subsequent loads/stores observe the call.
        //    StackOffsetDetect (the post-pass) reads `abi.mem_clobbers` to
        //    decide which per-partition chains to break across this
        //    CallOther.
        if abi.clobbers_memory {
            let mem_value = self.builder.function().graph().memory_output_of(node)?;
            self.builder.advance_cur_region_memory(mem_value)?;
        }

        // 7. Rebind tracked variables via the aliasing-aware write_vn.
        self.write_implicit_clobbers(insn, value, &implicit_writes_vns, clobber_outs)?;

        // 8. Record the vn-resolved ABI footprint for the CallOther node so
        //    downstream passes and pattern queries can recover the full
        //    register + memory footprint without re-resolving names.
        //    Constructed directly from the already-resolved vns — no second
        //    name-lookup pass.
        let built_abi = strider_target::BuiltCallOtherAbi {
            implicit_reads: implicit_reads_vns,
            implicit_writes: implicit_writes_vns,
            clobbers_memory: abi.clobbers_memory,
        };
        self.builder
            .function_mut()
            .set_call_descriptor(node, strider_ir::CallDescriptor::CallOther(built_abi));

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

    /// Rebind tracked variables for the pcode-explicit output and each
    /// implicit-write clobber slot.  The pcode-explicit output is
    /// written first; any implicit-writes entry that matches `out_vn`
    /// is skipped so the clobber-slot doesn't overwrite the modeled
    /// value.  Called by [`Self::handle_call_other_modeled`].
    ///
    /// Concrete case: `rdpkru` emits `EAX = rdpkru_u32()` in pcode while
    /// the ABI table also lists `EAX` as an implicit-write — without this
    /// skip the modeled CallOther output becomes a dead node.
    fn write_implicit_clobbers(
        &mut self,
        insn: &rsleigh::Insn,
        modeled_value: Option<strider_ir::Value>,
        implicit_writes_vns: &[rsleigh::Vn],
        clobber_outs: Vec<strider_ir::Value>,
    ) -> Result<()> {
        if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), modeled_value) {
            self.write_vn(out_vn, val)?;
        }
        for (vn, slot) in implicit_writes_vns.iter().zip(clobber_outs) {
            if insn.output.as_ref() == Some(vn) {
                continue;
            }
            self.write_vn(vn, slot)?;
        }
        Ok(())
    }

    /// Resolve an ABI-table register-name list against the cached
    /// Sleigh register table.  Surface an unknown name as a typed
    /// error referencing the user-op for traceability.
    fn resolve_abi_regs(&self, op_name: &str, reg_names: &[&str]) -> Result<Vec<rsleigh::Vn>> {
        resolve_abi_regs_impl(&self.strider.sleigh_regs, op_name, reg_names)
    }
}

/// Resolve a register-name list against a Sleigh register table.
/// Surface an unknown name as a typed error referencing the user-op for
/// traceability.
///
/// Exposed as a free function so tests can call it without constructing
/// a full `PerRegionDriver`.
fn resolve_abi_regs_impl(
    sleigh_regs: &rsleigh::SleighRegs,
    op_name: &str,
    reg_names: &[&str],
) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|n| {
            sleigh_regs.name_to_vn(n).ok_or_else(|| {
                anyhow!(
                    "user-op {op_name:?} ABI references unknown register {n:?}"
                )
            })
        })
        .collect()
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

    use super::resolve_abi_regs_impl;
    use strider_target::call_other_abi::{classify, CallOtherClass};

    /// Helper: build an x86_64 SleighRegs table for use in unit tests.
    fn x86_64_sleigh_regs() -> rsleigh::SleighRegs {
        let arch = strider_target::SleighArch::x86_64();
        arch.probe_regs().expect("probe_regs must succeed for x86_64")
    }

    /// The struct-literal `BuiltCallOtherAbi` path (what
    /// `handle_call_other_modeled` now builds at step 8) produces the same
    /// vns as directly resolving each name list and preserves `clobbers_memory`.
    #[test]
    fn built_call_other_abi_syscall_x86_64() {
        let regs = x86_64_sleigh_regs();

        let abi = match classify(strider_target::ArchPreset::X86_64, "syscall")
            .expect("syscall must classify")
        {
            CallOtherClass::Call(abi) => abi,
            other => panic!("expected Call(abi), got {other:?}"),
        };

        // Mirror the struct-literal construction from handle_call_other_modeled
        // step 8: resolve each list once, then build directly.
        let implicit_reads_vns =
            resolve_abi_regs_impl(&regs, "syscall", abi.implicit_reads)
                .expect("implicit_reads must resolve for syscall on x86_64");
        let implicit_writes_vns =
            resolve_abi_regs_impl(&regs, "syscall", abi.implicit_writes)
                .expect("implicit_writes must resolve for syscall on x86_64");
        let built = strider_target::BuiltCallOtherAbi {
            implicit_reads: implicit_reads_vns,
            implicit_writes: implicit_writes_vns,
            clobbers_memory: abi.clobbers_memory,
        };

        // The resolved Vns must be the same as individually looking up each name.
        let expected_reads: Vec<rsleigh::Vn> = abi
            .implicit_reads
            .iter()
            .map(|n| regs.name_to_vn(n).unwrap_or_else(|| panic!("reg {n:?} not found")))
            .collect();
        let expected_writes: Vec<rsleigh::Vn> = abi
            .implicit_writes
            .iter()
            .map(|n| regs.name_to_vn(n).unwrap_or_else(|| panic!("reg {n:?} not found")))
            .collect();

        assert_eq!(built.implicit_reads, expected_reads, "implicit_reads mismatch");
        assert_eq!(built.implicit_writes, expected_writes, "implicit_writes mismatch");
        assert_eq!(built.clobbers_memory, abi.clobbers_memory, "clobbers_memory mismatch");

        // Spot-check: syscall reads RAX and writes RAX per the ABI table.
        let rax = regs.name_to_vn("RAX").expect("RAX must exist");
        assert!(built.implicit_reads.contains(&rax), "RAX must be in implicit_reads");
        assert!(built.implicit_writes.contains(&rax), "RAX must be in implicit_writes");
        assert!(built.clobbers_memory, "syscall must clobber memory");
    }

    /// Empty register channels produce empty Vec lists and preserve
    /// `clobbers_memory = false`.
    #[test]
    fn built_call_other_abi_empty_channels() {
        let regs = x86_64_sleigh_regs();
        let implicit_reads_vns =
            resolve_abi_regs_impl(&regs, "rdtsc", &[]).expect("empty list must succeed");
        let implicit_writes_vns =
            resolve_abi_regs_impl(&regs, "rdtsc", &[]).expect("empty list must succeed");
        let built = strider_target::BuiltCallOtherAbi {
            implicit_reads: implicit_reads_vns,
            implicit_writes: implicit_writes_vns,
            clobbers_memory: false,
        };
        assert!(built.implicit_reads.is_empty());
        assert!(built.implicit_writes.is_empty());
        assert!(!built.clobbers_memory);
    }

    /// `resolve_abi_regs_impl` returns an error for an unknown register name.
    #[test]
    fn resolve_abi_regs_unknown_register_errors() {
        let regs = x86_64_sleigh_regs();
        let result = resolve_abi_regs_impl(&regs, "test_op", &["NONEXISTENT_REG_XYZZY"]);
        assert!(result.is_err(), "unknown register must produce an error");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("NONEXISTENT_REG_XYZZY"), "error must name the bad register");
    }
}
