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
    pub(crate) unresolved_branches: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
    /// Per-target-address CC override map.  Empty when the caller has no
    /// overrides (the `LiftOptions` default), so lookups are a plain `.get`.
    pub(crate) per_address_ccs:
        &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// `vn → largest tracked container` map: the machine-register-container
    /// knowledge that lives with the lifter (not the target-agnostic IR).
    /// Built once from the raw collected varnode set plus every
    /// calling-convention register, it is the O(1) fast path the
    /// register-aliasing read/write (`vn_io`) and the CC / CallOther register
    /// projections read on every access.  Ad-hoc varnodes absent from the map
    /// resolve through the `all_vns` scan fallback in
    /// [`FunctionLifter::container_of`].
    pub(crate) vn_to_container: rustc_hash::FxHashMap<rsleigh::Vn, rsleigh::Vn>,
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Creates a new `FunctionLifter` for the given CFG.
    ///
    /// Constructs the IR [`FunctionBuilder`] with the supplied
    /// `all_vns` (the set of every varnode any instruction in `cfg`
    /// references); the deterministic ordering that gives stable `InitialVnId`
    /// numbering is applied by [`strider_ir::FunctionBuilder::new`].  The
    /// Sleigh is reached through the `lifter` (which owns it).
    /// `per_address_ccs` is the lift-time CC override map; pass an empty map
    /// when the caller has no overrides.
    pub(crate) fn new(
        lifter: &'a Lifter<R>,
        cc: &'a strider_target::BuiltCallingConvention,
        cfg: &'a strider_cfg::Cfg,
        all_vns: Vec<rsleigh::Vn>,
        per_address_ccs: &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Result<Self> {
        let builder = strider_ir::FunctionBuilder::new(all_vns.clone(), cc, lifter.arch.endianness())?;
        // Build the container map from the deduped tracked survivors
        // (`function.all_vns()`), resolving every raw collected varnode plus
        // every calling-convention register (arg / ret / float-ret / stack /
        // callee-saved) to its largest tracked container.  A CC register
        // narrower than its tracked container (ABI says `eax`, function tracks
        // `rax`) thus resolves to the container.  This is the machine-register
        // knowledge relocated out of the IR into the lifter.
        let tracked = builder.function().all_vns();
        let cc_regs = cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .chain(cc.arg_passing_regs.iter())
            .chain(cc.callee_saved_regs.iter())
            .chain(std::iter::once(&cc.stack_vn))
            .copied();
        let queries = all_vns.iter().copied().chain(cc_regs);
        let vn_to_container = strider_ir::build_container_map(tracked, queries);
        Ok(Self {
            lifter,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
            per_address_ccs,
            vn_to_container,
        })
    }

    /// Asm-fingerprint attribution funnel: set the lift address, run a
    /// fallible body, then always clear the address — even on the error
    /// path — so every IR node born inside `f` picks up `addr` in its
    /// fingerprint side-table while no later node is mis-attributed.
    pub(crate) fn with_lift_addr<T>(
        &mut self,
        addr: Option<u64>,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.builder.set_lift_addr(addr);
        let res = f(self);
        self.builder.set_lift_addr(None);
        res
    }
}
