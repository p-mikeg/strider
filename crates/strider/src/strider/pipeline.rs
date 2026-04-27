use crate::error::{ErrorKind, Result};

use super::IrStrider;

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
            for wrapped_insn in &region.insns {
                ir_strider.process_insn(node_idx, &wrapped_insn.insn, ir_region_of)?;
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

        Ok(ir_strider.builder.build()?)
    }
}
