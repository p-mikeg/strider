//! Every CC register is resolved to its largest tracked container before
//! membership or exclusion, so a narrower ABI register (`eax`) matches the
//! tracked container (`rax`).

use super::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// O(1) via the precomputed `container_map`, falling back to an `all_vns`
    /// containment scan for ad-hoc varnodes not in it.  Returns `vn` unchanged
    /// when nothing tracked contains it, or when `vn` is not in an aliasable
    /// (REGISTER / UNIQUE) space.
    pub(crate) fn container_of(&self, vn: &rsleigh::Vn) -> rsleigh::Vn {
        self.container_map
            .container_of(self.builder.function().all_vns(), vn)
    }

    /// Ret-val varnodes in ABI order.  Test-only: production `build_cc_call`
    /// takes both halves from a single `ret_and_clobber_vns` scan.
    #[cfg(test)]
    pub(crate) fn call_ret_vals_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| self.container_of(v))
            .0
    }

    /// Non-ret caller-saved varnodes in `all_vns` order.  Test-only, same as
    /// [`Self::call_ret_vals_for`].
    #[cfg(test)]
    pub(crate) fn call_clobbered_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| self.container_of(v))
            .1
    }
}
