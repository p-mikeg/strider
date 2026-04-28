use crate::error::{ErrorKind, Result};

use super::IrStrider;

/// The full result of a strider lift, exposing both the lifted IR and
/// the placeholder-anchor side-table the strider-level fixed-point
/// loop's tier-2 resolver consumes.
///
/// Returned by [`Strider::analyze_cfg_with_unresolved`].  Callers that
/// don't need the deferred-branch table can use the simpler
/// [`Strider::analyze_cfg`] entry point.
pub struct AnalyzeOutcome {
    /// The lifted IR ready for the optimiser pipeline.
    pub graph: ir::BuiltFunctionGraph,
    /// One entry per region whose CFG terminator was
    /// [`cfg::RegionTerminator::UnresolvedIndirectBranch`] at lift
    /// time.  Each entry maps the offending `BranchIndirect`'s pcode
    /// address to the IR `NodeOutputId` that anchors its dispatch
    /// varnode (`target_vn`) in the placeholder Return.  Empty in
    /// the common case (no deferred branches).
    pub unresolved_branches: Vec<(cfg::PcodeInsnAddr, ir::Value)>,
}

/// Architecture-level binary analyser that lifts a [`cfg::Cfg`] to an IR
/// function graph.
///
/// Holds the target architecture description and the resolved calling
/// convention.  Create one `Strider` per architecture/ABI combination and
/// reuse it to analyse multiple functions.
pub struct Strider {
    pub(super) calling_convention: crate::BuiltCallingConvention,
    pub(super) arch: crate::SleighArch,
}

impl Strider {
    /// Creates a new `Strider` for `arch` with the given Sleigh register list
    /// and calling convention.
    ///
    /// Resolves all register names in `calling_convention` against
    /// `sleigh_regs`.  Returns an error if any name is unknown.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::TargetError`] (wrapping
    /// [`target::ErrorKind::UnknownRegName`]) if any register name in
    /// `calling_convention` (including the stack pointer) does not resolve
    /// against `sleigh_regs`.
    pub fn new(
        arch: crate::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: crate::CallingConvention,
    ) -> Result<Self> {
        let built_calling_convention = calling_convention.build(&sleigh_regs)?;
        Ok(Self {
            arch,
            calling_convention: built_calling_convention,
        })
    }

    /// Returns the resolved calling convention this Strider was built with.
    #[must_use]
    pub fn calling_convention(&self) -> &crate::BuiltCallingConvention {
        &self.calling_convention
    }

    /// Returns the [`crate::SleighArch`] this Strider was built with.
    /// Used by the strider-level fixed-point orchestrator to thread
    /// the arch's endianness through per-iteration CFG rebuilds.
    #[must_use]
    pub fn arch(&self) -> &crate::SleighArch {
        &self.arch
    }

    /// Builds an optimizer pipeline containing the default passes plus the
    /// convention-aware stack-argument passes:
    ///
    /// 1. All passes from [`opt::default_pipeline`] (constant folding,
    ///    known-bits, redundant-phi, dead-branch).
    /// 2. [`opt::StackStoreDetect`] inside the fixed-point loop, using the
    ///    convention's stack-pointer varnode.
    /// 3. [`opt::CallStackArgCollect`] as a post-pass (runs once after
    ///    convergence), using the convention's positional stack-arg offsets.
    /// 4. [`opt::FunctionArgDetect`] as a post-pass, canonicalising
    ///    register- and stack-passed argument reads into `FunctionArg` nodes.
    #[must_use]
    pub fn build_optimizer_pipeline(&self) -> opt::OptimizerPipeline {
        let mut p = opt::default_pipeline();
        p.add(opt::StackStoreDetect::from_convention(
            &self.calling_convention,
        ));
        p.add(opt::StackLoadForward::from_convention(
            &self.calling_convention,
            &self.arch,
        ));
        p.add_post_pass(opt::CallStackArgCollect::from_convention(
            &self.calling_convention,
        ));
        p.add_post_pass(opt::FunctionArgDetect::from_convention(
            &self.calling_convention,
        ));
        p
    }

    /// Builds the **stable** optimizer pipeline used by intermediate
    /// iterations of the indirect-branch fixed-point orchestrator.
    ///
    /// Composed of the passes whose rewrites survive the addition of
    /// new phi inputs in a later iteration (see the spec's
    /// "Stable vs destructive optimizer passes" table):
    ///
    /// 1. [`opt::stable_default_pipeline`] — `ConstantFold` + `KnownBits`.
    /// 2. [`opt::StackStoreDetect`] — rewrites stores in place, no
    ///    consumer detachment.
    /// 3. [`opt::StackLoadForward`] — rewrites loads in place.
    /// 4. [`opt::FunctionArgDetect`] (post-pass) — canonicalises
    ///    `FunctionArg` reads.
    ///
    /// CORRECTNESS: every pass in this pipeline rewrites or annotates
    /// nodes but never *removes* phi / `ControlState` / `If` nodes that
    /// the [`crate::RegionIrCache`] pins by `NodeId`.  Adding a
    /// destructive pass here would invalidate the cache's pinned
    /// boundary handles in the next iteration.
    ///
    /// `LoadReadOnly` is omitted because it requires a caller-supplied
    /// ROM image; the orchestrator wires it in itself when one is
    /// available (out of scope here — see the orchestrator).
    ///
    /// `CallStackArgCollect` is omitted because it is a one-shot
    /// post-pass that consumes resolved `Call` shapes; running it
    /// before the destructive subset has cleaned up the IR risks
    /// double-counting stack args at an unsettled call site.
    #[must_use]
    pub fn build_stable_optimizer_pipeline(&self) -> opt::OptimizerPipeline {
        let mut p = opt::stable_default_pipeline();
        p.add(opt::StackStoreDetect::from_convention(
            &self.calling_convention,
        ));
        p.add(opt::StackLoadForward::from_convention(
            &self.calling_convention,
            &self.arch,
        ));
        p.add_post_pass(opt::FunctionArgDetect::from_convention(
            &self.calling_convention,
        ));
        p
    }

    /// Builds the **destructive** optimizer pipeline that the
    /// indirect-branch fixed-point orchestrator runs **once** at the
    /// fixed-point exit (or in the no-`BranchIndirect` fast path).
    ///
    /// Composed of node-removal passes that would invalidate cached
    /// phi `NodeId`s if run mid-iteration:
    ///
    /// 1. [`opt::destructive_default_pipeline`] — `RedundantPhis` +
    ///    `DeadBranchElimination` + `CallOtherElide`.
    /// 2. [`opt::CallStackArgCollect`] (post-pass) — runs after the
    ///    destructive simplification so it sees the final Call shape.
    ///
    /// CORRECTNESS: safe to run only after the IR shape is final.
    /// During iteration the orchestrator uses
    /// [`Self::build_stable_optimizer_pipeline`] instead.
    #[must_use]
    pub fn build_destructive_optimizer_pipeline(&self) -> opt::OptimizerPipeline {
        let mut p = opt::destructive_default_pipeline();
        p.add_post_pass(opt::CallStackArgCollect::from_convention(
            &self.calling_convention,
        ));
        p
    }

    /// Collects the set of all distinct varnodes referenced by any instruction
    /// across all regions of `cfg`, sorted in a deterministic order.
    ///
    /// The result is used to pre-declare every variable the IR builder must
    /// track.  Determinism is required so that downstream `VarId` numbering
    /// (and any node IDs the IR cache derives from it) is stable across runs.
    pub(super) fn find_all_unique_vns<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
    ) -> Vec<rsleigh::Vn> {
        let mut all_vns: std::collections::HashSet<rsleigh::Vn> =
            std::collections::HashSet::new();
        for region in cfg.regions() {
            for wrapped in region.insns.iter() {
                for vn in wrapped.insn.all_vns() {
                    all_vns.insert(vn);
                }
            }
        }
        let mut vns: Vec<rsleigh::Vn> = all_vns.into_iter().collect();
        // Deterministic order: (space-shortcut, offset, size).  HashSet
        // iteration would otherwise depend on the random hasher seed.
        vns.sort_unstable_by_key(|vn| (vn.addr.space.shortcut_raw(), vn.addr.off, vn.size));
        vns
    }

    /// Translates a complete control-flow graph into an [`ir::BuiltFunctionGraph`].
    ///
    /// The pipeline:
    /// 1. Build the function entry node.
    /// 2. Map the CFG graph: each node stores `Option<ir::RegionId>` (None first,
    ///    then filled in fallibly via `node_weight_mut`).
    /// 3. Set the entry region.
    /// 4. Translate instructions in each region, resolving branch targets via
    ///    the mapped graph.
    /// 5. Link fallthrough edges by iterating `cfg_ir_graph`'s edges.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] wrapping the underlying failure when:
    /// - the IR builder fails to create or look up a region
    ///   ([`ErrorKind::CfgNoRegion`], [`ErrorKind::IrError`]);
    /// - an instruction translation fails (any of the per-opcode error variants
    ///   in [`ErrorKind`]: [`ErrorKind::UnimplementedOpcode`],
    ///   [`ErrorKind::MissingOutputVn`], [`ErrorKind::UnsupportedVnSpace`],
    ///   [`ErrorKind::UnsupportedRegSize`], etc.);
    /// - the CFG itself is malformed ([`ErrorKind::CfgError`]).
    pub fn analyze_cfg<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
    ) -> Result<ir::BuiltFunctionGraph> {
        // Discard the unresolved-branch side-table — callers that
        // need it use `analyze_cfg_with_unresolved` directly.  This
        // shim preserves the pre-R1.4 entry-point shape so existing
        // callers (the per-arch test suite, examples) compile
        // unchanged.
        Ok(self.analyze_cfg_with_unresolved(cfg)?.graph)
    }

    /// Variant of [`Self::analyze_cfg`] that also returns the
    /// placeholder-anchor side-table populated when the CFG contains
    /// `RegionTerminator::UnresolvedIndirectBranch` regions.
    ///
    /// Used by the strider-level fixed-point loop's tier-2 resolver
    /// (R2 onward).  Callers that do not need tier-2 information
    /// should prefer [`Self::analyze_cfg`].
    ///
    /// # Errors
    ///
    /// Same as [`Self::analyze_cfg`] — propagates IR builder
    /// failures, per-opcode lifting failures, and CFG inconsistencies.
    pub fn analyze_cfg_with_unresolved<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
    ) -> Result<AnalyzeOutcome> {
        let mut ir_strider = IrStrider::new(self, cfg)?;

        ir_strider.build_entry()?;

        // Step 1: create the structural clone of the CFG graph with None placeholders.
        // The map closure is infallible; IR region creation happens below.
        let mut cfg_ir_graph = cfg.graph.map(|_, _| None::<ir::RegionId>, |_, e| *e);

        // Step 2: fill in IR regions (fallible).
        for region_id in cfg.region_ids() {
            let ir_region = ir_strider.builder.create_region()?;
            *cfg_ir_graph
                .node_weight_mut(region_id)
                .ok_or(ErrorKind::CfgNoRegion(region_id))? = Some(ir_region);
        }

        // Helper closure: map a CFG region id to its IR region id via the graph.
        let ir_region_of = |region_id: cfg::RegionId| -> Result<ir::RegionId> {
            cfg_ir_graph
                .node_weight(region_id)
                .copied()
                .flatten()
                .ok_or_else(|| ErrorKind::CfgNoRegion(region_id).into())
        };

        // Set entry region.
        ir_strider
            .builder
            .set_entry_region(ir_region_of(cfg.entry)?)?;

        // Translate instructions for each region.
        for node_idx in cfg_ir_graph.node_indices() {
            let ir_region = ir_region_of(node_idx)?;
            ir_strider.builder.set_region(ir_region);
            let region = cfg
                .graph
                .node_weight(node_idx)
                .ok_or(ErrorKind::CfgNoRegion(node_idx))?;
            // R1.4: regions whose terminator is
            // `UnresolvedIndirectBranch` need their final
            // `BranchIndirect` insn lifted via the placeholder path
            // — `handle_unresolved_indirect_branch` emits
            // `Return(target_value)` instead of an ABI Return.  We
            // detect this case once per region (cheap match on the
            // terminator) and skip the BranchIndirect insn in the
            // per-instruction loop, then lift it via the dedicated
            // handler post-loop.  Other terminators use the existing
            // per-instruction dispatch path unchanged.
            let unresolved_terminator =
                if let cfg::RegionTerminator::UnresolvedIndirectBranch { target_vn, addr } =
                    &region.terminator
                {
                    Some((*target_vn, *addr))
                } else {
                    None
                };
            for wrapped_insn in &region.insns {
                // Skip the offending BranchIndirect — it will be
                // lifted as a placeholder Return below.  Every other
                // pcode op in the region (incl. the load + sp_adjust
                // a `pop pc` lifts to) still goes through the normal
                // per-instruction dispatch so the optimiser sees the
                // full computation graph.
                if unresolved_terminator.is_some()
                    && wrapped_insn.insn.opcode == rsleigh::Opcode::BranchIndirect
                {
                    continue;
                }
                ir_strider.process_insn(node_idx, &wrapped_insn.insn, ir_region_of)?;
            }
            if let Some((target_vn, addr)) = unresolved_terminator {
                // Tier-2 anchor: lifts to `Return(target_value)` and
                // pushes (addr, target_value) onto
                // `unresolved_branches`.  Per the soft contract this
                // is *not* an error; the strider-level outer loop
                // raises `UnresolvedIndirectBranch` only at fixed
                // point if tier 2 still can't classify.
                ir_strider.handle_unresolved_indirect_branch(&target_vn, addr)?;
            }
        }

        // Link fallthrough edges by inspecting cfg_ir_graph's edges directly.
        for edge_idx in cfg_ir_graph.edge_indices() {
            let Some(weight) = cfg_ir_graph.edge_weight(edge_idx) else {
                continue;
            };
            if *weight != cfg::RegionEdgeKind::Fallthrough {
                continue;
            }
            let Some((src, tgt)) = cfg_ir_graph.edge_endpoints(edge_idx) else {
                continue;
            };
            ir_strider
                .builder
                .link_regions(ir_region_of(src)?, ir_region_of(tgt)?)?;
        }

        let unresolved_branches = ir_strider.unresolved_branches.clone();
        let graph = ir_strider.builder.build()?;
        Ok(AnalyzeOutcome {
            graph,
            unresolved_branches,
        })
    }
}
