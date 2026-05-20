//! Per-region driver — the `set_lift_addr(Some(addr)) ... set_lift_addr(None)`
//! funnel that wraps every per-instruction lift so asm-fingerprints stamp
//! correctly.
//!
//! Phase 2 Task 2.5 of the strider v2 rewrite.
//!
//! # Why this lives here
//!
//! Every IR node born from a pcode insn must carry the parent
//! machine-instruction address in its asm-fingerprint side-table.  The
//! mechanism is a single funnel: before invoking the per-insn dispatch
//! we call [`FunctionBuilder::set_lift_addr`] with `Some(addr)`; after
//! the dispatch completes (success or error) we restore `None` so
//! region-setup helpers (fallthrough wiring, phi nodes, etc.) stay
//! unattributed.
//!
//! Historically this funnel was inlined in `strider`'s per-region
//! driver and in the per-terminator dispatch.  It is **purely a
//! function of `FunctionBuilder`** — no orchestrator state, no CFG
//! references — so it belongs in `strider-lift` next to the value
//! lifter and CFG builder.  Both call sites now route through
//! [`RegionDriver`].
//!
//! # Why open-call brackets, not a closure
//!
//! The natural API would be a `with_lift_addr(builder, addr, |b| …)`
//! helper, but every existing call site in `strider` runs the inner
//! work using methods on `PerRegionDriver` itself (e.g.
//! `self.process_insn_inner(...)`, `self.handle_switch(...)`).  A
//! closure that captures `&mut self` cannot coexist with a `&mut
//! self.builder` argument the helper would need — the borrow checker
//! rejects the split.  Splitting [`RegionDriver`] into a pair of
//! `set_lift_addr` and `clear_lift_addr` open-call methods sidesteps
//! the borrow without forcing a deeper refactor of `PerRegionDriver`.  The
//! caller writes:
//!
//! ```ignore
//! RegionDriver::set_lift_addr(&mut self.builder, Some(addr));
//! let res = self.process_insn_inner(...);
//! RegionDriver::clear_lift_addr(&mut self.builder);
//! res?
//! ```
//!
//! which is identical in semantics to the v1 inlined version but
//! routes the `set_lift_addr` calls through a documented funnel in
//! strider-lift so Phase 3's Salsa lift driver can adopt the same
//! pattern without forking the comment block.
//!
//! # Stateless by construction
//!
//! [`RegionDriver`] has no fields and no `new`.  It is a namespace for
//! the funnel helpers.  Callers retain ownership of the
//! `FunctionBuilder` and pass it as `&mut`.

use strider_ir::FunctionBuilder;

/// Per-region lift-time driver — a namespace for the
/// `set_lift_addr`/`clear_lift_addr` funnel.
///
/// Construct nothing; call the inherent methods.
pub struct RegionDriver;

impl RegionDriver {
    /// Stamp the asm-fingerprint attribution context: every IR node
    /// the builder creates from now on (until the next
    /// [`Self::clear_lift_addr`]) carries `addr` in its
    /// asm-fingerprint side-table.
    ///
    /// `addr` is `Option<u64>` so the empty-region terminator handler
    /// can pass `None` (when a region has zero pcode insns the
    /// terminator has no contributing-asm address to attribute).
    /// `Some(machine_addr)` is the usual per-insn path.
    pub fn set_lift_addr(builder: &mut FunctionBuilder, addr: Option<u64>) {
        builder.set_lift_addr(addr);
    }

    /// Clear the asm-fingerprint attribution context so subsequent
    /// region-setup helpers (fallthrough wiring, phi inputs, etc.)
    /// stay unattributed.
    ///
    /// Equivalent to `set_lift_addr(builder, None)` but reads more
    /// naturally at the call site.
    pub fn clear_lift_addr(builder: &mut FunctionBuilder) {
        builder.set_lift_addr(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal stateless FunctionBuilder for the funnel tests.
    /// No varnodes, no calling convention — we only exercise the
    /// `set_lift_addr` state machine.
    fn make_builder() -> FunctionBuilder {
        FunctionBuilder::empty().expect("FunctionBuilder::empty")
    }

    #[test]
    fn set_lift_addr_some_then_clear_round_trip() {
        let mut b = make_builder();
        RegionDriver::set_lift_addr(&mut b, Some(0x4000));
        assert_eq!(b.lift_addr(), Some(0x4000));
        RegionDriver::clear_lift_addr(&mut b);
        assert_eq!(b.lift_addr(), None);
    }

    #[test]
    fn set_lift_addr_none_is_clear() {
        let mut b = make_builder();
        RegionDriver::set_lift_addr(&mut b, Some(0x5000));
        RegionDriver::set_lift_addr(&mut b, None);
        assert_eq!(b.lift_addr(), None);
    }

    #[test]
    fn clear_idempotent() {
        let mut b = make_builder();
        RegionDriver::clear_lift_addr(&mut b);
        RegionDriver::clear_lift_addr(&mut b);
        assert_eq!(b.lift_addr(), None);
    }
}
