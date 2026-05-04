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
    /// Per-target-address CC override map.  `None` (the default) means
    /// every direct Call uses the function-default.  Set by
    /// [`Strider::analyze_cfg_with_vns_and_overrides`].
    pub(crate) per_address_ccs:
        Option<&'a std::collections::HashMap<u64, target::BuiltCallingConvention>>,
}

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    /// Creates a new `IrStrider` for the given CFG.
    ///
    /// Constructs the IR [`FunctionBuilder`] with the supplied
    /// `all_vns` (the set of every varnode any instruction in `cfg`
    /// references, sorted by `pcode_lift::vn_sort_key` for stable
    /// `VarId` numbering).  When the caller has a cached vn list
    /// (e.g. the orchestrator across rebuild iterations) it can pass
    /// it directly; the convenience [`Self::new_scanning`] does the
    /// scan in-place.
    pub(crate) fn new(
        strider: &'a Strider,
        cfg: &'a cfg::Cfg<R>,
        all_vns: Vec<rsleigh::Vn>,
    ) -> Result<Self> {
        let builder = ir::FunctionBuilder::new(all_vns, &strider.calling_convention)?;
        Ok(Self {
            strider,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
            per_address_ccs: None,
        })
    }

    /// Replaces the per-target-address CC override map with `map`.
    pub(crate) fn set_per_address_ccs(
        &mut self,
        map: &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
    ) {
        self.per_address_ccs = Some(map);
    }

}
