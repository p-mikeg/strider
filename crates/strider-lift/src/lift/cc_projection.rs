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

    /// The `Call` output split for one call site: the cached default-CC scan,
    /// or a fresh one under a `per_address_ccs` override.
    pub(crate) fn call_ret_and_clobber_vns(
        &self,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>) {
        match override_cc {
            None => self.default_ret_clobber_vns.clone(),
            Some(cc) => {
                cc.ret_and_clobber_vns(self.builder.function().all_vns(), |v| self.container_of(v))
            }
        }
    }

    /// The float argument varnodes for one call site: the cached default-CC
    /// list, or a fresh one under a `per_address_ccs` override.
    pub(crate) fn call_float_arg_vns(
        &self,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Vec<rsleigh::Vn> {
        match override_cc {
            None => self.default_float_arg_vns.clone(),
            Some(cc) => float_arg_prefix(cc, self.builder.function().all_vns(), |v| {
                self.container_of(v)
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

/// Float / vector arguments live in a register file the integer list never
/// names, so without them the callee's float argument cone has no consumer and
/// DCE deletes it.  Registers sharing a container (AAPCS-VFP `d0`/`d1` inside
/// `q0`) each pass their own slice of it, so two arguments never collapse into
/// one.
///
/// Every convention reaching here has its float argument registers seeded into
/// the tracked set, so a `None` slot means a caller built a convention this
/// function was not lifted against.  Taking the prefix before the first gap
/// keeps index `j` meaning ABI position `j`, which is what the callee side
/// records; flattening past a gap shifts every later argument down one.
///
/// `float_arg_slots` is quadratic in the float argument count and scans the
/// tracked set per register, hence the per-function cache over it.
pub(crate) fn float_arg_prefix(
    cc: &strider_target::BuiltCallingConvention,
    tracked_vns: &[rsleigh::Vn],
    container_of: impl Fn(&rsleigh::Vn) -> rsleigh::Vn,
) -> Vec<rsleigh::Vn> {
    cc.float_arg_slots(tracked_vns, container_of)
        .into_iter()
        .take_while(Option::is_some)
        .flatten()
        .collect()
}
