use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Function, IRViewer};

use super::alias::{Loc, alias_verdict};
use super::decompose::{allocator_return_base, call_sp};
use super::frame::in_own_frame;
use super::index::StoreIndex;
use super::query::{Calls, MemQuery, Policy};
use super::{AddrClass, AliasVerdict, frame_escape};
use crate::mem_ssa::ClobberProbe;

/// The SP-aware [`ClobberProbe`].
pub(super) struct MemWalker<'a> {
    pub(super) query: &'a MemQuery,
    pub(super) policy: Policy,
    /// The probed location, which need not belong to a `Load`.
    pub(super) loc: Loc,
    pub(super) stores: &'a StoreIndex,
    /// Memo of [`Self::frame_is_private`], fixed for the walk.
    pub(super) private_frame: std::cell::OnceCell<bool>,
}

#[cfg(test)]
thread_local! {
    /// [`MemWalker`] clobber calls.
    pub(crate) static WALK_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl ClobberProbe for MemWalker<'_> {
    fn candidate(&mut self, lo: u32, hi: u32) -> Option<u32> {
        self.stores.candidate(&self.loc, self.policy, lo, hi)
    }

    /// Whether `def` may write the probed location.
    fn clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        #[cfg(test)]
        WALK_STEPS.with(|c| c.set(c.get() + 1));
        match *function.node_kind(def) {
            NodeKind::Store(_) => {
                let store = Loc::written_by(function, def);
                // An `Anchor` store cannot name a private-frame slot without
                // an escaped frame address. A later forward may turn the
                // pointer into a constant, hence `stack_global_disjoint`.
                if self.policy.stack_global_disjoint
                    && self.private_frame_forward(function)
                    && matches!(store.class, AddrClass::Anchor { .. })
                {
                    return false;
                }
                // Anything but `Disjoint` clobbers; `MemQuery::reaching`
                // re-asks `alias_verdict` for an exact `Match`.
                alias_verdict(self.loc, store, self.policy) != AliasVerdict::Disjoint
            }
            NodeKind::Call { .. } => {
                // A caller assertion outside `AssumptionOptions`, so every
                // policy honours it.
                if function.get_cc(def).preserves_memory {
                    return false;
                }
                match self.policy.calls {
                    Calls::Block => true,
                    Calls::Pass => false,
                    Calls::Forward => !self.call_transparent(
                        function,
                        def,
                        function.assumptions().escape_analysis,
                    ),
                }
            }
            // A `CallOther` may write the stack without a frame address.
            _ => true,
        }
    }
}

impl MemWalker<'_> {
    /// Whether the probe is a slot of this function's own frame and no frame
    /// address escapes ([`frame_escape`]'s claims). Says nothing of the
    /// outgoing-argument window ([`Self::in_outgoing_arg_area`]).
    fn frame_is_private(&self, function: &Function) -> bool {
        *self.private_frame.get_or_init(|| match self.loc.class {
            AddrClass::StackRooted { base, offset } => {
                in_own_frame(function, base, offset, self.loc.size)
                    && !frame_escape::frame_address_escapes_cached(function)
            }
            _ => false,
        })
    }

    /// [`Self::frame_is_private`] under
    /// [`crate::AssumptionOptions::escape_analysis`].
    fn private_frame_forward(&self, function: &Function) -> bool {
        self.policy.calls == Calls::Forward
            && function.assumptions().escape_analysis
            && self.frame_is_private(function)
    }

    /// Whether the probed slot lies in the region `call` hands its callee:
    /// everything below `call_sp + base_offset`, any slot on another SP base,
    /// and above it the [`super::arg_window::ArgPrefix`] window, which an
    /// unwritten slot ends. Under `Calls::Forward`,
    /// [`crate::AssumptionOptions::callee_preserves_stack_args`] empties the
    /// window.
    pub(super) fn in_outgoing_arg_area(&self, function: &Function, call: NodeId) -> bool {
        let AddrClass::StackRooted {
            base: load_base,
            offset: load_off,
        } = self.loc.class
        else {
            return false;
        };
        let stack_args = function.get_cc(call).stack_args;
        let Some((call_base, call_sp_off)) = call_sp(function, call) else {
            // The window cannot be placed.
            return true;
        };
        if call_base != load_base {
            return true;
        }
        // The callee's frame and the reserved area.
        let window_lo = call_sp_off.saturating_add(stack_args.map_or(0, |s| s.base_offset));
        if load_off < window_lo {
            return true;
        }
        if self.policy.calls == Calls::Forward && function.assumptions().callee_preserves_stack_args
        {
            return false;
        }
        stack_args.is_some()
            && self.query.arg_window_covers(
                function,
                call,
                load_off,
                load_off.saturating_add(self.loc.size),
            )
    }

    /// Whether `call` cannot write the probed location. A private-frame slot
    /// outside the argument window passes under `escape_analysis` or a listed
    /// allocator; an allocator also passes a different heap object.
    fn call_transparent(&self, function: &Function, call: NodeId, escape_analysis: bool) -> bool {
        let allocated = allocator_return_base(function, call);
        match self.loc.class {
            AddrClass::StackRooted { .. } => {
                (escape_analysis || allocated.is_some())
                    && self.frame_is_private(function)
                    && !self.in_outgoing_arg_area(function, call)
            }
            AddrClass::HeapRooted { base, .. } => allocated.is_some_and(|ret| ret != base),
            AddrClass::HeapOpaque | AddrClass::Constant { .. } | AddrClass::Anchor { .. } => false,
        }
    }
}
