use crate::error::Result;

use super::super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    pub(super) fn handle_branch(
        &mut self,
        region_id: cfg::RegionId,
        region_lookup: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId>,
    ) -> Result<()> {
        let branch_region = self
            .cfg
            .region_branch(region_id)?
            .ok_or(cfg::Error::from(cfg::ErrorKind::InvalidRegion(region_id)))?;
        let dest_block = region_lookup(branch_region)?;
        self.builder.build_branch(dest_block)?;
        Ok(())
    }

    pub(super) fn handle_cond_branch(
        &mut self,
        region_id: cfg::RegionId,
        insn: &rsleigh::Insn,
        region_lookup: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId>,
    ) -> Result<()> {
        let cond = self.read_vn(&insn.inputs[1])?;
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

    pub(super) fn handle_return(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Emit the calling convention's return-value registers in ABI order —
        // Return inputs become `[ctrl, mem, ret_val_regs[0], ret_val_regs[1], …]`,
        // so pattern queries like `ret.ret_val(0, …)` line up with the ABI's
        // first return register (e.g. rax on x86_64).  The explicit `value`
        // parameter of `build_return` is reserved for synthetic test builds
        // that don't resolve against a real calling convention.
        let ret_regs = self.builder.ret_val_vars().to_vec();
        // Ignore the p-code `return` op's explicit input: on real targets the
        // lifter fabricates one (e.g. the popped return-address value) that
        // does not correspond to any ABI return-value slot.
        let _ = insn;
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
}
