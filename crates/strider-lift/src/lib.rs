#![forbid(unsafe_code)]

use anyhow::{Result, anyhow};

mod arithmetic;
mod call;
mod cast;
mod cc_projection;
mod control;
mod dispatch;
mod exit_free_sink;
mod float;
mod function_lifter;
mod memory;
mod pcode_consts;
pub(crate) mod pcode_util;
mod pruned_ssa;
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

    /// Every register a LOAD / STORE addresses through the REGISTER space: the
    /// FOURTH source of tracked varnodes, beside the decoded instructions, the
    /// convention, and a `CallOther`'s footprint.
    ///
    /// The address is computed, so the register appears in no pcode operand and
    /// `aliasable_vns` cannot see it; untracked, it has no slot for
    /// `write_vn` to write and the lift fails.
    ///
    /// Silent about an address that does not resolve: the lift takes its
    /// opaque path there, which clobbers registers already in the set.
    ///
    /// The ENCLOSING declared register is seeded, not the resolved slice. A
    /// computed offset need not land on a declared boundary, and seeding a
    /// partially-overlapping slice would break the nesting the tracked set
    /// relies on; see [`Self::register_slot_of`].
    fn register_space_vns<'s>(
        &'s self,
        cfg: &'s strider_cfg::Cfg,
    ) -> impl Iterator<Item = rsleigh::Vn> + 's {
        let declared = self.declared_reg_vns();
        cfg.regions().flat_map(move |region| {
            let mut consts = pcode_consts::PcodeConsts::default();
            region.insns.iter().filter_map(move |wrapped| {
                consts.observe(wrapped.addr, &wrapped.insn);
                self.register_slot_of(&wrapped.insn, &consts)
                    .and_then(|s| vn_container::smallest_enclosing(declared, &s))
            })
        })
    }

    /// The registers the `cfg`'s rounding-mode ops with no input are
    /// ([`strider_target::call_other_abi::rounding_mode_register`]), but for
    /// one an override classifies.
    fn rounding_mode_register_vns<'s>(
        &'s self,
        cfg: &'s strider_cfg::Cfg,
        overrides: &'s strider_target::call_other_abi::CallOtherOverrides,
    ) -> impl Iterator<Item = rsleigh::Vn> + 's {
        let preset = self.arch().preset();
        cfg.regions()
            .flat_map(|region| region.insns.iter())
            .map(|wrapped| &wrapped.insn)
            .filter(|insn| insn.opcode == rsleigh::Opcode::CallOther)
            .filter_map(|insn| strider_cfg::user_op_of(insn, self.user_op_names()))
            .filter(|(_, name)| overrides.get(name).is_none())
            .filter_map(move |(_, name)| {
                strider_target::call_other_abi::rounding_mode_register(preset, name)
            })
            .filter_map(|reg| self.sleigh_regs().name_to_vn(reg))
    }

    /// Adds to `tracked` every register a `CallOther` in `cfg` touches through
    /// its ABI footprint instead of its pcode operands: the third source of tracked varnodes,
    /// beside the decoded instructions and the calling convention. x86-64
    /// `syscall` reads `R10` and writes `R11`, and SysV gives neither a role.
    ///
    /// Silent about what it cannot resolve: an unclassified user-op and an ABI
    /// name outside this arch's register table are both errors of lifting the
    /// op, raised there against the op's own name.
    fn call_other_footprint_vns(
        &self,
        cfg: &strider_cfg::Cfg,
        overrides: &strider_target::call_other_abi::CallOtherOverrides,
        tracked: &mut rustc_hash::FxHashSet<rsleigh::Vn>,
    ) {
        use strider_target::call_other_abi::{CallOtherClass, CallOtherLookup, classify_with};

        // Same universe rule as `aliasable_vns`: a footprint a Rust caller
        // built by hand can name any space.
        let aliasable = |vn: &rsleigh::Vn| vn_container::is_aliasable_space(vn.addr_space);
        for insn in cfg
            .regions()
            .flat_map(|region| region.insns.iter())
            .map(|wrapped| &wrapped.insn)
            .filter(|insn| insn.opcode == rsleigh::Opcode::CallOther)
        {
            let Some((_, name)) = strider_cfg::user_op_of(insn, self.user_op_names()) else {
                continue;
            };
            match classify_with(overrides, self.arch().preset(), name) {
                None | Some(CallOtherLookup::Class(CallOtherClass::NoOp)) => {}
                Some(CallOtherLookup::Class(CallOtherClass::Call(abi))) => tracked.extend(
                    abi.implicit_reads
                        .iter()
                        .chain(abi.implicit_writes)
                        .filter_map(|reg| self.sleigh_regs().name_to_vn(reg))
                        .filter(aliasable),
                ),
                Some(CallOtherLookup::Built(abi)) => tracked.extend(
                    abi.implicit_reads
                        .iter()
                        .chain(&abi.implicit_writes)
                        .copied()
                        .filter(aliasable),
                ),
            }
        }
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
        // The CFG is rebuilt from scratch each lift, so the tracked set is
        // always scanned fresh.  `FunctionLifter::new` adds the stack vn; the
        // lifter is the SSoT for that.
        let mut tracked: rustc_hash::FxHashSet<rsleigh::Vn> = aliasable_vns(cfg).collect();
        // A user-op's implicit footprint is named by neither the pcode nor the
        // convention, so it must be seeded before the universe is frozen.
        self.call_other_footprint_vns(cfg, cfg_opts.call_other_overrides, &mut tracked);
        // A rounding-mode op with no input reads its register only when the
        // claim is off, so only then is that register tracked.
        if !claims.fp_rounding_mode_is_nearest {
            tracked.extend(self.rounding_mode_register_vns(cfg, cfg_opts.call_other_overrides));
        }
        // A register a LOAD / STORE reaches through the REGISTER space is named
        // by a computed address rather than a pcode operand, so it is invisible
        // to `aliasable_vns` and has to be seeded here too.
        tracked.extend(self.register_space_vns(cfg));
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

/// `Function::new` keeps the largest of nested varnodes and orders them.
///
/// Only REGISTER and UNIQUE are tracked, the two spaces `read_vn` /
/// `write_vn` route through the aliasing path that consults this set; CONST
/// becomes a literal and RAM a Load/Store.  On x86-64 every `call`
/// contributes an `inst_next` CONST and every rip-relative operand a RAM
/// address, so tracking those grows the set, and the per-region variable map
/// `inherit_variables` clones, with the function.
pub(crate) fn aliasable_vns(cfg: &strider_cfg::Cfg) -> impl Iterator<Item = rsleigh::Vn> + '_ {
    cfg.regions()
        .flat_map(|region| region.insns.iter())
        .flat_map(|wrapped| wrapped.insn.all_vns())
        .filter(|vn| vn_container::is_aliasable_space(vn.addr_space))
}

pub(crate) type RegionMap = rustc_hash::FxHashMap<strider_cfg::RegionId, strider_ir::RegionId>;

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Allocates one IR region per CFG region, keyed by CFG `RegionId`; every
    /// CFG region is present.
    fn build_region_map(&mut self, placement: &pruned_ssa::PhiPlacement) -> Result<RegionMap> {
        let cfg = self.cfg;
        let mut region_map: RegionMap = RegionMap::default();
        for cfg_rid in cfg.region_ids() {
            // Sorted, so `Phi` creation order (hence node-id assignment)
            // follows `InitialVnId` rather than the placement set's hash
            // layout.
            let mut placed: Vec<strider_ir::node::InitialVnId> = placement
                .get(&cfg_rid)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            placed.sort_unstable();
            let ir_rid = self.builder.create_region(&placed)?;
            region_map.insert(cfg_rid, ir_rid);
            let region = &cfg.region_graph()[cfg_rid];
            let last_addr = region
                .insns
                .last()
                .map_or(region.start_addr.machine_addr.addr, |wrapped| {
                    wrapped.addr.machine_addr.addr
                });
            let ctrl = self.builder.region_cur_ctrl(ir_rid);
            let node = self.builder.function().graph().value_definition(ctrl).0;
            self.region_last_addrs.insert(node, last_addr);
        }
        let entry_ir = region_map[&cfg.entry()];
        self.builder.set_entry_region(entry_ir)?;
        // `bind_entry_address_register` writes a variable, which needs a
        // current region.
        self.builder.set_region(entry_ir);
        self.record_register_arg_carriers()?;
        self.bind_entry_address_register()?;
        Ok(region_map)
    }

    /// Binds [`strider_target::BuiltCallingConvention::entry_address`] to
    /// the entry address, so the prologue that turns it into the TOC / GOT
    /// pointer folds to a constant.
    ///
    /// Bound whatever address the analysis starts at, although the ABI
    /// guarantees it only at a symbol's global entry: a function entered below
    /// that never reads the register before writing it, the prologue that reads
    /// it being above the entry.
    ///
    /// An entry that is also a loop header holds the register's phi, and the
    /// binding is that phi's entry-edge operand: the back edge carries the
    /// loop's own value.
    fn bind_entry_address_register(&mut self) -> Result<()> {
        use strider_ir::{IRBuilderExt as _, IRViewer as _, VnTypeExt as _};
        let Some(vn) = self.builder.function().default_cc().entry_address else {
            return Ok(());
        };
        let container = self.container_map.container_of(&vn);
        // Untracked means the function never names it, so nothing reads it.
        if self.builder.function().vn_id_of(&container).is_none() {
            return Ok(());
        }
        let current = self.builder.read_variable(&container)?;
        let function = self.builder.function();
        let phi = function.producer(current);
        let phi = matches!(function.node_kind(phi), strider_ir::node::NodeKind::Phi).then_some(phi);
        // Input 0 is the phi token; input 1 the entry edge's `InitialVar`.
        if let Some(phi) = phi {
            let initial = function.graph().nth_input(phi, 1).ok_or_else(|| {
                anyhow::anyhow!("the entry phi of {container:?} has no entry-edge operand")
            })?;
            self.builder.write_variable(&container, initial)?;
        }
        let addr = self.entry_machine_addr();
        let ty = vn.int_type()?;
        self.with_lift_addr(Some(addr), |s| {
            let value = s.builder.build_int_const(u128::from(addr), ty)?;
            s.write_vn(&vn, value)
        })?;
        if let Some(phi) = phi {
            let bound = self.builder.read_variable(&container)?;
            let graph = self.builder.function_mut().graph_mut();
            let entry_edge = graph.node_input_id_at(phi, 1)?;
            graph.update_input(entry_edge, bound);
            self.builder.write_variable(&container, current)?;
        }
        Ok(())
    }

    /// Each arg-passing register's largest-container `InitialVar` output is the
    /// carrier for its positional index: a narrow ABI alias (`edi`) routes
    /// through its tracked container (`rdi`).
    ///
    /// Integer and float registers are numbered in separate index spaces, so
    /// the j-th float parameter is float carrier `j`, indexed by ABI position:
    /// a register the function never names leaves a gap instead of shifting
    /// the ones after it down. Float argument registers sharing a container
    /// (AAPCS-VFP `d0`/`d1` inside `q0`) each carry their own slice of it.
    /// The integer half,
    /// [`strider_ir::FunctionBuilder::record_register_arg_carriers`], records
    /// each register's container outright, where the float loop below records
    /// the register itself when several share a container.
    /// No shipped convention names two integer argument registers inside one
    /// container, so the two cannot disagree today; a convention that did would
    /// make one argument out of two here while the caller side still passes
    /// each as its own slice.
    fn record_register_arg_carriers(&mut self) -> Result<()> {
        self.builder.record_register_arg_carriers();
        let float_slots = {
            let function = self.builder.function();
            function
                .default_cc()
                .float_arg_slots(function.all_vns(), |v| self.container_map.container_of(v))
        };
        // The entry machine address, so a materialised slice carries a
        // fingerprint like every other non-exempt node.
        let entry_addr = Some(self.entry_machine_addr());
        for (j, carrier) in float_slots.into_iter().enumerate() {
            let Some(carrier) = carrier else { continue };
            // A slice out of a shared container dedups with the function's own
            // first read of the register unless the entry is a loop header.
            let value = self.with_lift_addr(entry_addr, |s| s.read_initial_reg_vn(&carrier))?;
            if let Some(value) = value {
                self.builder
                    .function_mut()
                    .side_tables_mut()
                    .register_float_arg_value(j as u32, value);
            }
        }
        Ok(())
    }

    /// Translates every region's instructions and, when present, its special
    /// terminator.  A special terminator's pcode insn is skipped inside the
    /// per-insn loop and lifted post-loop by a dedicated handler, fingerprinted
    /// to the region's last machine address.
    fn translate_regions(
        &mut self,
        region_map: &RegionMap,
        doms: &graph_algorithms::dominance::Dominators<strider_cfg::RegionId>,
    ) -> Result<()> {
        let cfg = self.cfg;
        // Pre-order matters: a region must be processed after its immediate
        // dominator so `inherit_variables` reads that dominator's FINAL values.
        // Siblings go ascending by `RegionId`, and every body node's NodeId
        // follows this order. Regions unreachable from the entry are absent.
        for cfg_rid in doms.tree_preorder() {
            let ir_region = region_map[&cfg_rid];
            // The entry region has no idom; `set_entry_region` seeded it.
            if let Some(idom_cfg) = doms.immediate_dominator(cfg_rid) {
                self.builder
                    .inherit_variables(ir_region, region_map[&idom_cfg]);
            }
            self.builder.set_region(ir_region);
            #[cfg(debug_assertions)]
            {
                self.current_cfg_region = Some(cfg_rid);
            }
            let region = &cfg.region_graph()[cfg_rid];
            let terminator = &region.terminator;
            self.pcode_consts.reset();
            for wrapped_insn in &region.insns {
                // Observed BEFORE the skip, and reset per region above, because
                // `collect_def_sites` feeds every op of every region through an
                // identical resolver. The two must see the same sequence or a
                // register write can land where no phi was placed for it.
                self.pcode_consts
                    .observe(wrapped_insn.addr, &wrapped_insn.insn);
                if skips_opcode(terminator, wrapped_insn.insn.opcode) {
                    continue;
                }
                self.process_insn(cfg_rid, &wrapped_insn.insn, wrapped_insn.addr, region_map)?;
            }
            // Fingerprint contributor for the terminator handlers: the region's
            // last pcode insn.  A region with zero pcode insns is a synthetic
            // tail-call stub, whose `Call + Return` is proven by the
            // predecessor's branch or fall-through, so fall back to that or its
            // nodes carry no fingerprint and fail the validator's non-empty
            // check.  A predecessor with no pcode insn is a zero-pcode-op
            // instruction falling through, which starts at its region's start.
            // `max` picks one deterministic contributor when several branches
            // share a deduped stub.
            let term_addr = region
                .insns
                .last()
                .map(|wrapped| wrapped.addr.machine_addr.addr)
                .or_else(|| {
                    cfg.region_predecessors(cfg_rid)
                        .map(|pred| {
                            let pred = &cfg.region_graph()[pred];
                            pred.insns
                                .last()
                                .map_or(pred.start_addr, |w| w.addr)
                                .machine_addr
                                .addr
                        })
                        .max()
                });
            // A `NoReturn` region ending in a `Call` or `CallIndirect` has an
            // open control edge (the per-insn loop lifted the call, which does
            // not terminate), so sink it into `Unreachable`.  Gate on the
            // opcode: a `CallOther` NoReturn region already self-terminated
            // inside `handle_call_other`, and terminating it twice fails.
            let noreturn_call =
                matches!(region.terminator, strider_cfg::RegionTerminator::NoReturn)
                    && region.insns.last().is_some_and(|w| {
                        matches!(
                            w.insn.opcode,
                            rsleigh::Opcode::Call | rsleigh::Opcode::CallIndirect
                        )
                    });
            self.with_lift_addr(term_addr, |s| match terminator {
                strider_cfg::RegionTerminator::UnresolvedIndirectBranch { target_vn, addr } => {
                    s.handle_unresolved_indirect_branch(target_vn, *addr)
                }
                strider_cfg::RegionTerminator::Switch {
                    target_vn,
                    targets,
                    addr,
                } => s.handle_switch(cfg_rid, target_vn, targets, region_map, *addr),
                // A target's `isa_bit` (the callee's ISA mode on an
                // interworking tail call) is the callee function's own
                // concern, applied when it is analyzed at its entry, not
                // while lifting this caller.
                strider_cfg::RegionTerminator::TailCall { target } => {
                    s.handle_tail_call(target.addr)
                }
                _ if noreturn_call => s.builder.build_unreachable(),
                _ => Ok(()),
            })?;
        }
        Ok(())
    }

    /// Wires the region successors no terminator handler wired: only
    /// `Unconditional` regions, whose `Branch` lifts to nothing.  `CondBranch`
    /// and `Switch` are already wired by their handlers.
    fn link_region_edges(&mut self, region_map: &RegionMap) -> Result<()> {
        let graph = self.cfg.region_graph();
        for (src, tgt) in graph.edge_indices().filter_map(|e| graph.edge_endpoints(e)) {
            if matches!(
                graph[src].terminator,
                strider_cfg::RegionTerminator::Unconditional
            ) {
                // Every CFG edge against a `translate_regions` that walks the
                // dominator pre-order, which covers every region only because
                // `strider_cfg::DecodeEngine::build_cfg` removes the unreachable ones.
                self.builder
                    .link_regions(region_map[&src], region_map[&tgt])?;
            }
        }
        Ok(())
    }
}

/// Whether the per-instruction loop must skip `opcode`, leaving the region's
/// terminator to the post-loop dispatch below.
///
/// `TailCall` skips `BranchIndirect` as well as `Branch`: a `known_targets`
/// resolution pointing out of the function marks the `jmp reg` a tail call,
/// and processing the `BranchIndirect` would emit an `IndirectBranch` and
/// terminate the region, failing `handle_tail_call`'s `build_call`.  A
/// `CondBranch` never lives in a TailCall region: it keeps its own
/// terminator, and the stub regions on its out-of-bounds arms have no insns.
///
/// Skipping by opcode alone is safe by region closure: `Branch` and
/// `BranchIndirect`, the only opcodes skipped, close the region in
/// `RegionBuilder::process_new_insn`, so at most one appears per region and
/// it is always the trailing entry, never an inner pcode op.  (`Call` /
/// `CallIndirect` / `CallOther` can close it too, but are never skipped.)
fn skips_opcode(terminator: &strider_cfg::RegionTerminator, opcode: rsleigh::Opcode) -> bool {
    match terminator {
        strider_cfg::RegionTerminator::UnresolvedIndirectBranch { .. }
        | strider_cfg::RegionTerminator::Switch { .. } => opcode == rsleigh::Opcode::BranchIndirect,
        strider_cfg::RegionTerminator::TailCall { .. } => matches!(
            opcode,
            rsleigh::Opcode::Branch | rsleigh::Opcode::BranchIndirect
        ),
        _ => false,
    }
}
