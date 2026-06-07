//! Binary CFG → IR lifting.  Owns the region-by-region translation of a
//! `strider_cfg::Cfg` into a `strider_ir::Function`, given a resolved set
//! of indirect-branch targets.  No optimization — that is the
//! orchestrator's concern.

use anyhow::{Result, anyhow};

mod arithmetic;
mod boolean;
mod call;
mod cast;
mod control;
mod dispatch;
mod float;
mod integer;
mod memory;
mod misc;
pub mod pcode_util;
mod function_lifter;
mod vn_io;

#[cfg(test)]
mod handler_tests;

pub(crate) use function_lifter::FunctionLifter;

/// Deterministic varnode sort key (`(space-shortcut, offset, size)`).
/// Re-exported so the orchestrator can pre-sort its cached vn table to
/// match the lifter's `VarId` numbering across rebuilds.
pub use pcode_util::vn_sort_key;

/// The full result of a strider lift, exposing the lifted IR plus the
/// placeholder-anchor side-table the indirect-branch resolver consumes.
///
/// Returned by [`Lifter::analyze_cfg`].  Callers that only need the
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

impl std::fmt::Display for LiftOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LiftOutcome {{ unresolved_branches: {} }}",
            self.unresolved_branches.len(),
        )
    }
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
    pub(crate) arch: strider_target::SleighArch,
    /// The Sleigh context, owning the `MemReader`.  Borrowed `&mut` to
    /// build the CFG, then `&` to lift it; reused across rebuilds.
    pub(crate) sleigh: rsleigh::Sleigh<R>,
    /// Cached `SleighRegs` table from construction.  Used by the CallOther
    /// per-op-ABI dispatch in `FunctionLifter::handle_call_other` to
    /// resolve register names to `rsleigh::Vn`s without paying the
    /// per-call cost of `Sleigh::regs()` (an "expensive operation" per its
    /// docstring).
    pub(crate) sleigh_regs: rsleigh::SleighRegs,
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
        Ok(Self {
            arch,
            sleigh,
            sleigh_regs,
        })
    }

    /// Returns the target architecture description this `Lifter` owns.
    #[must_use]
    pub fn arch(&self) -> &strider_target::SleighArch {
        &self.arch
    }

    /// Read access to the owned Sleigh context (for dot rendering /
    /// fingerprint p-code resolution).
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
    pub fn build_cfg(
        &mut self,
        entry: strider_cfg::MachineInsnAddr,
        cfg_opts: &strider_cfg::CfgOptions,
    ) -> Result<strider_cfg::Cfg> {
        strider_cfg::Builder::for_arch(&self.arch, &mut self.sleigh, entry.addr, cfg_opts).build()
    }

    /// Builds the CFG for `entry` and lifts it to IR in one call.
    ///
    /// `cc` is the function-default calling convention; `opts` supplies the
    /// CFG-shaping knobs (`opts.cfg`) and the IR-lift knob
    /// (`per_address_ccs`).
    ///
    /// # Errors
    ///
    /// Propagates CFG build failures and every error
    /// [`Self::analyze_cfg_with`] surfaces.
    pub fn lift(
        &mut self,
        entry: strider_cfg::MachineInsnAddr,
        cc: &strider_target::BuiltCallingConvention,
        opts: &LiftOptions,
    ) -> Result<LiftOutcome> {
        let cfg = self.build_cfg(entry, &opts.cfg)?;
        self.analyze_cfg_with(&cfg, cc, opts)
    }

    /// Collects the set of all distinct varnodes referenced by any instruction
    /// across all regions of `cfg`, sorted in a deterministic order.
    ///
    /// Determinism (sort by `(space-shortcut, offset, size)`) is required
    /// so that downstream `VarId` numbering is stable across runs.
    pub(crate) fn find_all_unique_vns(&self, cfg: &strider_cfg::Cfg) -> Vec<rsleigh::Vn> {
        let mut all_vns: rustc_hash::FxHashSet<rsleigh::Vn> = rustc_hash::FxHashSet::default();
        for region in cfg.regions() {
            for wrapped in region.insns.iter() {
                for vn in wrapped.insn.all_vns() {
                    all_vns.insert(vn);
                }
            }
        }
        let mut vns: Vec<rsleigh::Vn> = all_vns.into_iter().collect();
        vns.sort_unstable_by_key(crate::lift::pcode_util::vn_sort_key);
        vns
    }

    /// Translates a pre-built control-flow graph into a [`LiftOutcome`]
    /// using the function-default calling convention `cc`.
    ///
    /// Equivalent to [`Self::analyze_cfg_with`] with default
    /// [`LiftOptions`] (no per-address CC overrides).
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` when the CFG is malformed (missing
    /// region, unknown terminator), instruction translation fails (an
    /// unsupported opcode or varnode), or IR validation fails.
    pub fn analyze_cfg(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Result<LiftOutcome> {
        self.analyze_cfg_with(cfg, cc, &LiftOptions::default())
    }

    /// Translates a pre-built CFG into a [`LiftOutcome`] with the
    /// function-default `cc` and caller-supplied [`LiftOptions`].
    ///
    /// The tracked-varnode set is scanned fresh from `cfg` (via
    /// [`Self::find_all_unique_vns`], sorted by
    /// `crate::lift::pcode_util::vn_sort_key` for deterministic `VarId`
    /// numbering).  Direct Calls whose target is in
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
    pub fn analyze_cfg_with(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: &strider_target::BuiltCallingConvention,
        opts: &LiftOptions,
    ) -> Result<LiftOutcome> {
        // The CFG is rebuilt from scratch each lift, so the tracked-varnode
        // set is always scanned fresh from it.
        let all_vns = self.find_all_unique_vns(cfg);
        // An empty override map behaves identically to "no overrides"
        // (lookups are `and_then(|m| m.get(addr))`), so always pass the
        // borrow.
        let mut driver =
            FunctionLifter::new(self, cc, cfg, all_vns, Some(&opts.per_address_ccs))?;
        let (cfg_region_ids, region_map) = init_region_map(&mut driver, cfg)?;
        let ir_region_of = |region_id: strider_cfg::RegionId| -> Result<strider_ir::RegionId> {
            region_map
                .get(region_id.index())
                .copied()
                .flatten()
                .ok_or_else(|| anyhow!("no region {region_id:?} in cfg"))
        };

        // Translate every region's instructions + non-trivial
        // terminator into IR.
        translate_regions(&mut driver, cfg, &cfg_region_ids, &ir_region_of)?;

        // Link region edges the per-insn loop didn't reach
        // (fallthrough edges, and Branch edges out of empty regions).
        link_region_edges(&mut driver, cfg, &ir_region_of)?;

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

/// `init_region_map` — first stage of [`Lifter::analyze_cfg_with`]:
/// build_entry, allocate one IR region per CFG region, set the
/// entry region.  Returns the CFG-region-id list (in iteration
/// order) and the `RegionId.index() -> Option<strider_ir::RegionId>`
/// map.
fn init_region_map<R: rsleigh::MemReader>(
    driver: &mut FunctionLifter<'_, R>,
    cfg: &strider_cfg::Cfg,
) -> Result<(Vec<strider_cfg::RegionId>, Vec<Option<strider_ir::RegionId>>)> {
    driver.builder.build_entry()?;

    // Map every CFG region id to its newly-allocated IR region id.
    // Indexed by `RegionId.index()` so the per-instruction loop can
    // resolve in O(1) without cloning the petgraph.
    let cfg_region_ids: Vec<strider_cfg::RegionId> = cfg.region_ids().collect();
    let max_index = cfg_region_ids.iter().map(|r| r.index()).max().unwrap_or(0);
    let mut region_map: Vec<Option<strider_ir::RegionId>> = vec![None; max_index + 1];
    for cfg_rid in &cfg_region_ids {
        region_map[cfg_rid.index()] = Some(driver.builder.create_region()?);
    }

    let entry_ir = region_map
        .get(cfg.entry().index())
        .copied()
        .flatten()
        .ok_or_else(|| anyhow!("entry region {:?} missing from region_map", cfg.entry()))?;
    driver.builder.set_entry_region(entry_ir)?;
    Ok((cfg_region_ids, region_map))
}

/// `translate_regions` — second stage of
/// [`Lifter::analyze_cfg_with`]: translate every region's
/// instructions + (when present) its special terminator into IR.
/// The special terminator's p-code insn is skipped inside the
/// per-insn loop and lifted via a dedicated handler with
/// asm-fingerprint attribution to the region's last machine address.
fn translate_regions<R, F>(
    driver: &mut FunctionLifter<'_, R>,
    cfg: &strider_cfg::Cfg,
    cfg_region_ids: &[strider_cfg::RegionId],
    ir_region_of: &F,
) -> Result<()>
where
    R: rsleigh::MemReader,
    F: Fn(strider_cfg::RegionId) -> Result<strider_ir::RegionId>,
{
    for &cfg_rid in cfg_region_ids {
        let ir_region = ir_region_of(cfg_rid)?;
        driver.builder.set_region(ir_region);
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
            driver.process_insn(cfg_rid, &wrapped_insn.insn, wrapped_insn.addr, ir_region_of)?;
        }
        // Asm-fingerprint context for the terminator handlers: every
        // node born inside one of these handlers is "caused by" the
        // region's terminator machine instruction.  Use the last
        // pcode insn's machine address as the contributor; when the
        // region is empty the field stays None.
        let term_addr = region
            .insns
            .last()
            .map(|wrapped| wrapped.addr.machine_addr.addr);
        // Per-terminator funnel: same asm-fingerprint attribution
        // pattern as `process_insn`.  `term_addr` may be `None` when
        // the region has zero pcode insns (e.g. empty Branch regions
        // produced by the bounded-lift CondBranch-OOB collapse);
        // `set_lift_addr` accepts `Option<u64>`.
        driver.builder.set_lift_addr(term_addr);
        let term_res = (|| -> Result<()> {
            match special_terminator {
                Some(SpecialTerm::UnresolvedIndirect { target_vn, addr }) => {
                    driver.handle_unresolved_indirect_branch(&target_vn, addr)?;
                }
                Some(SpecialTerm::Switch(target_vn, targets)) => {
                    driver.handle_switch(cfg_rid, &target_vn, &targets, ir_region_of)?;
                }
                Some(SpecialTerm::TailCall(target)) => {
                    driver.handle_tail_call(target)?;
                }
                None => {}
            }
            Ok(())
        })();
        driver.builder.set_lift_addr(None);
        term_res?;
    }
    Ok(())
}

/// `link_region_edges` — third stage of [`Lifter::analyze_cfg_with`]:
/// wire the region successors that no per-terminator handler wired.
/// CFG edges are unweighted, so the gate is the *source region's
/// terminator*: only `Unconditional` regions are wired here
/// (their successor has no dedicated handler — `handle_branch` is a
/// no-op).  `CondBranch` regions are wired by `handle_cond_branch`
/// (`region_if` + `build_if`) and `Switch` regions by `handle_switch`'s
/// If-ladder; re-linking either here would double-add a predecessor.
fn link_region_edges<R, F>(
    driver: &mut FunctionLifter<'_, R>,
    cfg: &strider_cfg::Cfg,
    ir_region_of: &F,
) -> Result<()>
where
    R: rsleigh::MemReader,
    F: Fn(strider_cfg::RegionId) -> Result<strider_ir::RegionId>,
{
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
            driver
                .builder
                .link_regions(ir_region_of(src)?, ir_region_of(tgt)?)?;
        }
    }
    Ok(())
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
    /// Direct branch to an out-of-function target (`fn_max_size`
    /// bound exceeded, or sub-`start_addr` with
    /// `allow_code_before_start_addr=false`).  Lifts to
    /// `Call(IntConst(target)) + Return`.  Skip the trailing
    /// `Branch`.
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
    /// direct-tail-call case), `CondBranch` (the
    /// `strider_cfg::RegionBuilder` collapse path for a
    /// conditional jump whose successors all leave the function),
    /// AND `BranchIndirect` — when the orchestrator hints a
    /// `known_targets` resolution for an indirect-jump address whose
    /// target lies outside the function, the cfg builder treats the
    /// `jmp reg` as a tail call (`RegionTerminator::TailCall`).  The
    /// per-insn loop must NOT process the underlying `BranchIndirect`
    /// (which would emit an `IndirectBranch` node and terminate the
    /// region), or `handle_tail_call`'s `build_call` /
    /// `build_return` would crash on "attempted to insert into
    /// terminated region".
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
                rsleigh::Opcode::Branch
                    | rsleigh::Opcode::CondBranch
                    | rsleigh::Opcode::BranchIndirect
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    #[test]
    fn display_summarises_unresolved_branches() {
        // Standard x86_64 `ret` byte sequence.  No `BranchIndirect`, so
        // `unresolved_branches.len() == 0`.
        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs().expect("probe regs");
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .expect("x86_64_systemv preset must be registered")
            .build(&regs)
            .expect("build cc");
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc3u8], 0x1000);
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
        // The Lifter owns the Sleigh; CC is a per-call argument.
        let lifter = super::Lifter::new(arch, sleigh).expect("lifter");
        let outcome = lifter.analyze_cfg(&cfg, &cc).expect("analyze_cfg");
        let s = format!("{outcome}");
        assert!(s.contains("unresolved_branches: 0"));
    }
}
