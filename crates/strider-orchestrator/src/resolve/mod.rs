mod merge;
mod reports;
mod round;
mod sites;
#[cfg(test)]
pub(crate) mod test_support;

use anyhow::Result;
use rustc_hash::FxHashMap;
use strider_cfg::{Incompleteness, MachineInsnAddr, PcodeInsnAddr, Reason, ResolvedTargets};

use crate::{AnalyzeOptions, AnalyzeResult, Strider};
use reports::{
    DerivedChannels, caller_seeded_return, live_unresolved_branches, refusals, seated_arm_losses,
    unverified_seeded_sites,
};
pub use round::Classifier;
use round::{Resolutions, Round};
use sites::SiteTable;

/// Caps the folds, and so the discovery depth: one fold seats one level of
/// indirect branches. A site still growing at the cap is reported.
const MAX_RESOLUTION_ITERATIONS: usize = 256;

/// `items`, counting each read under `cfg(test)`.
fn visit<I: IntoIterator>(items: I) -> impl Iterator<Item = I::Item> {
    items.into_iter().inspect(|_| {
        #[cfg(test)]
        test_support::TARGET_VISITS.with(|v| v.set(v.get() + 1));
    })
}

/// Why the loop stopped.
enum Termination {
    /// The last round built is the answer.
    Converged,
    /// The cap ran out on a round whose classifications nothing has folded.
    CapReached(Resolutions),
}

/// What the loop carries from one round to the next.
struct LoopState<'s> {
    /// Every site's answer, over the caller's seeds.
    sites: SiteTable<'s>,
    /// Sticky: sites that lost ground in any round, or whose seat the loop
    /// withdrew or trimmed.
    derived_incomplete: Vec<PcodeInsnAddr>,
    /// Sticky: only the round that seats a guessed-mode arm flags it.
    assumed_modes: Vec<PcodeInsnAddr>,
    /// Every round's target reasons: an inexact edge fed answers that
    /// outlive its round.
    incomplete: Incompleteness,
    /// Rounds folded into `sites`.
    folds: usize,
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
    /// [`Strider::analyze`] with `classify` in place of
    /// [`strider_opt::classify_sites`].
    pub(crate) fn resolve(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        pipeline: Option<strider_opt::OptimizerPipeline>,
        classify: Classifier<'_>,
    ) -> Result<AnalyzeResult> {
        anyhow::ensure!(
            opts.cfg.derived_targets.is_empty(),
            "analyze keeps its own answers in derived_targets; seed through \
             known_targets instead"
        );
        let pipeline = pipeline.unwrap_or_else(strider_opt::default_pipeline);
        let start_addr = MachineInsnAddr::from(entry);
        let mut st = LoopState::new(opts.cfg.known_targets);
        // Every round rebuilds everything from the seats alone: convergence
        // rests on the cfg being a function of them.
        let mut result = loop {
            let (round, resolutions) = self.build_round(
                start_addr,
                cc,
                opts,
                &st.sites.cfg_options(&opts.cfg),
                &pipeline,
                classify,
            )?;
            // A stale round still seats what the loop has since refused.
            let stale = st.observe(&round);
            if round.is_settled() && !stale {
                break st.finish(round, Termination::Converged)?;
            }
            if st.folds == MAX_RESOLUTION_ITERATIONS {
                // A stale round is rebuilt from the frozen seats unfolded:
                // each refusal shrinks a frozen seat, so this ends.
                if stale {
                    continue;
                }
                break st.finish(round, Termination::CapReached(resolutions))?;
            }
            if !st.fold(&round, resolutions)? && !stale {
                break st.finish(round, Termination::Converged)?;
            }
        };
        if opts.compact {
            result.function.compact();
        }
        Ok(result)
    }
}

impl<'s> LoopState<'s> {
    fn new(seeds: &'s FxHashMap<PcodeInsnAddr, ResolvedTargets>) -> Self {
        Self {
            sites: SiteTable::new(seeds),
            derived_incomplete: Vec::new(),
            assumed_modes: Vec::new(),
            incomplete: Incompleteness::default(),
            folds: 0,
        }
    }

    /// Records `round`'s reports and freezes every site whose seat it
    /// refused. Returns whether it refused any, so `round` is stale.
    fn observe(&mut self, round: &Round) -> bool {
        let cfg = &round.cfg;
        // Before the refusals: a withdrawn seat asks for no arm, which would
        // hide the loss.
        self.derived_incomplete
            .extend(seated_arm_losses(cfg, &self.sites));
        let clashing: Vec<PcodeInsnAddr> = cfg.incomplete().of(Reason::IsaModeConflict).collect();
        let refused = refusals(
            cfg.undecodable_seeded_targets(),
            &clashing,
            &self.sites,
            round.anchors(),
        );
        self.incomplete.absorb(cfg.incomplete());
        let stale = !refused.is_empty();
        // Frozen, or the classifier re-derives the cut arms and cycles.
        for (site, keep) in refused {
            self.sites.freeze(site, keep);
            self.derived_incomplete.push(site);
        }
        stale
    }

    /// Folds `round`'s classifications into the seats, withdrawing a site on
    /// its second narrowing. Returns whether any site's seat changed.
    fn fold(&mut self, round: &Round, resolutions: Resolutions) -> Result<bool> {
        let progress = self.sites.fold(&round.flowing_anchors(), resolutions)?;
        self.derived_incomplete
            .extend(self.sites.note_narrowed(&progress.narrowed));
        let changed = progress.changed();
        self.derived_incomplete.extend(progress.derived_incomplete);
        self.assumed_modes.extend(progress.assumed_modes);
        self.folds += 1;
        Ok(changed)
    }

    /// The result over `round`, the last one built.
    fn finish(mut self, round: Round, end: Termination) -> Result<AnalyzeResult> {
        // The cap round's classifications fold into a copy, which only the
        // reports read: the published cfg is built from the seats as they
        // stand.
        let (still_growing, folded) = match end {
            Termination::Converged => (Vec::new(), None),
            Termination::CapReached(resolutions) => {
                let mut folded = self.sites.clone();
                let progress = folded.fold(&round.flowing_anchors(), resolutions)?;
                self.derived_incomplete.extend(progress.derived_incomplete);
                self.assumed_modes.extend(progress.assumed_modes);
                (progress.grew, Some(folded))
            }
        };
        let settled = folded.as_ref().unwrap_or(&self.sites);
        let Round {
            cfg,
            function,
            placeholders,
            switches,
            live,
            unclassified,
            unverified_returns: returns,
        } = round;

        let mut unresolved = live_unresolved_branches(
            &live,
            &placeholders,
            &switches,
            &unclassified,
            settled,
            DerivedChannels {
                budget_exhausted: &still_growing,
                incomplete_derived: &self.derived_incomplete,
            },
        );
        // A caller-seeded unproven return is `UnverifiedSeed`.
        let (seeded_returns, mut returns): (Vec<_>, Vec<_>) = returns
            .into_iter()
            .partition(|addr| caller_seeded_return(settled, *addr));
        unresolved.append(&mut returns);
        let mut unverified = unverified_seeded_sites(&cfg, &switches, &unclassified, settled);
        unverified.extend(seeded_returns);
        // A withdrawn site carries no guessed arm.
        unverified.extend(
            self.assumed_modes
                .into_iter()
                .filter(|addr| settled.is_seated(*addr)),
        );
        let mut incomplete = self.incomplete;
        for addr in unresolved {
            incomplete.insert(addr, Reason::UnresolvedBranch);
        }
        for addr in unverified {
            incomplete.insert(addr, Reason::UnverifiedSeed);
        }
        Ok(AnalyzeResult {
            cfg: cfg.with_incompleteness(incomplete),
            function,
        })
    }
}
