use crate::error::Result;

mod insn;
mod pipeline;
mod register_aliasing;
mod vn_io;

pub use pipeline::Analyzer;

/// Per-function translation context that converts a [`cfg::Cfg`] into an IR
/// graph region by region.
///
/// Holds a reference to the shared [`Analyzer`] (register / calling-convention
/// information) and a fresh [`ir::FunctionBuilder`].
pub struct IrAnalyzer<'a, R: rsleigh::MemReader> {
    pub(crate) analyzer: &'a Analyzer,
    pub(crate) builder: ir::FunctionBuilder,
    pub(crate) cfg: &'a cfg::Cfg<R>,
}

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    /// Creates a new `IrAnalyzer` for the given CFG.
    ///
    /// Collects all unique varnodes referenced by any instruction in `cfg` and
    /// constructs the IR [`FunctionBuilder`] with calling-convention
    /// information from `analyzer`.
    fn new(analyzer: &'a Analyzer, cfg: &'a cfg::Cfg<R>) -> Result<Self> {
        // Find all variables
        let all_vns = analyzer.find_all_unique_vns(cfg);

        // Create the builder to create the ir graph
        let builder = ir::FunctionBuilder::new(
            all_vns,
            &analyzer.calling_convention.arg_passing_regs,
            &analyzer.calling_convention.callee_saved_regs,
            &analyzer.calling_convention.ret_val_regs,
            Some(analyzer.calling_convention.stack_ptr_vn),
            analyzer.calling_convention.ret_stack_pop,
        )?;

        Ok(Self {
            analyzer,
            builder,
            cfg,
        })
    }

    /// Emits the function entry node into the IR graph.
    fn build_entry(&mut self) -> Result<()> {
        Ok(self.builder.build_entry()?)
    }
}
