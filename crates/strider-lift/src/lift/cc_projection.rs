//! Calling-convention register-list projections for the per-CFG lifter.
//!
//! Deriving the ret-val / clobber / return register lists a `Call` /
//! `CallOther` / `Return` node needs from a resolved
//! [`strider_target::BuiltCallingConvention`] is machine-ABI knowledge, so it
//! lives here with the lifter rather than in the target-agnostic IR.  Each CC
//! register is resolved to its largest tracked container (via the IR's
//! `container_of`) before membership / exclusion so a narrower ABI register
//! (`eax`) matches the wider tracked container (`rax`).

use rustc_hash::FxHashSet;

use super::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Resolve `vn` to its largest tracked container (delegates to the IR's
    /// container map).
    fn container_of(&self, vn: &rsleigh::Vn) -> rsleigh::Vn {
        self.builder.function().container_of(vn)
    }

    /// The shared call-clobber predicate: a register (resolved to its tracked
    /// container) is clobbered iff it is neither callee-saved under `cc` nor
    /// the stack pointer.  The callee-saved set is hashed once so the
    /// predicate is O(1) per probe.
    fn clobber_oracle(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> impl Fn(&rsleigh::Vn) -> bool + use<R> {
        let stack_vn = cc.stack_vn;
        let callee_saved: FxHashSet<rsleigh::Vn> = cc
            .callee_saved_regs
            .iter()
            .map(|v| self.container_of(v))
            .collect();
        move |v: &rsleigh::Vn| !callee_saved.contains(v) && *v != stack_vn
    }

    /// The convention's combined return-register list (integer ++ float),
    /// each resolved to its tracked container.
    fn combined_ret_containers<'c>(
        &'c self,
        cc: &'c strider_target::BuiltCallingConvention,
    ) -> impl Iterator<Item = rsleigh::Vn> + 'c {
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .map(|v| self.container_of(v))
    }

    /// Derive the ret-val varnode list for a `Call` built under calling
    /// convention `cc`.  Returns only those tracked, clobbered varnodes that
    /// appear in the convention's combined return-register list
    /// (`ret_val_regs` then `ret_val_regs_float`), in ABI order.
    ///
    /// Each CC register is resolved to its tracked container before membership
    /// is tested, and the resolved CONTAINER is emitted, so a sub-register ABI
    /// ret reg (e.g. `eax`) is kept as the return value when the function
    /// tracks the wider container (`rax`).
    pub(crate) fn call_ret_vals_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        let all_vns = self.builder.function().all_vns();
        let is_clobbered = self.clobber_oracle(cc);
        self.combined_ret_containers(cc)
            .filter(|c| all_vns.contains(c) && is_clobbered(c))
            .collect()
    }

    /// Derive the call-clobbered varnode list for a `Call` built under
    /// calling convention `cc`, in the canonical `all_vns` (allocation) slot
    /// order.  Returns ONLY the non-ret caller-saved registers: a varnode is
    /// clobbered iff it is neither callee-saved nor the stack pointer, AND not
    /// in the convention's combined ret-val register list.
    pub(crate) fn call_clobbered_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        let is_clobbered = self.clobber_oracle(cc);
        let ret_vars: FxHashSet<rsleigh::Vn> = self.combined_ret_containers(cc).collect();
        self.builder
            .function()
            .all_vns()
            .iter()
            .copied()
            .filter(|v| {
                matches!(
                    v.addr_space,
                    rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE
                ) && is_clobbered(v)
                    && !ret_vars.contains(v)
            })
            .collect()
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
