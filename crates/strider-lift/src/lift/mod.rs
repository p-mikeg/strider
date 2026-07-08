//! Binary CFG → IR lifting.  Owns the region-by-region translation of a
//! `strider_cfg::Cfg` into a `strider_ir::Function`, given a resolved set
//! of indirect-branch targets.  No optimization — that is the
//! orchestrator's concern.

use anyhow::{Result, anyhow};

mod arithmetic;
mod boolean;
mod call;
mod cast;
mod cc_projection;
mod control;
mod dispatch;
mod dominance;
mod float;
mod function_lifter;
mod integer;
mod memory;
mod misc;
pub(crate) mod pcode_util;
mod pruned_ssa;
mod vn_io;

#[cfg(test)]
mod handler_tests;

#[cfg(test)]
mod aliasing_tests;

#[cfg(test)]
mod cc_projection_tests;

pub(crate) use function_lifter::FunctionLifter;

/// The full result of a strider lift, exposing the lifted IR plus the
/// placeholder-anchor side-table the indirect-branch resolver consumes.
///
/// Returned by [`Lifter::build_ir`].  Callers that only need the
/// function can use `outcome.function` directly; indirect-branch-resolver-aware
/// callers read `unresolved_branches`.
pub struct LiftOutcome {
    /// The lifted IR ready for the optimiser pipeline.
    pub function: strider_ir::Function,
    /// One entry per region whose CFG terminator was
    /// [`strider_cfg::RegionTerminator::UnresolvedIndirectBranch`] at lift
    /// time.  Each entry maps the offending `BranchIndirect`'s pcode
    /// address to the `NodeId` of the `IndirectBranch` placeholder that
    /// anchors its dispatch varnode.  The orchestrator uses this
    /// correlation to key the post-pass classifier's results (which are
    /// node-keyed) back to the dispatch pcode address.  Empty in the
    /// common case (no deferred branches).
    pub unresolved_branches: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
}

/// The single options type for the whole binary → IR lift, re-exported
/// from the crate root.  The CFG builder reads its CFG-shaping knobs
/// (`fn_max_size`, `allow_code_before_start_addr`, `known_targets`); the
/// lifter reads its IR-lift knob (`per_address_ccs`).
pub use crate::lift_options::LiftOptions;

/// The CFG→IR lift engine: owns the target `SleighArch`, the
/// `rsleigh::Sleigh<R>` (whose `lift_one` context state *is* the lift
/// engine's state), and a cached `SleighRegs` table.
///
/// Built once and reused across every function / rebuild iteration. The
/// calling convention is **not** stored — it is a per-call argument to the
/// lift methods, since it is a per-function property.
///
/// Not `Clone`: the owned `Sleigh` is not cheaply cloneable. Callers that
/// need a detached engine (e.g. the strider-py GIL-release path) rebuild a
/// fresh `Lifter` from a cloneable memory snapshot rather than cloning.
pub struct Lifter<R: rsleigh::MemReader> {
    arch: strider_target::SleighArch,
    /// The Sleigh context, owning the `MemReader`.  Borrowed `&mut` to
    /// build the CFG, then `&` to lift it; reused across rebuilds.
    sleigh: rsleigh::Sleigh<R>,
    /// Cached `SleighRegs` table from construction.  Used by the CallOther
    /// per-op-ABI dispatch in `FunctionLifter::handle_call_other` to
    /// resolve register names to `rsleigh::Vn`s without paying the
    /// per-call cost of `Sleigh::regs()` (an "expensive operation" per its
    /// docstring).
    sleigh_regs: rsleigh::SleighRegs,
    /// User-op name table snapshotted once at construction, indexed by
    /// `user_op_id`.  `FunctionLifter::handle_call_other` resolves a CallOther's
    /// name here rather than re-snapshotting the (fixed) table per instruction.
    user_op_names: Vec<String>,
}

impl<R: rsleigh::MemReader> Lifter<R> {
    /// Creates a `Lifter` for `arch` owning `sleigh`, caching its
    /// `SleighRegs` table once.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `Sleigh::regs()` fails.
    pub fn new(arch: strider_target::SleighArch, sleigh: rsleigh::Sleigh<R>) -> Result<Self> {
        let sleigh_regs = sleigh.regs()?;
        let user_op_names = sleigh.user_op_names().unwrap_or_default();
        Ok(Self {
            arch,
            sleigh,
            sleigh_regs,
            user_op_names,
        })
    }

    /// The user-op name table snapshotted at construction, indexed by
    /// `user_op_id`.  See [`Self::user_op_names`] field docs.
    #[must_use]
    pub fn user_op_names(&self) -> &[String] {
        &self.user_op_names
    }

    /// Read access to the owned Sleigh context (for dot rendering, and
    /// for cloning a fresh, throwaway Sleigh for the fingerprint-to-p-code
    /// audit-trail path — see `strider-py`'s `PyLifter::fingerprint_pcode`,
    /// which needs a Sleigh that starts with no inherited context-register
    /// state and whose mutations never reach this persistent instance;
    /// `Sleigh::lift_one` carries context-register state across calls,
    /// see the module doc).
    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        &self.sleigh
    }

    /// Returns the cached Sleigh register-name table.
    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        &self.sleigh_regs
    }

    /// Builds the CFG for the function at `entry` using the owned Sleigh.
    ///
    /// # Errors
    ///
    /// Propagates CFG build failures.
    /// `per_address_ccs` seeds the CFG builder with per-address CC overrides for
    /// call TARGETS; the builder reads their
    /// [`no_return`](strider_target::BuiltCallingConvention::no_return) flag to
    /// terminate a region at a no-return call.  Callers with no overrides pass
    /// an empty map.
    pub fn build_cfg(
        &mut self,
        entry: strider_cfg::MachineInsnAddr,
        cfg_opts: &strider_cfg::CfgOptions,
        per_address_ccs: &rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    ) -> Result<strider_cfg::Cfg> {
        // Reset the Sleigh's disassembly context before decoding this function.
        // `Sleigh::lift_one` carries context-register state across calls, so on a
        // reused `Lifter` a prior function's `globalset` (e.g. an ARM `bx`/`blx`
        // switching the Thumb `TMode`) can leak into this one and mis-decode it.
        // Each function is an independent entry point that must decode from the
        // processor-spec defaults, so clear committed context here; cheap and a
        // no-op for arches that never commit context.
        self.sleigh.reset_context()?;
        strider_cfg::Builder::for_arch(&self.arch, &mut self.sleigh, entry.addr, cfg_opts)
            .with_per_address_ccs(per_address_ccs.clone())
            .build()
    }

    /// Collects the set of all distinct varnodes referenced by any instruction
    /// across all regions of `cfg`.
    ///
    /// Ordering is owned by [`strider_ir::FunctionBuilder::new`], which
    /// sorts the tracked set deterministically (by
    /// `(space-shortcut, offset, size)`) so downstream `InitialVnId` numbering is
    /// stable across runs; the lifter only needs the unique used-vn set.
    pub(crate) fn find_all_unique_vns(&self, cfg: &strider_cfg::Cfg) -> Vec<rsleigh::Vn> {
        cfg.regions()
            .flat_map(|region| region.insns.iter())
            .flat_map(|wrapped| wrapped.insn.all_vns())
            .collect::<rustc_hash::FxHashSet<rsleigh::Vn>>()
            .into_iter()
            .collect()
    }

    /// Translates a pre-built control-flow graph into a [`LiftOutcome`]
    /// using the function-default calling convention `cc`.
    ///
    /// Equivalent to [`Self::build_ir_with`] with default
    /// [`LiftOptions`] (no per-address CC overrides).
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` when the CFG is malformed (missing
    /// region, unknown terminator), instruction translation fails (an
    /// unsupported opcode or varnode), or IR validation fails.
    pub fn build_ir(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
    ) -> Result<LiftOutcome> {
        self.build_ir_with(cfg, cc, &LiftOptions::default())
    }

    /// Translates a pre-built CFG into a [`LiftOutcome`] with the
    /// function-default `cc` and caller-supplied [`LiftOptions`].
    ///
    /// The tracked-varnode set is scanned fresh from `cfg` (via
    /// `find_all_unique_vns`); the deterministic ordering that gives
    /// stable `InitialVnId` numbering is applied by
    /// [`strider_ir::FunctionBuilder::new`].  Direct Calls whose target is in
    /// [`LiftOptions::per_address_ccs`] are built via
    /// [`strider_ir::FunctionBuilder::build_call`] with the override.
    ///
    /// # Errors
    ///
    /// Propagates errors from `FunctionLifter::new` (variable-table init),
    /// `FunctionBuilder::build_entry`, the per-region IR translation
    /// (value-producer failures, control-op routing, calling-convention
    /// plumbing), and final `FunctionBuilder::build`'s
    /// `strider_ir::validate::validate` pass.
    pub fn build_ir_with(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: strider_target::BuiltCallingConvention,
        opts: &LiftOptions,
    ) -> Result<LiftOutcome> {
        // The CFG is rebuilt from scratch each lift, so the tracked-varnode
        // set is always scanned fresh from it.  `FunctionLifter::new` adds the
        // stack vn to the tracked set (the lifter is the SSoT for that).
        let all_vns = self.find_all_unique_vns(cfg);
        // An empty override map behaves identically to "no overrides"
        // (the default is an empty map, so lookups are a plain `.get`).
        let mut driver = FunctionLifter::new(self, cc, cfg, all_vns, &opts.per_address_ccs)?;

        // Pruned-SSA phi placement (Cytron): dominators + dominance frontiers
        // over the CFG, then the iterated dominance frontier of each variable's
        // (exactly-collected) definition sites.  This is what stops the lifter
        // minting a value `Phi` for every varnode at every region (millions of
        // dead phis); a phi is placed only where it is actually needed.
        let dom = dominance::DomInfo::compute(cfg);
        let def_sites = driver.collect_def_sites();
        let placement = dom.iterated_frontier(&def_sites);

        // build_entry + one IR region per CFG region (each with only its placed
        // phis); returns the CFG-region → IR-region map the per-insn loop
        // resolves successors through (via the free `ir_region_of`).
        let region_map = driver.build_region_map(&placement)?;

        // Translate every region's instructions + non-trivial terminator into
        // IR — in dominator-tree pre-order so each region inherits its
        // reaching variable values from its (already-processed) immediate
        // dominator — then wire the fallthrough edges the per-insn loop didn't
        // reach (and Branch edges out of empty regions).
        driver.translate_regions(&region_map, &dom)?;
        driver.link_region_edges(&region_map)?;

        // Drain the indirect-branch anchors, then consume the builder
        // and emit the final outcome.
        let unresolved_branches = std::mem::take(&mut driver.unresolved_branches);
        let function = driver.builder.build()?;
        Ok(LiftOutcome {
            function,
            unresolved_branches,
        })
    }
}

/// Map from each CFG region to its freshly-allocated IR region, built
/// once per lift by [`FunctionLifter::build_region_map`].
pub(crate) type RegionMap = rustc_hash::FxHashMap<strider_cfg::RegionId, strider_ir::RegionId>;

/// Resolves a CFG region to its IR region via `region_map`, or returns a
/// typed "no such region" error.  Shared by the per-region translation
/// stages ([`FunctionLifter::translate_regions`] /
/// [`FunctionLifter::link_region_edges`]) and the control handlers
/// (`handle_cond_branch` / `handle_switch`) so the lookup + error message
/// live in one place.
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
    /// First stage of [`Lifter::build_ir_with`]: `build_entry`,
    /// allocate one IR region per CFG region, set the entry region, and
    /// return the CFG-region → IR-region [`RegionMap`].
    ///
    /// The map is keyed by the CFG `RegionId` so the per-instruction loop
    /// resolves a successor's IR region in O(1) without re-traversing the
    /// petgraph.  Every CFG region gets an IR region, so every key is
    /// present (no `Option` value).
    fn build_region_map(&mut self, placement: &pruned_ssa::PhiPlacement) -> Result<RegionMap> {
        self.builder.build_entry()?;
        let cfg = self.cfg;
        let mut region_map: RegionMap = RegionMap::default();
        for cfg_rid in cfg.region_ids() {
            // Only the Cytron IDF-placed variables get a phi at this region.
            let placed: Vec<strider_ir::node::InitialVnId> = placement
                .get(&cfg_rid)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            region_map.insert(cfg_rid, self.builder.create_region(&placed)?);
        }
        let entry_ir = *region_map
            .get(&cfg.entry())
            .ok_or_else(|| anyhow!("entry region {:?} missing from region_map", cfg.entry()))?;
        self.builder.set_entry_region(entry_ir)?;
        self.record_register_arg_carriers();
        Ok(region_map)
    }

    /// Record register-passed argument carriers on the function's arg table.
    ///
    /// Each arg-passing register's (largest-container) `InitialVar` output is
    /// the carrier for its positional index.  The container resolution is
    /// machine-register knowledge owned by the lifter (`container_of`), so this
    /// lives here rather than in `set_entry_region`: a narrow ABI arg alias
    /// (e.g. `edi`) routes through its tracked container (`rdi`), mirroring the
    /// CC ret-val / clobber projections.  We don't filter on use — an argument
    /// the function never reads is culled by DCE and dropped from the arg table
    /// by `Function::compact`, so patterns won't find it.
    fn record_register_arg_carriers(&mut self) {
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
    }

    /// Second stage of [`Lifter::build_ir_with`]: translate every
    /// region's instructions + (when present) its special terminator into
    /// IR, in CFG-region order.  The special terminator's p-code insn is
    /// skipped inside the per-insn loop and lifted via a dedicated handler
    /// with asm-fingerprint attribution to the region's last machine
    /// address.  `region_map` resolves a CFG region to its IR region (via
    /// the free [`ir_region_of`]).
    fn translate_regions(
        &mut self,
        region_map: &RegionMap,
        dom: &dominance::DomInfo,
    ) -> Result<()> {
        let cfg = self.cfg;
        // Dominator-tree pre-order: process each region only after its
        // immediate dominator, so the pruned-SSA current-value inheritance
        // (`inherit_variables`) reads the dominator's FINAL variable values.
        for &cfg_rid in dom.preorder() {
            let ir_region = ir_region_of(region_map, cfg_rid)?;
            // Seed this region's current-value map from its immediate dominator
            // (the entry region was seeded directly by `set_entry_region`
            // and has no idom).
            if let Some(idom_cfg) = dom.immediate_dominator(cfg_rid) {
                let idom_ir = ir_region_of(region_map, idom_cfg)?;
                self.builder.inherit_variables(ir_region, idom_ir);
            }
            self.builder.set_region(ir_region);
            let region = cfg
                .region_graph()
                .node_weight(cfg_rid)
                .ok_or_else(|| anyhow!("no region {cfg_rid:?} in cfg"))?;
            // Regions with non-trivial terminators have their terminator
            // p-code insn skipped inside the per-insn loop and lifted via
            // a dedicated handler post-loop:
            //   * `UnresolvedIndirectBranch` skips `BranchIndirect`,
            //     lifts via the placeholder path.
            //   * `Switch` skips `BranchIndirect`, lifts as an If-ladder.
            //   * `TailCall` skips `Branch`, lifts as
            //     `Call(IntConst(target)) + Return`.
            let special_terminator = SpecialTerm::from_terminator(&region.terminator);
            for wrapped_insn in &region.insns {
                if special_terminator
                    .as_ref()
                    .is_some_and(|s| s.skips_opcode(wrapped_insn.insn.opcode))
                {
                    continue;
                }
                self.process_insn(cfg_rid, &wrapped_insn.insn, wrapped_insn.addr, region_map)?;
            }
            // Asm-fingerprint context for the terminator handlers: every
            // node born inside one of these handlers is "caused by" the
            // region's terminator machine instruction.  Use the last
            // pcode insn's machine address as the contributor.  A region
            // with zero pcode insns is a synthetic tail-call stub (the
            // cfg builder's lowering of a CondBranch arm whose target
            // lies outside the function bound): the insn that proves its
            // `Call + Return` is the predecessor's conditional branch,
            // so fall back to the predecessors' trailing machine address
            // (`max` picks one deterministic contributor when several
            // branches share a deduped stub).  Without this fallback the
            // stub's nodes would carry no fingerprint and fail the
            // validator's always-on non-empty check.
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
            // A `NoReturn` region whose trailing insn is a DIRECT `Call` ends in
            // a no-return call whose return address left the function bound (the
            // cfg builder's `process_call`).  The per-insn loop already lifted
            // the `Call` (which keeps the region's control open), so sink that
            // control into an `Unreachable` terminator here.  A `CallOther`
            // NoReturn region self-terminates inside `handle_call_other`
            // (`terminate=true`), so gate on the direct-`Call` opcode to avoid
            // double-terminating it.
            let noreturn_direct_call =
                matches!(region.terminator, strider_cfg::RegionTerminator::NoReturn)
                    && region
                        .insns
                        .last()
                        .is_some_and(|w| w.insn.opcode == rsleigh::Opcode::Call);
            // Per-terminator funnel: same asm-fingerprint attribution
            // pattern as `process_insn` (see `with_lift_addr`).
            self.with_lift_addr(term_addr, |s| {
                match special_terminator {
                    Some(SpecialTerm::UnresolvedIndirect { target_vn, addr }) => {
                        s.handle_unresolved_indirect_branch(&target_vn, addr)?;
                    }
                    Some(SpecialTerm::Switch(target_vn, targets)) => {
                        s.handle_switch(cfg_rid, &target_vn, &targets, region_map)?;
                    }
                    Some(SpecialTerm::TailCall(target)) => {
                        s.handle_tail_call(target)?;
                    }
                    None => {
                        if noreturn_direct_call {
                            s.builder.build_unreachable()?;
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Third stage of [`Lifter::build_ir_with`]: wire the region
    /// successors that no per-terminator handler wired.  CFG edges are
    /// unweighted, so the gate is the *source region's terminator*: only
    /// `Unconditional` regions are wired here (their successor has no
    /// dedicated handler — `handle_branch` is a no-op).  `CondBranch`
    /// regions are wired by `handle_cond_branch` (`region_if` +
    /// `build_if`) and `Switch` regions by `handle_switch`'s If-ladder;
    /// re-linking either here would double-add a predecessor.
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

/// Per-region special-terminator marker the per-instruction loop uses
/// to skip the terminator p-code insn so the post-loop dispatch can
/// lift it via a dedicated handler.
enum SpecialTerm {
    /// IR-level indirect-branch resolver placeholder: emits an
    /// `IndirectBranch(target_value)` node (via
    /// `FunctionBuilder::build_indirect_branch`) and pushes the
    /// `(addr, target_value)` pair onto `unresolved_branches`.  The
    /// orchestrator's classifier later rewrites this in place to a
    /// `Call`/`Return` (link-register / tail-call shapes) or replaces
    /// the region terminator on CFG rebuild (jump-table shape).  Skip
    /// the trailing `BranchIndirect` p-code insn.
    UnresolvedIndirect {
        target_vn: rsleigh::Vn,
        addr: strider_cfg::PcodeInsnAddr,
    },
    /// Resolved jump table: lifts to an If-ladder dispatching `idx`
    /// against `targets`.  Skip the trailing `BranchIndirect`.
    Switch(rsleigh::Vn, Vec<u64>),
    /// Branch to an out-of-function target (`fn_max_size` bound
    /// exceeded, or sub-`start_addr` with
    /// `allow_code_before_start_addr=false`).  Lifts to
    /// `Call(IntConst(target)) + Return`.  Skip the trailing
    /// `Branch` / `BranchIndirect`.  The synthetic conditional-tail-call
    /// stub region (a CondBranch arm whose target is OOB) also carries
    /// this terminator; it has zero insns, so the per-insn loop has
    /// nothing to skip there.
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
            strider_cfg::RegionTerminator::Switch { target_vn, targets } => {
                Some(SpecialTerm::Switch(*target_vn, targets.clone()))
            }
            strider_cfg::RegionTerminator::TailCall { target } => {
                Some(SpecialTerm::TailCall(*target))
            }
            _ => None,
        }
    }

    /// Returns true when the per-region per-insn loop should skip
    /// `opcode` because the post-loop dispatcher will lift it via a
    /// dedicated handler.  `UnresolvedIndirect`/`Switch` skip
    /// `BranchIndirect`; `TailCall` skips `Branch` (the standard
    /// direct-tail-call case) AND `BranchIndirect` — when the
    /// orchestrator hints a `known_targets` resolution for an
    /// indirect-jump address whose target lies outside the function,
    /// the cfg builder treats the `jmp reg` as a tail call
    /// (`RegionTerminator::TailCall`).  The per-insn loop must NOT
    /// process the underlying `BranchIndirect` (which would emit an
    /// `IndirectBranch` node and terminate the region), or
    /// `handle_tail_call`'s `build_call` / `build_return` would crash
    /// on "attempted to insert into terminated region".  A `CondBranch`
    /// never lives in a TailCall region: a conditional jump with OOB
    /// successors keeps its `CondBranch` terminator, and the synthetic
    /// tail-call stub regions on its OOB arms carry no insns at all.
    ///
    /// Safe by region-closure invariant: `RegionBuilder::process_new_insn`
    /// finishes a region the moment ANY control-flow opcode (`Branch`,
    /// `CondBranch`, `Return`, `BranchIndirect`) is processed, so at
    /// most one such opcode appears in any region's insn list and it is
    /// always the trailing entry.  Widening this set is therefore
    /// mutually exclusive: the matched opcode is always the trailing
    /// terminator, never an inner pcode op.
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    /// AArch64 `ret` (which Sleigh lifts via `BranchIndirect` on the link
    /// register `x30`) routes through `handle_return`, NOT the
    /// `IndirectBranch` placeholder path: the cfg marks it
    /// `RegionTerminator::Return`, so the lift emits a CC `Return` and
    /// `unresolved_branches` stays empty.
    #[test]
    fn aarch64_bx_lr_lifts_to_cc_return_not_indirect() {
        use strider_ir::node::NodeKind;
        use strider_ir_test_utils::IrWalkerEx;

        let arch = strider_target::SleighArch::aarch64();
        // `probe_regs` consumes the arch, so build a second copy for the lift.
        let regs = strider_target::SleighArch::aarch64()
            .probe_regs()
            .expect("probe regs");
        let cc = strider_target::CallingConvention::aarch64_aapcs64()
            .build(&regs)
            .expect("build cc");
        // AArch64 `ret` = 0xD65F03C0, little-endian byte sequence.
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

    /// A reused `Lifter` must decode each function from a clean context.
    /// `Sleigh::lift_one` carries context-register state across calls: a Thumb
    /// `BLX <imm>` that switches to ARM `globalset`s the `TMode` for its target,
    /// and that commit persists.  Lifting a Thumb function A that ends in such a
    /// `BLX` and then a Thumb function B at the `BLX` target on the SAME lifter
    /// would, without a per-function reset, decode B as ARM — it mis-parses and
    /// walks off into unmapped memory.  `Lifter::build_cfg` resets the context
    /// before each function, so B decodes as Thumb identically to a fresh lift.
    #[test]
    fn reused_lifter_resets_thumb_context_between_functions() {
        use strider_ir::node::NodeKind;
        use strider_ir_test_utils::IrWalkerEx;

        let arch = strider_target::SleighArch::arm_thumb();
        let regs = strider_target::SleighArch::arm_thumb()
            .probe_regs()
            .expect("regs");
        let cc = strider_target::CallingConvention::arm_aapcs()
            .build(&regs)
            .expect("cc");
        // Buffer at 0x1000:
        //   0x1000: BLX 0x1010  (Thumb T2, switches to ARM at 0x1010) = 00 F0 03 E8
        //   0x1004: bx lr       (70 47) — ends function A
        //   0x1006: nop ×3      (00 bf) padding up to 0x1010
        //   0x1010: bx lr; nop; bx lr; nop — function B (a valid Thumb function)
        let code = vec![
            0x00, 0xf0, 0x03, 0xe8, // 0x1000 BLX 0x1010
            0x70, 0x47, // 0x1004 bx lr
            0x00, 0xbf, 0x00, 0xbf, 0x00, 0xbf, // 0x1006 nop padding
            0x70, 0x47, 0x00, 0xbf, 0x70, 0x47, 0x00, 0xbf, // 0x1010 function B
        ];
        let empty = rustc_hash::FxHashMap::default();
        let opts = strider_cfg::CfgOptions::default();
        let new_lifter = || {
            let reader = rsleigh::mem_readers::BufMemReader::new(code.clone(), 0x1000);
            let sleigh =
                rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
            super::Lifter::new(arch, sleigh).expect("lifter")
        };

        // Reused lifter: lift A (the BLX polluter) first, then B on the same engine.
        let mut lifter = new_lifter();
        let cfg_a = lifter
            .build_cfg(0x1000u64.into(), &opts, &empty)
            .expect("A cfg");
        lifter.build_ir(&cfg_a, cc.clone()).expect("A ir");
        let cfg_b = lifter.build_cfg(0x1010u64.into(), &opts, &empty).expect(
            "reused lifter must reset context so B's Thumb decode does not inherit A's ARM mode",
        );
        let reused_b = lifter.build_ir(&cfg_b, cc.clone()).expect("B ir");
        assert!(
            reused_b
                .function
                .has_kind(|k| matches!(k, NodeKind::Return)),
            "B (Thumb `bx lr`) lifted after A must decode as Thumb and emit a Return"
        );

        // Same B on a fresh lifter — the ground-truth decode to match against.
        let mut fresh = new_lifter();
        let cfg_fresh = fresh
            .build_cfg(0x1010u64.into(), &opts, &empty)
            .expect("fresh B cfg");
        let fresh_b = fresh.build_ir(&cfg_fresh, cc).expect("fresh B ir");
        assert_eq!(
            reused_b.function.count_kind(|_| true),
            fresh_b.function.count_kind(|_| true),
            "B lifted after A must have the same node count as a fresh Thumb lift"
        );
    }
}
