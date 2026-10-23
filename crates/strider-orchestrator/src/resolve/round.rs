//! One round of the resolve loop: the whole function rebuilt from one seat
//! set.

use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{CfgOptions, MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_ir::node::NodeId;
use strider_opt::{OptCtx, ReadOnlyMemory};

use crate::{AnalyzeOptions, LiftOutcome, Strider};

/// Each dispatch site's `BranchIndirect` p-code address, its anchor, with the
/// node lifted for it.
pub(super) type Anchors = Vec<(PcodeInsnAddr, NodeId)>;

/// The classifier's answers, by node: a site it could not derive has none.
pub(super) type Resolutions = FxHashMap<NodeId, ResolvedTargets>;

/// What a round classifies its dispatch sites with:
/// [`strider_opt::classify_sites`], or a test's stand-in.
pub type Classifier<'c> = &'c dyn Fn(
    &strider_ir::Function,
    Option<&dyn ReadOnlyMemory>,
) -> Result<strider_opt::ClassifiedSites>;

/// One round's cfg, the function lifted and optimised from it, and what the
/// loop reads off the two. The classifications stay out, so a fold can take
/// them by value.
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
    /// No dispatch site is left for a fold to change. A seated `Switch` counts
    /// as one: it is re-derived every round, so a table resolved before its
    /// loop closed widens here.
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
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
    /// Builds the cfg under `cfg_opts`, lifts and optimises it, and
    /// classifies its dispatch sites, keeping the node-keyed answers apart.
    ///
    /// # Errors
    ///
    /// Propagates CFG-build, lift (including IR validation), and pipeline
    /// failures.
    pub(super) fn build_round(
        &mut self,
        start_addr: MachineInsnAddr,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        cfg_opts: &CfgOptions<'_>,
        pipeline: &strider_opt::OptimizerPipeline,
        classify: Classifier<'_>,
    ) -> Result<(Round, Resolutions)> {
        // Destructured to split the borrow: `lifter` goes out `&mut` while
        // the optimiser ctx holds `&rom`.
        let Strider {
            ref mut lifter,
            ref rom,
            ..
        } = *self;
        let rom_ref: Option<&dyn ReadOnlyMemory> = rom.as_deref();

        // A `BranchIndirect` neither table seats is deferred here and resolved
        // at the full-function IR level by `classify`.
        let cfg = lifter.build_cfg(start_addr, cfg_opts)?;
        // `cc` is moved all the way into `Function::default_cc`, but the
        // loop builds again next round, so the clone is unavoidable here.
        let LiftOutcome {
            mut function,
            unresolved_branches: placeholders,
            switch_anchors: switches,
            return_sites,
        } = lifter.build_ir_with(
            &cfg,
            cc.clone(),
            cfg_opts,
            opts.per_address_ccs,
            &opts.assumptions,
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
        // Before `settle_return_sites` truncates the `Return`s and cleans, so
        // the classifier reads the IR the pipeline settled on.
        let strider_opt::ClassifiedSites { sites, resolved } = if opts.resolve_indirect_branches {
            classify(&function, rom_ref)?
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

/// The ISA-mode bit a target seated with no mode of its own decodes in, per
/// indirect site.
///
/// Read off the cfg, which sampled it while each site's region was decoding
/// ([`strider_cfg::Cfg::flowing_isa_bit_at_site`]). Probing the live engine again here
/// would answer for whichever region painted the branch address last, and
/// [`super::merge::adopt_known_modes`] would then take the wrong `interworking`
/// verdict.
/// [`strider_cfg::Cfg::function_isa_bit`] covers an address no indirect branch
/// was sealed at and every arch with no ISA-mode var.
pub(super) fn flowing_isa_bits(
    cfg: &strider_cfg::Cfg,
    placeholders: &Anchors,
    switches: &Anchors,
) -> FxHashMap<PcodeInsnAddr, bool> {
    let entry_bit = cfg.function_isa_bit().unwrap_or(false);
    placeholders
        .iter()
        .chain(switches.iter())
        .map(|(addr, _)| {
            (
                *addr,
                cfg.flowing_isa_bit_at_site(*addr).unwrap_or(entry_bit),
            )
        })
        .collect()
}

/// The return sites whose target is not the function's entry return address.
/// Each target is taken back off the `Return` [`Strider::build_round`] anchored
/// it on.
///
/// Verification reads the claims the round's lift was optimized under:
/// a claim that holds keeps the verdict right, and a claim that does not is
/// the caller's. There is no second, claim-cleared run to measure what a claim
/// bought, because nothing downstream would act on the difference.
///
/// A site the pipeline culled took its `Return` with it.
pub(super) fn settle_return_sites(
    function: &mut strider_ir::Function,
    sites: &[strider_lift::lift::ReturnSite],
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
