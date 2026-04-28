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
        addr: cfg::PcodeInsnAddr,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(cfg::RegionId) -> Result<ir::RegionId>,
    {
        // Coerce the generic closure to a trait object so control-flow helpers
        // in sibling modules don't need to be generic on `F`.
        let region_lookup_dyn: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId> = &region_lookup;
        // F1: convert cfg's PcodeInsnAddr into ir's; the two are
        // structurally identical but live in separate crates to avoid the
        // ir → cfg dependency cycle.
        let ir_addr = ir::PcodeInsnAddr::new(addr.machine_addr.addr, addr.insn_index);
        // F1: snapshot the node-arena length before dispatch so we can seed
        // every node freshly created by this pcode insn's lift with `ir_addr`.
        // Covers both the value-lifter path (lift_with_addr below — its own
        // seeding is then a no-op merge) and the control-flow / call / store
        // arms whose handlers don't go through the value lifter.
        let pre_count = self.builder.body().graph.node_count();
        // Try the pcode-lift value lifter first.  It returns `Ok(true)` for
        // value-producing opcodes (and is responsible for the IR-builder
        // calls); `Ok(false)` for control-flow / call / store ops which the
        // match arm below handles.
        //
        // F1: lift_with_addr seeds every newly-created node's fingerprint
        // with this pcode insn's address, so pattern matches downstream can
        // trace back to the originating disassembly.
        if self.value_lifter().lift_with_addr(insn, ir_addr)? {
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
        // F1: seed every node created by the control-flow / call / store
        // handlers (the value lifter's own path returns early above so we
        // only reach this for non-value-producing opcodes).  Merge instead
        // of overwrite so a node whose fingerprint was already populated by
        // an inner create_node auto-merge keeps that provenance.
        let post_count = self.builder.body().graph.node_count();
        let seed = ir::Fingerprint::from_single(ir_addr);
        for raw in pre_count..post_count {
            let node_id = ir::node::NodeId::from_u32(raw as u32);
            let merged = ir::Fingerprint::merge(
                self.builder.body().graph.fingerprint_of(node_id),
                &seed,
            );
            self.builder
                .body_mut()
                .graph
                .set_fingerprint(node_id, merged);
        }
        Ok(())
    }
}
