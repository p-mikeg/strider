use anyhow::{anyhow, bail, Result};
use strider_ir::node::NodeOutputType;
use rsleigh::Opcode;
use strider_lift::region_driver::RegionDriver;

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
        // side-table.  The `set_lift_addr` / `clear_lift_addr` pair
        // lives in `strider_lift::region_driver::RegionDriver` so the
        // funnel can be reused from the per-terminator handler in
        // `pipeline.rs` and any future incremental lift driver.  We
        // can't use a closure-passing API directly because
        // `process_insn_inner` also borrows `self.cfg` / `self.strider`,
        // which sits next to `self.builder` inside `PerRegionDriver` —
        // splitting into open-call brackets sidesteps the borrow.
        let machine_addr = addr.machine_addr_u64();
        RegionDriver::set_lift_addr(&mut self.builder, Some(machine_addr));
        let res = self.process_insn_inner(region_id, insn, region_lookup);
        RegionDriver::clear_lift_addr(&mut self.builder);
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
        // Coerce the generic closure to a trait object only on the
        // control-flow paths that actually need it; arithmetic/memory
        // arms above never see it.
        match insn.opcode {
            Opcode::Nop => {}
            Opcode::Branch => {
                let lookup: &dyn Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId> = &region_lookup;
                self.handle_branch(region_id, lookup)?
            }
            Opcode::CondBranch => {
                let lookup: &dyn Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId> = &region_lookup;
                self.handle_cond_branch(region_id, insn, lookup)?
            }
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
        let addr = self.read_vn(&insn.inputs[1])?;
        let data = self.read_vn(&insn.inputs[2])?;
        self.builder.build_store(addr, data, space)?;
        Ok(())
    }

    fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
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
        let name = self.cfg.sleigh().user_op_name(user_op_id_u32).ok_or_else(|| {
            anyhow!("CallOther user-op id {user_op_id_u32} not in Sleigh's user_op table")
        })?;

        let class = strider_target::call_other_abi::classify(self.strider.arch.preset(), name)
            .ok_or_else(|| strider_ir::error::UnknownCallOtherError {
                name: name.to_string(),
            })?;

        match class {
            strider_target::call_other_abi::CallOtherClass::NoOp => Ok(()),

            strider_target::call_other_abi::CallOtherClass::NoReturn => {
                let _ = self.builder.build_call_other_terminal(user_op_id, name)?;
                Ok(())
            }

            strider_target::call_other_abi::CallOtherClass::Call(abi) => {
                // 1. Resolve pcode-explicit inputs (args) via the
                //    aliasing-aware value lifter.
                let args: Vec<strider_ir::Value> = if insn.inputs.len() > 1 {
                    insn.inputs[1..]
                        .iter()
                        .map(|vn| self.read_vn(vn))
                        .collect::<Result<_>>()?
                } else {
                    Vec::new()
                };
                let output_ty: Option<NodeOutputType> = match insn.output.as_ref() {
                    Some(out_vn) => Some(out_vn.size.try_into()?),
                    None => None,
                };

                // 2. Resolve ABI register names -> Vns via Sleigh's
                //    cached register table on Strider.
                let regs = &self.strider.sleigh_regs;
                let resolve = |reg_names: &[&str]| -> Result<Vec<rsleigh::Vn>> {
                    reg_names
                        .iter()
                        .map(|n| {
                            regs.name_to_vn(n).ok_or_else(|| {
                                anyhow!(
                                    "user-op {name:?} ABI references unknown register {n:?}"
                                )
                            })
                        })
                        .collect()
                };
                let implicit_reads_vns = resolve(abi.implicit_reads)?;
                let implicit_writes_vns = resolve(abi.implicit_writes)?;

                // 3. Read implicit-read register values via the
                //    aliasing-aware value lifter (so EAX correctly
                //    reads the low 4 bytes of the RAX-tracked variable).
                let implicit_read_values: Vec<strider_ir::Value> = implicit_reads_vns
                    .iter()
                    .map(|vn| self.read_vn(vn))
                    .collect::<Result<_>>()?;

                // 4. Derive the slot kind for each implicit-write from
                //    the Vn's size (clobber slots match the written
                //    register's exact width — strider's write_vn below
                //    inserts any necessary insert/extract for aliasing).
                let implicit_write_kinds: Vec<strider_ir::node::NodeOutputKind> =
                    implicit_writes_vns
                        .iter()
                        .map(|vn| -> Result<strider_ir::node::NodeOutputKind> {
                            Ok(strider_ir::node::NodeOutputKind::OutputType(vn.size.try_into()?))
                        })
                        .collect::<Result<_>>()?;

                // 5. Build the precise CallOther node.
                let (node, value, clobber_outs) = self.builder.build_call_other_modeled(
                    user_op_id,
                    name,
                    &args,
                    output_ty,
                    &implicit_read_values,
                    &implicit_writes_vns,
                    &implicit_write_kinds,
                )?;

                // 6. Memory edge: strider decides whether to advance.
                if abi.memory_edge {
                    let mem_out = self.builder.body().graph.memory_output_of(node)?;
                    self.builder.advance_cur_region_memory(mem_out)?;
                }

                // 7. Rebind tracked variables via the aliasing-aware
                //    write_vn (so EAX clobber updates RAX-tracked
                //    variable through the appropriate insert/extract).
                //
                //    The pcode-explicit output is written first; any
                //    implicit-writes entry that matches `out_vn` is
                //    skipped so the clobber-slot doesn't overwrite the
                //    modeled value.  Concrete case: `rdpkru` emits
                //    `EAX = rdpkru_u32()` in pcode while the ABI table
                //    also lists `EAX` as an implicit-write — without
                //    this skip the modeled CallOther output becomes a
                //    dead node and pattern queries reading EAX see the
                //    clobber slot instead.
                if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), value) {
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
        }
    }
}

