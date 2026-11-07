//! Per-call CC register sets, resolved to tracked containers (`eax` to `rax`).

use super::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// The `Call` return and clobber varnodes for one call site.
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

    /// The float argument varnodes for one call site.
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
}
