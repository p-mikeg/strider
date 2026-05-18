use anyhow::Result;

mod insn;
mod pipeline;
mod vn_io;

pub use pipeline::{AnalyzeOptions, AnalyzeOutcome, RegionLiftHandles, Strider};

/// Per-function translation context that converts a [`cfg::Cfg`] into an IR
/// graph region by region.
///
/// Holds a reference to the shared [`Strider`] (register / calling-convention
/// information) and a fresh [`ir::FunctionBuilder`].
pub struct IrStrider<'a, R: rsleigh::MemReader> {
    pub(crate) strider: &'a Strider,
    pub(crate) builder: ir::FunctionBuilder,
    pub(crate) cfg: &'a cfg::Cfg<R>,
    /// Anchors for the indirect-branch resolver.  Each entry maps a
    /// `BranchIndirect`'s pcode address to the IR `NodeOutputId` whose
    /// producer represents `target_vn`'s value at that BranchIndirect
    /// site.  Populated by `handle_unresolved_indirect_branch` at lift
    /// time, drained by `analyze_cfg` into the [`AnalyzeOutcome`].
    pub(crate) unresolved_branches: Vec<(cfg::PcodeInsnAddr, ir::Value)>,
    /// Per-target-address CC override map.  Defaults to a process-wide
    /// empty map (`pipeline::EMPTY_PER_ADDRESS_CCS`); set to a real map
    /// at constructor time when the caller has overrides.  Lookup is a
    /// single `HashMap::get` regardless.
    pub(crate) per_address_ccs:
        &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
}

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    /// Creates a new `IrStrider` for the given CFG.
    ///
    /// Constructs the IR [`FunctionBuilder`] with the supplied
    /// `all_vns` (the set of every varnode any instruction in `cfg`
    /// references, sorted by `pcode_lift::vn_sort_key` for stable
    /// `VarId` numbering).  `per_address_ccs` is the lift-time CC
    /// override map; pass `&EMPTY_PER_ADDRESS_CCS` (or an empty
    /// `HashMap`) when the caller has no overrides.
    pub(crate) fn new(
        strider: &'a Strider,
        cfg: &'a cfg::Cfg<R>,
        all_vns: Vec<rsleigh::Vn>,
        per_address_ccs: &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
    ) -> Result<Self> {
        let builder = ir::FunctionBuilder::new(
            all_vns,
            &ir::FunctionBuilderCC::from(&strider.calling_convention),
        )?;
        Ok(Self {
            strider,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
            per_address_ccs,
        })
    }
}
