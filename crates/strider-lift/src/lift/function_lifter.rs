use anyhow::Result;

use super::Lifter;

/// Per-function translation context, borrowing the shared [`Lifter`] engine
/// and owning a fresh [`strider_ir::FunctionBuilder`].
pub(crate) struct FunctionLifter<'a, R: rsleigh::MemReader> {
    pub(crate) lifter: &'a Lifter<R>,
    pub(crate) builder: strider_ir::FunctionBuilder,
    pub(crate) cfg: &'a strider_cfg::Cfg,
    /// Maps each `BranchIndirect`'s pcode address to its `IndirectBranch`
    /// placeholder node.
    pub(crate) unresolved_branches: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
    pub(crate) per_address_ccs:
        &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// O(1) varnode-to-largest-container map, built over the raw varnode set
    /// plus every CC register.  Varnodes absent from it fall through to the
    /// scan in `container_of`.
    pub(crate) container_map: vn_container::ContainerMap,
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// `all_vns` is every varnode any instruction in `cfg` references.  Pass
    /// an empty `per_address_ccs` for no CC overrides.
    pub(crate) fn new(
        lifter: &'a Lifter<R>,
        cc: strider_target::BuiltCallingConvention,
        cfg: &'a strider_cfg::Cfg,
        mut all_vns: Vec<rsleigh::Vn>,
        per_address_ccs: &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Result<Self> {
        // The lifter is the SSoT for tracking SP; `FunctionBuilder::new` does
        // not seed it.  A function may never name SP yet still need it tracked
        // for stack analysis.
        if !all_vns.contains(&cc.stack_vn) {
            all_vns.push(cc.stack_vn);
        }
        // `FunctionBuilder::new` owns universe construction (seeding CC
        // registers, dropping enclosed sub-registers), so its `all_vns()` is
        // the canonical tracked set.  Resolving a varnode INTO that set is the
        // machine-register concern owned here: query every raw varnode plus
        // every CC register, so an ABI register narrower than its tracked
        // container (ABI says `eax`, the function tracks `rax`) resolves.
        //
        // Every `&cc` read is captured into an owned local BEFORE `cc` moves
        // into `FunctionBuilder::new`; `cc` threads by value all the way to
        // `Function::default_cc` with no clone.
        let stack_vn = cc.stack_vn;
        let cc_regs: Vec<rsleigh::Vn> = cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .chain(cc.arg_passing_regs.iter())
            .chain(cc.callee_saved_regs.iter())
            .chain(std::iter::once(&cc.stack_vn))
            .copied()
            .collect();
        let builder =
            strider_ir::FunctionBuilder::new(all_vns.clone(), cc, lifter.arch.endianness())?;
        let queries = all_vns.iter().copied().chain(cc_regs);
        let container_map =
            vn_container::ContainerMap::build(builder.function().all_vns(), queries);
        // Since the stack vn was added to `all_vns` above, after dedup it must
        // resolve to a tracked container: itself, or a larger tracked vn.
        debug_assert!(
            {
                let tracked = builder.function().all_vns();
                let sp_container = vn_container::largest_container_in(tracked, &stack_vn);
                tracked.contains(&sp_container)
                    && vn_container::vn_contains(&sp_container, &stack_vn)
            },
            "stack vn {:?} did not resolve to a tracked container",
            stack_vn,
        );
        Ok(Self {
            lifter,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
            per_address_ccs,
            container_map,
        })
    }

    /// Every IR node born inside `f` picks up `addr` in its fingerprint
    /// side-table.  The address is cleared even on the error path.
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
