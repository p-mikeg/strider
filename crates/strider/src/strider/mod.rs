use crate::error::Result;

mod insn;
mod pipeline;
mod register_aliasing;
mod vn_io;

pub use pipeline::Strider;

/// Per-function translation context that converts a [`cfg::Cfg`] into an IR
/// graph region by region.
///
/// Holds a reference to the shared [`Strider`] (register / calling-convention
/// information) and a fresh [`ir::FunctionBuilder`].
pub struct IrStrider<'a, R: rsleigh::MemReader> {
    pub(crate) strider: &'a Strider,
    pub(crate) builder: ir::FunctionBuilder,
    pub(crate) cfg: &'a cfg::Cfg<R>,
}

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    /// Creates a new `IrStrider` for the given CFG.
    ///
    /// Collects all unique varnodes referenced by any instruction in `cfg` and
    /// constructs the IR [`FunctionBuilder`] with calling-convention
    /// information from `strider`.
    fn new(strider: &'a Strider, cfg: &'a cfg::Cfg<R>) -> Result<Self> {
        // Find all variables
        let all_vns = strider.find_all_unique_vns(cfg);

        // Create the builder to create the ir graph
        let builder = ir::FunctionBuilder::new(all_vns, &strider.calling_convention)?;

        Ok(Self {
            strider,
            builder,
            cfg,
        })
    }

    /// Emits the function entry node into the IR graph.
    fn build_entry(&mut self) -> Result<()> {
        Ok(self.builder.build_entry()?)
    }
}
