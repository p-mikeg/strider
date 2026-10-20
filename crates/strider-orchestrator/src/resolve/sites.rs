//! Each dispatch site's state across rounds, keyed by its anchor: the
//! `BranchIndirect`'s own p-code address.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow};
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{CfgOptions, PcodeInsnAddr, ResolvedTargets, seated_from, seated_in};
use strider_ir::node::NodeId;

use super::merge::{adopt_known_modes, drop_contradicted, merge_derived, union_with_seed};
use super::round::Anchors;
use super::visit;

/// One fold's effect on the induced edge set.
#[derive(Default)]
pub(super) struct Progress {
    /// Addresses whose successor set changed without losing ground, which
    /// includes a target taking a proved mode at an unchanged address set
    /// (`ResolvedTargets::narrows_to`).
    pub(super) grew: Vec<PcodeInsnAddr>,
    /// Addresses that LOST a successor, so the signal is not monotone there.
    pub(super) narrowed: Vec<PcodeInsnAddr>,
    /// Addresses whose one target turned into or out of a `Single`: out of
    /// range the cfg seats a `Single` as a tail call but defers a table's
    /// arm, so an unchanged successor set is not an unchanged seat.
    pub(super) reshaped: Vec<PcodeInsnAddr>,
    /// Addresses whose fold dropped a successor the CLASSIFIER proved this
    /// round, or whose answer stopped being a superset. A seed says nothing
    /// about what the classifier derives, so this list is never suppressed.
    /// A successor carried over from an earlier round needs no list of its
    /// own: dropping one is exactly what stops the answer being a superset.
    pub(super) derived_incomplete: Vec<PcodeInsnAddr>,
    /// Addresses that seated a NEW arm on a mode nothing proved for that arm.
    /// The arm set is whole, so this is an unverified answer rather than an
    /// incomplete one.
    pub(super) assumed_modes: Vec<PcodeInsnAddr>,
}

impl Progress {
    /// Some address's seat differs from the previous round's: the loop's
    /// progress signal.
    pub(super) fn changed(&self) -> bool {
        !self.grew.is_empty() || !self.narrowed.is_empty() || !self.reshaped.is_empty()
    }
}

/// The loop's answers, over the caller's.
///
/// A caller can only spell a machine address, so its seed may sit at the
/// instruction's p-code index 0 ([`seated_in`]). The loop keys each answer of
/// its own at the anchor, where the cfg's
/// [`CfgOptions::derived_targets`] lookup is exact, so withdrawing one anchor
/// leaves a sibling `BranchIndirect` of the same instruction its seed.
#[derive(Clone)]
pub(super) struct SiteTable<'s> {
    /// The caller's `known_targets`: never copied or edited.
    seeds: &'s FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    /// The loop's own answers, lent to the cfg as `derived_targets`.
    seats: FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    /// Sites whose answer lost ground once; a second loss withdraws them.
    narrowed_once: FxHashSet<PcodeInsnAddr>,
    /// Sites that take no further classification.
    frozen: FxHashSet<PcodeInsnAddr>,
}

impl<'s> SiteTable<'s> {
    pub(super) fn new(seeds: &'s FxHashMap<PcodeInsnAddr, ResolvedTargets>) -> Self {
        Self {
            seeds,
            seats: FxHashMap::default(),
            narrowed_once: FxHashSet::default(),
            frozen: FxHashSet::default(),
        }
    }

    /// What the cfg seats at `site`: the loop's answer, else the caller's.
    pub(super) fn seat(&self, site: PcodeInsnAddr) -> Option<&ResolvedTargets> {
        seated_from(&self.seats, self.seeds, site)
    }

    /// The caller's answer at `site`.
    pub(super) fn seed(&self, site: PcodeInsnAddr) -> Option<&'s ResolvedTargets> {
        seated_in(self.seeds, site)
    }

    /// Whether the loop holds an answer at `site` with an arm to seat. No
    /// seed fallback: a withdrawn site is not seated whatever its seed says.
    pub(super) fn is_seated(&self, site: PcodeInsnAddr) -> bool {
        self.seats
            .get(&site)
            .is_some_and(|t| !matches!(t, ResolvedTargets::Withdrawn))
    }

    /// The sites the loop holds an answer for.
    pub(super) fn seated_sites(&self) -> impl Iterator<Item = PcodeInsnAddr> + '_ {
        self.seats.keys().copied()
    }

    /// `base` with the loop's answers layered over its seeds.
    pub(super) fn cfg_options<'o>(&'o self, base: &CfgOptions<'o>) -> CfgOptions<'o> {
        // Cloned whole: naming the other fields would silently drop any field
        // added to `CfgOptions` later from the whole re-lift loop.
        CfgOptions {
            derived_targets: &self.seats,
            ..base.clone()
        }
    }

    /// Holds `keep` at `site` and stops classifying it.
    pub(super) fn freeze(&mut self, site: PcodeInsnAddr, keep: ResolvedTargets) {
        self.frozen.insert(site);
        self.seats.insert(site, keep);
    }

    /// Withdraws each site in `narrowed` losing ground for the second time,
    /// and returns those: a second loss means the answer depends on what the
    /// previous round seated, so no member of the cycle is trustworthy.
    pub(super) fn note_narrowed(&mut self, narrowed: &[PcodeInsnAddr]) -> Vec<PcodeInsnAddr> {
        let mut withdrawn_now = Vec::new();
        for &site in narrowed {
            if !self.narrowed_once.insert(site) && !self.frozen.contains(&site) {
                self.freeze(site, ResolvedTargets::Withdrawn);
                withdrawn_now.push(site);
            }
        }
        withdrawn_now
    }

    /// Folds a round's classifications into the seats, keyed back to their
    /// anchors.
    ///
    /// Within one round, two anchors can share one address: their target sets
    /// are UNIONED, not last-write-wins. Across rounds the staged set REPLACES
    /// the previous seat.
    ///
    /// # Errors
    ///
    /// A classified node that is no anchor of `placeholders` or `switches`
    /// (internal-consistency violation).
    pub(super) fn fold(
        &mut self,
        placeholders: &Anchors,
        switches: &Anchors,
        resolutions: impl IntoIterator<Item = (NodeId, ResolvedTargets)>,
        flowing_isa_bit: &FxHashMap<PcodeInsnAddr, bool>,
    ) -> Result<Progress> {
        let node_to_addr: FxHashMap<NodeId, PcodeInsnAddr> = placeholders
            .iter()
            .chain(switches)
            .map(|&(addr, node)| (node, addr))
            .collect();
        // In the resolve loop every staged address came from `node_to_addr`,
        // built from the same anchors `flowing_isa_bits` mapped, so the lookup
        // hits; the default is for a caller that folds without one (the unit
        // tests), where `false` is the base ISA mode rather than a third rule
        // for the bit.
        let flowing_at = |addr: PcodeInsnAddr| flowing_isa_bit.get(&addr).copied().unwrap_or(false);
        // Staged per address first, so same-address collisions merge before
        // anything touches the seats.
        let mut staged: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        // Every successor the round's classifications name, before any mode
        // filter: what the fold has to account for. `None` is the
        // `LinkRegister` successor, which `ResolvedTargets` cannot hold
        // alongside concrete targets, so a merge that drops it has to be
        // visible here.
        let mut reported: FxHashMap<PcodeInsnAddr, FxHashSet<Option<u64>>> = FxHashMap::default();
        // Every `(target, mode)` the classifications prove, per address: a
        // target proved in both modes is dropped however many anchors there
        // are, where merging them pairwise can forget the contradiction.
        let mut proved: FxHashMap<PcodeInsnAddr, FxHashSet<(u64, bool)>> = FxHashMap::default();
        for (node, targets) in resolutions {
            let addr = node_to_addr.get(&node).copied().ok_or_else(|| {
                anyhow!("classified IndirectBranch node {node:?} has no recorded pcode address")
            })?;
            // A frozen site takes no further classification: folding one back
            // in would re-enter the cycle that froze it.
            if self.frozen.contains(&addr) {
                continue;
            }
            let reported_at = reported.entry(addr).or_default();
            let proved_at = proved.entry(addr).or_default();
            for (target, bit) in visit(targets.successors()) {
                reported_at.insert(target);
                if let (Some(target), Some(bit)) = (target, bit) {
                    proved_at.insert((target, bit));
                }
            }
            staged
                .entry(addr)
                .and_modify(|e| *e = merge_derived(e, &targets, flowing_at(addr)))
                .or_insert(targets);
        }
        // Across rounds, this round's staged set REPLACES the previous round's
        // seat (within the round they merged above), so a set that narrows
        // from unseatable to seatable once other branches resolve is not
        // stranded; convergence rides on the per-address diff, so an unchanged
        // cone re-inserts an equal value and reads as no growth. Every
        // successor the round reported yet did not seat comes back in
        // `Progress::derived_incomplete`.
        let mut progress = Progress::default();
        for (addr, targets) in staged {
            let (targets, contradicted) = match proved.get(&addr) {
                Some(proved_at) => drop_contradicted(targets, proved_at),
                None => (targets, false),
            };
            let seat = self.seat(addr);
            // A re-derivation reports no mode where the dispatch commits none,
            // including at an address a caller seed gave a mode. Re-deriving
            // must widen the arm set, not re-decode the old arms in the mode
            // flowing into the branch.
            let (targets, assumed_mode) = adopt_known_modes(seat, targets, flowing_at(addr));
            // The caller's own seed is unioned in by ADDRESS every round, after
            // both mode filters and never subject to them: a seed carries a
            // mode only if the caller built one, and filtering it against a
            // mode-bearing classification would delete the whole caller answer.
            let targets = match self.seed(addr) {
                Some(seed) => union_with_seed(seed, &targets),
                None => targets,
            };
            let seated: FxHashSet<Option<u64>> = visit(targets.successors())
                .map(|(target, _bit)| target)
                .collect();
            let derived_dropped = contradicted
                || reported
                    .get(&addr)
                    .into_iter()
                    .flatten()
                    .any(|a| !seated.contains(a));
            let prev: BTreeSet<_> = seat
                .into_iter()
                .flat_map(|seat| visit(seat.successors()))
                .collect();
            let next: BTreeSet<_> = visit(targets.successors()).collect();
            let narrowed = seat.is_some_and(|seat| seat.narrows_to(&targets));
            if prev != next {
                if narrowed {
                    progress.narrowed.push(addr);
                } else {
                    progress.grew.push(addr);
                }
            } else if seat.is_some_and(|seat| is_single(seat) != is_single(&targets)) {
                // Settles in one round: in range both shapes seat the same
                // `Switch`, and out of range a `Single` is consumed as a tail
                // call, which nothing classifies again.
                progress.reshaped.push(addr);
            }
            if derived_dropped || narrowed {
                progress.derived_incomplete.push(addr);
            }
            // Decodable, and the arm set is whole: nothing evaluated THIS arm's
            // mode, which is `UnverifiedSeed`, not `UnresolvedBranch`.
            if assumed_mode {
                progress.assumed_modes.push(addr);
            }
            self.seats.insert(addr, targets);
        }
        Ok(progress)
    }
}

fn is_single(targets: &ResolvedTargets) -> bool {
    matches!(targets, ResolvedTargets::Single(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::test_support::{
        TARGET_VISITS, assert_linear_visits, fn_with_live_indirect_branch, mid_insn_addr, multiple,
        pcode_addr, successor_set,
    };

    /// The classifier's answers as the tests write them, `None` for a site it
    /// could not derive.
    type Answers = FxHashMap<NodeId, Option<ResolvedTargets>>;

    /// [`SiteTable::fold`] over `seats`, the loop's answers so far, skipping
    /// `frozen` sites; `seats` comes back folded.
    fn fold_into(
        seats: &mut FxHashMap<PcodeInsnAddr, ResolvedTargets>,
        seeds: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
        placeholders: &Anchors,
        switches: &Anchors,
        answers: Answers,
        flowing: &FxHashMap<PcodeInsnAddr, bool>,
        frozen: &FxHashSet<PcodeInsnAddr>,
    ) -> Result<Progress> {
        let mut sites = SiteTable {
            seeds,
            seats: std::mem::take(seats),
            narrowed_once: FxHashSet::default(),
            frozen: frozen.clone(),
        };
        let progress = sites.fold(
            placeholders,
            switches,
            answers
                .into_iter()
                .filter_map(|(node, answer)| Some((node, answer?))),
            flowing,
        );
        *seats = sites.seats;
        progress
    }

    /// What every lookup reads, for each way a site can hold an answer. The
    /// sibling column is a second `BranchIndirect` of the anchor's
    /// instruction: it keeps the caller's machine-start seed whatever the
    /// loop does at the anchor.
    #[test]
    fn every_lookup_reads_the_loops_answer_over_the_callers() {
        let start = pcode_addr(0x1000);
        let anchor = mid_insn_addr(0x1000);
        let sibling = PcodeInsnAddr {
            insn_index: 5,
            ..anchor
        };
        let seed = multiple(&[(0x4000, None), (0x6000, None)]);
        let fold = multiple(&[(0x5000, None)]);
        let kept = multiple(&[(0x4000, None)]);
        let none = ResolvedTargets::Withdrawn;
        let at_start: FxHashMap<_, _> = std::iter::once((start, seed.clone())).collect();
        let at_anchor: FxHashMap<_, _> = std::iter::once((anchor, seed.clone())).collect();
        // (case, seeds, the loop's answer at the anchor and whether frozen,
        //  seat at [anchor, start, sibling], is_seated(anchor))
        let rows = [
            (
                "a seed at the machine start",
                &at_start,
                None,
                [Some(&seed), Some(&seed), Some(&seed)],
                false,
            ),
            (
                "a seed at the exact key",
                &at_anchor,
                None,
                [Some(&seed), None, None],
                false,
            ),
            (
                "a fold over a machine-start seed",
                &at_start,
                Some((&fold, false)),
                [Some(&fold), Some(&seed), Some(&seed)],
                true,
            ),
            (
                "a site frozen with arms",
                &at_start,
                Some((&kept, true)),
                [Some(&kept), Some(&seed), Some(&seed)],
                true,
            ),
            (
                "a site withdrawn",
                &at_start,
                Some((&none, true)),
                [Some(&none), Some(&seed), Some(&seed)],
                false,
            ),
        ];
        for (case, seeds, held, want, seated) in rows {
            let mut sites = SiteTable::new(seeds);
            match held {
                Some((answer, true)) => sites.freeze(anchor, answer.clone()),
                Some((answer, false)) => {
                    sites.seats.insert(anchor, answer.clone());
                }
                None => {}
            }
            assert_eq!(
                [sites.seat(anchor), sites.seat(start), sites.seat(sibling)],
                want,
                "{case}"
            );
            assert_eq!(sites.is_seated(anchor), seated, "{case}");
        }
    }

    /// The cfg borrows the caller's tables and the loop's own, however many
    /// folds and freezes ran: the loop never copies the seeds or overrides.
    #[test]
    fn the_cfg_borrows_the_callers_table_after_folds_and_freezes() {
        let (_function, node) = fn_with_live_indirect_branch();
        let site = pcode_addr(0x1000);
        let seeds: FxHashMap<_, _> = (0..1000u64)
            .map(|i| {
                (
                    pcode_addr(0x10_0000 + 4 * i),
                    multiple(&[(0x20_0000 + 4 * i, None)]),
                )
            })
            .collect();
        let overrides = strider_target::call_other_abi::CallOtherOverrides::new(vec![(
            "trap".to_owned(),
            strider_target::call_other_abi::CallOtherClass::NO_RETURN.into(),
        )])
        .expect("unique override names");
        let base = CfgOptions {
            known_targets: &seeds,
            call_other_overrides: &overrides,
            ..CfgOptions::default()
        };
        let mut sites = SiteTable::new(&seeds);
        sites
            .fold(
                &vec![(site, node)],
                &Vec::new(),
                [(node, multiple(&[(0x2000, None)]))],
                &FxHashMap::default(),
            )
            .expect("fold");
        sites.freeze(site, ResolvedTargets::Withdrawn);
        let opts = sites.cfg_options(&base);
        assert!(std::ptr::eq(opts.known_targets, &seeds));
        assert!(std::ptr::eq(opts.derived_targets, &sites.seats));
        assert!(std::ptr::eq(opts.call_other_overrides, &overrides));
        assert_eq!(sites.seats.len(), 1, "the loop holds its own answers only");
    }

    /// Withdrawing one dispatch of a two-dispatch instruction leaves the other
    /// the caller's machine-start seed, whether or not the classifier answers
    /// it, and its own answer is unioned with that seed.
    #[test]
    fn withdrawing_one_dispatch_keeps_the_caller_seed_of_its_neighbour() {
        let (_function, node) = fn_with_live_indirect_branch();
        let first = mid_insn_addr(0x1000);
        let second = PcodeInsnAddr {
            insn_index: 5,
            ..first
        };
        let seeds: FxHashMap<_, _> =
            std::iter::once((pcode_addr(0x1000), multiple(&[(0x4000, None)]))).collect();
        let mut sites = SiteTable::new(&seeds);
        sites.freeze(first, ResolvedTargets::Withdrawn);
        assert_eq!(
            sites.seat(second),
            Some(&multiple(&[(0x4000, None)])),
            "a silent round leaves the neighbour its seed"
        );
        sites
            .fold(
                &vec![(second, node)],
                &Vec::new(),
                [(node, multiple(&[(0x5000, None)]))],
                &FxHashMap::default(),
            )
            .expect("fold");
        assert_eq!(
            successor_set(sites.seat(second).expect("seated")),
            successor_set(&multiple(&[(0x4000, None), (0x5000, None)])),
        );
    }

    /// The first narrowing is only reported; the second withdraws the site,
    /// once. Mutation: drop `&& !self.frozen.contains(&site)` from
    /// `note_narrowed`.
    #[test]
    fn a_second_narrowing_withdraws_the_site() {
        let site = pcode_addr(0x1000);
        let none = FxHashMap::default();
        let mut sites = SiteTable::new(&none);
        assert_eq!(sites.note_narrowed(&[site]), Vec::new());
        assert_eq!(sites.note_narrowed(&[site]), vec![site]);
        assert_eq!(sites.seat(site), Some(&ResolvedTargets::Withdrawn));
        assert!(!sites.is_seated(site));
        assert_eq!(sites.note_narrowed(&[site]), Vec::new(), "already frozen");
    }

    /// One fold at a `BRANCHIND` anchored mid-instruction whose previous
    /// mode-bearing answer is the loop's own, at the anchor, or the caller's,
    /// under the machine start.
    fn fold_mid_insn(caller_seeded: bool) -> (ResolvedTargets, Progress) {
        let (_function, node) = fn_with_live_indirect_branch();
        let anchor = mid_insn_addr(0x1000);
        let answer = multiple(&[(0x2000, Some(true))]);
        let seeds: FxHashMap<_, _> = if caller_seeded {
            std::iter::once((pcode_addr(0x1000), answer.clone())).collect()
        } else {
            FxHashMap::default()
        };
        let mut sites = SiteTable::new(&seeds);
        if !caller_seeded {
            sites.seats.insert(anchor, answer);
        }
        let progress = sites
            .fold(
                &Vec::new(),
                &vec![(anchor, node)],
                [(node, multiple(&[(0x2000, None), (0x3000, None)]))],
                &FxHashMap::default(),
            )
            .expect("fold");
        (sites.seat(anchor).expect("seated").clone(), progress)
    }

    /// A caller can only spell the machine address, so at a site whose
    /// `BRANCHIND` is not the instruction's first p-code op its mode-bearing
    /// seed sits under the machine start. Adoption must read it there: an
    /// exact-key miss adopts nothing, and the interworking arm the fold drops
    /// is seated mode-less with every report empty.
    #[test]
    fn fold_adopts_a_mode_the_caller_seeded_under_the_machine_start_key() {
        let (anchored, anchored_progress) = fold_mid_insn(false);
        let (seeded, seeded_progress) = fold_mid_insn(true);

        assert_eq!(
            successor_set(&anchored),
            successor_set(&multiple(&[(0x2000, Some(true))])),
            "the mode-less 0x3000 must not be seated at an interworking site",
        );
        assert_eq!(
            anchored_progress.derived_incomplete,
            vec![mid_insn_addr(0x1000)]
        );
        assert_eq!(successor_set(&seeded), successor_set(&anchored));
        assert_eq!(
            seeded_progress.derived_incomplete,
            anchored_progress.derived_incomplete
        );
        assert_eq!(
            seeded_progress.assumed_modes,
            anchored_progress.assumed_modes
        );
    }

    #[test]
    fn fold_skips_identical_reclassification_but_applies_improved() {
        // Pins convergence without dropping improvements. An identical
        // re-classification must report no growth (or an unchanged cone
        // churns to the iteration cap), while a narrowed set must still be
        // applied (or an unseatable site is stranded unresolved forever).
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: Anchors = vec![(addr, node)];

        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(
            addr,
            ResolvedTargets::table([0x2000, 0x3000].map(strider_cfg::ResolvedTarget::from)),
        );
        let mut same: Answers = FxHashMap::default();
        same.insert(
            node,
            Some(ResolvedTargets::table(
                [0x2000, 0x3000].map(strider_cfg::ResolvedTarget::from),
            )),
        );
        let progress = fold_into(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            same,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert!(
            !progress.changed(),
            "identical re-classification must not report growth"
        );
        assert_eq!(
            known[&addr],
            ResolvedTargets::table([0x2000, 0x3000].map(strider_cfg::ResolvedTarget::from))
        );

        let mut improved: Answers = FxHashMap::default();
        improved.insert(
            node,
            Some(ResolvedTargets::table(
                [0x2000, 0x2004].map(strider_cfg::ResolvedTarget::from),
            )),
        );
        let progress = fold_into(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            improved,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert!(
            progress.changed(),
            "an improved classification must be applied"
        );
        assert!(
            !progress.narrowed.is_empty(),
            "dropping 0x3000 for 0x2004 is not monotone discovery"
        );
        assert_eq!(
            known[&addr],
            ResolvedTargets::table([0x2000, 0x2004].map(strider_cfg::ResolvedTarget::from))
        );
    }

    #[test]
    fn fold_first_classification_grows() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: Anchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: Answers = FxHashMap::default();
        resolutions.insert(node, Some(ResolvedTargets::Single(0x2000.into())));
        let progress = fold_into(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert!(
            progress.changed(),
            "a first-time classification must register as growth"
        );
        assert_eq!(progress.grew, vec![addr]);
        assert!(progress.narrowed.is_empty());
        assert_eq!(known[&addr], ResolvedTargets::Single(0x2000.into()));
    }

    /// A seeded one-arm table and a derived `Single` of the same target name
    /// one successor, but the cfg defers the table out of range and seats the
    /// `Single` as a tail call, so the change of shape is progress.
    #[test]
    fn a_table_turning_single_at_one_target_is_progress() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let seeds: FxHashMap<_, _> = std::iter::once((addr, multiple(&[(0x9000, None)]))).collect();
        let mut sites = SiteTable::new(&seeds);
        let progress = sites
            .fold(
                &vec![(addr, node)],
                &Vec::new(),
                [(node, ResolvedTargets::Single(0x9000.into()))],
                &FxHashMap::default(),
            )
            .expect("fold");
        assert_eq!(
            sites.seat(addr),
            Some(&ResolvedTargets::Single(0x9000.into()))
        );
        assert!(progress.changed(), "the seat changed shape");
    }

    /// Scales ARMS at one site, where
    /// [`fold_does_not_go_quadratic_in_accumulated_targets`]
    /// scales addresses: a mode-less re-derivation of an interworking table
    /// checks every new arm against the seated set.
    #[test]
    fn fold_does_not_go_quadratic_in_arms_at_one_site() {
        use strider_cfg::ResolvedTarget;
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: Anchors = vec![(addr, node)];
        assert_linear_visits("folding seated arms against re-derived ones", |arms| {
            let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
            known.insert(
                addr,
                ResolvedTargets::table(
                    (0..arms).map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(true))),
                ),
            );
            let mut resolutions: Answers = FxHashMap::default();
            resolutions.insert(
                node,
                Some(ResolvedTargets::table(
                    (0..arms * 2).map(|i| ResolvedTarget::new(0x2_0000 + i * 4, None)),
                )),
            );
            let progress = fold_into(
                &mut known,
                &FxHashMap::default(),
                &anchors,
                &Vec::new(),
                resolutions,
                &FxHashMap::default(),
                &rustc_hash::FxHashSet::default(),
            )
            .expect("apply");
            assert_eq!(progress.derived_incomplete, vec![addr]);
        });
    }

    #[test]
    fn fold_merges_two_anchors_at_same_addr() {
        // Two distinct placeholders sharing one address must union, not let
        // the second insert overwrite the first. Any two `NodeId`s work as
        // anchor keys since `SiteTable::fold` never looks at node kind, so
        // the fixture reuses the function's `IntConst` as the second.
        use strider_ir::node::NodeKind;
        use strider_ir::{IRViewer, IRWalker};
        let (function, indirect) = fn_with_live_indirect_branch();
        let other = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)))
            .expect("const node");
        assert_ne!(indirect, other, "need two distinct NodeIds");
        let addr = pcode_addr(0x1000);
        let unresolved: Anchors = vec![(addr, indirect), (addr, other)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: Answers = FxHashMap::default();
        resolutions.insert(indirect, Some(ResolvedTargets::Single(0x2000.into())));
        resolutions.insert(other, Some(ResolvedTargets::Single(0x3000.into())));
        fold_into(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert_eq!(
            known[&addr],
            ResolvedTargets::table([0x2000, 0x3000].map(strider_cfg::ResolvedTarget::from)),
            "two same-addr classifications must be merged, not overwritten"
        );
    }

    /// `ResolvedTargets` cannot hold "returns AND jumps to X", so merging a
    /// `LinkRegister` classification with a concrete one at the same address
    /// loses the return successor. It must not read as clean growth.
    #[test]
    fn fold_reports_a_dropped_link_register_successor() {
        use strider_ir::node::NodeKind;
        use strider_ir::{IRViewer, IRWalker};
        let (function, indirect) = fn_with_live_indirect_branch();
        let other = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)))
            .expect("const node");
        let addr = pcode_addr(0x1000);
        let unresolved: Anchors = vec![(addr, indirect), (addr, other)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: Answers = FxHashMap::default();
        resolutions.insert(indirect, Some(ResolvedTargets::LinkRegister));
        resolutions.insert(other, Some(ResolvedTargets::Single(0x2000.into())));
        let progress = fold_into(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert_eq!(
            known[&addr],
            ResolvedTargets::Single(0x2000.into()),
            "the concrete side is what the cfg can seat"
        );
        assert_eq!(
            progress.derived_incomplete,
            vec![addr],
            "the return successor vanished from the seated answer unreported"
        );
    }

    /// Merging one classification must not touch the targets already
    /// accumulated for every other address.
    #[test]
    fn fold_does_not_go_quadratic_in_accumulated_targets() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: Anchors = vec![(addr, node)];
        let visits = |accumulated: u64| {
            let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = (0..accumulated)
                .map(|i| {
                    (
                        pcode_addr(0x10_0000 + i * 4),
                        ResolvedTargets::table(vec![
                            (0x20_0000 + i * 8).into(),
                            (0x20_0004 + i * 8).into(),
                        ]),
                    )
                })
                .collect();
            let mut resolutions: Answers = FxHashMap::default();
            resolutions.insert(
                node,
                Some(ResolvedTargets::table(vec![0x2000.into(), 0x2004.into()])),
            );
            TARGET_VISITS.with(|v| v.set(0));
            let progress = fold_into(
                &mut known,
                &FxHashMap::default(),
                &unresolved,
                &Vec::new(),
                resolutions,
                &FxHashMap::default(),
                &rustc_hash::FxHashSet::default(),
            )
            .expect("apply");
            assert!(progress.changed());
            TARGET_VISITS.with(std::cell::Cell::get)
        };
        let alone = visits(1);
        assert!(alone > 0, "the merge reads its own targets");
        assert_eq!(
            visits(200_000),
            alone,
            "merging one resolution reads targets of other addresses",
        );
    }

    /// Round 1 seats an interworking table with a proven per-target mode.
    /// Round 2 re-derives the now-seated `Switch` with no mode to evaluate, so
    /// every target comes back mode-less. The seated modes must
    /// survive the fold, or the arms decode in the superseded ISA, and an
    /// address only the mode-less side knows must not be seated: the cfg would
    /// decode it in the mode flowing into the branch, the same guess the
    /// within-round merge refuses to make.
    #[test]
    fn fold_keeps_a_seated_mode_a_rederivation_does_not_report() {
        use strider_cfg::ResolvedTarget;
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: Anchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(
            addr,
            ResolvedTargets::table(vec![
                ResolvedTarget::new(0x2000, Some(true)),
                ResolvedTarget::new(0x2004, Some(true)),
            ]),
        );
        let mut rederived: Answers = FxHashMap::default();
        rederived.insert(
            node,
            Some(ResolvedTargets::table(vec![
                ResolvedTarget::new(0x2000, None),
                ResolvedTarget::new(0x2004, None),
                ResolvedTarget::new(0x2008, None),
            ])),
        );
        fold_into(
            &mut known,
            &FxHashMap::default(),
            &Vec::new(),
            &anchors,
            rederived,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert_eq!(
            ResolvedTargets::arms(&known[&addr]),
            &[
                ResolvedTarget::new(0x2000, Some(true)),
                ResolvedTarget::new(0x2004, Some(true)),
            ],
            "a mode-less re-derivation must adopt the modes already seated, and \
             must not seat 0x2008 with an inherited mode",
        );
    }

    /// The drop is scoped to sites with a proved mode: a table nothing ever
    /// proved a mode for still widens on re-derivation.
    #[test]
    fn fold_widens_a_mode_less_site_on_rederivation() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, None)])),
            multiple(&[(0x2000, None), (0x2004, None)]),
            None,
        );
        assert!(progress.changed());
        assert_eq!(folded, multiple(&[(0x2000, None), (0x2004, None)]));
    }

    /// A proved mode EQUAL to the flowing one seats identically to no mode at
    /// all, so the set still widens: the drop is for sites that switch ISA.
    ///
    /// The site is still reported: 0x2004's own mode was never evaluated, and
    /// only the OTHER arms proved the one it is decoded in, so seating it stays
    /// an assumption. Dropping the arm instead would cost every same-mode
    /// dispatch its widening; reporting keeps the arm and refuses to call the
    /// answer settled.
    #[test]
    fn fold_widens_a_site_whose_proved_mode_is_the_flowing_one() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, Some(false))])),
            multiple(&[(0x2000, None), (0x2004, None)]),
            None,
        );
        assert!(progress.changed());
        assert_eq!(folded, multiple(&[(0x2000, Some(false)), (0x2004, None)]));
        assert!(
            progress.derived_incomplete.is_empty(),
            "nothing was dropped and nothing narrowed"
        );
        assert_eq!(
            progress.assumed_modes,
            vec![pcode_addr(0x1000)],
            "an arm seated on a mode nothing proved for it must be reported"
        );
    }

    /// The same site, re-derived with nothing new, assumes nothing and so
    /// reports nothing: the flag is for arms the site had not already seated.
    #[test]
    fn fold_reports_nothing_when_no_new_arm_is_assumed() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, Some(false)), (0x2004, Some(false))])),
            multiple(&[(0x2000, None), (0x2004, None)]),
            None,
        );
        assert_eq!(
            folded,
            multiple(&[(0x2000, Some(false)), (0x2004, Some(false))])
        );
        assert!(progress.assumed_modes.is_empty());
    }

    /// An address the seated set already holds without a proved mode stays;
    /// only addresses NEW to a mode-bearing site are dropped.
    #[test]
    fn fold_keeps_a_known_mode_less_arm_but_drops_a_new_one() {
        let (_progress, folded) = round(
            Some(multiple(&[(0x2000, Some(true)), (0x3000, None)])),
            multiple(&[(0x2000, None), (0x3000, None), (0x4000, None)]),
            None,
        );
        assert_eq!(folded, multiple(&[(0x2000, Some(true)), (0x3000, None)]));
    }

    /// The caller's seed is mode-less by construction, so a classification that
    /// proves a mode must not delete it.
    #[test]
    fn fold_keeps_the_caller_seed_against_a_mode_bearing_answer() {
        use strider_cfg::ResolvedTarget;
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: Anchors = vec![(addr, node)];
        let mut seed: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        seed.insert(
            addr,
            ResolvedTargets::table(vec![
                ResolvedTarget::new(0x2000, None),
                ResolvedTarget::new(0x3000, None),
            ]),
        );
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: Answers = FxHashMap::default();
        resolutions.insert(
            node,
            Some(ResolvedTargets::Single(ResolvedTarget::new(
                0x4000,
                Some(true),
            ))),
        );
        fold_into(
            &mut known,
            &seed,
            &anchors,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert_eq!(
            ResolvedTargets::arms(&known[&addr]),
            &[
                ResolvedTarget::new(0x2000, None),
                ResolvedTarget::new(0x3000, None),
                ResolvedTarget::new(0x4000, Some(true)),
            ],
            "seeding may only ADD edges, whatever the classifier proves about the mode",
        );
    }

    /// One [`SiteTable::fold`] round at one address: `known` holding `prev`,
    /// the classifier reporting `next`, no caller seed unless `seed`.
    fn round(
        prev: Option<ResolvedTargets>,
        next: ResolvedTargets,
        seed: Option<ResolvedTargets>,
    ) -> (Progress, ResolvedTargets) {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: Anchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        if let Some(prev) = prev {
            known.insert(addr, prev);
        }
        let mut seeds: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        if let Some(seed) = seed {
            seeds.insert(addr, seed);
        }
        let mut resolutions: Answers = FxHashMap::default();
        resolutions.insert(node, Some(next));
        let progress = fold_into(
            &mut known,
            &seeds,
            &anchors,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        (progress, known.remove(&addr).expect("folded"))
    }

    /// A one-arm table answer keeps its shape through a merge: the cfg seats an
    /// out-of-range `Single` as a tail call but defers an out-of-range arm, so
    /// whether a seed or a known mode passed through must not decide which.
    #[test]
    fn fold_keeps_a_one_arm_multiple_a_multiple() {
        let (_progress, seeded) = round(
            None,
            multiple(&[(0x2000, None)]),
            Some(multiple(&[(0x2000, None)])),
        );
        assert_eq!(
            seeded,
            multiple(&[(0x2000, None)]),
            "through the caller-seed union"
        );
        let (_progress, moded) = round(
            Some(multiple(&[(0x2000, Some(true))])),
            multiple(&[(0x2000, None)]),
            None,
        );
        assert_eq!(
            moded,
            multiple(&[(0x2000, Some(true))]),
            "through a known mode"
        );
    }

    #[test]
    fn fold_a_wider_address_set_grows() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, None)])),
            multiple(&[(0x2000, None), (0x3000, None)]),
            None,
        );
        assert!(progress.changed());
        assert!(
            progress.narrowed.is_empty(),
            "adding an address is discovery"
        );
        assert_eq!(progress.grew, vec![pcode_addr(0x1000)]);
        assert_eq!(folded, multiple(&[(0x2000, None), (0x3000, None)]));
    }

    /// The shrunken fold is applied, so the smaller set is what the next CFG is
    /// built from and the round after it reads as converged.
    #[test]
    fn fold_a_lost_address_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, None), (0x3000, None)])),
            multiple(&[(0x2000, None)]),
            None,
        );
        assert!(progress.changed());
        assert!(
            !progress.narrowed.is_empty(),
            "losing 0x3000 is not monotone discovery"
        );
        assert!(progress.grew.is_empty());
        assert_eq!(progress.derived_incomplete, vec![pcode_addr(0x1000)]);
    }

    /// A caller seed is mode-less by construction, so the round that PROVES an
    /// interworking mode for a seeded target changes its key. Refining one
    /// successor is not losing one, and reading it as a loss would withdraw a
    /// seeded trampoline chain that was merely refining.
    #[test]
    fn fold_proving_a_mode_for_a_seeded_target_grows() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, None)])),
            ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true))),
            Some(multiple(&[(0x2000, None)])),
        );
        assert!(progress.changed());
        assert!(
            progress.narrowed.is_empty(),
            "a None -> Some mode upgrade at a fixed address set is growth",
        );
        assert_eq!(
            folded,
            ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true))),
        );
    }

    /// Two rounds proving OPPOSITE modes for one address decode it two
    /// different ways, so the induced edge set is not converging.
    #[test]
    fn fold_a_mode_flip_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, Some(true))])),
            multiple(&[(0x2000, Some(false))]),
            None,
        );
        assert!(progress.changed());
        assert!(
            !progress.narrowed.is_empty(),
            "Thumb -> ARM at one address is a flip"
        );
        assert!(progress.grew.is_empty());
    }

    #[test]
    fn fold_gaining_and_losing_in_one_round_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, None), (0x3000, None)])),
            multiple(&[(0x2000, None), (0x4000, None)]),
            None,
        );
        assert!(progress.changed());
        assert!(!progress.narrowed.is_empty());
        assert!(progress.grew.is_empty());
    }

    /// Round 1 seats one interworking arm before the dispatch loop closes.
    /// Round 2 re-derives the now-seated `Switch` with no mode to evaluate, so
    /// it reports every arm mode-less, and the three NEW arms cannot be seated
    /// in a mode. The fold then equals the seated set, so the loop
    /// converges here: the site has to come back through `incomplete` or the
    /// caller reads a one-arm table alongside an empty `unresolved`.
    #[test]
    fn fold_reports_a_site_whose_widening_was_dropped() {
        let (progress, folded) = round(
            Some(multiple(&[(0x1030, Some(true))])),
            multiple(&[
                (0x1030, None),
                (0x1040, None),
                (0x1050, None),
                (0x1060, None),
            ]),
            None,
        );
        assert!(
            !progress.changed(),
            "the fold matches the seated set, so this round converges",
        );
        assert_eq!(
            ResolvedTargets::arms(&folded),
            &[strider_cfg::ResolvedTarget::new(0x1030, Some(true))],
        );
        assert_eq!(
            progress.derived_incomplete,
            vec![pcode_addr(0x1000)],
            "a dropped widening must be reported, not silently seated",
        );
    }

    /// The interworking test reads the flowing bit AT THE SITE, so two sites in
    /// one function can disagree about it: the cfg decodes a mode-less target in
    /// the mode at its own branch, not in the entry's.
    #[test]
    fn the_flowing_mode_is_read_per_site() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: Anchors = vec![(addr, node)];
        // The site already proved Thumb at 0x2000; the round re-derives it
        // mode-less and adds 0x3000.
        let seated = multiple(&[(0x2000, Some(true))]);
        let rederived = multiple(&[(0x2000, None), (0x3000, None)]);

        let fold = |flowing: bool| {
            let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
            known.insert(addr, seated.clone());
            let mut resolutions: Answers = FxHashMap::default();
            resolutions.insert(node, Some(rederived.clone()));
            let mut flowing_map: FxHashMap<PcodeInsnAddr, bool> = FxHashMap::default();
            flowing_map.insert(addr, flowing);
            fold_into(
                &mut known,
                &FxHashMap::default(),
                &anchors,
                &Vec::new(),
                resolutions,
                &flowing_map,
                &rustc_hash::FxHashSet::default(),
            )
            .expect("apply");
            ResolvedTargets::arms(&known[&addr]).len()
        };

        assert_eq!(
            fold(true),
            2,
            "Thumb flowing in: the proved mode matches, so the new arm is kept",
        );
        assert_eq!(
            fold(false),
            1,
            "ARM flowing in: the site interworks, so a mode-less new arm is a guess",
        );
    }

    #[test]
    fn fold_equal_sized_oscillation_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, None)])),
            multiple(&[(0x3000, None)]),
            None,
        );
        assert!(progress.changed());
        assert!(
            !progress.narrowed.is_empty(),
            "swapping one successor for another is an oscillation, not growth",
        );
    }

    /// One fold of `answers`, each on its own anchor at one address over
    /// `seed`, once per order of `answers` over the anchors' node ids: the
    /// merge order is the resolutions' hash order, which must not decide it.
    fn fold_anchors_every_order(
        answers: &[ResolvedTargets],
        seed: Option<ResolvedTargets>,
    ) -> Vec<(ResolvedTargets, Vec<PcodeInsnAddr>)> {
        let addr = pcode_addr(0x1000);
        let seeds: FxHashMap<_, _> = seed.into_iter().map(|s| (addr, s)).collect();
        let nodes: Vec<NodeId> = (0..answers.len() as u32).map(NodeId::from_u32).collect();
        let anchors: Anchors = nodes.iter().map(|&n| (addr, n)).collect();
        let mut out = Vec::new();
        for shift in 0..answers.len() {
            for reversed in [false, true] {
                let mut order: Vec<&ResolvedTargets> = answers.iter().collect();
                order.rotate_left(shift);
                if reversed {
                    order.reverse();
                }
                let resolutions: Answers = nodes
                    .iter()
                    .zip(order)
                    .map(|(&n, a)| (n, Some(a.clone())))
                    .collect();
                let mut known = FxHashMap::default();
                let progress = fold_into(
                    &mut known,
                    &seeds,
                    &anchors,
                    &Vec::new(),
                    resolutions,
                    &FxHashMap::default(),
                    &FxHashSet::default(),
                )
                .expect("fold");
                out.push((
                    known.remove(&addr).expect("folded"),
                    progress.derived_incomplete,
                ));
            }
        }
        out
    }

    /// A target two of three anchors prove in opposite modes stays dropped,
    /// whichever pair merges first.
    #[test]
    fn a_contradiction_across_three_anchors_is_not_forgotten() {
        let addr = pcode_addr(0x1000);
        let answers = [
            multiple(&[(0x2000, Some(true)), (0x3000, Some(true))]),
            multiple(&[(0x2000, Some(false))]),
            multiple(&[(0x2000, Some(true))]),
        ];
        for (seat, reported) in fold_anchors_every_order(&answers, None) {
            assert_eq!(seat, multiple(&[(0x3000, Some(true))]));
            assert_eq!(reported, vec![addr]);
        }
    }

    /// A seed naming a contradicted target seats it again, but the site is
    /// reported as it is without the seed.
    #[test]
    fn a_contradiction_a_seed_names_is_still_reported() {
        let addr = pcode_addr(0x1000);
        let seed = multiple(&[(0x2000, None)]);
        let answers = [
            ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true))),
            ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(false))),
        ];
        for (seat, reported) in fold_anchors_every_order(&answers, Some(seed.clone())) {
            assert_eq!(seat, seed, "a seed only adds");
            assert_eq!(reported, vec![addr]);
        }
    }

    /// A frozen site takes no further classification, so the arms
    /// `refusals` cut cannot be folded back in.
    #[test]
    fn a_withdrawn_site_folds_nothing() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: Anchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(addr, multiple(&[(0x2000, None)]));
        let mut resolutions: Answers = FxHashMap::default();
        resolutions.insert(node, Some(multiple(&[(0x2000, None), (0x3000, None)])));
        let withdrawn: rustc_hash::FxHashSet<PcodeInsnAddr> = std::iter::once(addr).collect();

        let progress = fold_into(
            &mut known,
            &FxHashMap::default(),
            &anchors,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &withdrawn,
        )
        .expect("fold");

        assert!(!progress.changed());
        assert_eq!(
            successor_set(&known[&addr]),
            successor_set(&multiple(&[(0x2000, None)])),
        );
    }
}
