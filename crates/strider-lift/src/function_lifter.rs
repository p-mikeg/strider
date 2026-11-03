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
    /// Seated `Switch` sites, so the resolver can re-derive and widen one.
    pub(crate) switch_anchors: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
    /// See [`super::LiftOutcome::return_sites`].
    pub(crate) return_sites: Vec<super::ReturnSite>,
    pub(crate) per_address_ccs:
        &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// What `per_address_ccs` answers a ppc64 local entry through, from
    /// [`strider_cfg::CfgOptions`].
    pub(crate) local_entries: &'a strider_cfg::LocalEntries,
    /// Caller classifications for user-op names, from
    /// [`strider_cfg::CfgOptions`]; consulted before the built-in tables.
    pub(crate) call_other_overrides: &'a strider_target::call_other_abi::CallOtherOverrides,
    /// O(1) varnode-to-largest-container map, built over the tracked set;
    /// queried with the raw varnodes and every CC register.  Varnodes absent
    /// from it fall through to the scan in `container_of`.
    pub(crate) container_map: vn_container::ContainerMap,
    /// Constant values of the current machine instruction's temporaries, fed
    /// every op in `region.insns` order. `collect_def_sites` runs an identical
    /// one over the same sequence, so a register store resolves to the same
    /// varnode on both sides.
    pub(crate) pcode_consts: super::pcode_consts::PcodeConsts,
    /// The FIRST value committed to `ISAModeSwitch` at a machine address (ARM's
    /// `setISAMode` reads it, MIPS's `JXWritePC` writes it) and that address:
    /// the mode the instruction decodes its branch target(s) in. First, not
    /// last, because the ARM sla writes it twice for the call-shaped
    /// `ldr pc,addrmode2` and the second write is a constant 0 after the call;
    /// see `vn_io::write_vn`. The region's terminating `IndirectBranch` takes it as
    /// its mode input iff they share that address.
    pub(crate) pending_isa_mode: Option<(strider_ir::node::ValueId, u64)>,
    /// Each `Region` node's last machine address: the branch leaving it.
    pub(crate) region_last_addrs: rustc_hash::FxHashMap<strider_ir::node::NodeId, u64>,
    /// `ret_and_clobber_vns` under the function-default CC.  The tracked set
    /// and the container map are both frozen for the lift, so every call site
    /// without a `per_address_ccs` override yields this same split.
    pub(super) default_ret_clobber_vns: (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>),
    /// [`strider_target::BuiltCallingConvention::float_arg_prefix`] under the
    /// function-default CC, cached for the same reason: it scans the tracked
    /// set per float register.
    pub(super) default_float_arg_vns: Vec<rsleigh::Vn>,
    /// What [`Self::collect_def_sites`] mirrored, for the write-path check in
    /// [`Self::write_variable`]. Empty until `build_ir` fills it.
    #[cfg(debug_assertions)]
    pub(super) def_sites: rustc_hash::FxHashMap<
        strider_ir::node::InitialVnId,
        rustc_hash::FxHashSet<strider_cfg::RegionId>,
    >,
    /// The region [`Self::translate_regions`] is lifting, for the same check.
    #[cfg(debug_assertions)]
    pub(super) current_cfg_region: Option<strider_cfg::RegionId>,
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// `tracked` is the whole universe, [`Lifter::tracked_vns`], so it holds
    /// `cc`'s stack vn.
    pub(crate) fn new(
        lifter: &'a Lifter<R>,
        cc: strider_target::BuiltCallingConvention,
        cfg: &'a strider_cfg::Cfg,
        tracked: rustc_hash::FxHashSet<rsleigh::Vn>,
        per_address_ccs: &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
        cfg_opts: &'a strider_cfg::CfgOptions<'a>,
    ) -> Result<Self> {
        let mut builder = strider_ir::FunctionBuilder::new(
            tracked.iter().copied().collect(),
            cc,
            lifter.arch().byte_order(),
        )?;
        builder.set_internal_registers(lifter.internal_register_vns());
        // `FunctionBuilder::new` seeds the CC registers and `Function::new`
        // drops enclosed sub-registers, so the function's `all_vns()` is the
        // canonical tracked set.  Resolving a varnode INTO that set is the
        // machine-register concern owned here: query every raw varnode plus
        // every CC register, so an ABI register narrower than its tracked
        // container (ABI says `eax`, the function tracks `rax`) resolves.
        let f = builder.function();
        let cc = f.default_cc();
        let queries = tracked
            .iter()
            .chain(&cc.ret_val_regs)
            .chain(&cc.ret_val_regs_float)
            .chain(&cc.arg_passing_regs)
            .chain(&cc.arg_passing_regs_float)
            .chain(&cc.callee_saved_regs)
            .chain([&cc.stack_ptr])
            .copied();
        let container_map = vn_container::ContainerMap::build(f.all_vns(), queries);
        // Since `tracked` holds the stack vn, after dedup it must
        // resolve to a tracked container: itself, or a larger tracked vn.
        debug_assert!(
            {
                let sp_container = vn_container::largest_container_in(f.all_vns(), &cc.stack_ptr);
                f.all_vns().contains(&sp_container)
                    && vn_container::vn_contains(&sp_container, &cc.stack_ptr)
            },
            "stack vn {:?} did not resolve to a tracked container",
            cc.stack_ptr,
        );
        let container_of = |v: &rsleigh::Vn| container_map.container_of(v);
        let default_ret_clobber_vns = cc.ret_and_clobber_vns(f.all_vns(), container_of);
        let default_float_arg_vns = cc.float_arg_prefix(f.all_vns(), container_of);
        Ok(Self {
            lifter,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
            return_sites: Vec::new(),
            switch_anchors: Vec::new(),
            per_address_ccs,
            local_entries: cfg_opts.local_entries,
            call_other_overrides: cfg_opts.call_other_overrides,
            container_map,
            pcode_consts: super::pcode_consts::PcodeConsts::default(),
            pending_isa_mode: None,
            region_last_addrs: rustc_hash::FxHashMap::default(),
            default_ret_clobber_vns,
            default_float_arg_vns,
            #[cfg(debug_assertions)]
            def_sites: rustc_hash::FxHashMap::default(),
            #[cfg(debug_assertions)]
            current_cfg_region: None,
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

    /// The `per_address_ccs` override a direct `Call` selects, if any.
    pub(crate) fn call_override(
        &self,
        insn: &rsleigh::Insn,
    ) -> Option<&'a strider_target::BuiltCallingConvention> {
        strider_cfg::direct_call_target(insn, self.lifter.sleigh().default_code_space())
            .and_then(|target| self.override_at(target))
    }

    /// The `per_address_ccs` override a call to `target` selects, if any.
    pub(crate) fn override_at(
        &self,
        target: u64,
    ) -> Option<&'a strider_target::BuiltCallingConvention> {
        self.local_entries.get(self.per_address_ccs, target)
    }

    /// The entry region's start: a region's first instruction sits at it, and
    /// a zero-pcode-op entry region has none.
    pub(crate) fn entry_machine_addr(&self) -> u64 {
        self.cfg.region_graph()[self.cfg.entry()]
            .start_addr
            .machine_addr
            .addr
    }
}
