use crate::error::Result;

use super::super::IrStrider;

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    pub(super) fn handle_branch(
        &mut self,
        region_id: cfg::RegionId,
        region_lookup: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId>,
    ) -> Result<()> {
        // Most unconditional p-code `Branch` ops correspond to a `Branch`
        // CFG edge, which we lower into an explicit IR branch.  Per
        // BUG-25 the cfg builder reclassifies a `Branch` whose target is
        // the next machine instruction (clang -O0 idiom on aarch64be /
        // ppc32le — see `crates/cfg/src/cfg/builder/region_builder.rs`)
        // as a `Fallthrough` edge.  In that case the IR-level fallthrough
        // linker (`pipeline.rs`, post-loop pass) will wire the edge using
        // `cur_ctrl` / `cur_memory`, so we must skip emitting an explicit
        // IR branch here — otherwise we'd either fail to find the
        // (non-existent) Branch edge, or double-link the successor.
        if let Some(branch_region) = self.cfg.region_branch(region_id)? {
            let dest_block = region_lookup(branch_region)?;
            self.builder.build_branch(dest_block)?;
            return Ok(());
        }
        if self.cfg.region_fallthrough(region_id)?.is_some() {
            // Fallthrough successor — leave to the post-loop linker.
            return Ok(());
        }
        Err(cfg::Error::from(cfg::ErrorKind::InvalidRegion(region_id)).into())
    }

    pub(super) fn handle_cond_branch(
        &mut self,
        region_id: cfg::RegionId,
        insn: &rsleigh::Insn,
        region_lookup: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId>,
    ) -> Result<()> {
        let cond_raw = self.read_vn(&insn.inputs[1])?;
        // Most archs feed `If` a Bool-typed flag-register or compare result,
        // but a few lift conditional branches off an integer varnode (e.g.
        // ARM's status flags are written as integers when the analyzer's
        // write-side coercion stores them as the variable's declared U8).
        // `build_if` requires Bool, so coerce here at the read site.
        let cond = self.builder.convert_to_bool_if_needed(cond_raw)?;
        let res = self.cfg.region_if(region_id)?;
        let if_true_region = res
            .if_true_region
            .ok_or(cfg::Error::from(cfg::ErrorKind::InvalidRegion(region_id)))?;
        let if_false_region = res
            .if_false_region
            .ok_or(cfg::Error::from(cfg::ErrorKind::InvalidRegion(region_id)))?;
        let true_block = region_lookup(if_true_region)?;
        let false_block = region_lookup(if_false_region)?;
        self.builder.build_if(cond, true_block, false_block)?;
        Ok(())
    }

    /// Lowers a p-code `Return` into the IR's calling-convention-aware return
    /// node, emitting the convention's `ret_val_regs` in ABI order.
    ///
    /// The p-code `Return` op carries a single fabricated input (typically the
    /// popped return address on stack-push ISAs).  That value is *not* an ABI
    /// return slot, so we discard the lifted input here and let the IR resolve
    /// the real return values from the calling convention's resolved register
    /// list.
    pub(super) fn handle_return(&mut self, _insn: &rsleigh::Insn) -> Result<()> {
        let ret_regs = self.builder.ret_val_vars().to_vec();
        self.builder.build_return(None, &ret_regs)?;
        Ok(())
    }

    pub(super) fn handle_call(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Direct call: the target varnode is in the code space and its offset
        // *is* the target address — it's not a pointer to dereference.
        let target_vn = &insn.inputs[0];
        let space_info = self.cfg.sleigh.space_info(target_vn.addr.space);
        let call_address = self
            .builder
            .build_int_const(target_vn.addr.off, space_info.addr_size().try_into()?);
        self.builder.build_call(call_address)?;
        Ok(())
    }

    pub(super) fn handle_call_indirect(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Indirect call: target is a register/memory value holding the address.
        let call_address = self.read_vn(&insn.inputs[0])?;
        self.builder.build_call(call_address)?;
        Ok(())
    }

    /// Lifts a region whose CFG terminator is
    /// [`cfg::RegionTerminator::UnresolvedIndirectBranch`] by emitting
    /// a synthetic single-input `Return(target_value)` that anchors
    /// `target_vn`'s lifted value in the IR.  This is the placeholder
    /// the strider-level outer loop's tier-2 resolver inspects after
    /// the optimiser has run on the full graph.
    ///
    /// We deliberately use a *single-input* Return (no
    /// calling-convention `ret_val_regs` appended) so tier 2 has a
    /// stable anchor for `target_value` at slot 2.  When tier 2
    /// resolves to `LinkRegister`, the in-place edit (lands in R3)
    /// promotes the placeholder into the convention's full ABI
    /// Return; for `Single` tail-call targets it splices a Call+Return
    /// pair.  For both rewrites the placeholder shape is the
    /// canonical "before" state.
    ///
    /// The `(addr, target_value)` pair is recorded on
    /// `IrStrider::unresolved_branches` so tier 2 can walk the table
    /// and correlate each placeholder with the offending pcode
    /// address.  No-op if the table is later consumed (R2/R3); R1.4
    /// just populates it.
    pub(crate) fn handle_unresolved_indirect_branch(
        &mut self,
        target_vn: &rsleigh::Vn,
        addr: cfg::PcodeInsnAddr,
    ) -> Result<()> {
        // Read target_vn through pcode-lift's register-aliasing path
        // so sub-register dispatches (e.g. `jmp *eax` on x86-64) fold
        // correctly via the same Piece/Insert chain the rest of the
        // lifter uses.
        let target_value = self.read_vn(target_vn)?;
        // SINGLE-input Return — no `ret_val_regs`.  Tier 2 reads the
        // value at slot 2 of this Return and inspects its producer.
        self.builder.build_return(Some(target_value), &[])?;
        // Track for tier 2.  Pushing here (rather than at the
        // call-site in pipeline.rs) keeps the side-table population
        // local to the lifting step and avoids leaking the lifter's
        // ir::Value out into the orchestrator.
        self.unresolved_branches.push((addr, target_value));
        Ok(())
    }
}
