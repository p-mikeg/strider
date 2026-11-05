use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{CfgOptions, MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_ir::node::NodeId;
use strider_opt::{OptCtx, ReadOnlyMemory};

use crate::{AnalyzeOptions, LiftOutcome, LiftSites, Strider};

/// Each dispatch site's `BranchIndirect` p-code address, its anchor, with the
/// node lifted for it.
pub(super) type Anchors = Vec<(PcodeInsnAddr, NodeId)>;

/// An anchor with the ISA-mode bit a target seated there with no mode of its
/// own decodes in.
pub(super) type FlowingAnchor = (PcodeInsnAddr, NodeId, bool);

/// The classifier's answers, by node: a site it could not derive has none.
pub(super) type Resolutions = FxHashMap<NodeId, ResolvedTargets>;

/// A dispatch-site classifier, as [`strider_opt::classify_sites`].
pub type Classifier<'c> =
    &'c dyn Fn(&strider_ir::Function, Option<&dyn ReadOnlyMemory>) -> strider_opt::ClassifiedSites;

/// One round's cfg, the function lifted and optimised from it, and its sites.
pub(super) struct Round {
    /// The cfg `function` was lifted from.
    pub(super) cfg: strider_cfg::Cfg,
    pub(super) function: strider_ir::Function,
    /// Each deferred `BranchIndirect` with its `IndirectBranch` placeholder.
    pub(super) placeholders: Anchors,
    /// Each seated `Switch` site with its `Switch` node.
    pub(super) switches: Anchors,
    /// Every reachable placeholder and `Switch`.
    pub(super) live: FxHashSet<NodeId>,
    /// The live sites this round's classifications name no targets for.
    pub(super) unclassified: FxHashSet<NodeId>,
    /// Returns whose target is not provably the entry return address.
    pub(super) unverified_returns: Vec<PcodeInsnAddr>,
}

impl Round {
    /// No dispatch site is left for a fold to change. A seated `Switch` is
    /// one: it is re-derived, and widens, every round.
    pub(super) fn is_settled(&self) -> bool {
        self.placeholders.is_empty() && self.switches.is_empty()
    }

    /// Every dispatch site this round lifted.
    pub(super) fn anchors(&self) -> impl Iterator<Item = PcodeInsnAddr> + '_ {
        self.placeholders
            .iter()
            .chain(&self.switches)
            .map(|&(site, _)| site)
    }

    /// Every dispatch site with its flowing ISA-mode bit, as sampled while its
    /// region decoded ([`strider_cfg::Cfg::flowing_isa_bit_at_site`]), not as
    /// the engine holds it now. `false` on an arch with no ISA-mode var.
    pub(super) fn flowing_anchors(&self) -> Vec<FlowingAnchor> {
        self.placeholders
            .iter()
            .chain(&self.switches)
            .map(|&(addr, node)| {
                let bit = self.cfg.flowing_isa_bit_at_site(addr).unwrap_or(false);
                (addr, node, bit)
            })
            .collect()
    }
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
    /// Builds the cfg under `cfg_opts`, lifts and optimises it, and
    /// classifies its dispatch sites. Errs on a CFG-build, lift, validation or
    /// pipeline failure.
    pub(super) fn build_round(
        &mut self,
        start_addr: MachineInsnAddr,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        cfg_opts: &CfgOptions<'_>,
        pipeline: &strider_opt::OptimizerPipeline,
        classify: Classifier<'_>,
    ) -> Result<(Round, Resolutions)> {
        // Split borrow: `lifter` is `&mut` while the ctx holds `&rom`.
        let Strider {
            ref mut lifter,
            ref rom,
            ..
        } = *self;
        let rom_ref: Option<&dyn ReadOnlyMemory> = rom.as_deref();

        let cfg = lifter.build_cfg(start_addr, cfg_opts)?;
        let LiftOutcome {
            mut function,
            sites:
                LiftSites {
                    unresolved_branches: placeholders,
                    switch_anchors: switches,
                    return_sites,
                },
        } = lifter.build_ir_with(
            &cfg,
            cc.clone(),
            cfg_opts,
            opts.per_address_ccs,
            &opts.assumptions,
            opts.entry_values,
        )?;

        // A `Return` consumes no target, so the pipeline would drop the value
        // each return jumps to. It rides on its `Return` for the run.
        {
            let mut edit = strider_opt::EditFunction::new(&mut function);
            for &(_, ret, target) in &return_sites {
                edit.add_node_input(ret, target);
            }
        }
        pipeline.run(&mut function, &OptCtx::new(rom_ref))?;
        // Before `settle_return_sites` truncates the `Return`s.
        let strider_opt::ClassifiedSites { sites, resolved } = if opts.resolve_indirect_branches {
            classify(&function, rom_ref)
        } else {
            strider_opt::ClassifiedSites::unresolved(&function)
        };
        let unverified_returns = settle_return_sites(&mut function, &return_sites);
        // The truncation drops only a data input, never a dispatch node, so
        // the sites listed before it are the live ones after it.
        let live: FxHashSet<NodeId> = sites.into_iter().collect();
        let unclassified = live
            .iter()
            .filter(|node| !resolved.contains_key(node))
            .copied()
            .collect();
        let round = Round {
            cfg,
            function,
            placeholders,
            switches,
            live,
            unclassified,
            unverified_returns,
        };
        Ok((round, resolved))
    }
}

/// The live return sites whose target, anchored last on the `Return` by
/// [`Strider::build_round`], is not the entry return address. Verified under
/// the claims the round was optimized with.
pub(super) fn settle_return_sites(
    function: &mut strider_ir::Function,
    sites: &[strider_lift::ReturnSite],
) -> Vec<PcodeInsnAddr> {
    let mut unverified = Vec::new();
    let mut live = Vec::with_capacity(sites.len());
    let mut edit = strider_opt::EditFunction::new(function);
    {
        let mut targets = strider_opt::ReturnTargets::new(edit.function());
        for &(addr, ret, _) in sites {
            if !edit.is_live(ret) {
                continue;
            }
            let inputs = strider_ir::IRViewer::node_inputs(edit.function(), ret);
            let anchored = inputs.len() - 1;
            let &target = inputs
                .get(anchored)
                .expect("an anchored Return carries its target last");
            if !targets.returns_to_caller(ret, target) {
                unverified.push(addr);
            }
            live.push((ret, anchored));
        }
    }
    for (ret, anchored) in live {
        edit.truncate_node_inputs(ret, anchored);
    }
    edit.clean();
    unverified
}
