//! One round of the resolve loop: the whole function rebuilt from one seat
//! set.

use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_ir::node::NodeId;
use strider_opt::{OptCtx, ReadOnlyMemory};

use crate::{
    AnalyzeOptions, IndirectResolutions, LiftOutcome, Strider, UnresolvedAnchors,
    live_dispatch_sites, settle_return_sites, unclassified_nodes,
};

/// One round's cfg, the function lifted and optimised from it, and what the
/// loop reads off the two. The classifications stay out, so a fold can take
/// them by value.
pub(crate) struct Round {
    /// The cfg `function` was lifted from.
    pub(crate) cfg: strider_cfg::Cfg,
    pub(crate) function: strider_ir::Function,
    /// Each deferred `BranchIndirect` with its `IndirectBranch` placeholder.
    pub(crate) placeholders: UnresolvedAnchors,
    /// Each seated `Switch` site with its `Switch` node.
    pub(crate) switches: UnresolvedAnchors,
    /// Every reachable placeholder and `Switch`.
    pub(crate) live: FxHashSet<NodeId>,
    /// The live sites this round's classifications name no targets for.
    pub(crate) unclassified: FxHashSet<NodeId>,
    /// Returns whose target is not provably the entry return address.
    pub(crate) unverified_returns: Vec<PcodeInsnAddr>,
}

impl Round {
    /// No dispatch site is left for a fold to change. A seated `Switch` counts
    /// as one: it is re-derived every round, so a table resolved before its
    /// loop closed widens here.
    pub(crate) fn is_settled(&self) -> bool {
        self.placeholders.is_empty() && self.switches.is_empty()
    }
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
    /// Builds the cfg from `opts` over the seat set `working`, lifts and
    /// optimises it, and keeps the classifier post-pass's node-keyed answers
    /// apart.
    ///
    /// # Errors
    ///
    /// Propagates CFG-build, lift (including IR validation), and pipeline
    /// failures.
    pub(crate) fn build_round(
        &mut self,
        start_addr: MachineInsnAddr,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        working: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
        pipeline: &strider_opt::OptimizerPipeline,
    ) -> Result<(Round, IndirectResolutions)> {
        // Destructured to split the borrow: `lifter` goes out `&mut` while
        // the optimiser ctx holds `&rom`.
        let Strider {
            ref mut lifter,
            ref rom,
            ..
        } = *self;
        let rom_ref: Option<&dyn ReadOnlyMemory> = rom.as_deref();

        // Cloned whole: naming the other fields would silently drop any field
        // added to `CfgOptions` later from the whole re-lift loop.
        let cfg_opts = strider_cfg::CfgOptions {
            known_targets: working,
            ..opts.cfg.clone()
        };
        // A `BranchIndirect` outside `known_targets` is deferred here and
        // resolved at the full-function IR level by the classifier post-pass.
        let cfg = lifter.build_cfg(start_addr, &cfg_opts)?;
        // `cc` is moved all the way into `Function::default_cc`, but the
        // loop builds again next round, so the clone is unavoidable here.
        let LiftOutcome {
            mut function,
            unresolved_branches: placeholders,
            switch_anchors: switches,
            return_sites,
        } = lifter.build_ir_with(&cfg, cc.clone(), &cfg_opts, opts.per_address_ccs)?;

        // A `Return` consumes no target, so the pipeline would drop the value
        // each return jumps to. It rides on its `Return` for the run.
        {
            let mut edit = strider_opt::EditFunction::new(&mut function);
            for &(_, ret, target) in &return_sites {
                edit.add_node_input(ret, target)?;
            }
        }
        let mut ctx = OptCtx::new(rom_ref);
        ctx.options = opts.opt.clone();
        pipeline.run(&mut function, &mut ctx)?;
        let resolutions = std::mem::take(&mut ctx.indirect_resolutions);
        let unverified_returns =
            settle_return_sites(&mut function, &return_sites, &opts.opt.assumptions);
        // Read off the function, not the classification map, which any
        // post-pass can rewrite.
        let live = live_dispatch_sites(&function);
        let unclassified = unclassified_nodes(&live, &resolutions);
        let round = Round {
            cfg,
            function,
            placeholders,
            switches,
            live,
            unclassified,
            unverified_returns,
        };
        Ok((round, resolutions))
    }
}
