use anyhow::{Result, anyhow};

mod arithmetic;
mod boolean;
mod call;
mod cast;
mod cc_projection;
mod control;
mod dispatch;
mod dominance;
mod exit_free_sink;
mod float;
mod function_lifter;
mod integer;
mod memory;
mod misc;
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
}

pub use crate::lift_options::LiftOptions;

/// The CFG-to-IR lift engine, built once and reused across every function and
/// rebuild iteration.  The calling convention is per-function, hence a per-call
/// argument.
pub struct Lifter<R: rsleigh::MemReader> {
    arch: strider_target::SleighArch,
    /// Borrowed `&mut` to build the CFG, then `&` to lift it.
    sleigh: rsleigh::Sleigh<R>,
    /// Cached at construction: `Sleigh::regs()` is expensive.
    sleigh_regs: rsleigh::SleighRegs,
    user_op_names: Vec<String>,
    /// Flowing context vars, discovered once (constant per sla) and lent to
    /// every `build_cfg` so decode mode propagates along CFG edges.
    flow_vars: strider_cfg::FlowVars,
    /// The flow context of a cold entry on a fresh engine (the pspec defaults),
    /// captured before any decode.  Each function's entry is reset to this to
    /// undo a prior function's leaked commit on the reused engine.
    entry_defaults: strider_cfg::FlowContext,
    /// The same, for the `noflow` vars that still change the decode
    /// ([`SleighArch::transient_decode_vars`]).  They are outside `flow_vars`
    /// by construction, so `reset_at` cannot reach them.
    transient_defaults: Vec<(&'static str, u32)>,
}

impl<R: rsleigh::MemReader> Lifter<R> {
    pub fn new(arch: strider_target::SleighArch, sleigh: rsleigh::Sleigh<R>) -> Result<Self> {
        let sleigh_regs = sleigh.regs()?;
        let user_op_names = sleigh.user_op_names().unwrap_or_default();
        let flow_vars = strider_cfg::FlowVars::discover(&sleigh)?;
        // Read on the still-fresh engine, so this is the pspec default, not a
        // leak.  The address is immaterial: a flowing var's default is committed
        // globally, so any address reads the same value.
        let entry_defaults = flow_vars.snapshot(&sleigh, 0);
        let transient_defaults = arch
            .transient_decode_vars()
            .iter()
            .filter_map(|name| Some((*name, sleigh.get_context_at(0, name).ok()?)))
            .collect();
        Ok(Self {
            arch,
            sleigh,
            sleigh_regs,
            user_op_names,
            flow_vars,
            entry_defaults,
            transient_defaults,
        })
    }

    #[must_use]
    pub fn arch(&self) -> strider_target::SleighArch {
        self.arch
    }

    /// Indexed by `user_op_id`.
    #[must_use]
    pub fn user_op_names(&self) -> &[String] {
        &self.user_op_names
    }

    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        &self.sleigh
    }

    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        &self.sleigh_regs
    }

    /// `per_address_ccs` supplies CC overrides for call TARGETS.  Pass an
    /// empty map for no overrides.
    pub fn build_cfg(
        &mut self,
        entry: strider_cfg::MachineInsnAddr,
        cfg_opts: &strider_cfg::CfgOptions,
        per_address_ccs: &rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Result<strider_cfg::Cfg> {
        // A prior function's `globalset` holds forward until the next change
        // point and leaks into this cold entry on a reused engine.
        let entry_mode = self.arch.entry_mode_context(entry.addr);
        let decode_addr = if entry_mode.is_some() {
            entry.addr & !1
        } else {
            entry.addr
        };
        self.flow_vars
            .reset_at(&mut self.sleigh, decode_addr, &self.entry_defaults)?;
        // A `noflow` commit holds at exactly the address it was made for, so a
        // prior function's `mov lr,pc` leaves `LRset` set at THIS entry and its
        // `bx` would decode as an indirect call. `reset_at` covers the flowing
        // vars only, so clear these by name.
        for (name, default) in &self.transient_defaults {
            if self.sleigh.get_context_at(decode_addr, name)? != *default {
                self.sleigh.set_context_at(decode_addr, name, *default)?;
            }
        }
        // Take the entry mode from the address low bit (ARM Thumb via `TMode`,
        // MIPS16 via `ISA_MODE`), overriding the default reset above.
        if let Some((var, value)) = entry_mode {
            self.sleigh.set_context_at(decode_addr, var, value)?;
        }
        // The function's now-committed ISA mode, the base context the builder
        // decodes a strider-resolved target in.
        let function_mode = self.flow_vars.snapshot(&self.sleigh, decode_addr);
        strider_cfg::Builder::for_arch(&self.arch, &mut self.sleigh, decode_addr, cfg_opts)
            .with_flow_context(&self.flow_vars, function_mode)
            .with_per_address_ccs(per_address_ccs.clone())
            .build()
    }

    /// Returns the unique set only; ordering is applied by
    /// `FunctionBuilder::new`.
    ///
    /// Only REGISTER and UNIQUE are tracked, the two spaces `read_vn` /
    /// `write_vn` route through the aliasing path that consults this set; CONST
    /// becomes a literal and RAM a Load/Store.  On x86-64 every `call`
    /// contributes an `inst_next` CONST and every rip-relative operand a RAM
    /// address, so tracking those grows the set, and the per-region variable map
    /// `inherit_variables` clones, with the function.
    pub(crate) fn find_all_unique_vns(&self, cfg: &strider_cfg::Cfg) -> Vec<rsleigh::Vn> {
        cfg.regions()
            .flat_map(|region| region.insns.iter())
            .flat_map(|wrapped| wrapped.insn.all_vns())
            .filter(|vn| {
                matches!(
                    vn.addr_space,
                    rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE
                )
            })
            .collect::<rustc_hash::FxHashSet<rsleigh::Vn>>()
            .into_iter()
            .collect()
    }

    /// Every register a LOAD / STORE addresses through the REGISTER space: the
    /// FOURTH source of tracked varnodes, beside the decoded instructions, the
    /// convention, and a `CallOther`'s footprint.
    ///
    /// The address is computed, so the register appears in no pcode operand and
    /// `find_all_unique_vns` cannot see it. Without this the register is not in
    /// the universe, `write_vn` has nothing to write, and the lift fails on a
    /// function that used to (wrongly) lift the write as memory.
    ///
    /// Silent about an address that does not fold: that is the lift's error to
    /// raise, against the op, where it can say so.
    fn register_space_vns(&self, cfg: &strider_cfg::Cfg) -> Vec<rsleigh::Vn> {
        let mut found: rustc_hash::FxHashSet<rsleigh::Vn> = rustc_hash::FxHashSet::default();
        for region in cfg.regions() {
            let mut consts = pcode_consts::PcodeConsts::default();
            for wrapped in &region.insns {
                consts.observe(wrapped.addr, &wrapped.insn);
                let vn = match wrapped.insn.opcode {
                    rsleigh::Opcode::Store => {
                        pcode_consts::register_store_target(&wrapped.insn, &consts)
                    }
                    rsleigh::Opcode::Load => {
                        pcode_consts::register_load_source(&wrapped.insn, &consts)
                    }
                    _ => None,
                };
                if let Some(vn) = vn {
                    found.insert(vn);
                }
            }
        }
        found.into_iter().collect()
    }

    /// Every register a `CallOther` in `cfg` touches through its ABI footprint
    /// instead of its pcode operands: the third source of tracked varnodes,
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
    ) -> Vec<rsleigh::Vn> {
        use strider_target::call_other_abi::{CallOtherClass, CallOtherLookup, classify_with};

        let mut found: rustc_hash::FxHashSet<rsleigh::Vn> = rustc_hash::FxHashSet::default();
        for insn in cfg
            .regions()
            .flat_map(|region| region.insns.iter())
            .map(|wrapped| &wrapped.insn)
            .filter(|insn| insn.opcode == rsleigh::Opcode::CallOther)
        {
            let Ok((_, name)) = call::decode_user_op(insn, self.user_op_names()) else {
                continue;
            };
            match classify_with(overrides, self.arch.preset(), name) {
                None | Some(CallOtherLookup::Class(CallOtherClass::NoOp)) => {}
                Some(CallOtherLookup::Class(CallOtherClass::Call(abi))) => found.extend(
                    abi.implicit_reads
                        .iter()
                        .chain(abi.implicit_writes)
                        .filter_map(|reg| self.sleigh_regs.name_to_vn(reg)),
                ),
                Some(CallOtherLookup::Built(abi)) => found.extend(
                    abi.implicit_reads
                        .iter()
                        .chain(&abi.implicit_writes)
                        .copied(),
                ),
            }
        }
        // Same universe rule as `find_all_unique_vns`: a footprint a Rust
        // caller built by hand can name any space.
        found
            .into_iter()
            .filter(|vn| {
                matches!(
                    vn.addr_space,
                    rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE
                )
            })
            .collect()
    }

    /// [`Self::build_ir_with`] under default [`LiftOptions`].
    pub fn build_ir(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
    ) -> Result<LiftOutcome> {
        self.build_ir_with(cfg, cc, &LiftOptions::default())
    }

    /// `cc` is the function default; a direct Call whose target appears in
    /// [`LiftOptions::per_address_ccs`] is built with that override instead.
    pub fn build_ir_with(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
        opts: &LiftOptions,
    ) -> Result<LiftOutcome> {
        self.build_ir_counting_sink_visits(cfg, cc, opts)
            .map(|(outcome, _)| outcome)
    }

    /// Also reports the node visits exit-free-sink seating performed, which
    /// the scaling test pins against the number of cycles.
    pub(crate) fn build_ir_counting_sink_visits(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
        opts: &LiftOptions,
    ) -> Result<(LiftOutcome, usize)> {
        // The CFG is rebuilt from scratch each lift, so the tracked set is
        // always scanned fresh.  `FunctionLifter::new` adds the stack vn; the
        // lifter is the SSoT for that.
        let mut all_vns = self.find_all_unique_vns(cfg);
        // A user-op's implicit footprint is named by neither the pcode nor the
        // convention, so it must be seeded before the universe is frozen.  Set
        // membership, not a scan: the Vec is the SSoT for order only.
        let mut seen: rustc_hash::FxHashSet<rsleigh::Vn> = all_vns.iter().copied().collect();
        for vn in self.call_other_footprint_vns(cfg, &opts.cfg.call_other_overrides) {
            if seen.insert(vn) {
                all_vns.push(vn);
            }
        }
        // A register a LOAD / STORE reaches through the REGISTER space is named
        // by a computed address rather than a pcode operand, so it is invisible
        // to `find_all_unique_vns` and has to be seeded here too.
        for vn in self.register_space_vns(cfg) {
            if seen.insert(vn) {
                all_vns.push(vn);
            }
        }
        let mut driver = FunctionLifter::new(
            self,
            cc,
            cfg,
            all_vns,
            &opts.per_address_ccs,
            &opts.cfg.call_other_overrides,
        )?;

        // Cytron pruned-SSA phi placement: iterated dominance frontier of each
        // variable's definition sites.  This is what stops the lifter minting a
        // value `Phi` for every varnode at every region.
        let dom = dominance::DomInfo::compute(cfg);
        let def_sites = driver.collect_def_sites()?;
        let placement = dom.iterated_frontier(&def_sites);

        let region_map = driver.build_region_map(&placement)?;

        // Dominator-tree pre-order, so each region inherits reaching variable
        // values from its already-processed immediate dominator.  Then wire the
        // fallthrough edges the per-insn loop didn't reach.
        driver.translate_regions(&region_map, &dom)?;
        driver.link_region_edges(&region_map)?;
        let sink_visits = driver.seat_exit_free_sinks()?;

        let unresolved_branches = std::mem::take(&mut driver.unresolved_branches);
        let switch_anchors = std::mem::take(&mut driver.switch_anchors);
        let function = driver.builder.build()?;
        Ok((
            LiftOutcome {
                function,
                unresolved_branches,
                switch_anchors,
            },
            sink_visits,
        ))
    }
}

pub(crate) type RegionMap = rustc_hash::FxHashMap<strider_cfg::RegionId, strider_ir::RegionId>;

pub(crate) fn ir_region_of(
    region_map: &RegionMap,
    cfg_rid: strider_cfg::RegionId,
) -> Result<strider_ir::RegionId> {
    region_map
        .get(&cfg_rid)
        .copied()
        .ok_or_else(|| anyhow!("no region {cfg_rid:?} in cfg"))
}

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
            let region = cfg
                .region_graph()
                .node_weight(cfg_rid)
                .ok_or_else(|| anyhow!("no region {cfg_rid:?} in cfg"))?;
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
        let entry_ir = *region_map
            .get(&cfg.entry())
            .ok_or_else(|| anyhow!("entry region {:?} missing from region_map", cfg.entry()))?;
        self.builder.set_entry_region(entry_ir)?;
        // Carriers for float arguments sharing a container are read out of it,
        // which needs a current region.
        self.builder.set_region(entry_ir);
        self.record_register_arg_carriers()?;
        Ok(region_map)
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
    /// The integer loop records `container_of(reg)` outright, where the float
    /// loop below records the register itself when several share a container.
    /// No shipped convention names two integer argument registers inside one
    /// container, so the two cannot disagree today; a convention that did would
    /// make one argument out of two here while the caller side still passes
    /// each as its own slice.
    fn record_register_arg_carriers(&mut self) -> Result<()> {
        let arg_regs = self
            .builder
            .function()
            .default_cc()
            .arg_passing_regs
            .clone();
        for (i, reg) in arg_regs.iter().enumerate() {
            let container = self.container_of(reg);
            if let Some(value) = self.builder.function().initial_var_value(&container) {
                self.builder
                    .function_mut()
                    .side_tables_mut()
                    .register_arg_value(i as u32, value);
            }
        }
        let float_slots = {
            let function = self.builder.function();
            function
                .default_cc()
                .float_arg_slots(function.all_vns(), |v| self.container_of(v))
        };
        // The entry machine address, so a materialised slice carries a
        // fingerprint like every other non-exempt node.
        let entry_addr = self.entry_machine_addr();
        for (j, carrier) in float_slots.into_iter().enumerate() {
            let Some(carrier) = carrier else { continue };
            let value = if self.container_of(&carrier) == carrier {
                self.builder.function().initial_var_value(&carrier)
            } else {
                // A slice out of a shared container dedups with the function's
                // own first read of the register while the container still
                // holds its incoming value.
                Some(self.with_lift_addr(entry_addr, |s| s.read_reg_vn(&carrier))?)
            };
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
        dom: &dominance::DomInfo,
    ) -> Result<()> {
        let cfg = self.cfg;
        // Pre-order matters: a region must be processed after its immediate
        // dominator so `inherit_variables` reads that dominator's FINAL values.
        for &cfg_rid in dom.preorder() {
            let ir_region = ir_region_of(region_map, cfg_rid)?;
            // The entry region has no idom; `set_entry_region` seeded it.
            if let Some(idom_cfg) = dom.immediate_dominator(cfg_rid) {
                let idom_ir = ir_region_of(region_map, idom_cfg)?;
                self.builder.inherit_variables(ir_region, idom_ir);
            }
            self.builder.set_region(ir_region);
            let region = cfg
                .region_graph()
                .node_weight(cfg_rid)
                .ok_or_else(|| anyhow!("no region {cfg_rid:?} in cfg"))?;
            let special_terminator = SpecialTerm::from_terminator(&region.terminator);
            self.pcode_consts.reset();
            for wrapped_insn in &region.insns {
                // Observed BEFORE the skip, and reset per region above, because
                // `collect_def_sites` feeds every op of every region through an
                // identical resolver. The two must see the same sequence or a
                // register write can land where no phi was placed for it.
                self.pcode_consts
                    .observe(wrapped_insn.addr, &wrapped_insn.insn);
                if special_terminator
                    .as_ref()
                    .is_some_and(|s| s.skips_opcode(wrapped_insn.insn.opcode))
                {
                    continue;
                }
                self.process_insn(cfg_rid, &wrapped_insn.insn, wrapped_insn.addr, region_map)?;
            }
            // Fingerprint contributor for the terminator handlers: the region's
            // last pcode insn.  A region with zero pcode insns is a synthetic
            // tail-call stub, whose `Call + Return` is proven by the
            // predecessor's conditional branch, so fall back to that or its
            // nodes carry no fingerprint and fail the validator's non-empty
            // check.  `max` picks one deterministic contributor when several
            // branches share a deduped stub.
            let term_addr = region
                .insns
                .last()
                .map(|wrapped| wrapped.addr.machine_addr.addr)
                .or_else(|| {
                    cfg.region_predecessors(cfg_rid)
                        .filter_map(|pred| pred.insns.last())
                        .map(|wrapped| wrapped.addr.machine_addr.addr)
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
            self.with_lift_addr(term_addr, |s| {
                match special_terminator {
                    Some(SpecialTerm::UnresolvedIndirect { target_vn, addr }) => {
                        s.handle_unresolved_indirect_branch(&target_vn, addr)?;
                    }
                    Some(SpecialTerm::Switch(target_vn, targets, switch_addr)) => {
                        s.handle_switch(cfg_rid, &target_vn, &targets, region_map, switch_addr)?;
                    }
                    Some(SpecialTerm::TailCall(target)) => {
                        s.handle_tail_call(target)?;
                    }
                    None => {
                        if noreturn_call {
                            s.builder.build_unreachable()?;
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Wires the region successors no terminator handler wired: only
    /// `Unconditional` regions, whose `handle_branch` is a no-op.  `CondBranch`
    /// and `Switch` are already wired by their handlers.
    fn link_region_edges(&mut self, region_map: &RegionMap) -> Result<()> {
        let cfg = self.cfg;
        for edge_idx in cfg.region_graph().edge_indices() {
            let Some((src, tgt)) = cfg.region_graph().edge_endpoints(edge_idx) else {
                continue;
            };
            let src_terminator = &cfg
                .region_graph()
                .node_weight(src)
                .ok_or_else(|| anyhow!("no region {src:?} in cfg"))?
                .terminator;
            if matches!(src_terminator, strider_cfg::RegionTerminator::Unconditional) {
                self.builder.link_regions(
                    ir_region_of(region_map, src)?,
                    ir_region_of(region_map, tgt)?,
                )?;
            }
        }
        Ok(())
    }
}

/// Marks a region whose terminator pcode insn the per-instruction loop must
/// skip, so the post-loop dispatch can lift it via a dedicated handler.
enum SpecialTerm {
    /// Emits an `IndirectBranch` placeholder anchoring the dispatch varnode.
    UnresolvedIndirect {
        target_vn: rsleigh::Vn,
        addr: strider_cfg::PcodeInsnAddr,
    },
    /// Resolved jump table; lifts to a single `Switch` node with one control
    /// output per target.  Carries the dispatch address so the resolver can
    /// re-derive and widen an already-seated site.
    Switch(rsleigh::Vn, Vec<u64>, strider_cfg::PcodeInsnAddr),
    /// Branch out of the function (`fn_max_size` exceeded, or below
    /// `start_addr` with `allow_code_before_start_addr=false`).  Lifts to
    /// `Call(IntConst(target)) + Return`.  The synthetic conditional-tail-call
    /// stub region carries this too, but has zero insns to skip.
    TailCall(u64),
}

impl SpecialTerm {
    fn from_terminator(t: &strider_cfg::RegionTerminator) -> Option<Self> {
        match t {
            strider_cfg::RegionTerminator::UnresolvedIndirectBranch { target_vn, addr } => {
                Some(SpecialTerm::UnresolvedIndirect {
                    target_vn: *target_vn,
                    addr: *addr,
                })
            }
            strider_cfg::RegionTerminator::Switch {
                target_vn,
                targets,
                addr,
            } => Some(SpecialTerm::Switch(
                *target_vn,
                targets.iter().map(|t| t.addr).collect(),
                *addr,
            )),
            // A target's `isa_bit` (the callee's ISA mode on an interworking
            // tail call) is the callee function's own concern, applied when it
            // is analyzed at its entry, not while lifting this caller.
            strider_cfg::RegionTerminator::TailCall { target } => {
                Some(SpecialTerm::TailCall(target.addr))
            }
            _ => None,
        }
    }

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
    fn skips_opcode(&self, opcode: rsleigh::Opcode) -> bool {
        match self {
            SpecialTerm::UnresolvedIndirect { .. } | SpecialTerm::Switch(..) => {
                opcode == rsleigh::Opcode::BranchIndirect
            }
            SpecialTerm::TailCall(..) => matches!(
                opcode,
                rsleigh::Opcode::Branch | rsleigh::Opcode::BranchIndirect
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    /// AArch64 `ret` is a `BranchIndirect` on `x30` in Sleigh, but the cfg
    /// marks it `RegionTerminator::Return`, so it must route through
    /// `handle_return` and leave `unresolved_branches` empty.
    #[test]
    fn aarch64_bx_lr_lifts_to_cc_return_not_indirect() {
        use strider_ir::node::NodeKind;
        use strider_ir_test_utils::IrWalkerEx;

        let arch = strider_target::SleighArch::aarch64();
        // `probe_regs` consumes the arch, hence the second copy.
        let regs = strider_target::SleighArch::aarch64()
            .probe_regs()
            .expect("probe regs");
        let cc = strider_target::CallingConvention::aarch64_aapcs64()
            .build(&regs)
            .expect("build cc");
        // AArch64 `ret` = 0xD65F03C0, little-endian.
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc0, 0x03, 0x5f, 0xd6], 0x1000);
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        let cfg = strider_cfg::Builder::for_arch(
            &arch,
            &mut sleigh,
            0x1000,
            &strider_cfg::CfgOptions::default(),
        )
        .build()
        .expect("cfg");
        let lifter = super::Lifter::new(arch, sleigh).expect("lifter");
        let outcome = lifter.build_ir(&cfg, cc).expect("build_ir");

        assert!(
            outcome.unresolved_branches.is_empty(),
            "`ret` must NOT defer an indirect branch; got {} unresolved",
            outcome.unresolved_branches.len(),
        );
        let function = &outcome.function;
        assert!(
            function.has_kind(|k| matches!(k, NodeKind::Return)),
            "`ret` must lift to a CC Return node"
        );
        assert!(
            !function.has_kind(|k| matches!(k, NodeKind::IndirectBranch)),
            "`ret` must NOT emit an IndirectBranch placeholder"
        );
    }

    /// A reused `Lifter` must decode each function in its own mode.  A Thumb
    /// `BLX <imm>` that switches to ARM `globalset`s `TMode` for its target,
    /// and that commit holds forward across `lift_one` calls.  `build_cfg` pins
    /// each entry's mode (here Thumb, the `arm_thumb` default) at the entry
    /// address, overriding the leaked commit; without it, lifting Thumb
    /// function A (ending in such a `BLX`) then Thumb function B at an address
    /// the commit reaches decodes B as ARM and walks off into unmapped memory.
    #[test]
    fn reused_lifter_pins_thumb_mode_between_functions() {
        use strider_ir::node::NodeKind;
        use strider_ir_test_utils::IrWalkerEx;

        let arch = strider_target::SleighArch::arm_thumb();
        let regs = strider_target::SleighArch::arm_thumb()
            .probe_regs()
            .expect("regs");
        let cc = strider_target::CallingConvention::arm_aapcs()
            .build(&regs)
            .expect("cc");
        // Buffer at 0x1000; both entries are passed with the Thumb bit set
        // (0x1001 / 0x1011), which is what selects Thumb state.  The BLX
        // target is `((inst_start + 4) & !3) + (part2off_10 << 2)` = 0x1008,
        // and its `globalset(TMode = 0)` holds forward from there over B.
        //   0x1000: BLX 0x1008  (Thumb T2, switches to ARM at 0x1008)
        //   0x1004: bx lr       ends function A
        //   0x1006: nop x3      padding, then bx lr; nop at 0x100c
        //   0x1010: bx lr; nop   function B, valid Thumb
        let code = vec![
            0x00, 0xf0, 0x03, 0xe8, // 0x1000 BLX 0x1008
            0x70, 0x47, // 0x1004 bx lr
            0x00, 0xbf, 0x00, 0xbf, 0x00, 0xbf, // 0x1006 nop padding
            0x70, 0x47, 0x00, 0xbf, // 0x100c padding
            0x70, 0x47, 0x00, 0xbf, // 0x1010 function B
        ];
        let empty = rustc_hash::FxHashMap::default();
        let opts = strider_cfg::CfgOptions::default();
        let new_lifter = || {
            let reader = rsleigh::mem_readers::BufMemReader::new(code.clone(), 0x1000);
            let sleigh =
                rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
            super::Lifter::new(arch, sleigh).expect("lifter")
        };

        // Lift A (the polluter) first, then B on the same engine.
        let mut lifter = new_lifter();
        let cfg_a = lifter
            .build_cfg(0x1001u64.into(), &opts, &empty)
            .expect("A cfg");
        lifter.build_ir(&cfg_a, cc.clone()).expect("A ir");
        let cfg_b = lifter.build_cfg(0x1011u64.into(), &opts, &empty).expect(
            "reused lifter must pin B's Thumb mode so its decode does not inherit A's ARM mode",
        );
        let reused_b = lifter.build_ir(&cfg_b, cc.clone()).expect("B ir");
        assert!(
            reused_b
                .function
                .has_kind(|k| matches!(k, NodeKind::Return)),
            "B (Thumb `bx lr`) lifted after A must decode as Thumb and emit a Return"
        );

        // Same B on a fresh lifter: the ground-truth decode.
        let mut fresh = new_lifter();
        let cfg_fresh = fresh
            .build_cfg(0x1011u64.into(), &opts, &empty)
            .expect("fresh B cfg");
        let fresh_b = fresh.build_ir(&cfg_fresh, cc).expect("fresh B ir");
        assert_eq!(
            reused_b.function.count_kind(|_| true),
            fresh_b.function.count_kind(|_| true),
            "B lifted after A must have the same node count as a fresh Thumb lift"
        );
    }

    /// Under the plain `arm` arch (ARM by default), a function whose entry
    /// carries the Thumb bit (odd address) decodes as Thumb at `addr & !1`, and
    /// the pinned mode holds forward past the entry: the second instruction (at
    /// `addr + 2`) must also decode Thumb, not fall back to the ARM default.
    #[test]
    fn arm_arch_uses_the_thumb_bit_to_decode_a_thumb_function() {
        use strider_ir::node::NodeKind;
        use strider_ir_test_utils::IrWalkerEx;

        let arch = strider_target::SleighArch::arm();
        let regs = strider_target::SleighArch::arm()
            .probe_regs()
            .expect("regs");
        let cc = strider_target::CallingConvention::arm_aapcs()
            .build(&regs)
            .expect("cc");
        // Two Thumb instructions at 0x1000: `movs r0, #1` (0x1000) then `bx lr`
        // (0x1002). The `bx lr` is only reachable if 0x1002 also decodes Thumb;
        // as a 4-byte ARM insn it would run past the 4-byte buffer and fail.
        let code = vec![0x01, 0x20, 0x70, 0x47];
        let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        let mut lifter = super::Lifter::new(arch, sleigh).expect("lifter");
        let opts = strider_cfg::CfgOptions::default();
        let empty = rustc_hash::FxHashMap::default();

        // Entry passed with the Thumb bit set (0x1001). Under the ARM default
        // this would try to decode the misaligned 0x1001 as ARM; the bit must
        // select Thumb and decode at 0x1000.
        let cfg = lifter
            .build_cfg(0x1001u64.into(), &opts, &empty)
            .expect("the Thumb bit must select Thumb mode and decode at addr & !1");
        let ir = lifter.build_ir(&cfg, cc).expect("ir");
        assert!(
            ir.function.has_kind(|k| matches!(k, NodeKind::Return)),
            "the `bx lr` at 0x1002 must lift to a Return, proving the pin held forward"
        );
    }

    /// Every register the arch DECLARES must have a `ValueType`, whether or
    /// not the current sla references it: the tracked-varnode set is mapped
    /// wholesale, so one unmappable width fails the whole function.  x86-64
    /// declares `GDTR`/`IDTR` at 12 bytes and `LDTR`/`TR` at 14.
    #[test]
    fn every_declared_register_width_maps_to_a_value_type() {
        for arch in [
            strider_target::SleighArch::x86_64(),
            strider_target::SleighArch::x86(),
            strider_target::SleighArch::aarch64(),
            strider_target::SleighArch::arm(),
            strider_target::SleighArch::mipsle64(),
            strider_target::SleighArch::ppc64be(),
        ] {
            let preset = arch.preset();
            let regs = arch.probe_regs().expect("probe regs");
            let unmappable: Vec<(&str, u32)> = regs
                .iter()
                .filter(|r| strider_ir::ValueType::int_for_byte_size(r.vn.size).is_err())
                .map(|r| (r.name, r.vn.size))
                .collect();
            assert!(
                unmappable.is_empty(),
                "{preset:?}: registers with no ValueType: {unmappable:?}"
            );
        }
    }

    /// `Phi` creation order comes from the placement set, so it must be sorted
    /// by `InitialVnId` rather than left to hash layout: node-id assignment,
    /// and every IR dump with it, otherwise moves with the hasher.
    #[test]
    fn placed_phis_are_ordered_by_initial_vn_id() {
        use strider_ir::node::NodeKind;
        use strider_ir::{IRViewer, IRWalker};

        let arch = strider_target::SleighArch::x86_64();
        let regs = strider_target::SleighArch::x86_64()
            .probe_regs()
            .expect("regs");
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .build(&regs)
            .expect("cc");
        // test edi,edi ; je +0x14 ; mov eax,1 ; mov ecx,2 ; mov edx,3 ;
        // mov esi,4 ; ret
        // Four registers live into one join region.
        let code = vec![
            0x85, 0xff, 0x74, 0x14, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xb9, 0x02, 0x00, 0x00, 0x00,
            0xba, 0x03, 0x00, 0x00, 0x00, 0xbe, 0x04, 0x00, 0x00, 0x00, 0xc3,
        ];
        let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        let cfg = strider_cfg::Builder::for_arch(
            &arch,
            &mut sleigh,
            0x1000,
            &strider_cfg::CfgOptions::default(),
        )
        .build()
        .expect("cfg");
        let lifter = super::Lifter::new(arch, sleigh).expect("lifter");
        let f = lifter.build_ir(&cfg, cc).expect("ir").function;

        // Ascending node id, so the group order below is creation order.
        let mut walk = f.walk();
        walk.by_ref().for_each(|_| {});
        let reachable = walk.into_visited();

        // Grouped by the region's phi token (a `Phi`'s input 0), so phis from
        // different regions do not interleave.
        let mut per_region: rustc_hash::FxHashMap<
            strider_ir::node::ValueId,
            Vec<strider_ir::node::InitialVnId>,
        > = rustc_hash::FxHashMap::default();
        for (node, _) in f
            .reachable_kind_iter(&reachable)
            .filter(|(_, k)| matches!(k, NodeKind::Phi))
        {
            let token = f.node_inputs(node)[0];
            let [out] = f.node_outputs_exact::<1>(node).expect("phi output");
            let vn = f.get_vn_for_value(out).expect("phi carries a vn tag");
            let id = f.vn_id_of(&vn).expect("tagged vn is tracked");
            per_region.entry(token).or_default().push(id);
        }
        let widest = per_region
            .into_values()
            .max_by_key(Vec::len)
            .expect("at least one region with phis");
        assert!(widest.len() >= 2, "need a multi-phi join, got {widest:?}");
        assert!(
            widest.windows(2).all(|w| w[0] < w[1]),
            "ascending node ids must carry ascending InitialVnIds, got {widest:?}"
        );
    }

    /// The tracked set exists to serve `read_vn` / `write_vn`'s aliasing path,
    /// which only REGISTER and UNIQUE take; CONST becomes a literal and RAM a
    /// Load/Store against a constant address. Tracking either of the latter
    /// mints an `InitialVar` for something that is not a variable, and widens
    /// the per-region variable map `inherit_variables` clones.
    #[test]
    fn only_aliasable_space_varnodes_are_tracked() {
        let arch = strider_target::SleighArch::x86_64();
        // 1000: e8 00 00 00 00          call 0x1005      ; CONST inst_next
        // 1005: 48 8b 05 00 00 00 00    mov rax,[rip+0x0] ; RAM 0x100c
        // 100c: eb 00                   jmp 0x100e        ; RAM 0x100e
        // 100e: c3                      ret
        let code = vec![
            0xe8, 0x00, 0x00, 0x00, 0x00, 0x48, 0x8b, 0x05, 0x00, 0x00, 0x00, 0x00, 0xeb, 0x00,
            0xc3,
        ];
        let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        let mut lifter = super::Lifter::new(arch, sleigh).expect("lifter");
        let opts = strider_cfg::CfgOptions::default();
        let empty = rustc_hash::FxHashMap::default();
        let cfg = lifter
            .build_cfg(0x1000u64.into(), &opts, &empty)
            .expect("cfg");

        let tracked = lifter.find_all_unique_vns(&cfg);
        assert!(!tracked.is_empty(), "the lift tracks something");
        let stray: Vec<_> = tracked
            .iter()
            .filter(|vn| {
                !matches!(
                    vn.addr_space,
                    rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE
                )
            })
            .map(|vn| (vn.addr_space.shortcut(), vn.addr_off, vn.size))
            .collect();
        assert!(
            stray.is_empty(),
            "only REGISTER/UNIQUE may be tracked; got {stray:?}"
        );
    }
}
