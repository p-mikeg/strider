use anyhow::{anyhow, Result};
use super::PerRegionDriver;

/// Per-region exit-state snapshot needed by the orchestrator's
/// indirect-branch placeholder lookup.  Captured during lift before
/// `FunctionBuilder::build()` consumes the builder's region map.  Used
/// by [`crate::orchestrator::RegionIndex`] to map a placeholder's
/// pre-edit ctrl input back to the region whose exit produced it (so
/// it can read the region's exit `vn_to_value` for in-place edit ABI
/// threading).
#[derive(Debug)]
pub(crate) struct RegionLiftHandles {
    /// Exit control output (consumed by the region's terminator).
    pub(crate) exit_control: strider_ir::node::NodeOutputId,
    /// Per-var exit-boundary value `NodeOutputId`s, keyed by `Vn`.
    ///
    /// Moved by value (not `Arc::clone`d) into the orchestrator's
    /// per-iteration [`crate::orchestrator::RegionIndex`] via
    /// `into_iter`; never mutated post-build.
    pub(crate) exit_vn_to_value:
        rustc_hash::FxHashMap<rsleigh::Vn, strider_ir::node::NodeOutputId>,
}

/// The full result of a strider lift, exposing the lifted IR plus the
/// placeholder-anchor side-table the indirect-branch resolver consumes
/// plus per-region IR-handle snapshots.
///
/// Returned by [`Strider::analyze_cfg`].  Callers that only need the
/// function can use `outcome.function` directly; indirect-branch-resolver-aware
/// callers read `unresolved_branches` and `region_handles`.
pub struct AnalyzeOutcome {
    /// The lifted IR ready for the optimiser pipeline.
    pub function: strider_ir::Function,
    /// One entry per region whose CFG terminator was
    /// [`strider_lift::cfg::RegionTerminator::UnresolvedIndirectBranch`] at lift
    /// time.  Each entry maps the offending `BranchIndirect`'s pcode
    /// address to the IR `NodeOutputId` that anchors its dispatch
    /// varnode (`target_vn`) in the placeholder Return.  Empty in
    /// the common case (no deferred branches).
    pub unresolved_branches: Vec<(strider_lift::cfg::PcodeInsnAddr, strider_ir::Value)>,
    /// Per-region IR-handle snapshots captured at lift time.  The
    /// orchestrator's per-iteration index uses these to map a
    /// placeholder's pre-edit ctrl input back to the region whose
    /// exit produced it (so it can read the region's exit
    /// `vn_to_value` for the in-place edit's ABI threading).
    pub(crate) region_handles: Vec<RegionLiftHandles>,
}

impl AnalyzeOutcome {
    /// Returns the number of per-region lift-handle snapshots
    /// captured at lift time.  Equivalent to the count of regions
    /// the orchestrator's indirect-branch resolver tracks.
    #[must_use]
    pub fn region_count(&self) -> usize {
        self.region_handles.len()
    }

    /// Iterates the per-region exit-control `NodeOutputId`s captured at
    /// lift time, in lift order.
    ///
    /// Each `NodeOutputId` identifies the control output a region's
    /// terminator consumed — sufficient to seed a backward walk that
    /// collects the region's node set (see
    /// [`crate::orchestrator::dump_per_region`] for the canonical use).
    pub fn region_exit_controls(&self) -> impl Iterator<Item = strider_ir::node::NodeOutputId> + '_ {
        self.region_handles.iter().map(|h| h.exit_control)
    }

}

impl std::fmt::Display for AnalyzeOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AnalyzeOutcome {{ unresolved_branches: {}, regions: {} }}",
            self.unresolved_branches.len(),
            self.region_handles.len(),
        )
    }
}

/// Per-call lift options for [`Strider::analyze_cfg_with`].  Empty
/// defaults match [`Strider::analyze_cfg`]'s convenience
/// behaviour: the orchestrator uses this with both fields set;
/// strider-py's custom-pipeline path uses it with `per_address_ccs` set.
#[derive(Default)]
pub struct AnalyzeOptions<'a> {
    /// Pre-computed varnode set.  When `None`, `Strider` calls
    /// `Strider::find_all_unique_vns` itself.  When `Some`, must be
    /// sorted by `strider_lift::pcode_lift::vn_sort_key` and must include every
    /// varnode any instruction in `cfg` references.  Under-tracking
    /// drops pcode reads; over-tracking is safe but allocates one
    /// extra `InitialVar` per superfluous vn.  The orchestrator passes
    /// `Some(cached_vns)` so it shares one vn table across rebuild
    /// iterations.
    ///
    pub all_vns: Option<Vec<rsleigh::Vn>>,

    /// Per-target-address CC override map.  Keys are direct-call
    /// target addresses; values are CCs already resolved against the
    /// same Sleigh register table the function-default CC was built
    /// against.  `None` by default — every direct `Call` uses the
    /// function-default CC.
    pub per_address_ccs:
        Option<&'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>>,
}

/// Architecture-level binary analyser that lifts a [`strider_lift::cfg::Cfg`] to an IR
/// function graph.
///
/// Holds the target architecture description and the resolved calling
/// convention.  Create one `Strider` per architecture/ABI combination and
/// reuse it to analyse multiple functions.
///
/// `Clone` is cheap: every field is itself `Clone`/`Copy`.  The strider-py
/// `run` path uses this to detach a `Strider` snapshot from a `PyRef` so
/// it can release the GIL across `strider::run` (otherwise Python threads
/// would be unable to make progress while a long lift / fixed-point loop
/// runs).
#[derive(Clone)]
pub struct Strider {
    pub(super) calling_convention: strider_target::BuiltCallingConvention,
    pub(crate) arch: strider_target::SleighArch,
    /// Cached `SleighRegs` table from Strider construction.  Used by the
    /// CallOther per-op-ABI dispatch in `PerRegionDriver::handle_call_other`
    /// to resolve `CallOtherAbi::implicit_reads`/`implicit_writes` register
    /// names to `rsleigh::Vn`s without paying the per-call cost of
    /// `Sleigh::regs()` (an "expensive operation" per its docstring).
    pub(super) sleigh_regs: rsleigh::SleighRegs,
    /// Alias-analysis precision propagated to every SP-aware pass the
    /// pipeline builders construct.  Default is
    /// [`crate::opt::AliasMode::Strict`].
    pub(super) alias_mode: crate::opt::AliasMode,
}

impl Strider {
    /// Creates a new `Strider` for `arch` with the given Sleigh register list
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
        let built_calling_convention = calling_convention.build(&sleigh_regs)?;
        Ok(Self {
            arch,
            calling_convention: built_calling_convention,
            sleigh_regs,
            alias_mode: crate::opt::AliasMode::default(),
        })
    }

    /// Constructs a `Strider` from an already-resolved
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
    pub fn new_with_built_cc(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::BuiltCallingConvention,
    ) -> Self {
        Self {
            arch,
            calling_convention,
            sleigh_regs,
            alias_mode: crate::opt::AliasMode::default(),
        }
    }

    /// Returns the resolved calling convention this Strider was built with.
    #[must_use]
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        &self.calling_convention
    }

    /// Overrides the [`crate::opt::AliasMode`] propagated to the
    /// SP-aware passes constructed by the pipeline builders.
    #[must_use]
    pub const fn with_alias_mode(mut self, mode: crate::opt::AliasMode) -> Self {
        self.alias_mode = mode;
        self
    }

    /// Builds an optimizer pipeline containing the default passes plus the
    /// convention-aware stack-argument passes:
    ///
    /// 1. All passes from [`crate::opt::default_pipeline`] (constant folding,
    ///    known-bits, flag-cmp canonicalisation, if-cond inversion,
    ///    redundant-phi, dead-branch).
    /// 2. [`crate::opt::StackOffsetDetect`] to stamp `Function::stack_offsets`
    ///    with each SP-relative Store / Load's concrete offset.
    /// 3. [`crate::opt::LoadForward`] inside the fixed-point loop, using
    ///    the convention's stack-pointer varnode.
    /// 4. [`crate::opt::CallStackArgCollect`] as a post-pass (runs once after
    ///    convergence), using the convention's positional stack-arg offsets.
    /// 5. [`crate::opt::FunctionArgDetect`] as a post-pass, registering
    ///    register- and stack-passed argument carriers in the side-table.
    #[must_use]
    pub fn build_optimizer_pipeline(&self) -> crate::opt::OptimizerPipeline {
        let mut p = crate::opt::default_pipeline();
        p.add(crate::opt::StackOffsetDetect::from_convention(
            &self.calling_convention,
        ));
        p.add(
            crate::opt::LoadForward::from_convention(&self.calling_convention, &self.arch)
                .alias_mode(self.alias_mode),
        );
        p.add_post_pass(
            crate::opt::CallStackArgCollect::from_convention(&self.calling_convention)
                .alias_mode(self.alias_mode),
        );
        p.add_post_pass(
            crate::opt::FunctionArgDetect::from_convention(&self.calling_convention)
                .alias_mode(self.alias_mode),
        );
        p
    }

    /// Builds the **stable** optimizer pipeline used by intermediate
    /// iterations of the indirect-branch fixed-point orchestrator.
    ///
    /// Composed of passes whose rewrites survive a later iteration that
    /// adds new phi inputs.  Inherits `ConstantFold`, `KnownBits`,
    /// `FlagCmpCanonicalize`, and `IfCondInversion` from
    /// `crate::opt::stable_default_pipeline()`, then adds
    /// `StackOffsetDetect`, `LoadForward`, and the
    /// `FunctionArgDetect` post-pass.  The destructive passes
    /// (`RedundantPhis` / `DeadBranchElimination`) are deferred to the
    /// final iteration because they remove nodes that the
    /// orchestrator's per-iteration index pins.
    #[must_use]
    pub fn build_stable_optimizer_pipeline(&self) -> crate::opt::OptimizerPipeline {
        let mut p = crate::opt::stable_default_pipeline();
        p.add(crate::opt::StackOffsetDetect::from_convention(
            &self.calling_convention,
        ));
        p.add(
            crate::opt::LoadForward::from_convention(&self.calling_convention, &self.arch)
                .alias_mode(self.alias_mode),
        );
        p.add_post_pass(
            crate::opt::FunctionArgDetect::from_convention(&self.calling_convention)
                .alias_mode(self.alias_mode),
        );
        p
    }

    /// Builds the **destructive** optimizer pipeline that the
    /// indirect-branch fixed-point orchestrator runs **once** at the
    /// fixed-point exit (or in the no-`BranchIndirect` fast path).
    ///
    /// Composed of node-removal passes safe to run only after the IR
    /// shape is final: `RedundantPhis`, `DeadBranchElimination`, plus
    /// the `CallStackArgCollect` post-pass.  CallOther no-op handling
    /// is now done at construction time in `strider_target::call_other_abi::classify`.
    #[must_use]
    pub fn build_destructive_optimizer_pipeline(&self) -> crate::opt::OptimizerPipeline {
        let mut p = crate::opt::destructive_default_pipeline();
        p.add_post_pass(
            crate::opt::CallStackArgCollect::from_convention(&self.calling_convention)
                .alias_mode(self.alias_mode),
        );
        p
    }

    /// Collects the set of all distinct varnodes referenced by any instruction
    /// across all regions of `cfg`, sorted in a deterministic order.
    ///
    /// Determinism (sort by `(space-shortcut, offset, size)`) is required
    /// so that downstream `VarId` numbering is stable across runs.
    pub(crate) fn find_all_unique_vns<R: rsleigh::MemReader>(
        &self,
        cfg: &strider_lift::cfg::Cfg<R>,
    ) -> Vec<rsleigh::Vn> {
        let mut all_vns: rustc_hash::FxHashSet<rsleigh::Vn> =
            rustc_hash::FxHashSet::default();
        for region in cfg.regions() {
            for wrapped in region.insns.iter() {
                for vn in wrapped.insn.all_vns() {
                    all_vns.insert(vn);
                }
            }
        }
        let mut vns: Vec<rsleigh::Vn> = all_vns.into_iter().collect();
        vns.sort_unstable_by_key(strider_lift::pcode_lift::vn_sort_key);
        vns
    }

    /// Translates a complete control-flow graph into an [`AnalyzeOutcome`].
    ///
    /// Equivalent to [`Self::analyze_cfg_with`] with default
    /// [`AnalyzeOptions`] — empty override map, scans `cfg` for varnodes.
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
        cfg: &strider_lift::cfg::Cfg<R>,
    ) -> Result<AnalyzeOutcome> {
        self.analyze_cfg_with(cfg, AnalyzeOptions::default())
    }

    /// Translates a complete CFG into an [`AnalyzeOutcome`] with
    /// caller-supplied [`AnalyzeOptions`].
    ///
    /// Equivalent to [`Self::analyze_cfg`] when given
    /// `AnalyzeOptions::default()`.  When [`AnalyzeOptions::all_vns`]
    /// is `Some`, the supplied `all_vns` must be sorted by
    /// `strider_lift::pcode_lift::vn_sort_key` (otherwise downstream `VarId`
    /// numbering loses determinism) and must include every varnode
    /// any instruction in `cfg` references — under-tracking would
    /// drop pcode reads.  Over-tracking is safe but allocates one
    /// extra `InitialVar` per superfluous vn.  Direct Calls whose
    /// target is in [`AnalyzeOptions::per_address_ccs`] are built via
    /// [`strider_ir::FunctionBuilder::build_call_with_cc`] with the override.
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
        cfg: &strider_lift::cfg::Cfg<R>,
        opts: AnalyzeOptions<'_>,
    ) -> Result<AnalyzeOutcome> {
        // Allocate one IR region per CFG region and wire the entry region.
        let all_vns = opts
            .all_vns
            .unwrap_or_else(|| self.find_all_unique_vns(cfg));
        let mut driver = PerRegionDriver::new(self, cfg, all_vns, opts.per_address_ccs)?;
        let (cfg_region_ids, region_map) = init_region_map(&mut driver, cfg)?;
        let ir_region_of = |region_id: strider_lift::cfg::RegionId| -> Result<strider_ir::RegionId> {
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

/// `init_region_map` — first stage of [`Strider::analyze_cfg_with`]:
/// build_entry, allocate one IR region per CFG region, set the
/// entry region.  Returns the CFG-region-id list (in iteration
/// order) and the `RegionId.index() -> Option<strider_ir::RegionId>`
/// map.
fn init_region_map<R: rsleigh::MemReader>(
    driver: &mut PerRegionDriver<'_, R>,
    cfg: &strider_lift::cfg::Cfg<R>,
) -> Result<(Vec<strider_lift::cfg::RegionId>, Vec<Option<strider_ir::RegionId>>)> {
    driver.builder.build_entry()?;

    // Map every CFG region id to its newly-allocated IR region id.
    // Indexed by `RegionId.index()` so the per-instruction loop can
    // resolve in O(1) without cloning the petgraph.
    let cfg_region_ids: Vec<strider_lift::cfg::RegionId> = cfg.region_ids().collect();
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
/// [`Strider::analyze_cfg_with`]: translate every region's
/// instructions + (when present) its special terminator into IR.
/// The special terminator's p-code insn is skipped inside the
/// per-insn loop and lifted via a dedicated handler with
/// asm-fingerprint attribution to the region's last machine address.
fn translate_regions<R, F>(
    driver: &mut PerRegionDriver<'_, R>,
    cfg: &strider_lift::cfg::Cfg<R>,
    cfg_region_ids: &[strider_lift::cfg::RegionId],
    ir_region_of: &F,
) -> Result<()>
where
    R: rsleigh::MemReader,
    F: Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId>,
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
            driver.process_insn(
                cfg_rid,
                &wrapped_insn.insn,
                wrapped_insn.addr,
                ir_region_of,
            )?;
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
                Some(SpecialTerm::PendingIndirect { target_vn, addr }) => {
                    driver.handle_unresolved_indirect_branch(&target_vn, addr)?;
                }
                Some(SpecialTerm::Switch(target_vn, targets, target_value)) => {
                    driver.handle_switch(
                        cfg_rid,
                        &target_vn,
                        &targets,
                        target_value,
                        ir_region_of,
                    )?;
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

/// `link_region_edges` — third stage of [`Strider::analyze_cfg_with`]:
/// wire region edges that the per-insn loop didn't.  Fallthrough
/// edges always need linking here; Branch edges out of empty regions
/// (produced by the bounded-lift CondBranch-OOB collapse) too, since
/// their absent pcode means no per-insn `handle_branch` call ran.
/// Non-empty Branch regions are already wired by the trailing
/// `Branch` p-code insn — re-linking would double-add the edge and
/// break graph-invariants predecessor counts.
fn link_region_edges<R, F>(
    driver: &mut PerRegionDriver<'_, R>,
    cfg: &strider_lift::cfg::Cfg<R>,
    ir_region_of: &F,
) -> Result<()>
where
    R: rsleigh::MemReader,
    F: Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId>,
{
    for edge_idx in cfg.region_graph().edge_indices() {
        let Some(weight) = cfg.region_graph().edge_weight(edge_idx) else {
            continue;
        };
        let Some((src, tgt)) = cfg.region_graph().edge_endpoints(edge_idx) else {
            continue;
        };
        match weight {
            strider_lift::cfg::RegionEdgeKind::Fallthrough => {
                driver
                    .builder
                    .link_regions(ir_region_of(src)?, ir_region_of(tgt)?)?;
            }
            strider_lift::cfg::RegionEdgeKind::Branch => {
                let src_region = cfg
                    .region_graph()
                    .node_weight(src)
                    .ok_or_else(|| anyhow!("no region {src:?} in cfg"))?;
                if src_region.insns.is_empty() {
                    driver
                        .builder
                        .link_regions(ir_region_of(src)?, ir_region_of(tgt)?)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// `finalise_outcome` — final stage of
/// [`Strider::analyze_cfg_with`]: capture per-region exit handles
/// before `build()` consumes the builder, then materialise the final
/// `AnalyzeOutcome` with the post-build generation snapshot.
fn finalise_outcome<R, F>(
    mut driver: PerRegionDriver<'_, R>,
    _cfg: &strider_lift::cfg::Cfg<R>,
    cfg_region_ids: &[strider_lift::cfg::RegionId],
    ir_region_of: &F,
) -> Result<AnalyzeOutcome>
where
    R: rsleigh::MemReader,
    F: Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId>,
{
    // Capture per-region IR handles BEFORE `build()` consumes the
    // builder.  `NodeId` / `NodeOutputId` are stable across the
    // build-time arena move, so the snapshots remain valid for the
    // returned `Graph`.
    let mut region_handles: Vec<RegionLiftHandles> = Vec::new();
    for &cfg_rid in cfg_region_ids {
        let ir_region_id = ir_region_of(cfg_rid)?;

        let mut exit_vn_to_value: rustc_hash::FxHashMap<
            rsleigh::Vn,
            strider_ir::node::NodeOutputId,
        > = rustc_hash::FxHashMap::default();
        for (var_id, val_out) in driver.builder.region_exit_variables(ir_region_id) {
            if let Some(vn) = driver.builder.vn_of_var(var_id) {
                exit_vn_to_value.insert(vn, val_out);
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
    Ok(AnalyzeOutcome {
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
    PendingIndirect {
        target_vn: rsleigh::Vn,
        addr: strider_lift::cfg::PcodeInsnAddr,
    },
    /// Resolved jump table: lifts to an If-ladder dispatching `idx`
    /// against `targets`.  Skip the trailing `BranchIndirect`.
    Switch(rsleigh::Vn, Vec<u64>, Option<strider_ir::Value>),
    /// Direct branch to an out-of-function target (`fn_max_size`
    /// bound exceeded, or sub-`start_addr` with
    /// `allow_code_before_start_addr=false`).  Lifts to
    /// `Call(IntConst(target)) + Return`.  Skip the trailing
    /// `Branch`.
    TailCall(u64),
}

impl SpecialTerm {
    fn from_terminator(t: &strider_lift::cfg::RegionTerminator) -> Option<Self> {
        match t {
            strider_lift::cfg::RegionTerminator::UnresolvedIndirectBranch { target_vn, addr } => {
                Some(SpecialTerm::PendingIndirect {
                    target_vn: *target_vn,
                    addr: *addr,
                })
            }
            strider_lift::cfg::RegionTerminator::Switch {
                target_vn,
                targets,
                target_value,
            } => Some(SpecialTerm::Switch(
                *target_vn,
                targets.clone(),
                *target_value,
            )),
            strider_lift::cfg::RegionTerminator::TailCall { target } => Some(SpecialTerm::TailCall(*target)),
            _ => None,
        }
    }

    /// Returns true when the per-region per-insn loop should skip
    /// `opcode` because the post-loop dispatcher will lift it via a
    /// dedicated handler.  `PendingIndirect`/`Switch` skip
    /// `BranchIndirect`; `TailCall` skips `Branch` (the standard
    /// direct-tail-call case), `CondBranch` (the
    /// `strider_lift::cfg::RegionBuilder` collapse path for a
    /// conditional jump whose successors all leave the function),
    /// AND `BranchIndirect` — when the orchestrator hints a
    /// `known_targets` resolution for an indirect-jump address whose
    /// target lies outside the function, the cfg builder treats the
    /// `jmp reg` as a tail call (`RegionTerminator::TailCall`).  The
    /// per-insn loop must NOT process the underlying `BranchIndirect`
    /// (which would emit an `IndirectBranch` node and terminate the
    /// region), or `handle_tail_call`'s `build_call_with_cc` /
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
            SpecialTerm::PendingIndirect { .. } | SpecialTerm::Switch(..) => {
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
        let strider = crate::Strider::new(arch, regs, cc).expect("strider");
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc3u8], 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
            .expect("sleigh");
        let cfg = strider_lift::cfg::Builder::for_arch(&arch, sleigh, 0x1000, strider_lift::cfg::OptionsBuilder::new().build())
            .build()
            .expect("cfg");
        let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
        let s = format!("{outcome}");
        assert!(s.contains("unresolved_branches: 0"));
        assert!(s.contains("regions: 1"));
    }
}
