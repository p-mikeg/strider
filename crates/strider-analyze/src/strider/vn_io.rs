//! Strider-side wrapper around [`strider_lift::pcode_lift::ValueLifter::read_vn`] /
//! [`strider_lift::pcode_lift::ValueLifter::write_vn`].
//!
//! Both methods used to live directly on `PerRegionDriver` (along with the
//! register-aliasing logic).  They have moved into the lower-layer
//! `pcode-lift` crate so that `cfg`'s indirect-branch resolver can
//! reuse them; this module's only job is to construct a `ValueLifter`
//! around `PerRegionDriver`'s existing borrows and delegate.

use anyhow::Result;

use super::PerRegionDriver;

impl<'a, R: rsleigh::MemReader> PerRegionDriver<'a, R> {
    /// Builds a [`strider_lift::pcode_lift::ValueLifter`] sharing this `PerRegionDriver`'s IR
    /// builder, sleigh context, and target endianness.
    pub(super) fn value_lifter(&mut self) -> strider_lift::pcode_lift::ValueLifter<'_, R> {
        strider_lift::pcode_lift::ValueLifter::new(
            &mut self.builder,
            self.sleigh,
            self.strider.arch.endianness(),
        )
    }

    /// Reads any varnode into an IR value.  Delegates to
    /// [`strider_lift::pcode_lift::ValueLifter::read_vn`].
    pub(super) fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<strider_ir::Value> {
        self.value_lifter().read_vn(vn)
    }

    /// Writes an IR value into any writable varnode.  Delegates to
    /// [`strider_lift::pcode_lift::ValueLifter::write_vn`].
    pub(super) fn write_vn(&mut self, vn: &rsleigh::Vn, val: strider_ir::Value) -> Result<()> {
        self.value_lifter().write_vn(vn, val)
    }
}
