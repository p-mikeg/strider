//! `MemWalker`, the clobber predicate a memory-SSA climb asks per def.

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
    /// The probed location.  Precomputed rather than a `Load` `NodeId`, since
    /// a probe need not have a backing `Load` node.
    pub(super) loc: Loc,
    /// Absent without a position index, which is what a range of defs means.
    pub(super) stores: Option<&'a StoreIndex>,
    /// [`Self::frame_is_private`] reads only `loc` and the policy, both
    /// fixed for the whole walk, and falls through to a bounded SP-spine climb.
    /// Asked repeatedly along one walk, so recomputing multiplies the walk by
    /// the spine depth.
    pub(super) private_frame: std::cell::OnceCell<bool>,
}

#[cfg(test)]
thread_local! {
    /// Total [`MemWalker`] clobber calls, the unit the walk-complexity tests
    /// count.
    pub(crate) static WALK_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl ClobberProbe for MemWalker<'_> {
    fn candidate(&mut self, lo: u32, hi: u32) -> Option<u32> {
        self.stores
            .expect("only a positioned layout asks for a candidate")
            .candidate(&self.loc, self.policy, lo, hi)
    }

    /// Does `def` overlap the probed location?
    fn clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        #[cfg(test)]
        WALK_STEPS.with(|c| c.set(c.get() + 1));
        match *function.node_kind(def) {
            NodeKind::Store(_) => {
                let store = Loc::written_by(function, def);
                // A private-frame SP slot cannot be aliased by an opaque
                // (`Anchor`) store: for that pointer to name this slot a frame
                // address would have had to escape, which the private-frame gate
                // rules out.  Same gate as the `Call` relaxation.  A later
                // forward can turn the pointer into a constant, which may alias
                // the slot unless `stack_global_disjoint` holds, and cannot take
                // back a reload already forwarded past it.
                if self.policy.stack_global_disjoint
                    && self.private_frame_forward(function)
                    && matches!(store.class, AddrClass::Anchor { .. })
                {
                    return false;
                }
                // Anything but `Disjoint` clobbers; `load_forward` re-checks
                // for an exact `Match` afterward.  Same store derivation as
                // `verdict`, so the walk stops at exactly the stores that
                // re-check sees.
                alias_verdict(self.loc, store, self.policy) != AliasVerdict::Disjoint
            }
            NodeKind::Call { .. } => {
                // A callee declared transparent to memory writes none, so the
                // load steps through whatever the frame analysis proves.  A
                // caller assertion, outside `AssumptionOptions`, so every
                // purpose honours it and a permanent rewire may rest on it.
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
            // A `CallOther` on the chain is a user-op whose ABI row declares
            // `clobbers_memory`, and it may write the stack without taking a
            // frame address, which no relaxation above speaks for.
            _ => true,
        }
    }
}

impl MemWalker<'_> {
    /// The frame-privacy FACT every stack relaxation rests on: this probe is a
    /// slot of THIS function's frame and no frame address escapes, so neither a
    /// callee nor an opaque pointer can name it.  Assumes valid input (a
    /// fabricated pointer numerically equal to the slot is excluded).
    ///
    /// A fact, not a claim, so it carries no relaxation mask of its own: each
    /// caller pairs it with the claim that buys the step.
    ///
    /// Says nothing about the outgoing-argument window, which the ABI hands to
    /// the callee: pair it with [`Self::in_outgoing_arg_area`] at a `Call`.
    fn frame_is_private(&self, function: &Function) -> bool {
        *self.private_frame.get_or_init(|| match self.loc.class {
            AddrClass::StackRooted { base, offset } => {
                in_own_frame(function, base, offset, self.loc.size)
                    && !frame_escape::frame_address_escapes_cached(function)
            }
            _ => false,
        })
    }

    /// The privacy fact under the [`crate::AssumptionOptions::escape_analysis`]
    /// knob, which is what buys forwarding past an `Anchor` store.
    fn private_frame_forward(&self, function: &Function) -> bool {
        self.policy.calls == Calls::Forward
            && function.assumptions().escape_analysis
            && self.frame_is_private(function)
    }

    /// Does the probed slot fall in the region `call` hands its callee?  A
    /// private frame does NOT cover it: the caller writes arguments there and
    /// the callee owns them, so it may write them back.
    ///
    /// The region is everything below `call_sp + base_offset` (the callee's
    /// own frame under the call's SP plus the ABI-reserved area above it: x86's
    /// return-address slot, PPC64's linkage area, MIPS o32's 16 bytes, Windows
    /// x64's home space), and above that the stack-argument window
    /// `[call_sp + base_offset, call_sp + offset_of(k))`, `k` being the
    /// contiguous prefix of arg slots with a store reaching the call, the same
    /// prefix [`crate::post_opt::call_stack_args`] collects.
    ///
    /// `k` proxies the callee's declared argument count, which needs a
    /// signature this crate does not have.  Once lowered to memory an argument
    /// push is indistinguishable from an incidental in-window write, so `k`
    /// errs WIDE: a spill just above the arguments blocks a forward that would
    /// have been fine.
    ///
    /// [`crate::AssumptionOptions::callee_preserves_stack_args`] empties the
    /// window instead, dropping `k` and everything above `base_offset`.
    ///
    /// Erring NARROW would be unsound: an argument store the inner probe
    /// cannot see past would end the prefix below the slot it wrote and let a
    /// load from a slot the callee owns forward across it.  A def the probe
    /// cannot see through therefore CONTINUES the window
    /// ([`super::arg_window::SlotReach::Blinded`]).
    ///
    /// Runs from inside a memory-SSA walk and starts one of its own, so the
    /// inner walk runs under [`Policy::strict`]: a `Call` relaxation would re-enter
    /// this method at every earlier `Call`, one fresh walk each, for
    /// `T(k) = sum(T(j), j < k)`, exponential, and recurse without bound
    /// through a loop-header back-edge.  Relaxation-free, the inner walk stops
    /// at the first `Call`, so the argument prefix ends there.
    ///
    /// The window itself is a property of the call, so the prefix walk is
    /// memoised on the query ([`super::arg_window::ArgPrefix`]) and every
    /// later probe of the same call is an interval test.
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
            // The call's SP is unknown, so the window cannot be placed and any
            // slot might be in it.  A non-stack SP is not a coordinate system
            // the probed slot shares.
            return true;
        };
        // Offsets are only comparable within one base.
        if call_base != load_base {
            return true;
        }
        // Below the first argument slot is the callee's frame and the reserved
        // area, both of which it may scratch.
        let window_lo = call_sp_off.saturating_add(stack_args.map_or(0, |s| s.base_offset));
        if load_off < window_lo {
            return true;
        }
        // The relaxation empties the argument window, leaving only the
        // reserved area below it callee-writable.
        if self.policy.calls == Calls::Forward && function.assumptions().callee_preserves_stack_args
        {
            return false;
        }
        // Nothing declared above the reserved area, so nothing left to overlap.
        stack_args.is_some()
            && self.query.arg_window_covers(
                function,
                call,
                load_off,
                load_off.saturating_add(self.loc.size),
            )
    }

    /// Whether `call` cannot write the probed location under a private-frame
    /// relaxation.
    ///
    /// A private frame has no escaping stack address, so the callee cannot
    /// name this slot and the load steps through, unless the slot is one the
    /// ABI hands it, which privacy says nothing about.  `escape_analysis`
    /// buys that for any callee; a listed pure allocator earns it from the
    /// fact alone, since it writes only its own fresh allocation (plus
    /// internal bookkeeping the caller cannot name).  An allocator is still a
    /// callee: the ABI hands it the outgoing-argument area, which it may
    /// scratch, and an escaped frame address is one it may already hold.
    /// Purity bounds what a callee writes THROUGH the pointers it has, not
    /// which pointers those are.
    ///
    /// An allocator also steps past a *different* heap object; its own
    /// allocation, a global (its private state may live there) and an opaque
    /// or unknown-identity pointer do not.
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
