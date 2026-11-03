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
    /// One entry per region that terminated in an unresolved indirect branch,
    /// mapping the `BranchIndirect`'s pcode address to the `IndirectBranch`
    /// placeholder anchoring its dispatch varnode.
    pub unresolved_branches: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,

    /// Seated `Switch` sites, keyed like `unresolved_branches`.  A seated site
    /// keeps its selector, so the resolver re-derives it each round and can
    /// WIDEN a table that resolved before the CFG finished growing.
    pub switch_anchors: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,

    /// Every Sleigh `RETURN` op lifted to a `Return`: where the jump back to the
    /// caller is taken from is a value like any other, and only a check against
    /// the optimised IR can tell it is the caller's return address.
    pub return_sites: Vec<ReturnSite>,
}

/// A Sleigh `RETURN` op's pcode address, the `Return` node lifted for it, and
/// the address it jumps to, which that node does not consume and which is
/// therefore dead until something anchors it.
pub type ReturnSite = (
    strider_cfg::PcodeInsnAddr,
    strider_ir::node::NodeId,
    strider_ir::node::ValueId,
);

/// The CFG-to-IR lift engine, built once and reused across every function and
/// rebuild iteration.  The calling convention is per-function, hence a per-call
/// argument.
pub struct Lifter<R: rsleigh::MemReader> {
    /// The Sleigh and the decode context it carries across functions: `&mut`
    /// to build a CFG, `&` to lift one.
    engine: strider_cfg::DecodeEngine<R>,
}

impl<R: rsleigh::MemReader> Lifter<R> {
    /// A lift engine over `reader`, with the Sleigh built from `arch`.
    ///
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
    /// this engine's `ram`: the intra-convention half of what every
    /// convention reaching the lift is checked by, default
    /// ([`Self::build_ir_with`]) or per-address override
    /// ([`Self::check_override_cc`]). A preset or Python `custom` is
    /// validated already; a Rust struct literal is not.
    pub(crate) fn check_cc(&self, cc: &strider_target::BuiltCallingConvention) -> Result<()> {
        cc.validate()
            .and_then(|()| cc.check_addr_size(self.arch().names().ram_addr_size()))
            .map_err(|e| anyhow!("{:?}: {e}", self.arch().preset()))
    }

    /// [`Self::check_cc`] plus the one CROSS-convention rule: a per-address
    /// override must name `stack_ptr`, the stack pointer of the function it is
    /// used in. `strider_ir::FunctionBuilder::build_call` applies the post-call
    /// adjust to the FUNCTION's stack pointer while the clobber split
    /// ([`strider_target::BuiltCallingConvention::ret_and_clobber_vns`]) and
    /// the def-site mirror read the OVERRIDE's, so two names leave the
    /// function's SP a clobber: its writeback overwrites the adjust with the
    /// opaque `Call` output and every stack offset past the call is lost.
    /// Analysis never leaves the function, and SP is its frame anchor, so an
    /// override renaming it describes another frame.
    pub(crate) fn check_override_cc(
        &self,
        cc: &strider_target::BuiltCallingConvention,
        stack_vn: rsleigh::Vn,
    ) -> Result<()> {
        self.check_cc(cc)?;
        if cc.stack_ptr != stack_vn {
            return Err(anyhow!(
                "{:?}: per-address convention's stack pointer {:?} is not the \
                 lifted function's {stack_vn:?}",
                self.arch().preset(),
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

    /// This engine's resolved [`strider_target::SleighArch::internal_registers`].
    pub(crate) fn internal_register_vns(&self) -> &'static [rsleigh::Vn] {
        &self.engine.tables().internal_register_vns
    }

    /// See [`strider_target::DecodeTables::isa_mode_switch_vn`].
    pub(crate) fn isa_mode_switch_vn(&self) -> Option<rsleigh::Vn> {
        self.engine.tables().isa_mode_switch_vn
    }

    /// Whether `vn` shares a byte with a volatile register.
    pub(crate) fn is_volatile(&self, vn: &rsleigh::Vn) -> bool {
        let (start, end) = (vn.addr_off, vn.addr_off + u64::from(vn.size));
        vn.addr_space == rsleigh::VnSpace::REGISTER
            && self
                .engine
                .tables()
                .volatile_ranges
                .iter()
                .any(|r| r.start < end && start < r.end)
    }

    /// The declared register file, for containment queries against a computed
    /// REGISTER-space address.
    #[must_use]
    pub(crate) fn declared_reg_vns(&self) -> &'static [rsleigh::Vn] {
        &self.engine.tables().declared_reg_vns
    }

    /// The CFG of the function at `entry`; see
    /// [`strider_cfg::DecodeEngine::build_cfg`]. The resolve loop calls it
    /// every round.
    pub fn build_cfg(
        &mut self,
        entry: strider_cfg::MachineInsnAddr,
        cfg_opts: &strider_cfg::CfgOptions,
    ) -> Result<strider_cfg::Cfg> {
        self.engine.build_cfg(entry, cfg_opts)
    }

    /// Rejects a `Cfg` whose Load / Store space ids were not produced by this
    /// engine, before [`Self::decode_space_id`] reads any.
    ///
    /// A `Cfg` holds cloned `rsleigh::Insn`s and carries no lifetime, so it
    /// outlives the engine that decoded it; the space id in a Load / Store's
    /// `inputs[0]` is that engine's raw `AddrSpace` pointer.
    ///
    /// What it establishes is that the id is one of THIS engine's live space
    /// pointers; it is not proof of origin.  A dropped engine's id whose
    /// address this engine's allocator handed back names a live space of this
    /// engine, so it passes and the Load / Store silently takes that space.
    /// Distinguishing the two needs an id rsleigh derives from the space
    /// rather than from its address.
    fn check_cfg_space_ids(&self, cfg: &strider_cfg::Cfg) -> Result<()> {
        for wrapped in cfg.regions().flat_map(|region| region.insns.iter()) {
            if !matches!(
                wrapped.insn.opcode,
                rsleigh::Opcode::Load | rsleigh::Opcode::Store
            ) {
                continue;
            }
            // A missing or non-CONST slot is a malformed encoding rather than a
            // foreign one; `decode_space_id` names it precisely at lift time.
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
        )
    }

    /// `cfg_opts` classifies each `CallOther`'s footprint under its
    /// `call_other_overrides`; whether one ends control is the `cfg`'s own
    /// `NoReturn` seal. Pass the options `cfg` was built with. `cc` is
    /// the function default; a direct Call whose target is in
    /// `per_address_ccs` is built with that override instead. The overrides
    /// must be built against this engine's registers and fit its arch, as
    /// `cc` must. The lifted function holds `claims`
    /// ([`strider_ir::Function::assumptions`]), and the lift itself reads
    /// [`strider_ir::AssumptionOptions::msr_register_keeps_mode`] and
    /// [`strider_ir::AssumptionOptions::fp_rounding_mode_is_nearest`].
    ///
    /// Never errs on incompleteness:
    /// [`strider_cfg::Cfg::incomplete`] reports that.
    ///
    /// # Errors
    ///
    /// Among others, when `cc` fails
    /// [`strider_target::BuiltCallingConvention::validate`], or `cc` or an
    /// override this `cfg` selects fails
    /// [`strider_target::BuiltCallingConvention::check_addr_size`] for this arch.
    pub fn build_ir_with(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
        cfg_opts: &strider_cfg::CfgOptions,
        per_address_ccs: &rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
        claims: &strider_ir::AssumptionOptions,
    ) -> Result<LiftOutcome> {
        self.check_cfg_space_ids(cfg)?;
        self.check_cc(&cc)
            .map_err(|e| anyhow!("default convention: {e}"))?;
        let tracked = self.tracked_vns(cfg, &cc, cfg_opts, per_address_ccs, claims)?;
        let mut driver = FunctionLifter::new(self, cc, cfg, tracked, per_address_ccs, cfg_opts)?;
        driver.builder.function_mut().assume(claims);

        // Semi-pruned SSA phi placement: iterated dominance frontier of each
        // variable's definition sites, less the region-local temporaries.  This
        // is what stops the lifter minting a value `Phi` for every varnode at
        // every region.
        let graph = cfg.region_graph();
        let doms = graph_algorithms::dominance::dominators(cfg.entry(), |r| graph.neighbors(r));
        // Where a definition in `r` first stops dominating, hence where a phi
        // may be needed.  An absent key means an empty frontier.
        let frontiers =
            graph_algorithms::dominance::dominance_frontiers(&doms, |r| cfg.region_predecessors(r));
        let (mut def_sites, nonlocal) = driver.collect_def_sites()?;
        // The write funnel checks against the FULL map, hence the copy before
        // the narrowing below.
        #[cfg(debug_assertions)]
        {
            driver.def_sites = def_sites.clone();
        }
        def_sites.retain(|&id, _| !driver.is_local_temporary(id, &nonlocal));
        let placement = graph_algorithms::dominance::phi_placement(&frontiers, &def_sites);

        let region_map = driver.build_region_map(&placement)?;

        // Dominator-tree pre-order, so each region inherits reaching variable
        // values from its already-processed immediate dominator.  Then wire the
        // fallthrough edges the per-insn loop didn't reach.
        driver.translate_regions(&region_map, &doms)?;
        driver.link_region_edges(&region_map)?;
        driver.seat_exit_free_sinks()?;

        Ok(LiftOutcome {
            function: driver.builder.build()?,
            unresolved_branches: driver.unresolved_branches,
            switch_anchors: driver.switch_anchors,
            return_sites: driver.return_sites,
        })
    }
}
