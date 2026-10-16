//! The resolve loop's state across rounds, and the result it settles on.

mod round;

use std::borrow::Cow;

use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{PcodeInsnAddr, ResolvedTargets, seated_in};

use crate::{
    AnalyzeResult, DerivedChannels, DispatchAnchors, IndirectResolutions, abandon_site,
    abandon_undecodable, apply_resolutions, flowing_isa_bits, live_unresolved_branches,
    seated_arm_losses, unverified_seeded_sites,
};
use round::Round;

/// Why the loop stopped.
pub(crate) enum Termination {
    /// The last round built is the answer.
    Converged,
    /// The cap ran out on a round whose classifications nothing has folded.
    CapReached(IndirectResolutions),
}

/// What the loop carries from one round to the next.
pub(crate) struct LoopState<'s> {
    /// The caller's `known_targets`; never mutated.
    caller_seed: &'s FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    /// The seats each round builds from: the caller's, grown by every fold,
    /// which re-unions the caller's own. Copy-on-write, so a function with
    /// nothing to resolve never copies a whole-binary seed table.
    pub(crate) working: Cow<'s, FxHashMap<PcodeInsnAddr, ResolvedTargets>>,
    /// Sites given up on: their answer never settled, or named an address
    /// that would not decode. Each takes no further classification and keeps
    /// a live placeholder, reported unresolved.
    abandoned: FxHashSet<PcodeInsnAddr>,
    /// Narrowings per site. Growth alone cannot cycle, so a cap spent on it is
    /// the depth limit; a second narrowing is the cycle, and abandons the site.
    narrow_rounds: FxHashMap<PcodeInsnAddr, u32>,
    /// Sticky: a site that lost ground in ANY round, or that the loop
    /// abandoned, is not claimed complete by a later round that re-derives the
    /// smaller set.
    derived_incomplete: Vec<PcodeInsnAddr>,
    /// Sticky: the flag fires on the round that seats the guessed arm, and
    /// every later round finds that arm already seated.
    assumed_modes: Vec<PcodeInsnAddr>,
    /// The four direct-edge reports, sticky: a round's inexact or stubbed edge
    /// fed the classifier, whose answers persist into every later round, so a
    /// final cfg without that edge does not mean none was used.
    interior: Vec<PcodeInsnAddr>,
    /// Sharper for this one: `abandon_undecodable` drops the site whose seat
    /// raised the clash, so the next round rebuilds without that edge.
    isa_conflicts: Vec<PcodeInsnAddr>,
    unmapped: Vec<PcodeInsnAddr>,
    undecodable: Vec<PcodeInsnAddr>,
    /// Rounds folded into `working`.
    pub(crate) folds: usize,
}

impl<'s> LoopState<'s> {
    pub(crate) fn new(caller_seed: &'s FxHashMap<PcodeInsnAddr, ResolvedTargets>) -> Self {
        Self {
            caller_seed,
            working: Cow::Borrowed(caller_seed),
            abandoned: FxHashSet::default(),
            narrow_rounds: FxHashMap::default(),
            derived_incomplete: Vec::new(),
            assumed_modes: Vec::new(),
            interior: Vec::new(),
            isa_conflicts: Vec::new(),
            unmapped: Vec::new(),
            undecodable: Vec::new(),
            folds: 0,
        }
    }

    /// Records `round`'s reports and abandons every site it found naming a
    /// bad target. Returns whether `round` still carries a seat `working` has
    /// since dropped: converging on it would publish a `Switch` at a site the
    /// reports call unresolved.
    pub(crate) fn observe(&mut self, round: &Round) -> bool {
        let cfg = &round.cfg;
        // BEFORE `abandon_undecodable`, which drops the site or its bad arms
        // from `working`, the map `seated_arm_losses` looks the site up in.
        // Reversed, every site abandoned this round loses its arm-loss report,
        // and for a classifier-derived table that is the only channel that
        // fires.
        self.derived_incomplete
            .extend(seated_arm_losses(cfg, &self.working));
        let dropped = abandon_undecodable(
            cfg.undecodable_seeded_targets(),
            cfg.isa_mode_conflicts(),
            &DispatchAnchors::new(cfg),
            &mut self.abandoned,
            &mut self.working,
        );
        self.interior
            .extend_from_slice(cfg.interior_branch_targets());
        self.isa_conflicts
            .extend_from_slice(cfg.isa_mode_conflicts());
        self.unmapped
            .extend_from_slice(cfg.unmapped_branch_targets());
        self.undecodable
            .extend_from_slice(cfg.undecodable_branch_targets());
        let stale = !dropped.is_empty();
        self.derived_incomplete.extend(dropped);
        stale
    }

    /// Folds `round`'s classifications into `working`, abandoning a site on
    /// its second narrowing. Returns whether any site's successors changed.
    ///
    /// # Errors
    ///
    /// As [`apply_resolutions`].
    pub(crate) fn fold(&mut self, round: &Round, resolutions: IndirectResolutions) -> Result<bool> {
        let flowing = flowing_isa_bits(&round.cfg, &round.placeholders, &round.switches);
        let progress = apply_resolutions(
            self.working.to_mut(),
            self.caller_seed,
            &round.placeholders,
            &round.switches,
            resolutions,
            &flowing,
            &self.abandoned,
        )?;
        for addr in &progress.narrowed {
            let seen = self.narrow_rounds.entry(*addr).or_default();
            *seen += 1;
            // `apply_resolutions` already reported this narrowing. A second
            // means the answer depends on what the previous round seated, so
            // no member of the cycle is trustworthy: stop resolving the site,
            // rather than failing the function.
            if *seen > 1 && !self.abandoned.contains(addr) {
                abandon_site(self.working.to_mut(), &mut self.abandoned, *addr);
                self.derived_incomplete.push(*addr);
            }
        }
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
    /// As [`apply_resolutions`].
    pub(crate) fn finish(mut self, round: Round, end: Termination) -> Result<AnalyzeResult> {
        let Round {
            cfg,
            function,
            placeholders,
            switches,
            live,
            unclassified,
            unverified_returns: mut returns,
        } = round;
        // The cap is spent on a round nothing has folded yet, so its own
        // verdict is only known by folding it. Against a COPY, with no
        // narrow-twice freeze: the returned `cfg` was built from `working` as
        // it stands, and this is a report, not a decision. Kept, where the
        // fold that produced it is discarded: it is the settled answer BOTH
        // seed-aware reports are read against, and reading one against the
        // pre-fold set would call a site that the last round grew past its
        // seed "exactly the seed".
        let (still_growing, folded) = match end {
            Termination::Converged => (Vec::new(), None),
            Termination::CapReached(resolutions) => {
                let mut folded = (*self.working).clone();
                let flowing = flowing_isa_bits(&cfg, &placeholders, &switches);
                let progress = apply_resolutions(
                    &mut folded,
                    self.caller_seed,
                    &placeholders,
                    &switches,
                    resolutions,
                    &flowing,
                    &self.abandoned,
                )?;
                self.derived_incomplete.extend(progress.derived_incomplete);
                self.assumed_modes.extend(progress.assumed_modes);
                // Only the LAST round's growth: a site that settled earlier is
                // not "still growing when the cap ran out".
                (progress.grew, Some(folded))
            }
        };
        let settled = folded.as_ref().unwrap_or(&self.working);

        let mut unresolved = live_unresolved_branches(
            &live,
            &placeholders,
            &switches,
            &unclassified,
            self.caller_seed,
            settled,
            DerivedChannels {
                budget_exhausted: &still_growing,
                incomplete_derived: &self.derived_incomplete,
            },
        );
        // A caller that seeded `return` asserted the answer, and
        // `unverified_seeded_sites` names that seat -- but only for a seat the
        // CFG build actually CONSUMED, which is what `link_register_seated`
        // records. A site whose terminator came from the p-code `RETURN` op is
        // never in it, so dropping one here on the strength of a seed left it
        // in none of the six channels and `is_complete()` true over a CFG
        // missing the edge control takes.
        // Hashed: both lists are bounded by the region count and at the sound
        // floor they can be the same set, so a slice scan per element is
        // quadratic.
        let seated_returns: FxHashSet<_> = cfg.link_register_seated().iter().copied().collect();
        returns.retain(|addr| {
            !seated_returns.contains(addr)
                || !matches!(
                    seated_in(self.caller_seed, *addr),
                    Some(ResolvedTargets::LinkRegister)
                )
        });
        unresolved.append(&mut returns);
        let mut unverified =
            unverified_seeded_sites(&cfg, &switches, &unclassified, self.caller_seed, settled);
        // Derived once from the final cfg, then extended with the rounds'
        // accumulated guesses, filtered to the ones still seated: a site whose
        // seat the loop dropped carries no guessed arm any more, and this
        // channel claims the answer is whole.
        unverified.extend(
            self.assumed_modes
                .into_iter()
                .filter(|addr| settled.contains_key(addr)),
        );
        Ok(AnalyzeResult {
            cfg,
            function,
            unresolved_indirect_branches: sorted(unresolved),
            isa_mode_conflicts: sorted(self.isa_conflicts),
            interior_branch_targets: sorted(self.interior),
            unmapped_branch_targets: sorted(self.unmapped),
            undecodable_branch_targets: sorted(self.undecodable),
            unverified_seeded_sites: sorted(unverified),
        })
    }
}

/// `v` sorted and deduplicated.
fn sorted(mut v: Vec<PcodeInsnAddr>) -> Vec<PcodeInsnAddr> {
    v.sort_unstable();
    v.dedup();
    v
}
