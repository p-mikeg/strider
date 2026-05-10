//! Strider-side wrapper around [`pcode_lift::ValueLifter::read_vn`] /
//! [`pcode_lift::ValueLifter::write_vn`].
//!
//! Both methods used to live directly on `IrStrider` (along with the
//! register-aliasing logic).  They have moved into the lower-layer
//! `pcode-lift` crate so that `cfg`'s indirect-branch resolver can
//! reuse them; this module's only job is to construct a `ValueLifter`
//! around `IrStrider`'s existing borrows and delegate.

use anyhow::Result;

use super::IrStrider;

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    /// Builds a [`pcode_lift::ValueLifter`] sharing this `IrStrider`'s IR
    /// builder, sleigh context, and target endianness.
    pub(super) fn value_lifter(&mut self) -> pcode_lift::ValueLifter<'_, R> {
        pcode_lift::ValueLifter::new(
            &mut self.builder,
            &self.cfg.sleigh,
            self.strider.arch.endianness(),
        )
    }

    /// Reads any varnode into an IR value.  Delegates to
    /// [`pcode_lift::ValueLifter::read_vn`].
    pub(super) fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<ir::Value> {
        self.value_lifter().read_vn(vn)
    }

    /// Writes an IR value into any writable varnode.  Delegates to
    /// [`pcode_lift::ValueLifter::write_vn`].
    pub(super) fn write_vn(&mut self, vn: &rsleigh::Vn, val: ir::Value) -> Result<()> {
        self.value_lifter().write_vn(vn, val)
    }
}
