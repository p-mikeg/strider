use crate::error::{ErrorKind, Result};

use super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    /// Reads any varnode into an IR value.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → an integer constant node.
    /// - `UNIQUE` → delegates to [`read_reg_vn`] for sub-view aliasing
    ///   (Sleigh occasionally writes a wide unique and reads a narrow slice
    ///    of it — e.g. MIPS MULT writes a 64-bit unique then Copy reads a
    ///    32-bit slice).
    /// - default code space → a [`NodeKind::Load`] from the code address space.
    /// - `REGISTER` → delegates to [`read_reg_vn`] for aliasing handling.
    pub(super) fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<ir::Value> {
        let default_code_space = self.cfg.sleigh.default_code_space();
        let space = vn.addr.space;
        match space {
            rsleigh::VnSpace::CONST => Ok(self
                .builder
                .build_int_const(vn.addr.off, vn.size.try_into()?)),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.read_reg_vn(vn),
            space if space == default_code_space => {
                let space_info = self.cfg.sleigh.space_info(space);
                let addr = self
                    .builder
                    .build_int_const(vn.addr.off, space_info.addr_size().try_into()?);
                Ok(self.builder.build_load(addr, space, vn.size.try_into()?)?)
            }
            _ => Err(ErrorKind::UnsupportedVnSpace(space).into()),
        }
    }

    /// Writes an IR value into any writable varnode.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → error (constants cannot be written).
    /// - `UNIQUE` → delegates to [`write_reg_vn`] for sub-view aliasing.
    /// - default code space → a [`NodeKind::Store`] to the code address space.
    /// - `REGISTER` → delegates to [`write_reg_vn`] for aliasing handling.
    pub(super) fn write_vn(&mut self, vn: &rsleigh::Vn, val: ir::Value) -> Result<()> {
        let default_code_space = self.cfg.sleigh.default_code_space();
        let space = vn.addr.space;
        match space {
            rsleigh::VnSpace::CONST => Err(ErrorKind::WriteToConstSpace(space).into()),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.write_reg_vn(vn, val),
            space if space == default_code_space => {
                let space_info = self.cfg.sleigh.space_info(space);
                let addr = self
                    .builder
                    .build_int_const(vn.addr.off, space_info.addr_size().try_into()?);
                Ok(self.builder.build_store(addr, val, space)?)
            }
            _ => Err(ErrorKind::UnsupportedVnSpace(space).into()),
        }
    }
}
