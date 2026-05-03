use anyhow::{anyhow, Result};

use super::IrStrider;

/// Per-region IR-handle snapshot, captured during lift before
/// `FunctionBuilder::build()` consumes the builder's region map.  Used
/// by the orchestrator to build a per-iteration `NodeOutputId → region`
/// index for the in-place editors' "find the placeholder's region"
/// queries.
#[derive(Debug, Clone)]
pub struct RegionLiftHandles {
    /// Region's start address.
    pub start_addr: cfg::PcodeInsnAddr,
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
    /// Per-var entry-boundary `VarPhi` `NodeId`s, keyed by `Vn`.
    pub entry_var_phis: std::collections::HashMap<rsleigh::Vn, ir::node::NodeId>,
    /// Per-var exit-boundary value `NodeOutputId`s, keyed by `Vn`.
    ///
    /// Wrapped in `Arc` so the orchestrator's per-iteration
    /// `RegionIndex::from_handles` can `Arc::clone` instead of
    /// deep-cloning the map (the map is never mutated post-build).
    // TODO(Task17): remove after incremental indirect-resolve lands —
    // see docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md
    pub exit_vn_to_value:
        std::sync::Arc<std::collections::HashMap<rsleigh::Vn, ir::node::NodeOutputId>>,
}

/// The full result of a strider lift, exposing the lifted IR plus the
/// placeholder-anchor side-table the tier-2 resolver consumes plus
/// per-region IR-handle snapshots.
///
/// Returned by [`Strider::analyze_cfg`].  Callers that only need the
/// graph can use `outcome.graph` directly; tier-2-aware callers read
/// `unresolved_branches` and `region_handles`.
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
    /// Per-region IR-handle snapshots captured at lift time.  The
    /// orchestrator's per-iteration index uses these to map a
    /// placeholder's pre-edit ctrl input back to the region whose
    /// exit produced it (so it can read the region's exit
    /// `vn_to_value` for the in-place edit's ABI threading).
    pub region_handles: Vec<RegionLiftHandles>,
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
    /// `sleigh_regs`.
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` if any register name in
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
    #[must_use]
    pub(crate) fn arch(&self) -> &crate::SleighArch {
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
    /// Composed of passes whose rewrites survive a later iteration that
    /// adds new phi inputs: `ConstantFold`, `KnownBits`,
    /// `StackStoreDetect`, `StackLoadForward`, and the
    /// `FunctionArgDetect` post-pass.  The destructive passes
    /// (`RedundantPhis` / `DeadBranchElimination` / `CallOtherElide`)
    /// are deferred to the final iteration because they remove nodes
    /// that the orchestrator's per-iteration index pins.
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
    /// Composed of node-removal passes safe to run only after the IR
    /// shape is final: `RedundantPhis`, `DeadBranchElimination`,
    /// `CallOtherElide`, plus the `CallStackArgCollect` post-pass.
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
    /// Determinism (sort by `(space-shortcut, offset, size)`) is required
    /// so that downstream `VarId` numbering is stable across runs.
    pub(crate) fn find_all_unique_vns<R: rsleigh::MemReader>(
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
        vns.sort_unstable_by_key(pcode_lift::vn_sort_key);
        vns
    }

    /// Translates a complete control-flow graph into an [`AnalyzeOutcome`].
    ///
    /// Callers that only need the graph use `outcome.graph` directly.
    /// Tier-2-aware callers read `outcome.unresolved_branches` and
    /// `outcome.region_handles`.
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` when the CFG is malformed (missing
    /// region, unknown terminator), instruction translation fails (an
    /// unsupported opcode or varnode), or IR validation fails.
    pub fn analyze_cfg<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
    ) -> Result<AnalyzeOutcome> {
        self.analyze_cfg_with_vns(cfg, self.find_all_unique_vns(cfg))
    }

    /// Variant of [`Self::analyze_cfg`] that takes a pre-computed
    /// vn set — used by the orchestrator to share its
    /// [`super::super::orchestrator`] vn cache across rebuild
    /// iterations.  The supplied `all_vns` must be sorted by
    /// `pcode_lift::vn_sort_key` (otherwise downstream `VarId`
    /// numbering loses determinism) and must include every varnode
    /// any instruction in `cfg` references — under-tracking would
    /// drop pcode reads.  Over-tracking is safe but allocates one
    /// extra `InitialVar` per superfluous vn.
    ///
    /// # Errors
    ///
    /// Propagates errors from `IrStrider::new` (variable-table init,
    /// CC build), `FunctionBuilder::build_entry`, the per-region IR
    /// translation (`pcode-lift` value-producer failures, control-op
    /// routing, calling-convention plumbing), and final
    /// `FunctionBuilder::build`'s `ir::validate::validate` pass.
    pub fn analyze_cfg_with_vns<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
        all_vns: Vec<rsleigh::Vn>,
    ) -> Result<AnalyzeOutcome> {
        let mut ir_strider = IrStrider::new(self, cfg, all_vns)?;
        ir_strider.builder.build_entry()?;

        // Map every CFG region id to its newly-allocated IR region id.
        // Indexed by `RegionId.index()` so the per-instruction loop
        // can resolve in O(1) without cloning the petgraph.
        let cfg_region_ids: Vec<cfg::RegionId> = cfg.region_ids().collect();
        let max_index = cfg_region_ids.iter().map(|r| r.index()).max().unwrap_or(0);
        let mut region_map: Vec<Option<ir::RegionId>> = vec![None; max_index + 1];
        for cfg_rid in &cfg_region_ids {
            region_map[cfg_rid.index()] = Some(ir_strider.builder.create_region()?);
        }
        let ir_region_of = |region_id: cfg::RegionId| -> Result<ir::RegionId> {
            region_map
                .get(region_id.index())
                .copied()
                .flatten()
                .ok_or_else(|| anyhow!("no region {region_id:?} in cfg"))
        };

        ir_strider
            .builder
            .set_entry_region(ir_region_of(cfg.entry)?)?;

        // Translate instructions for each region.
        for &cfg_rid in &cfg_region_ids {
            let ir_region = ir_region_of(cfg_rid)?;
            ir_strider.builder.set_region(ir_region);
            let region = cfg
                .graph
                .node_weight(cfg_rid)
                .ok_or_else(|| anyhow!("no region {cfg_rid:?} in cfg"))?;
            // Regions with non-trivial terminators have their
            // terminator p-code insn skipped inside the per-insn loop
            // and lifted via a dedicated handler post-loop:
            //   * `UnresolvedIndirectBranch` skips `BranchIndirect`,
            //     lifts via the placeholder path.
            //   * `Switch` skips `BranchIndirect`, lifts as an
            //     If-ladder.
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
                ir_strider.process_insn(
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
            ir_strider.builder.set_lift_addr(term_addr);
            let term_res = (|| -> Result<()> {
                match special_terminator {
                    Some(SpecialTerm::Unresolved(target_vn, addr)) => {
                        ir_strider.handle_unresolved_indirect_branch(&target_vn, addr)?;
                    }
                    Some(SpecialTerm::Switch(target_vn, targets, target_value)) => {
                        ir_strider.handle_switch(
                            cfg_rid,
                            &target_vn,
                            &targets,
                            target_value,
                            &ir_region_of,
                        )?;
                    }
                    Some(SpecialTerm::TailCall(target)) => {
                        ir_strider.handle_tail_call(target)?;
                    }
                    None => {}
                }
                Ok(())
            })();
            ir_strider.builder.set_lift_addr(None);
            term_res?;
        }

        // Link fallthrough edges.  Walk the CFG's edges directly —
        // there's no need to mirror the petgraph.
        for edge_idx in cfg.graph.edge_indices() {
            let Some(weight) = cfg.graph.edge_weight(edge_idx) else {
                continue;
            };
            if *weight != cfg::RegionEdgeKind::Fallthrough {
                continue;
            }
            let Some((src, tgt)) = cfg.graph.edge_endpoints(edge_idx) else {
                continue;
            };
            ir_strider
                .builder
                .link_regions(ir_region_of(src)?, ir_region_of(tgt)?)?;
        }

        // Capture per-region IR handles BEFORE `build()` consumes the
        // builder.  `NodeId`/`NodeOutputId` are stable across the
        // build-time arena move, so the snapshots remain valid for the
        // returned `BuiltFunctionGraph`.
        let mut region_handles: Vec<RegionLiftHandles> = Vec::new();
        for &cfg_rid in &cfg_region_ids {
            let ir_region_id = ir_region_of(cfg_rid)?;
            let region = cfg
                .graph
                .node_weight(cfg_rid)
                .ok_or_else(|| anyhow!("no region {cfg_rid:?} in cfg"))?;

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
                entry_control_state: ir_strider.builder.region_control_node(ir_region_id),
                entry_mem_phi: ir_strider.builder.region_memory_node(ir_region_id),
                entry_control,
                entry_memory,
                exit_control,
                exit_memory,
                entry_var_phis,
                exit_vn_to_value: std::sync::Arc::new(exit_vn_to_value),
            });
        }

        let unresolved_branches = std::mem::take(&mut ir_strider.unresolved_branches);
        let graph = ir_strider.builder.build()?;
        Ok(AnalyzeOutcome {
            graph,
            unresolved_branches,
            region_handles,
        })
    }
}

/// Per-region special-terminator marker the per-instruction loop uses
/// to skip the terminator p-code insn so the post-loop dispatch can
/// lift it via a dedicated handler.
enum SpecialTerm {
    /// Tier-2 placeholder: lifts to `Return(target_value)` and pushes
    /// the (addr, target_value) pair onto `unresolved_branches`.  Skip
    /// the trailing `BranchIndirect`.
    Unresolved(rsleigh::Vn, cfg::PcodeInsnAddr),
    /// Resolved jump table: lifts to an If-ladder dispatching `idx`
    /// against `targets`.  Skip the trailing `BranchIndirect`.
    Switch(rsleigh::Vn, Vec<u64>, Option<ir::Value>),
    /// Direct branch to an out-of-function target (`fn_max_size`
    /// bound exceeded, or sub-`start_addr` with
    /// `allow_code_before_start_addr=false`).  Lifts to
    /// `Call(IntConst(target)) + Return`.  Skip the trailing
    /// `Branch`.
    TailCall(u64),
}

impl SpecialTerm {
    fn from_terminator(t: &cfg::RegionTerminator) -> Option<Self> {
        match t {
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
            cfg::RegionTerminator::TailCall { target } => Some(SpecialTerm::TailCall(*target)),
            _ => None,
        }
    }

    /// Returns true when the per-region per-insn loop should skip
    /// `opcode` because the post-loop dispatcher will lift it via a
    /// dedicated handler.  `Unresolved`/`Switch` skip `BranchIndirect`;
    /// `TailCall` skips both `Branch` (the standard direct-tail-call
    /// case) AND `CondBranch` (the `cfg::RegionBuilder` collapse path
    /// for a conditional jump whose successors all leave the function).
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
            SpecialTerm::Unresolved(..) | SpecialTerm::Switch(..) => {
                opcode == rsleigh::Opcode::BranchIndirect
            }
            SpecialTerm::TailCall(..) => matches!(
                opcode,
                rsleigh::Opcode::Branch | rsleigh::Opcode::CondBranch
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
        let arch = crate::SleighArch::x86_64();
        let regs = arch.probe_regs().expect("probe regs");
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
        let s = format!("{outcome}");
        assert!(s.contains("unresolved_branches: 0"));
        assert!(s.contains("regions: 1"));
    }
}
