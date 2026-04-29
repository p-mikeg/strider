use ir::node::NodeOutputType;
use rsleigh::Opcode;

use crate::error::{ErrorKind, Result};

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
        _addr: cfg::PcodeInsnAddr,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(cfg::RegionId) -> Result<ir::RegionId>,
    {
        // Coerce the generic closure to a trait object so control-flow helpers
        // in sibling modules don't need to be generic on `F`.
        let region_lookup_dyn: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId> = &region_lookup;
        // Try the pcode-lift value lifter first.  It returns `Ok(true)` for
        // value-producing opcodes (and is responsible for the IR-builder
        // calls); `Ok(false)` for control-flow / call / store ops which the
        // match arm below handles.
        if self.value_lifter().lift(insn)? {
            return Ok(());
        }
        match insn.opcode {
            Opcode::Nop => {}
            Opcode::Branch => self.handle_branch(region_id, region_lookup_dyn)?,
            Opcode::CondBranch => self.handle_cond_branch(region_id, insn, region_lookup_dyn)?,
            Opcode::Store => self.handle_store(insn)?,
            // `Return` and `BranchIndirect` share a handler.  The
            // BranchIndirect classification is **only correct for the
            // function-return case** (target = link register, e.g. ARM
            // `bx lr` / `pop {pc}`, MIPS `jr ra`).  Other BranchIndirect
            // sources are misclassified — the analyzer here treats them
            // all as Returns:
            //
            //   * Real tail call (`bx <target>` after computing target):
            //     should be Call + Return.  Our fixtures suppress real
            //     tail calls via `-fno-optimize-sibling-calls`, so this
            //     case doesn't fire here, but external binaries will
            //     lose the call site information.
            //   * Jump table (`ldr pc, [tbl + idx*4]`): should produce
            //     N successor edges, one per case label.  Our fixtures
            //     don't compile any switch as a jump table, so this
            //     case doesn't fire either.
            //   * Computed goto (`goto *ptr`): should be an intra-
            //     function indirect dispatch.  Not present in fixtures.
            //
            // A cleaner future refinement would inspect `insn.inputs[0]`
            // to detect link-register reads vs other targets, but
            // distinguishing the four cases requires data-flow analysis
            // that the per-instruction handler doesn't have.  Left as a
            // known limitation — see `analyzer-known-issues` BUG-5.
            Opcode::Return | Opcode::BranchIndirect => self.handle_return(insn)?,
            Opcode::Call => self.handle_call(insn)?,
            Opcode::CallIndirect => self.handle_call_indirect(insn)?,

            // ── remaining Sleigh opcodes ──────────────────────────────────────

            // MultiEqual is a decompiler-internal phi; raw p-code should not
            // contain it.  Report instead of guessing semantics.
            Opcode::MultiEqual => {
                return Err(ErrorKind::UnexpectedDecompilerOpcode(insn.opcode).into());
            }

            // CallOther: user-defined CPU intrinsic (cpuid, rdtsc, syscall, …).
            // inputs[0] is a CONST user-op id; remaining inputs are arguments.
            // Clobbers memory.  The instruction's output varnode, if present,
            // receives the intrinsic's result value.  Stays in strider (not
            // pcode-lift) because it touches the memory chain and resolves
            // user-op names against the sleigh context strider owns.
            Opcode::CallOther => self.handle_call_other(insn)?,

            _ => return Err(ErrorKind::UnimplementedOpcode(insn.opcode).into()),
        }
        Ok(())
    }

    fn handle_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = decode_space_id(insn)?;
        let addr = self.read_vn(&insn.inputs[1])?;
        let data = self.read_vn(&insn.inputs[2])?;
        self.builder.build_store(addr, data, space)?;
        Ok(())
    }

    fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        if insn.inputs.is_empty() {
            return Err(ErrorKind::TooFewInputs(insn.opcode, 1, 0).into());
        }
        let id_vn = &insn.inputs[0];
        if id_vn.addr.space != rsleigh::VnSpace::CONST {
            return Err(ErrorKind::ExpectedConstInput(insn.opcode, 0).into());
        }
        let user_op_id = id_vn.addr.off;
        let args: Vec<ir::Value> = insn.inputs[1..]
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;
        let output_ty: Option<NodeOutputType> = match insn.output.as_ref() {
            Some(out_vn) => Some(out_vn.size.try_into()?),
            None => None,
        };
        let (node_id, result) = self
            .builder
            .build_call_other(user_op_id, &args, output_ty)?;
        // Resolve the user-op id to its Sleigh-defined name (e.g.
        // `setISAMode`, `LOCK`, `cpuid`) and stash it in the graph's
        // side-table.  Used by `opt::CallOtherElide` to drop CallOthers
        // whose effect is a true no-op in the IR's value/memory model.
        // u32 is sleigh's native id width — anything wider is malformed.
        if let Ok(id_u32) = u32::try_from(user_op_id)
            && let Some(name) = self.cfg.sleigh.user_op_name(id_u32)
        {
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

/// Decodes the target address space of a p-code `LOAD`/`STORE`.
///
/// P-code encodes the target space as a CONST-space varnode at `inputs[0]`
/// whose offset is a pointer to the Sleigh `AddrSpace` object. Reading
/// `.addr.space` directly yields `CONST` (the space of that encoding varnode),
/// not the actual target space — callers that care about the target must
/// decode via [`rsleigh::VnSpace::by_id`].
fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
    let space_id_vn = *insn
        .inputs
        .first()
        .ok_or(ErrorKind::TooFewInputs(insn.opcode, 1, 0))?;
    if space_id_vn.addr.space != rsleigh::VnSpace::CONST {
        return Err(ErrorKind::ExpectedConstInput(insn.opcode, 0).into());
    }
    // SAFETY: `space_id_vn` is the `inputs[0]` of a LOAD/STORE p-code insn and
    // was just verified to live in CONST space, which is the precondition of
    // `VnSpace::by_id`.
    Ok(unsafe { rsleigh::VnSpace::by_id(space_id_vn) })
}
