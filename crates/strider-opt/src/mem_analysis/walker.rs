//! `MemWalker`, the clobber predicate a memory-SSA climb asks per def.

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Function, IRViewer};

use super::alias::{SizedAddr, alias_verdict};
use super::analyzer::{MemAnalyzer, MemOptions};
use super::arg_window::ArgWindowGeometry;
use super::decompose::{allocator_return_base, decompose};
use super::frame::in_own_frame;
use super::{AddrClass, AliasVerdict, MemExpr, MemKind, frame_escape};

/// The SP-aware [`crate::mem_ssa::ClobberProbe`] predicate carrier.
pub(super) struct MemWalker<'a> {
    pub(super) analyzer: &'a MemAnalyzer,
    /// The analyzer's own, or the relaxation-free ones of a window scan.
    pub(super) options: &'a MemOptions,
    /// The probed location.  Precomputed rather than a `Load` `NodeId`, since
    /// a probe need not have a backing `Load` node.
    pub(super) load: SizedAddr,
    /// Distinct spaces never alias, even at the same numeric address.
    pub(super) load_space: rsleigh::VnSpace,
    /// [`Self::frame_is_private`] reads only `load` and the options, both
    /// fixed for the whole walk, and falls through to a bounded SP-spine climb.
    /// Asked repeatedly along one walk, so recomputing multiplies the walk by
    /// the spine depth.
    pub(super) private_frame: std::cell::Cell<Option<bool>>,
}

#[cfg(test)]
thread_local! {
    /// Total [`MemWalker::def_clobbers`] calls, the unit the walk-complexity
    /// tests count.
    pub(crate) static WALK_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl MemWalker<'_> {
    /// The [`crate::mem_ssa::ClobberProbe::clobbers`] predicate: does `def`
    /// overlap the probed location?
    pub(super) fn def_clobbers(&mut self, function: &Function, def: NodeId) -> bool {
        #[cfg(test)]
        WALK_STEPS.with(|c| c.set(c.get() + 1));
        match *function.node_kind(def) {
            // A cross-space store cannot clobber this load, so the walk
            // continues to a same-space def.  Stopping here would let
            // `load_forward` forward a different-space store's value into the
            // load, a miscompile.
            NodeKind::Store(store_space) => {
                if store_space != self.load_space {
                    return false;
                }
                let store = self.analyzer.store_sized(function, def);
                // A private-frame SP slot cannot be aliased by an opaque
                // (`Anchor`) store: for that pointer to name this slot a frame
                // address would have had to escape, which the private-frame gate
                // rules out.  Same gate as the `Call` relaxation.
                if self.private_frame_forward(function)
                    && matches!(store.class, AddrClass::Anchor { .. })
                {
                    return false;
                }
                // Anything but `Disjoint` clobbers; `load_forward` re-checks
                // for an exact `Match` afterward.  Same store derivation as
                // `verdict`, so the walk stops at exactly the stores that
                // re-check sees.
                alias_verdict(self.load, store, self.options) != AliasVerdict::Disjoint
            }
            NodeKind::Call { .. } => {
                // A callee declared transparent to memory writes none, so the
                // load steps through whatever the frame analysis proves.  A
                // caller assertion, outside `AssumptionOptions`, so every
                // purpose honours it and a permanent rewire may rest on it.
                if function.get_cc(def).preserves_memory {
                    return false;
                }
                // A private frame has no escaping stack address, so the callee
                // cannot name this slot and the load steps through, unless the
                // slot is one the ABI hands it, which privacy says nothing
                // about.
                if (self.private_frame_forward(function)
                    && !self.in_outgoing_arg_area(function, def))
                    || self.allocator_transparent(function, def)
                {
                    false
                } else {
                    self.options.calls_block
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
        if let Some(cached) = self.private_frame.get() {
            return cached;
        }
        let verdict = match self.load.class {
            AddrClass::StackRooted { base, offset } => {
                in_own_frame(function, base, offset, self.load.size)
                    && !frame_escape::frame_address_escapes_cached(function)
            }
            _ => false,
        };
        self.private_frame.set(Some(verdict));
        verdict
    }

    /// The privacy fact under the [`crate::AssumptionOptions::escape_analysis`]
    /// knob, which is what buys forwarding across an arbitrary `Call` and past
    /// an `Anchor` store.  A listed allocator earns the same stack forwarding
    /// from the fact alone ([`Self::allocator_transparent`]), the knob being a
    /// claim about arbitrary callees that a pure allocator does not need.
    fn private_frame_forward(&self, function: &Function) -> bool {
        self.options.escape_analysis && self.frame_is_private(function)
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
    /// inner walk drops every `Call` relaxation: keeping them would re-enter
    /// this method at every earlier `Call`, one fresh walk each, for
    /// `T(k) = sum(T(j), j < k)`, exponential, and recurse without bound
    /// through a loop-header back-edge.  Relaxation-free, the inner walk stops
    /// at the first `Call`, so the argument prefix ends there.
    ///
    /// The window itself is a property of the call, so the prefix walk is
    /// memoised on the analyzer ([`super::arg_window::ArgWindow`]) and every
    /// later probe of the same call is an interval test.
    pub(super) fn in_outgoing_arg_area(&self, function: &Function, call: NodeId) -> bool {
        let AddrClass::StackRooted {
            base: load_base,
            offset: load_off,
        } = self.load.class
        else {
            return false;
        };
        let stack_args = function.get_cc(call).stack_args;
        let sp_value = function.node_inputs(call)[NodeKind::CALL_SP_SLOT];
        let Some(MemExpr {
            base: call_base,
            offset: call_sp_off,
            kind: MemKind::Stack,
        }) = decompose(function, sp_value)
        else {
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
        if self.options.callee_preserves_stack_args {
            return false;
        }
        // Nothing declared above the reserved area, so nothing left to overlap.
        let Some(stack_args) = stack_args else {
            return false;
        };
        let Some(mem_value) = function.memory_input_of(call) else {
            return true;
        };
        let window_hi = load_off.saturating_add(self.load.size);
        let geometry = ArgWindowGeometry {
            mem_start: mem_value,
            base: call_base,
            lo: window_lo,
            sp_offset: call_sp_off,
            args: stack_args,
        };
        self.analyzer.arg_window_covers(
            function,
            self.options,
            call,
            &geometry,
            load_off,
            window_hi,
        )
    }

    /// A pure allocator writes only its own fresh allocation (plus internal
    /// bookkeeping the caller cannot name), so it is transparent to a probe with
    /// a known base other than this call's return.  A slot of this function's
    /// PRIVATE frame and a *different* heap object step through; the call's own
    /// allocation, an escaped or foreign stack slot, a global (the
    /// allocator's private state may live there), or an opaque pointer do not.
    fn allocator_transparent(&self, function: &Function, call: NodeId) -> bool {
        if !self.options.call_relaxations {
            return false;
        }
        let Some(ret_base) = allocator_return_base(function, call) else {
            return false;
        };
        match self.load.class {
            // An allocator is still a callee: the ABI hands it the
            // outgoing-argument area, which it may scratch, and an escaped
            // frame address is one it may already hold, so a stack slot needs
            // the same privacy `escape_analysis` demands.  Purity bounds what a
            // callee writes THROUGH the pointers it has, not which pointers
            // those are.
            AddrClass::StackRooted { .. } => {
                self.frame_is_private(function) && !self.in_outgoing_arg_area(function, call)
            }
            AddrClass::HeapRooted { base, .. } => base != ret_base,
            // The unknown allocation may be this call's own.
            AddrClass::HeapOpaque | AddrClass::Constant { .. } | AddrClass::Anchor { .. } => false,
        }
    }
}
