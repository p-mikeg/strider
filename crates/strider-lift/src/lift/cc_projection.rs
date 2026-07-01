//! Calling-convention register-list projections for the per-CFG lifter.
//!
//! Deriving the ret-val / clobber / return register lists a `Call` /
//! `CallOther` / `Return` node needs from a resolved
//! [`strider_target::BuiltCallingConvention`] is machine-ABI knowledge, so it
//! lives here with the lifter rather than in the target-agnostic IR.  Each CC
//! register is resolved to its largest tracked container (via the IR's
//! `container_of`) before membership / exclusion so a narrower ABI register
//! (`eax`) matches the wider tracked container (`rax`).


use super::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Resolve `vn` to its largest tracked container.
    ///
    /// Fast path: the lifter's precomputed `vn_to_container` map (covers every
    /// raw collected varnode + every CC register).  Fallback: an on-the-fly
    /// containment scan of `all_vns` for ad-hoc REGISTER / UNIQUE varnodes not
    /// in the map.  Returns `vn` unchanged when nothing tracked contains it, or
    /// when `vn` is not in an aliasable (REGISTER / UNIQUE) space.
    pub(crate) fn container_of(&self, vn: &rsleigh::Vn) -> rsleigh::Vn {
        self.container_map
            .container_of(self.builder.function().all_vns(), vn)
    }

    /// Derive the ret-val varnode list for a `Call` built under calling
    /// convention `cc` — the tracked, clobbered containers of the combined
    /// return-register list, in ABI order.  Delegates to the SSoT
    /// [`strider_target::BuiltCallingConvention::ret_and_clobber_vns`],
    /// injecting the lifter's O(1) [`Self::container_of`] map.
    pub(crate) fn call_ret_vals_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| self.container_of(v))
            .0
    }

    /// Derive the call-clobbered varnode list (non-ret caller-saved registers)
    /// for a `Call` built under `cc`, in `all_vns` order.  Delegates to the
    /// SSoT [`strider_target::BuiltCallingConvention::ret_and_clobber_vns`].
    pub(crate) fn call_clobbered_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| self.container_of(v))
            .1
    }

    /// The calling convention's combined return-value register list (integer
    /// then float, in ABI order), at each register's declared width — no
    /// tracked-container projection.  The registers are read through the
    /// aliasing-aware `read_vn` at use sites, which resolves each declared
    /// register to its tracked container, so the raw declared list is the
    /// right shape.
    pub(crate) fn cc_ret_val_regs(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .copied()
            .collect()
    }
}
