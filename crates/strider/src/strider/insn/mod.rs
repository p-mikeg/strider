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
        if id_vn.addr.space != rsleigh::VnSpace::CONST {
            bail!("opcode {:?} expects a CONST input at position 0", insn.opcode);
        }
        let user_op_id = id_vn.addr.off;
        // Sleigh's native user-op id width is u32; an offset that
        // doesn't fit signals malformed input.  `set_call_other_name`
        // would silently no-op below; surface explicitly so the error
        // is attributable to the lift, not to a downstream
        // `CallOtherElide` miss.
        let user_op_id_u32 = u32::try_from(user_op_id)
            .map_err(|_| anyhow!("CallOther user-op id {user_op_id:#x} exceeds u32"))?;
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
        let (node_id, result) = self
            .builder
            .build_call_other(user_op_id, &args, output_ty)?;
        if let Some(name) = self.cfg.sleigh.user_op_name(user_op_id_u32) {
            self.builder
                .body_mut()
                .graph
                .set_call_other_name(node_id, name.to_string());
        }
        if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), result) {
            self.write_vn(out_vn, val)?;
        }
        Ok(())
    }
}

