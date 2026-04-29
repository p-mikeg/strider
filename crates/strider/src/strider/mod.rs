use anyhow::Result;

mod insn;
mod pipeline;
mod vn_io;

pub use pipeline::{AnalyzeOutcome, RegionLiftHandles, Strider};

/// Per-function translation context that converts a [`cfg::Cfg`] into an IR
/// graph region by region.
///
/// Holds a reference to the shared [`Strider`] (register / calling-convention
/// information) and a fresh [`ir::FunctionBuilder`].
pub struct IrStrider<'a, R: rsleigh::MemReader> {
    pub(crate) strider: &'a Strider,
    pub(crate) builder: ir::FunctionBuilder,
    pub(crate) cfg: &'a cfg::Cfg<R>,
    /// Anchors for the tier-2 resolver.  Each entry maps a
    /// `BranchIndirect`'s pcode address to the IR `NodeOutputId` whose
    /// producer represents `target_vn`'s value at that BranchIndirect
    /// site.  Populated by `handle_unresolved_indirect_branch` at lift
    /// time, drained by `analyze_cfg` into the [`AnalyzeOutcome`].
    pub(crate) unresolved_branches: Vec<(cfg::PcodeInsnAddr, ir::Value)>,
}

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    /// Creates a new `IrStrider` for the given CFG.
    ///
    /// Collects all unique varnodes referenced by any instruction in `cfg` and
    /// constructs the IR [`FunctionBuilder`] with calling-convention
    /// information from `strider`.
    fn new(strider: &'a Strider, cfg: &'a cfg::Cfg<R>) -> Result<Self> {
        let all_vns = strider.find_all_unique_vns(cfg);
        let builder = ir::FunctionBuilder::new(all_vns, &strider.calling_convention)?;
        Ok(Self {
            strider,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
        })
    }
}
