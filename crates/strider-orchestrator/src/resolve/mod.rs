//! The resolve loop: every round rebuilds the function from one seat set,
//! until the seats stop changing.

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
use round::{Resolutions, Round, flowing_isa_bits};
use sites::SiteTable;

/// Caps the folds, so a run builds at most one more round than this.
///
/// Doubles as a DEPTH limit: one iteration seats one LEVEL of indirect-branch
/// discovery, so a chain of trampolines each jumping to the next needs one
/// round per link. [`SiteTable::fold`] reporting no change is the terminator
/// for everything shallower. Exhausting the cap is never an `Err`: the sites
/// still growing are reported, and a site whose answer never settles is
/// withdrawn and reported the same way.
const MAX_RESOLUTION_ITERATIONS: usize = 256;

/// `items`, counting each read in tests: the unit the fold's cost bounds are
/// stated in. Every target read in the fold must go through it, or the cost
/// tests cannot see that read.
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
    /// Sticky: a site that lost ground in ANY round, or whose seat the loop
    /// withdrew or trimmed, is not claimed complete by a later round that
    /// re-derives the smaller set.
    derived_incomplete: Vec<PcodeInsnAddr>,
    /// Sticky: the flag fires on the round that seats the guessed arm, and
    /// every later round finds that arm already seated.
    assumed_modes: Vec<PcodeInsnAddr>,
    /// Every round's target reasons, sticky: a round's inexact or stubbed
    /// edge fed the classifier, whose answers persist into every later round,
    /// so a final cfg without that edge does not mean none was used.
    incomplete: Incompleteness,
    /// Rounds folded into `sites`.
    folds: usize,
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
    /// The resolve loop behind [`Strider::analyze`], before compaction.
    ///
    /// # Errors
    ///
    /// As [`Self::build_round`] and [`SiteTable::fold`].
    pub(crate) fn resolve(
        &mut self,
        start_addr: MachineInsnAddr,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        pipeline: &strider_opt::OptimizerPipeline,
        classify: Classifier<'_>,
    ) -> Result<AnalyzeResult> {
        let mut st = LoopState::new(opts.cfg.known_targets);
        // Every round rebuilds the whole function from the seat set: cfg, lift
        // and pipeline. Carrying a round's work forward would make the cfg a
        // function of more than the seats, which is what convergence rests on.
        loop {
            let (round, resolutions) = self.build_round(
                start_addr,
                cc,
                opts,
                &st.sites.cfg_options(&opts.cfg),
                pipeline,
                classify,
            )?;
            // A stale round still carries a seat the loop has since withdrawn
            // or trimmed, so it is no answer even when nothing else moves.
            let stale = st.observe(&round);
            if round.is_settled() && !stale {
                return st.finish(round, Termination::Converged);
            }
            if st.folds == MAX_RESOLUTION_ITERATIONS {
                return st.finish(round, Termination::CapReached(resolutions));
            }
            if !st.fold(&round, resolutions)? && !stale {
                return st.finish(round, Termination::Converged);
            }
        }
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
    /// refused. Returns whether `round` still carries a seat the loop has
    /// since changed: converging on it would publish a `Switch` at a site the
    /// reports call unresolved.
    fn observe(&mut self, round: &Round) -> bool {
        let cfg = &round.cfg;
        // BEFORE the refusals land: a withdrawn seat asks for no arm, so the
        // loss would vanish, and for a classifier-derived table it is the
        // only report the site gets.
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
        // Frozen: the classifier re-derives the cut arms every round, so an
        // open site would re-seat them and cycle until the iteration cap.
        for (site, keep) in refused {
            self.sites.freeze(site, keep);
            self.derived_incomplete.push(site);
        }
        stale
    }

    /// Folds `round`'s classifications into the seats, withdrawing a site on
    /// its second narrowing. Returns whether any site's seat changed.
    ///
    /// # Errors
    ///
    /// As [`SiteTable::fold`].
    fn fold(&mut self, round: &Round, resolutions: Resolutions) -> Result<bool> {
        let flowing = flowing_isa_bits(&round.cfg, &round.placeholders, &round.switches);
        let progress =
            self.sites
                .fold(&round.placeholders, &round.switches, resolutions, &flowing)?;
        self.derived_incomplete
            .extend(self.sites.note_narrowed(&progress.narrowed));
        let changed = progress.changed();
        self.derived_incomplete.extend(progress.derived_incomplete);
        self.assumed_modes.extend(progress.assumed_modes);
        self.folds += 1;
        Ok(changed)
    }

    /// The result over `round`, the last one built.
    ///
    /// # Errors
    ///
    /// As [`SiteTable::fold`].
    fn finish(mut self, round: Round, end: Termination) -> Result<AnalyzeResult> {
        let Round {
            cfg,
            function,
            placeholders,
            switches,
            live,
            unclassified,
            unverified_returns: returns,
        } = round;
        // The cap is spent on a round nothing has folded yet, so its own
        // verdict is only known by folding it. Into a COPY of the loop's own
        // answers (the seeds are borrowed, never copied), with no narrow-twice
        // freeze: the returned `cfg` was built from the seats as they stand,
        // and this is a report, not a decision. Kept, where the fold that
        // produced it is discarded: it is the settled answer BOTH seed-aware
        // reports are read against, and reading one against the pre-fold set
        // would call a site that the last round grew past its seed "exactly the
        // seed".
        let (still_growing, folded) = match end {
            Termination::Converged => (Vec::new(), None),
            Termination::CapReached(resolutions) => {
                let mut folded = self.sites.clone();
                let flowing = flowing_isa_bits(&cfg, &placeholders, &switches);
                let progress = folded.fold(&placeholders, &switches, resolutions, &flowing)?;
                self.derived_incomplete.extend(progress.derived_incomplete);
                self.assumed_modes.extend(progress.assumed_modes);
                // Only the LAST round's growth: a site that settled earlier is
                // not "still growing when the cap ran out".
                (progress.grew, Some(folded))
            }
        };
        let settled = folded.as_ref().unwrap_or(&self.sites);

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
        // A caller that seeded `return` asserted the answer the IR could not
        // prove, whether the CFG seated it at a `BranchIndirect` or a p-code
        // `RETURN` op ended the region: `UnverifiedSeed`, not unresolved.
        let (seeded_returns, mut returns): (Vec<_>, Vec<_>) = returns
            .into_iter()
            .partition(|addr| caller_seeded_return(settled, *addr));
        unresolved.append(&mut returns);
        let mut unverified = unverified_seeded_sites(&cfg, &switches, &unclassified, settled);
        unverified.extend(seeded_returns);
        // Derived once from the final cfg, then extended with the rounds'
        // accumulated guesses, filtered to the ones still seated: a site whose
        // seat the loop withdrew carries no guessed arm any more, and
        // `UnverifiedSeed` claims the answer is whole.
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
