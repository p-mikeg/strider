use anyhow::Result;

use super::Lifter;

/// Per-function translation context that converts a [`strider_cfg::Cfg`] into an IR
/// graph region by region.
///
/// Borrows the shared [`Lifter`] engine (arch / owned Sleigh / register
/// table — reach the Sleigh via `self.lifter.sleigh()`) and the
/// per-function calling convention, and owns a fresh
/// [`strider_ir::FunctionBuilder`].
pub(crate) struct FunctionLifter<'a, R: rsleigh::MemReader> {
    pub(crate) lifter: &'a Lifter<R>,
    pub(crate) builder: strider_ir::FunctionBuilder,
    pub(crate) cfg: &'a strider_cfg::Cfg,
    /// Anchors for the indirect-branch resolver.  Each entry maps a
    /// `BranchIndirect`'s pcode address to the `NodeId` of the
    /// `IndirectBranch` placeholder lifted for it.  Populated by
    /// `handle_unresolved_indirect_branch` at lift time, drained by
    /// `build_ir` into the [`super::LiftOutcome`].  The resolver reads each
    /// placeholder's live dispatch input from the node directly, so the
    /// correlation never goes stale under optimizer rewrites.
    pub(crate) unresolved_branches:
        Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
    /// Per-target-address CC override map.  `None` when the caller has
    /// no overrides; lookups become `and_then(|m| m.get(addr))`.
    pub(crate) per_address_ccs:
        Option<&'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>>,
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Creates a new `FunctionLifter` for the given CFG.
    ///
    /// Constructs the IR [`FunctionBuilder`] with the supplied
    /// `all_vns` (the set of every varnode any instruction in `cfg`
    /// references); the deterministic ordering that gives stable `VarId`
    /// numbering is applied by [`strider_ir::FunctionBuilder::new`].  The
    /// Sleigh is reached through the `lifter` (which owns it).
    /// `per_address_ccs` is the lift-time CC override map; pass `None`
    /// when the caller has no overrides.
    pub(crate) fn new(
        lifter: &'a Lifter<R>,
        cc: &'a strider_target::BuiltCallingConvention,
        cfg: &'a strider_cfg::Cfg,
        all_vns: Vec<rsleigh::Vn>,
        per_address_ccs: Option<
            &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
        >,
    ) -> Result<Self> {
        let builder = strider_ir::FunctionBuilder::new(all_vns, cc, lifter.arch.endianness())?;
        Ok(Self {
            lifter,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
            per_address_ccs,
        })
    }
}
