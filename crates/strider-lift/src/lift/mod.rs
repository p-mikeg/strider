//! Binary CFG → IR lifting.  Owns the region-by-region translation of a
//! `strider_cfg::Cfg` into a `strider_ir::Function`, given a resolved set
//! of indirect-branch targets.  No optimization — that is the
//! orchestrator's concern.

use anyhow::{Result, anyhow};

mod insn;
pub mod pcode_util;
mod region_driver;
mod value;
mod vn_io;

pub(crate) use region_driver::PerRegionDriver;

/// Deterministic varnode sort key (`(space-shortcut, offset, size)`).
/// Re-exported so the orchestrator can pre-sort its cached vn table to
/// match the lifter's `VarId` numbering across rebuilds.
pub use pcode_util::vn_sort_key;

/// Per-region exit-state snapshot captured during lift before
/// `FunctionBuilder::build()` consumes the builder's region map.
/// Retained for diagnostics (e.g. `dump_per_region`) and future use.
#[derive(Debug)]
pub(crate) struct RegionLiftHandles {
    /// Exit control output (consumed by the region's terminator).
    pub(crate) exit_control: strider_ir::node::ValueId,
    /// Per-var exit-boundary value `ValueId`s, keyed by `Vn`.
    ///
    /// Captured at lift time; retained for future use.  The rebuild-driven
    /// orchestrator no longer consumes this for in-place editing, but it
    /// remains available for diagnostics and future extensions.
    #[allow(dead_code)]
    pub(crate) exit_vn_to_value: rustc_hash::FxHashMap<rsleigh::Vn, strider_ir::node::ValueId>,
}

/// The full result of a strider lift, exposing the lifted IR plus the
/// placeholder-anchor side-table the indirect-branch resolver consumes
/// plus per-region IR-handle snapshots.
///
/// Returned by [`Lifter::analyze_cfg`].  Callers that only need the
/// function can use `outcome.function` directly; indirect-branch-resolver-aware
/// callers read `unresolved_branches` and `region_handles`.
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
    /// Per-region IR-handle snapshots captured at lift time.  Used by
    /// `dump_per_region` (via `region_exit_controls`) and retained for
    /// diagnostics and future extensions.
    pub(crate) region_handles: Vec<RegionLiftHandles>,
}

impl LiftOutcome {
    /// Returns the number of per-region lift-handle snapshots
    /// captured at lift time.  Equivalent to the count of regions
    /// the orchestrator's indirect-branch resolver tracks.
    #[must_use]
    pub fn region_count(&self) -> usize {
        self.region_handles.len()
    }

    /// Iterates the per-region exit-control `ValueId`s captured at
    /// lift time, in lift order.
    ///
    /// Each `ValueId` identifies the control output a region's
    /// terminator consumed — sufficient to seed a backward walk that
    /// collects the region's node set (see the orchestrator's
    /// `dump_per_region` for the canonical use).
    pub fn region_exit_controls(&self) -> impl Iterator<Item = strider_ir::node::ValueId> + '_ {
        self.region_handles.iter().map(|h| h.exit_control)
    }
}

impl std::fmt::Display for LiftOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LiftOutcome {{ unresolved_branches: {}, regions: {} }}",
            self.unresolved_branches.len(),
            self.region_handles.len(),
        )
    }
}

/// The single options type for the whole binary → IR lift, re-exported
/// from the crate root.  The CFG builder reads its CFG-shaping knobs
/// (`fn_max_size`, `allow_code_before_start_addr`, `known_targets`); the
/// lifter reads its IR-lift knobs (`all_vns`, `per_address_ccs`).
pub use crate::lift_options::LiftOptions;

/// Architecture-level CFG→IR lifter: the target `SleighArch`, the
/// resolved calling convention, and a cached `SleighRegs` table.
///
/// `Lifter` translates a [`strider_cfg::Cfg`] into an IR function graph
/// region by region.  It carries no optimization concern — that lives in
/// the orchestrator's `LiftDriver`, which wraps a `Lifter` and forwards
/// the lift calls.
///
/// `Clone` copies the resolved calling convention and the cached
/// `SleighRegs` table — the latter a register-name lookup table that isn't
/// free to clone, but far cheaper than re-running the "expensive"
/// `Sleigh::regs()` to rebuild it.  The strider-py `run` path uses this to
/// detach a snapshot from a `PyRef` so it can release the GIL across the
/// lift / fixed-point loop (otherwise Python threads would be unable to
/// make progress while a long lift runs).
#[derive(Clone)]
pub struct Lifter {
    pub(crate) calling_convention: strider_target::BuiltCallingConvention,
    pub(crate) arch: strider_target::SleighArch,
    /// Cached `SleighRegs` table from construction.  Used by the
    /// CallOther per-op-ABI dispatch in `PerRegionDriver::handle_call_other`
    /// to resolve `CallOtherAbi::implicit_reads`/`implicit_writes` register
    /// names to `rsleigh::Vn`s without paying the per-call cost of
    /// `Sleigh::regs()` (an "expensive operation" per its docstring).
    pub(crate) sleigh_regs: rsleigh::SleighRegs,
}

impl Lifter {
    /// Creates a new `Lifter` for `arch` with the given Sleigh register list
    /// and calling convention.
    ///
    /// Resolves all register names in `calling_convention` against
    /// `sleigh_regs`.
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` if any register name in
    /// `calling_convention` (including the stack pointer) does not resolve
    /// against `sleigh_regs`.
    pub fn new(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::CallingConvention,
    ) -> Result<Self> {
        let calling_convention = calling_convention.build(&sleigh_regs)?;
        Ok(Self {
            arch,
            calling_convention,
            sleigh_regs,
        })
    }

    /// Constructs a `Lifter` from an already-resolved
    /// `BuiltCallingConvention`.  Use this when the CC was built
    /// outside the standard preset path (e.g. a custom CC constructed
    /// from runtime register-name lists at the Python boundary).
    ///
    /// Unlike [`Self::new`], no name resolution runs — the caller is
    /// responsible for ensuring `calling_convention`'s varnodes resolve
    /// against `sleigh_regs`.  ABI invariants are pinned at
    /// [`strider_target::BuiltCallingConvention::try_new`] construction
    /// time; this constructor trusts that contract.
    #[must_use]
    pub fn from_built_cc(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::BuiltCallingConvention,
    ) -> Self {
        Self {
            arch,
            calling_convention,
            sleigh_regs,
        }
    }

    /// Returns the resolved calling convention this `Lifter` was built with.
    #[must_use]
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        &self.calling_convention
    }

    /// Returns the target architecture description this `Lifter` was built with.
    #[must_use]
    pub fn arch(&self) -> &strider_target::SleighArch {
        &self.arch
    }

    /// Returns the cached Sleigh register-name table.
    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        &self.sleigh_regs
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

    /// Translates a complete control-flow graph into a [`LiftOutcome`].
    ///
    /// Equivalent to [`Self::analyze_cfg_with`] with default
    /// [`LiftOptions`] — empty override map, scans `cfg` for varnodes.
    /// Callers that need either knob (the orchestrator's cached vn table,
    /// or strider-py's per-address CC override map) use `analyze_cfg_with`.
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` when the CFG is malformed (missing
    /// region, unknown terminator), instruction translation fails (an
    /// unsupported opcode or varnode), or IR validation fails.
    pub fn analyze_cfg<R: rsleigh::MemReader>(
        &self,
        cfg: &strider_cfg::Cfg,
        sleigh: &rsleigh::Sleigh<R>,
    ) -> Result<LiftOutcome> {
        self.analyze_cfg_with(cfg, sleigh, &LiftOptions::default())
    }

    /// Translates a complete CFG into a [`LiftOutcome`] with
    /// caller-supplied [`LiftOptions`].
    ///
    /// Equivalent to [`Self::analyze_cfg`] when given
    /// `LiftOptions::default()`.  When [`LiftOptions::all_vns`]
    /// is `Some`, the supplied `all_vns` must be sorted by
    /// `crate::lift::pcode_util::vn_sort_key` (otherwise downstream `VarId`
    /// numbering loses determinism) and must include every varnode
    /// any instruction in `cfg` references — under-tracking would
    /// drop pcode reads.  Over-tracking is safe but allocates one
    /// extra `InitialVar` per superfluous vn.  Direct Calls whose
    /// target is in [`LiftOptions::per_address_ccs`] are built via
    /// [`strider_ir::FunctionBuilder::build_call`] with the override.
    ///
    /// # Errors
    ///
    /// Propagates errors from `PerRegionDriver::new` (variable-table init,
    /// CC build), `FunctionBuilder::build_entry`, the per-region IR
    /// translation (`pcode-lift` value-producer failures, control-op
    /// routing, calling-convention plumbing), and final
    /// `FunctionBuilder::build`'s `strider_ir::validate::validate` pass.
    pub fn analyze_cfg_with<R: rsleigh::MemReader>(
        &self,
        cfg: &strider_cfg::Cfg,
        sleigh: &rsleigh::Sleigh<R>,
        opts: &LiftOptions,
    ) -> Result<LiftOutcome> {
        // Allocate one IR region per CFG region and wire the entry region.
        // The CFG is rebuilt from scratch each lift, so the tracked-varnode
        // set is always scanned fresh from it.
        let all_vns = self.find_all_unique_vns(cfg);
        // An empty override map behaves identically to "no overrides"
        // (lookups are `and_then(|m| m.get(addr))`), so always pass the
        // borrow.
        let mut driver =
            PerRegionDriver::new(self, cfg, sleigh, all_vns, Some(&opts.per_address_ccs))?;
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

        // Capture per-region exit handles, then consume the builder
        // and emit the final outcome.
        finalise_outcome(driver, cfg, &cfg_region_ids, &ir_region_of)
    }
}

/// Builds the CFG for the function at `entry` and lifts it to IR in one
/// call — the convenience seam over [`strider_cfg::Builder`] +
/// [`Lifter`].
///
/// All CFG-shaping and IR-lift knobs come from the single
/// [`crate::LiftOptions`]: the CFG builder reads `fn_max_size` /
/// `allow_code_before_start_addr` / `known_targets`; the lifter reads
/// `all_vns` / `per_address_ccs`.
///
/// This convenience re-derives the `SleighRegs` table via
/// [`rsleigh::Sleigh::regs`] (an expensive call per its docstring) to
/// construct the `Lifter`.  Callers that lift many functions with one
/// shared register table (the orchestrator) should cache a `Lifter` and
/// drive [`strider_cfg::Builder`] + [`Lifter::analyze_cfg_with`] directly
/// rather than paying that cost per call.
///
/// # Errors
///
/// Propagates CFG build failures, `Sleigh::regs()` failures, and every
/// error [`Lifter::analyze_cfg_with`] surfaces.
pub fn lift_function<R: rsleigh::MemReader>(
    sleigh: &mut rsleigh::Sleigh<R>,
    arch: strider_target::SleighArch,
    entry: strider_cfg::MachineInsnAddr,
    cc: &strider_target::BuiltCallingConvention,
    options: &LiftOptions,
) -> Result<LiftOutcome> {
    let cfg = strider_cfg::Builder::for_arch(&arch, sleigh, entry.addr, &options.cfg).build()?;
    // `regs()` borrows `&sleigh`; the CFG builder's `&mut sleigh` borrow
    // ended when `build()` returned, so this is free to call here.
    let sleigh_regs = sleigh.regs()?;
    let lifter = Lifter::from_built_cc(arch, sleigh_regs, cc.clone());
    lifter.analyze_cfg_with(&cfg, sleigh, options)
}

/// `init_region_map` — first stage of [`Lifter::analyze_cfg_with`]:
/// build_entry, allocate one IR region per CFG region, set the
/// entry region.  Returns the CFG-region-id list (in iteration
/// order) and the `RegionId.index() -> Option<strider_ir::RegionId>`
/// map.
fn init_region_map<R: rsleigh::MemReader>(
    driver: &mut PerRegionDriver<'_, R>,
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
    driver: &mut PerRegionDriver<'_, R>,
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
    driver: &mut PerRegionDriver<'_, R>,
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

/// `finalise_outcome` — final stage of
/// [`Lifter::analyze_cfg_with`]: capture per-region exit handles
/// before `build()` consumes the builder, then materialise the final
/// `LiftOutcome` with the post-build generation snapshot.
fn finalise_outcome<R, F>(
    mut driver: PerRegionDriver<'_, R>,
    _cfg: &strider_cfg::Cfg,
    cfg_region_ids: &[strider_cfg::RegionId],
    ir_region_of: &F,
) -> Result<LiftOutcome>
where
    R: rsleigh::MemReader,
    F: Fn(strider_cfg::RegionId) -> Result<strider_ir::RegionId>,
{
    // Capture per-region IR handles BEFORE `build()` consumes the
    // builder.  `NodeId` / `ValueId` are stable across the
    // build-time arena move, so the snapshots remain valid for the
    // returned `Graph`.
    let mut region_handles: Vec<RegionLiftHandles> = Vec::new();
    for &cfg_rid in cfg_region_ids {
        let ir_region_id = ir_region_of(cfg_rid)?;

        let mut exit_vn_to_value: rustc_hash::FxHashMap<rsleigh::Vn, strider_ir::node::ValueId> =
            rustc_hash::FxHashMap::default();
        for (var_id, exit_value) in driver.builder.region_exit_variables(ir_region_id) {
            if let Some(vn) = driver.builder.vn_of_var(var_id) {
                exit_vn_to_value.insert(vn, exit_value);
            }
        }

        let exit_control = driver.builder.region_cur_ctrl(ir_region_id);

        region_handles.push(RegionLiftHandles {
            exit_control,
            exit_vn_to_value,
        });
    }

    let unresolved_branches = std::mem::take(&mut driver.unresolved_branches);
    let function = driver.builder.build()?;
    Ok(LiftOutcome {
        function,
        unresolved_branches,
        region_handles,
    })
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
    fn display_summarises_unresolved_branches_and_region_count() {
        // Standard x86_64 `ret` byte sequence.  No `BranchIndirect`, so
        // `unresolved_branches.len() == 0`.
        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs().expect("probe regs");
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .expect("x86_64_systemv preset must be registered");
        let lifter = super::Lifter::new(arch, regs, cc).expect("lifter");
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
        let outcome = lifter.analyze_cfg(&cfg, &sleigh).expect("analyze_cfg");
        let s = format!("{outcome}");
        assert!(s.contains("unresolved_branches: 0"));
        assert!(s.contains("regions: 1"));
    }
}
