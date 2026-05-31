use anyhow::Result;

mod insn;
mod pipeline;
mod vn_io;

pub use pipeline::{AnalyzeOptions, AnalyzeOutcome, LiftDriver};
pub(crate) use pipeline::RegionLiftHandles;

/// Per-function translation context that converts a [`strider_lift::cfg::Cfg`] into an IR
/// graph region by region.
///
/// Holds a reference to the shared [`LiftDriver`] (register / calling-convention
/// information) and a fresh [`strider_ir::FunctionBuilder`].
pub(crate) struct PerRegionDriver<'a, R: rsleigh::MemReader> {
    pub(crate) strider: &'a LiftDriver,
    pub(crate) builder: strider_ir::FunctionBuilder,
    pub(crate) cfg: &'a strider_lift::cfg::Cfg,
    /// The Sleigh handle that built `cfg`.  The CFG is a pure data
    /// structure and no longer owns it; the caller threads it in so the
    /// lifter can resolve register aliasing, the code space, and
    /// CallOther names.
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>,
    /// Anchors for the indirect-branch resolver.  Each entry maps a
    /// `BranchIndirect`'s pcode address to the IR `NodeOutputId` whose
    /// producer represents `target_vn`'s value at that BranchIndirect
    /// site.  Populated by `handle_unresolved_indirect_branch` at lift
    /// time, drained by `analyze_cfg` into the [`AnalyzeOutcome`].
    pub(crate) unresolved_branches: Vec<(strider_lift::cfg::PcodeInsnAddr, strider_ir::Value)>,
    /// Per-target-address CC override map.  `None` when the caller has
    /// no overrides; lookups become `and_then(|m| m.get(addr))`.
    pub(crate) per_address_ccs:
        Option<&'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>>,
}

impl<'a, R: rsleigh::MemReader> PerRegionDriver<'a, R> {
    /// Creates a new `PerRegionDriver` for the given CFG.
    ///
    /// Constructs the IR [`FunctionBuilder`] with the supplied
    /// `all_vns` (the set of every varnode any instruction in `cfg`
    /// references, sorted by `strider_lift::pcode_lift::vn_sort_key` for stable
    /// `VarId` numbering).  `per_address_ccs` is the lift-time CC
    /// override map; pass `None` when the caller has no overrides.
    pub(crate) fn new(
        strider: &'a LiftDriver,
        cfg: &'a strider_lift::cfg::Cfg,
        sleigh: &'a rsleigh::Sleigh<R>,
        all_vns: Vec<rsleigh::Vn>,
        per_address_ccs: Option<
            &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
        >,
    ) -> Result<Self> {
        let builder = strider_ir::FunctionBuilder::new(
            all_vns,
            &strider.calling_convention,
        )?;
        Ok(Self {
            strider,
            builder,
            cfg,
            sleigh,
            unresolved_branches: Vec::new(),
            per_address_ccs,
        })
    }
}
