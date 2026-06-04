//! Varnode read/write dispatch for the value-producing lifter.
//!
//! Translates a [`rsleigh::Vn`] (Sleigh's location descriptor — register,
//! unique temp, constant, or memory address) into the IR primitives the
//! caller needs.  Register / unique sub-view aliasing is handled by the
//! lower-layer `strider_ir::FunctionBuilder` (`read_reg_vn` / `write_reg_vn`),
//! which owns the largest-containing-register read/write logic and the
//! per-arch bit-shift / mask formulas; this module only dispatches on the
//! varnode's address space and delegates the REGISTER / UNIQUE cases there.

use anyhow::anyhow;
use strider_ir::IRBuilderExt;

use crate::pcode_lift::Result;
use crate::pcode_lift::ValueLifter;

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Reads any varnode into an IR value.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → an integer constant node.
    /// - `UNIQUE` → delegates to the builder's `read_reg_vn` for sub-view
    ///   aliasing (Sleigh occasionally writes a wide unique and reads
    ///   a narrow slice of it — e.g. MIPS MULT writes a 64-bit unique
    ///   then Copy reads a 32-bit slice).
    /// - default code space → a `Load` from the code address space.
    /// - `REGISTER` → delegates to the builder's `read_reg_vn` for aliasing
    ///   handling.
    ///
    /// # Errors
    ///
    /// Returns an error when the varnode lives in an unsupported address
    /// space, has an unsupported size, or the IR builder rejects the
    /// resulting node.
    pub fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<strider_ir::Value> {
        let default_code_space = self.sleigh.default_code_space();
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => self
                .builder
                .build_int_const(vn.addr_off, strider_ir::ValueType::int_for_byte_size(vn.size)?),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.builder.read_reg_vn(vn),
            space if space == default_code_space => {
                let space_info = self
                    .sleigh
                    .space_info(space)
                    .ok_or_else(|| anyhow!("no space info for default code space {space:?}"))?;
                let addr = self
                    .builder
                    .build_int_const(vn.addr_off, strider_ir::ValueType::int_for_byte_size(space_info.addr_size())?)?;
                Ok(self.builder.build_load(addr, space, strider_ir::ValueType::int_for_byte_size(vn.size)?)?)
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
    /// - default code space → a `Store` to the code address space.
    /// - `REGISTER` → delegates to the builder's `write_reg_vn` for aliasing
    ///   handling.
    ///
    /// # Errors
    ///
    /// Returns an error when the varnode lives in an unsupported or
    /// non-writable address space, has an unsupported size, or the IR
    /// builder rejects the resulting node.
    pub fn write_vn(&mut self, vn: &rsleigh::Vn, val: strider_ir::Value) -> Result<()> {
        let default_code_space = self.sleigh.default_code_space();
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => Err(anyhow!("attempted to write to CONST space: {space:?}")),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.builder.write_reg_vn(vn, val),
            space if space == default_code_space => {
                let space_info = self
                    .sleigh
                    .space_info(space)
                    .ok_or_else(|| anyhow!("no space info for default code space {space:?}"))?;
                let addr = self
                    .builder
                    .build_int_const(vn.addr_off, strider_ir::ValueType::int_for_byte_size(space_info.addr_size())?)?;
                Ok(self.builder.build_store(addr, val, space)?)
            }
            _ => Err(anyhow!("unsupported varnode space {space:?}")),
        }
    }
}
