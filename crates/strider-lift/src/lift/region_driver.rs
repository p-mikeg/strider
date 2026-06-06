use anyhow::Result;

use super::Lifter;

/// Per-function translation context that converts a [`strider_cfg::Cfg`] into an IR
/// graph region by region.
///
/// Holds a reference to the shared [`Lifter`] (register / calling-convention
/// information) and a fresh [`strider_ir::FunctionBuilder`].
pub(crate) struct PerRegionDriver<'a, R: rsleigh::MemReader> {
    pub(crate) lifter: &'a Lifter,
    pub(crate) builder: strider_ir::FunctionBuilder,
    pub(crate) cfg: &'a strider_cfg::Cfg,
    /// The Sleigh handle that built `cfg`.  The CFG is a pure data
    /// structure and no longer owns it; the caller threads it in so the
    /// lifter can resolve register aliasing, the code space, and
    /// CallOther names.
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>,
    /// Anchors for the indirect-branch resolver.  Each entry maps a
    /// `BranchIndirect`'s pcode address to the `NodeId` of the
    /// `IndirectBranch` placeholder lifted for it.  Populated by
    /// `handle_unresolved_indirect_branch` at lift time, drained by
    /// `analyze_cfg` into the [`super::LiftOutcome`].  The resolver reads each
    /// placeholder's live dispatch input from the node directly, so the
    /// correlation never goes stale under optimizer rewrites.
    pub(crate) unresolved_branches:
        Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
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
    /// references, sorted by `crate::pcode_lift::vn_sort_key` for stable
    /// `VarId` numbering).  `per_address_ccs` is the lift-time CC
    /// override map; pass `None` when the caller has no overrides.
    pub(crate) fn new(
        lifter: &'a Lifter,
        cfg: &'a strider_cfg::Cfg,
        sleigh: &'a rsleigh::Sleigh<R>,
        all_vns: Vec<rsleigh::Vn>,
        per_address_ccs: Option<
            &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
        >,
    ) -> Result<Self> {
        let builder = strider_ir::FunctionBuilder::new(
            all_vns,
            &lifter.calling_convention,
            lifter.arch.endianness(),
        )?;
        Ok(Self {
            lifter,
            builder,
            cfg,
            sleigh,
            unresolved_branches: Vec::new(),
            per_address_ccs,
        })
    }
}
