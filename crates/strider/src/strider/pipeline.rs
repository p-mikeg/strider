use crate::error::{ErrorKind, Result};

use super::IrStrider;

/// Per-region IR-handle snapshot, captured during lift before
/// `FunctionBuilder::build()` consumes the builder's region map.  Used
/// by the strider [`crate::ir_cache::RegionIrCache`] to populate cache
/// entries with live `NodeId`s that pin per-region phi-extension
/// targets across orchestrator iterations.
///
/// Layout mirrors [`crate::RegionIrEntry`] but tracks `(VarId, ...)`
/// pairs with the `Vn` resolved from the `FunctionBuilder` at lift
/// time (the cache stores `Vn`-keyed maps for cross-iteration
/// stability — `VarId` can renumber if the builder is rebuilt).
#[derive(Debug, Clone)]
pub struct RegionLiftHandles {
    /// Region's start address (cache key).
    pub start_addr: cfg::PcodeInsnAddr,
    /// Number of CFG predecessors at lift time.  The cache uses this
    /// to detect "the predecessor count grew since last iteration."
    pub predecessor_count: usize,
    /// `ControlState` `NodeId` (entry-boundary).
    pub entry_control_state: ir::node::NodeId,
    /// `MemPhi` `NodeId` (entry-boundary).
    pub entry_mem_phi: ir::node::NodeId,
    /// Entry control output produced by the `ControlState`.
    pub entry_control: ir::node::NodeOutputId,
    /// Entry memory output produced by the `MemPhi`.
    pub entry_memory: ir::node::NodeOutputId,
    /// Exit control output (consumed by the region's terminator).
    pub exit_control: ir::node::NodeOutputId,
    /// Exit memory output (consumed by the region's terminator).
    pub exit_memory: ir::node::NodeOutputId,
    /// Per-var entry-boundary `ControlPhi` `NodeId`s, keyed by `Vn`.
    /// Used by `extend_predecessors_into` to know which phi node to
    /// append a new predecessor's value to.
    pub entry_var_phis: std::collections::HashMap<rsleigh::Vn, ir::node::NodeId>,
    /// Per-var exit-boundary value `NodeOutputId`s, keyed by `Vn`.
    /// Used by `extend_predecessors_into` to source values for new
    /// predecessor edges.
    pub exit_vn_to_value: std::collections::HashMap<rsleigh::Vn, ir::node::NodeOutputId>,
}

/// The full result of a strider lift, exposing the lifted IR plus the
/// placeholder-anchor side-table the strider-level fixed-point loop's
/// tier-2 resolver consumes plus per-region IR-handle snapshots.
///
/// Returned by [`Strider::analyze_cfg`] (W2: single canonical entry
/// point).  Callers that only need the graph can use `outcome.graph`
/// directly; tier-2-aware callers read `unresolved_branches` and
/// `region_handles`.
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
    /// Per-region IR-handle snapshots captured at lift time.  Keyed
    /// by the region's machine start address (matches the cache key
    /// used by [`crate::ir_cache::RegionIrCache`]).  Always populated
    /// after W2 — the previous "empty if caller didn't ask" branch
    /// went away with the consolidation onto a single canonical
    /// `analyze_cfg` entry point.
    pub region_handles: Vec<RegionLiftHandles>,
}

/// W10 — single-line summary of an [`AnalyzeOutcome`] for diagnostics
/// ("why didn't tier 2 resolve X?").  When a user runs the orchestrator
/// and inspects the outcome, formatting the outcome with `{}` gives a
/// quick readout of the data the outcome actually carries — unresolved-
/// branch count and per-region handle count.
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

    /// Translates a complete control-flow graph into an [`AnalyzeOutcome`]
    /// — the unified return type that bundles the lifted IR with the
    /// placeholder-anchor side-table (populated when the CFG contains
    /// `RegionTerminator::UnresolvedIndirectBranch` regions) and the
    /// per-region IR-handle snapshots used by the strider fixed-point
    /// orchestrator's cache.
    ///
    /// Callers that only need the graph can use `outcome.graph` directly
    /// (most per-arch fixture tests do this).  Tier-2-aware callers read
    /// `outcome.unresolved_branches` and `outcome.region_handles`.
    ///
    /// W2 — single canonical entry point.  Replaces the pre-W2
    /// `analyze_cfg` (returned `BuiltFunctionGraph`) and
    /// `analyze_cfg_with_unresolved` (returned `AnalyzeOutcome`) pair —
    /// having two near-identical methods invited drift on every
    /// orchestrator change.  Callers that previously called
    /// `analyze_cfg(&cfg)?` now write `analyze_cfg(&cfg)?.graph`; the
    /// extra `.graph` field-access is the only caller-side change.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] wrapping the underlying failure when:
    /// - the IR builder fails to create or look up a region
    ///   ([`ErrorKind::CfgNoRegion`], [`ErrorKind::IrError`]);
    /// - an instruction translation fails (any of the per-opcode error
    ///   variants in [`ErrorKind`]: [`ErrorKind::UnimplementedOpcode`],
    ///   [`ErrorKind::MissingOutputVn`], [`ErrorKind::UnsupportedVnSpace`],
    ///   [`ErrorKind::UnsupportedRegSize`], etc.);
    /// - the CFG itself is malformed ([`ErrorKind::CfgError`]).
    pub fn analyze_cfg<R: rsleigh::MemReader>(
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
            // `Return(target_value)` instead of an ABI Return.
            //
            // F7: regions whose terminator is `Switch` need their
            // final `BranchIndirect` insn lifted via the If-ladder
            // path — `handle_switch` emits N-1 chained
            // `IntCmpOp::Equal + If` nodes against the resolved
            // targets instead of a single Return.
            //
            // Both shapes detect the special case once per region
            // (cheap match on the terminator) and skip the
            // BranchIndirect insn in the per-instruction loop, then
            // lift it via the dedicated handler post-loop.  Other
            // terminators use the existing per-instruction dispatch
            // path unchanged.
            enum SpecialTerm {
                Unresolved(rsleigh::Vn, cfg::PcodeInsnAddr),
                Switch(rsleigh::Vn, Vec<u64>, Option<ir::Value>),
            }
            let special_terminator: Option<SpecialTerm> = match &region.terminator {
                cfg::RegionTerminator::UnresolvedIndirectBranch { target_vn, addr } => {
                    Some(SpecialTerm::Unresolved(*target_vn, *addr))
                }
                cfg::RegionTerminator::Switch {
                    target_vn,
                    targets,
                    target_value,
                } => Some(SpecialTerm::Switch(
                    *target_vn,
                    targets.clone(),
                    *target_value,
                )),
                _ => None,
            };
            for wrapped_insn in &region.insns {
                // Skip the offending BranchIndirect — it will be
                // lifted as a placeholder Return / If-ladder below.
                // Every other pcode op in the region (incl. the
                // load + sp_adjust a `pop pc` lifts to) still goes
                // through the normal per-instruction dispatch so
                // the optimiser sees the full computation graph.
                if special_terminator.is_some()
                    && wrapped_insn.insn.opcode == rsleigh::Opcode::BranchIndirect
                {
                    continue;
                }
                ir_strider.process_insn(
                    node_idx,
                    &wrapped_insn.insn,
                    wrapped_insn.addr,
                    ir_region_of,
                )?;
            }
            match special_terminator {
                Some(SpecialTerm::Unresolved(target_vn, addr)) => {
                    // Tier-2 anchor: lifts to `Return(target_value)`
                    // and pushes (addr, target_value) onto
                    // `unresolved_branches`.  Per the soft contract
                    // this is *not* an error; the strider-level
                    // outer loop raises `UnresolvedIndirectBranch`
                    // only at fixed point if tier 2 still can't
                    // classify.
                    ir_strider.handle_unresolved_indirect_branch(&target_vn, addr)?;
                }
                Some(SpecialTerm::Switch(target_vn, targets, target_value)) => {
                    // F7: jump-table dispatch — emit the If-ladder.
                    ir_strider.handle_switch(
                        node_idx,
                        &target_vn,
                        &targets,
                        target_value,
                        &ir_region_of,
                    )?;
                }
                None => {}
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

        // CORRECTNESS — region-handle snapshot: capture per-region IR
        // handles BEFORE `build()` consumes the builder.  The cache
        // populates from these snapshots in
        // `crate::ir_cache::lift_new_regions_into`.  Each snapshot
        // records `NodeId`s that survive `build()` (the IR graph is
        // moved out of the builder, but `NodeId`s are stable indices
        // into its arena), so cache entries built from these handles
        // are valid for the lifetime of the returned
        // `BuiltFunctionGraph`.
        let mut region_handles: Vec<RegionLiftHandles> = Vec::new();
        for cfg_region_id in cfg.region_ids() {
            let ir_region_id = ir_region_of(cfg_region_id)?;
            let region = cfg
                .graph
                .node_weight(cfg_region_id)
                .ok_or(ErrorKind::CfgNoRegion(cfg_region_id))?;
            let predecessor_count = cfg.predecessor_count(cfg_region_id);

            // Per-var phi node IDs, keyed by Vn.  We resolve each
            // VarId to its Vn via the builder's vn_of_var accessor.
            let mut entry_var_phis: std::collections::HashMap<rsleigh::Vn, ir::node::NodeId> =
                std::collections::HashMap::new();
            for (var_id, phi_out) in ir_strider.builder.region_initial_variables(ir_region_id) {
                if let Some(vn) = ir_strider.builder.vn_of_var(var_id) {
                    let phi_node = ir_strider
                        .builder
                        .body()
                        .graph
                        .output_definition(phi_out)
                        .0;
                    entry_var_phis.insert(vn, phi_node);
                }
            }

            // Per-var exit values, keyed by Vn.
            let mut exit_vn_to_value: std::collections::HashMap<
                rsleigh::Vn,
                ir::node::NodeOutputId,
            > = std::collections::HashMap::new();
            for (var_id, val_out) in ir_strider.builder.region_exit_variables(ir_region_id) {
                if let Some(vn) = ir_strider.builder.vn_of_var(var_id) {
                    exit_vn_to_value.insert(vn, val_out);
                }
            }

            let entry_control = ir_strider.builder.region_entry_control(ir_region_id)?;
            let entry_memory = ir_strider.builder.region_entry_memory(ir_region_id)?;
            let exit_control = ir_strider.builder.region_cur_ctrl(ir_region_id);
            let exit_memory = ir_strider.builder.region_cur_memory(ir_region_id);

            region_handles.push(RegionLiftHandles {
                start_addr: region.start_addr,
                predecessor_count,
                entry_control_state: ir_strider.builder.region_control_node(ir_region_id),
                entry_mem_phi: ir_strider.builder.region_memory_node(ir_region_id),
                entry_control,
                entry_memory,
                exit_control,
                exit_memory,
                entry_var_phis,
                exit_vn_to_value,
            });
        }

        let unresolved_branches = ir_strider.unresolved_branches.clone();
        let graph = ir_strider.builder.build()?;
        Ok(AnalyzeOutcome {
            graph,
            unresolved_branches,
            region_handles,
        })
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`AnalyzeOutcome`] formatting helpers (W10).

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]


    /// W10 — build a real (but minimal) `AnalyzeOutcome` by analysing
    /// a one-instruction `ret` program, then assert the `Display`
    /// readout contains the unresolved-branch and region counts.  Going
    /// through the real `analyze_cfg` path keeps this test honest about
    /// what the outcome's fields actually carry — synthesising a fake
    /// `BuiltFunctionGraph` would lose that grounding.
    #[test]
    fn display_summarises_unresolved_branches_and_region_count() {
        // Standard x86_64 `ret` byte sequence.  No `BranchIndirect`, so
        // `unresolved_branches.len() == 0`.
        let arch = crate::SleighArch::x86_64();
        let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
        let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe)
            .expect("probe sleigh")
            .regs()
            .expect("probe regs");
        let strider = crate::Strider::new(
            arch,
            regs,
            crate::CallingConvention::x86_64_systemv_abi(),
        )
        .expect("strider");
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc3u8], 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
            .expect("sleigh");
        let cfg = cfg::Builder::new(sleigh, 0x1000, cfg::OptionsBuilder::new().build())
            .build()
            .expect("cfg");
        let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
        // Plain `ret`: zero unresolved branches, exactly one region.
        let s = format!("{outcome}");
        assert!(
            s.contains("unresolved_branches: 0"),
            "Display output must surface the unresolved-branch count; got {s:?}",
        );
        assert!(
            s.contains("regions: 1"),
            "Display output must surface the region count; got {s:?}",
        );
    }
}
