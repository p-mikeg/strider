#![forbid(unsafe_code)]

use anyhow::{Result, anyhow};

mod arithmetic;
mod call;
mod cast;
mod cc_projection;
mod control;
mod dispatch;
mod entry;
mod exit_free_sink;
mod float;
mod function_lifter;
mod memory;
mod pcode_consts;
pub(crate) mod pcode_util;
mod pruned_ssa;
mod regions;
mod tracked;
mod vn_io;

#[cfg(test)]
mod handler_tests;

#[cfg(test)]
mod aliasing_tests;

#[cfg(test)]
mod exit_free_sink_tests;

#[cfg(test)]
mod cc_projection_tests;

#[cfg(test)]
mod tests;

pub(crate) use function_lifter::FunctionLifter;
pub(crate) use regions::RegionMap;

pub struct LiftOutcome {
    pub function: strider_ir::Function,
    pub sites: LiftSites,
}

/// The control sites a lift records against their nodes, for the resolver.
#[derive(Default)]
pub struct LiftSites {
    /// Each unresolved `BranchIndirect`'s pcode address and its
    /// `IndirectBranch` placeholder.
    pub unresolved_branches: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,

    /// Seated `Switch` sites, keyed like `unresolved_branches`.
    pub switch_anchors: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,

    /// Each Sleigh `RETURN`'s `Return`, and each `BranchIndirect` seated
    /// `LinkRegister`; a tail call's `Return` is not one.
    pub return_sites: Vec<ReturnSite>,
}

/// A return's pcode address, its `Return` node, and its target, which the
/// node does not consume (dead until something anchors it).
pub type ReturnSite = (
    strider_cfg::PcodeInsnAddr,
    strider_ir::node::NodeId,
    strider_ir::node::ValueId,
);

/// The CFG-to-IR lift engine, reused across functions and rebuild rounds.
pub struct Lifter<R: rsleigh::MemReader> {
    /// `&mut` to build a CFG, `&` to lift one.
    engine: strider_cfg::DecodeEngine<R>,
}

impl<R: rsleigh::MemReader> Lifter<R> {
    /// # Errors
    ///
    /// When [`strider_cfg::DecodeEngine::new`] fails.
    pub fn new(arch: strider_target::SleighArch, reader: R) -> Result<Self> {
        Ok(Self {
            engine: strider_cfg::DecodeEngine::new(arch, reader)?,
        })
    }

    /// Indexed by `user_op_id`, which for `read_volatile` is past the table.
    #[must_use]
    pub fn user_op_names(&self) -> &[String] {
        self.engine.user_op_names()
    }

    /// [`strider_target::BuiltCallingConvention::validate`] and
    /// [`strider_target::BuiltCallingConvention::check_addr_size`] against
    /// this engine's `ram`. A Rust struct literal is not validated otherwise.
    pub(crate) fn check_cc(&self, cc: &strider_target::BuiltCallingConvention) -> Result<()> {
        cc.validate()
            .and_then(|()| cc.check_addr_size(self.arch().names().ram_addr_size()))
            .map_err(|e| anyhow!("{}: {e}", self.arch().preset().name()))
    }

    /// [`Self::check_cc`], plus: an override must name the function's own
    /// stack pointer; otherwise the call clobbers the function's SP, losing
    /// every stack offset past it.
    pub(crate) fn check_override_cc(
        &self,
        cc: &strider_target::BuiltCallingConvention,
        stack_vn: rsleigh::Vn,
    ) -> Result<()> {
        self.check_cc(cc)?;
        if cc.stack_ptr != stack_vn {
            return Err(anyhow!(
                "{}: per-address convention's stack pointer {} is not the \
                 lifted function's {stack_vn}",
                self.arch().preset().name(),
                cc.stack_ptr,
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        self.engine.sleigh()
    }

    #[must_use]
    pub fn arch(&self) -> strider_target::SleighArch {
        self.engine.arch()
    }

    #[must_use]
    pub fn sleigh_regs(&self) -> &'static rsleigh::SleighRegs {
        self.arch().names().regs()
    }

    /// This engine's sla decode and lift facts.
    pub(crate) fn tables(&self) -> &'static strider_target::DecodeTables {
        self.engine.tables()
    }

    /// Whether `vn` shares a byte with a volatile register.
    pub(crate) fn is_volatile(&self, vn: &rsleigh::Vn) -> bool {
        let (start, end) = (vn.addr_off, vn.addr_off + u64::from(vn.size));
        vn.addr_space == rsleigh::VnSpace::REGISTER
            && self
                .tables()
                .volatile_ranges
                .iter()
                .any(|r| r.start < end && start < r.end)
    }

    /// See [`strider_cfg::DecodeEngine::build_cfg`].
    pub fn build_cfg(
        &mut self,
        entry: strider_cfg::MachineInsnAddr,
        cfg_opts: &strider_cfg::CfgOptions,
    ) -> Result<strider_cfg::Cfg> {
        self.engine.build_cfg(entry, cfg_opts)
    }

    /// Rejects a `Cfg` whose Load / Store space ids (raw `AddrSpace` pointers
    /// of the engine that decoded it) are not live spaces of this engine. Not
    /// proof of origin: a dropped engine's reused address passes.
    fn check_cfg_space_ids(&self, cfg: &strider_cfg::Cfg) -> Result<()> {
        for wrapped in cfg.regions().flat_map(|region| region.insns.iter()) {
            if !matches!(
                wrapped.insn.opcode,
                rsleigh::Opcode::Load | rsleigh::Opcode::Store
            ) {
                continue;
            }
            // A malformed slot is left for `decode_space_id` to report.
            let Some(space_id) = wrapped.insn.inputs.first() else {
                continue;
            };
            if space_id.addr_space != rsleigh::VnSpace::CONST {
                continue;
            }
            if self.engine.space_ids().resolve(*space_id).is_none() {
                return Err(anyhow!(
                    "{:?} at {:#x} names an address space this Sleigh engine did not declare: \
                     the CFG was built by a different engine, whose spaces this one cannot \
                     resolve",
                    wrapped.insn.opcode,
                    wrapped.addr.machine_addr.addr,
                ));
            }
        }
        Ok(())
    }

    /// [`Self::build_ir_with`] under [`strider_cfg::CfgOptions::default`], no
    /// CC overrides and no claims: the right pairing only for a `cfg` built
    /// with no `call_other_overrides`.
    pub fn build_ir(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
    ) -> Result<LiftOutcome> {
        self.build_ir_with(
            cfg,
            cc,
            &strider_cfg::CfgOptions::default(),
            &rustc_hash::FxHashMap::default(),
            &strider_ir::AssumptionOptions::none(),
            &[],
        )
    }

    /// Lifts `cfg`, built with `cfg_opts`, under default convention `cc`; a
    /// direct call to a `per_address_ccs` key uses its override. The function
    /// holds `claims`, of which the lift reads `msr_register_keeps_mode` and
    /// `fp_rounding_mode_is_nearest`. Each `entry_values` register holds its
    /// value at entry (ppc64 ELFv1 `r2`, the descriptor's TOC), after
    /// `cc.entry_address`. Incompleteness is not an error. `cc` and every
    /// override must be built against this engine's registers.
    ///
    /// # Errors
    ///
    /// Among others, when a convention it uses fails
    /// [`strider_target::BuiltCallingConvention::validate`] or `check_addr_size`
    /// for this arch, `cc` fails `validate_default`, or an `entry_values`
    /// varnode is not a register or its value does not fit it.
    pub fn build_ir_with(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
        cfg_opts: &strider_cfg::CfgOptions,
        per_address_ccs: &rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
        claims: &strider_ir::AssumptionOptions,
        entry_values: &[(rsleigh::Vn, u64)],
    ) -> Result<LiftOutcome> {
        self.check_cfg_space_ids(cfg)?;
        self.check_cc(&cc)
            .and_then(|()| cc.validate_default())
            .map_err(|e| anyhow!("default convention: {e}"))?;
        for &(vn, value) in entry_values {
            let fits = vn.size >= 8 || value >> (8 * vn.size) == 0;
            if vn.addr_space != rsleigh::VnSpace::REGISTER || vn.size == 0 || !fits {
                return Err(anyhow!(
                    "entry value {value:#x} for {vn}: not a register it fits"
                ));
            }
        }
        let tracked = self.tracked_vns(cfg, &cc, cfg_opts, per_address_ccs, claims)?;
        let mut driver = FunctionLifter::new(
            self,
            cc,
            cfg,
            tracked,
            per_address_ccs,
            cfg_opts,
            entry_values,
        )?;
        driver.builder.function_mut().assume(claims);

        let graph = cfg.region_graph();
        let doms = graph_algorithms::dominance::dominators(cfg.entry(), |r| graph.neighbors(r));
        let (mut def_sites, nonlocal) = driver.collect_def_sites()?;
        // The write funnel checks against the unnarrowed map.
        #[cfg(debug_assertions)]
        {
            driver.def_sites = def_sites.clone();
        }
        def_sites.retain(|&id, _| !driver.is_local_temporary(id, &nonlocal));
        let placement = graph_algorithms::dominance::phi_placement(
            &doms,
            |r| cfg.region_predecessors(r),
            &def_sites,
        );

        let region_map = driver.build_region_map(&placement)?;

        driver.translate_regions(&region_map, &doms)?;
        driver.link_region_edges(&region_map)?;
        driver.seat_exit_free_sinks()?;

        Ok(LiftOutcome {
            function: driver.builder.build()?,
            sites: driver.sites,
        })
    }
}
