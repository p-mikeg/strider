//! RAII guard that scopes [`FunctionBuilder::set_lift_addr`].
//!
//! The strider per-region driver has to keep mutating its own enclosing
//! struct fields (e.g. `IrStrider::process_insn_inner` reads
//! `self.cfg`/`self.strider` while emitting via `self.builder`), so it
//! can't pass the whole builder into [`FunctionBuilder::lift_at`]'s
//! closure.  This guard plugs the same gap: take a `&mut FunctionBuilder`
//! at the start of the insn, hold it past the insn body, drop at the
//! end — and the previous lift-addr is restored automatically, even on
//! `?`-propagated errors.

use super::FunctionBuilder;

/// Scope-guard for [`FunctionBuilder::lift_addr`].  Constructed by
/// [`Self::set`]; restores the previous value on drop.
pub struct LiftAddrGuard<'a> {
    builder: &'a mut FunctionBuilder,
    previous: Option<u64>,
}

impl<'a> LiftAddrGuard<'a> {
    /// Set `builder.lift_addr = addr` and return a guard whose drop
    /// restores the previous value.
    pub fn set(builder: &'a mut FunctionBuilder, addr: Option<u64>) -> Self {
        let previous = builder.lift_addr();
        builder.set_lift_addr(addr);
        Self { builder, previous }
    }
}

impl Drop for LiftAddrGuard<'_> {
    fn drop(&mut self) {
        self.builder.set_lift_addr(self.previous);
    }
}
