use rsleigh::Opcode;

use crate::error::{ErrorKind, Result};

use super::IrStrider;

mod control;
mod memory;
mod misc;

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
}
