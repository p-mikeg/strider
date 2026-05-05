use anyhow::{anyhow, bail, Result};
use ir::node::NodeOutputType;
use rsleigh::Opcode;

use super::IrStrider;

mod control;

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    /// Translates a single p-code instruction `insn` from `region_id` into
    /// one or more IR nodes.
    ///
    /// Matches on the opcode and delegates to the appropriate `process_*`
    /// helper or inline logic.  `region_lookup` resolves a CFG region id to its
    /// IR counterpart; it is called only for branch and conditional-branch
    /// opcodes.  Unimplemented opcodes return an error.
    pub(super) fn process_insn<F>(
        &mut self,
        region_id: cfg::RegionId,
        insn: &rsleigh::Insn,
        addr: cfg::PcodeInsnAddr,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(cfg::RegionId) -> Result<ir::RegionId>,
    {
        // Set the asm-fingerprint attribution context for every node the
        // builder will produce while handling this pcode insn — value-lifter
        // path, control-flow handlers, store, call, etc.  Cleared on the
        // way out so region-setup helpers (e.g. fallthrough wiring) stay
        // unattributed.  This is the single funnel where every IR node
        // born from a pcode insn picks up its parent machine-instruction
        // address; later optimisation passes only ever absorb fingerprints,
        // never set them.
        let machine_addr = addr.machine_addr.addr;
        self.builder.set_lift_addr(Some(machine_addr));
        let res = self.process_insn_inner(region_id, insn, region_lookup);
        self.builder.set_lift_addr(None);
        res
    }

    fn process_insn_inner<F>(
        &mut self,
        region_id: cfg::RegionId,
        insn: &rsleigh::Insn,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(cfg::RegionId) -> Result<ir::RegionId>,
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
                let lookup: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId> = &region_lookup;
                self.handle_branch(region_id, lookup)?
            }
            Opcode::CondBranch => {
                let lookup: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId> = &region_lookup;
                self.handle_cond_branch(region_id, insn, lookup)?
            }
            Opcode::Store => self.handle_store(insn)?,
            // `Return` and `BranchIndirect` share a handler that emits a
            // calling-convention `Return`.  This is correct for the
            // link-register-return case (e.g. ARM `bx lr`); the cfg
            // builder's tier-1 indirect-branch resolver detects tail
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
        let space = pcode_lift::decode_space_id(insn)?;
        let addr = self.read_vn(&insn.inputs[1])?;
        let data = self.read_vn(&insn.inputs[2])?;
        self.builder.build_store(addr, data, space)?;
        Ok(())
    }

    fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let id_vn = pcode_lift::first_input_or_err(insn)?;
        if id_vn.addr_space != rsleigh::VnSpace::CONST {
            bail!(
                "opcode {:?} expects a CONST input at position 0",
                insn.opcode
            );
        }
        let user_op_id = id_vn.addr_off;
        let user_op_id_u32 = u32::try_from(user_op_id)
            .map_err(|_| anyhow!("CallOther user-op id {user_op_id:#x} exceeds u32"))?;
        let name = self.cfg.sleigh.user_op_name(user_op_id_u32).ok_or_else(|| {
            anyhow!("CallOther user-op id {user_op_id_u32} not in Sleigh's user_op table")
        })?;

        let class = target::user_ops::classify(name).ok_or_else(|| {
            ir::error::UnknownUserOpError {
                name: name.to_string(),
            }
        })?;

        match class {
            target::user_ops::UserOpClass::NoOp => Ok(()),

            target::user_ops::UserOpClass::NoReturn => {
                let _ = self.builder.build_call_other_terminal(user_op_id, name)?;
                Ok(())
            }

            target::user_ops::UserOpClass::Call(abi) => {
                // 1. Resolve pcode-explicit inputs (args) via the
                //    aliasing-aware value lifter.
                let args: Vec<ir::Value> = if insn.inputs.len() > 1 {
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
                let implicit_read_values: Vec<ir::Value> = implicit_reads_vns
                    .iter()
                    .map(|vn| self.read_vn(vn))
                    .collect::<Result<_>>()?;

                // 4. Derive the slot kind for each implicit-write from
                //    the Vn's size (clobber slots match the written
                //    register's exact width — strider's write_vn below
                //    inserts any necessary insert/extract for aliasing).
                let implicit_write_kinds: Vec<ir::node::NodeOutputKind> =
                    implicit_writes_vns
                        .iter()
                        .map(|vn| -> Result<ir::node::NodeOutputKind> {
                            Ok(ir::node::NodeOutputKind::OutputType(vn.size.try_into()?))
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
                    let mem_out = self.builder.body().graph.node_outputs(node)[1];
                    self.builder.advance_cur_region_memory(mem_out)?;
                }

                // 7. Rebind tracked variables via the aliasing-aware
                //    write_vn (so EAX clobber updates RAX-tracked
                //    variable through the appropriate insert/extract).
                if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), value) {
                    self.write_vn(out_vn, val)?;
                }
                for (vn, slot) in implicit_writes_vns.iter().zip(clobber_outs) {
                    self.write_vn(vn, slot)?;
                }

                Ok(())
            }
        }
    }
}

