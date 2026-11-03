//! Every CC register is resolved to its largest tracked container before
//! membership or exclusion, so a narrower ABI register (`eax`) matches the
//! tracked container (`rax`).

use super::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// The `Call` output split for one call site: the cached default-CC scan,
    /// or a fresh one under a `per_address_ccs` override.
    pub(crate) fn call_ret_and_clobber_vns(
        &self,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>) {
        match override_cc {
            None => self.default_ret_clobber_vns.clone(),
            Some(cc) => cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| {
                self.container_map.container_of(v)
            }),
        }
    }

    /// The float argument varnodes for one call site: the cached default-CC
    /// list, or a fresh one under a `per_address_ccs` override. The integer
    /// list never names them, so without them a callee's float argument cone
    /// has no consumer.
    pub(crate) fn call_float_arg_vns(
        &self,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Vec<rsleigh::Vn> {
        match override_cc {
            None => self.default_float_arg_vns.clone(),
            Some(cc) => cc.float_arg_prefix(self.builder.function().all_vns(), |v| {
                self.container_map.container_of(v)
            }),
        }
    }

    /// Ret-val varnodes in ABI order.  Test-only: production `build_cc_call`
    /// takes both halves from a single `ret_and_clobber_vns` scan.
    #[cfg(test)]
    pub(crate) fn call_ret_vals_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| {
            self.container_map.container_of(v)
        })
        .0
    }

    /// Non-ret caller-saved varnodes in `all_vns` order.  Test-only, same as
    /// [`Self::call_ret_vals_for`].
    #[cfg(test)]
    pub(crate) fn call_clobbered_for(
        &self,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Vec<rsleigh::Vn> {
        cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| {
            self.container_map.container_of(v)
        })
        .1
    }
}
