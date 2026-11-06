use anyhow::Result;

use super::Lifter;

/// Per-function lift state.
pub(crate) struct FunctionLifter<'a, R: rsleigh::MemReader> {
    pub(crate) lifter: &'a Lifter<R>,
    pub(crate) builder: strider_ir::FunctionBuilder,
    pub(crate) cfg: &'a strider_cfg::Cfg,
    pub(crate) sites: super::LiftSites,
    pub(crate) per_address_ccs:
        &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// The options `cfg` was built with.
    pub(crate) cfg_opts: &'a strider_cfg::CfgOptions<'a>,
    /// Registers bound to a known value at entry.
    pub(crate) entry_values: &'a [(rsleigh::Vn, u64)],
    /// Varnode to largest tracked container, for the raw varnodes and every CC
    /// register; other varnodes fall back to a scan.
    pub(crate) container_map: vn_container::ContainerMap,
    /// Constant temporaries of the current instruction. `collect_def_sites`
    /// runs an identical one, so a register store resolves alike on both sides.
    pub(crate) pcode_consts: super::pcode_consts::PcodeConsts,
    /// The first value written to `ISAModeSwitch` at a machine address, and
    /// that address: the mode its branch targets decode in. The region's
    /// `IndirectBranch` or `Switch` takes it iff they share the address.
    pub(crate) pending_isa_mode: Option<(strider_ir::node::ValueId, u64)>,
    /// Each `Region` node's last machine address: the branch leaving it.
    pub(crate) region_last_addrs: rustc_hash::FxHashMap<strider_ir::node::NodeId, u64>,
    /// `ret_and_clobber_vns` under the default CC, shared by every call site
    /// without an override.
    pub(super) default_ret_clobber_vns: (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>),
    /// [`strider_target::BuiltCallingConvention::float_arg_prefix`] under the
    /// default CC.
    pub(super) default_float_arg_vns: Vec<rsleigh::Vn>,
    /// What [`Self::collect_def_sites`] mirrored, checked by
    /// [`Self::write_variable`].
    #[cfg(debug_assertions)]
    pub(super) def_sites: rustc_hash::FxHashMap<
        strider_ir::node::InitialVnId,
        rustc_hash::FxHashSet<strider_cfg::RegionId>,
    >,
    /// The region [`Self::translate_regions`] is lifting.
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
        entry_values: &'a [(rsleigh::Vn, u64)],
    ) -> Result<Self> {
        let mut builder = strider_ir::FunctionBuilder::new(
            tracked.iter().copied().collect(),
            cc,
            lifter.arch().byte_order(),
        )?;
        builder.set_internal_registers(&lifter.tables().internal_register_vns);
        // Query every CC register too, so an ABI register narrower than its
        // tracked container (`eax` in `rax`) resolves.
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
            sites: super::LiftSites::default(),
            per_address_ccs,
            cfg_opts,
            entry_values,
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

    /// Fingerprints every node `f` creates with `addr`; cleared on error too.
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
        self.cfg_opts
            .local_entries
            .get(self.per_address_ccs, target)
    }

    /// The entry region's start address.
    pub(crate) fn entry_machine_addr(&self) -> u64 {
        self.cfg.region_graph()[self.cfg.entry()]
            .start_addr
            .machine_addr
            .addr
    }
}
