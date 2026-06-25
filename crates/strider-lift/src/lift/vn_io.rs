//! Varnode read/write dispatch for the per-CFG lifter.
//!
//! Translates a [`rsleigh::Vn`] (Sleigh's location descriptor — register,
//! unique temp, constant, or memory address) into the IR primitives the
//! caller needs.  Register / unique sub-view aliasing is handled by the
//! lower-layer `strider_ir::FunctionBuilder` (`read_reg_vn` / `write_reg_vn`),
//! which owns the largest-containing-register read/write logic and the
//! per-arch bit-shift / mask formulas; this module only dispatches on the
//! varnode's address space and delegates the REGISTER / UNIQUE cases there.

use anyhow::anyhow;
use strider_ir::{IRBuilderExt, VnTypeExt};

use super::{FunctionLifter, pcode_util::Result};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Builds an address-width integer constant for `off` in `space`.
    ///
    /// The constant's width is the address size of `space` (queried from
    /// Sleigh's `space_info`). `what` names the space in the error when the
    /// lookup fails, preserving each call site's diagnostic.
    pub(crate) fn build_addr_const(
        &mut self,
        space: rsleigh::VnSpace,
        off: u64,
        what: &str,
    ) -> Result<strider_ir::Value> {
        let space_info = self
            .lifter
            .sleigh()
            .space_info(space)
            .ok_or_else(|| anyhow!("no space info for {what} {space:?}"))?;
        self.builder.build_int_const(
            off,
            strider_ir::ValueType::int_for_byte_size(space_info.addr_size())?,
        )
    }

    /// Reads a sequence of varnodes into IR values, preserving order.
    pub(crate) fn read_vns(&mut self, vns: &[rsleigh::Vn]) -> Result<Vec<strider_ir::Value>> {
        vns.iter().map(|vn| self.read_vn(vn)).collect()
    }

    /// Reads the value of input varnode `n` of `insn` (checked index).
    pub(super) fn read_input(
        &mut self,
        insn: &rsleigh::Insn,
        n: usize,
    ) -> Result<strider_ir::Value> {
        let vn = crate::lift::pcode_util::nth_input_or_err(insn, n)?;
        self.read_vn(vn)
    }

    /// Reads any varnode into an IR value.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → an integer constant node.
    /// - `UNIQUE` → delegates to the builder's `read_reg_vn` for sub-view
    ///   aliasing (Sleigh occasionally writes a wide unique and reads
    ///   a narrow slice of it — e.g. MIPS MULT writes a 64-bit unique
    ///   then Copy reads a 32-bit slice).
    /// - `RAM` → a `Load` from the RAM address space.
    /// - `REGISTER` → delegates to the builder's `read_reg_vn` for aliasing
    ///   handling.
    ///
    /// # Errors
    ///
    /// Returns an error when the varnode lives in an unsupported address
    /// space, has an unsupported size, or the IR builder rejects the
    /// resulting node.
    pub(crate) fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<strider_ir::Value> {
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => self.builder.build_int_const(vn.addr_off, vn.int_type()?),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.builder.read_reg_vn(vn),
            rsleigh::VnSpace::RAM => {
                let addr = self.build_addr_const(space, vn.addr_off, "RAM space")?;
                Ok(self.builder.build_load(addr, space, vn.int_type()?)?)
            }
            _ => Err(anyhow!("unsupported varnode space {space:?}")),
        }
    }

    /// Writes an IR value into any writable varnode.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → error (constants cannot be written).
    /// - `UNIQUE` → delegates to the builder's `write_reg_vn` for sub-view
    ///   aliasing.
    /// - `RAM` → a `Store` to the RAM address space.
    /// - `REGISTER` → delegates to the builder's `write_reg_vn` for aliasing
    ///   handling.
    ///
    /// # Errors
    ///
    /// Returns an error when the varnode lives in an unsupported or
    /// non-writable address space, has an unsupported size, or the IR
    /// builder rejects the resulting node.
    pub(crate) fn write_vn(&mut self, vn: &rsleigh::Vn, val: strider_ir::Value) -> Result<()> {
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => Err(anyhow!("attempted to write to CONST space: {space:?}")),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => {
                self.builder.write_reg_vn(vn, val)
            }
            rsleigh::VnSpace::RAM => {
                let addr = self.build_addr_const(space, vn.addr_off, "RAM space")?;
                Ok(self.builder.build_store(addr, val, space)?)
            }
            _ => Err(anyhow!("unsupported varnode space {space:?}")),
        }
    }
}
