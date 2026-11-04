//! Each dispatch site's state across rounds, keyed by its anchor: the
//! `BranchIndirect`'s own p-code address.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow};
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{CfgOptions, PcodeInsnAddr, ResolvedTargets, seated_from, seated_in};
use strider_ir::node::NodeId;

use super::merge::{adopt_known_modes, drop_contradicted, merge_derived, union_with_seed};
use super::round::FlowingAnchor;
use super::visit;

/// One fold's effect on the induced edge set.
#[derive(Default)]
pub(super) struct Progress {
    /// Addresses whose successor set changed without losing ground, a target
    /// gaining a proved mode included.
    pub(super) grew: Vec<PcodeInsnAddr>,
    /// Addresses that lost a successor or flipped a mode.
    pub(super) narrowed: Vec<PcodeInsnAddr>,
    /// Some address's one target turned into or out of a `Single`, which the
    /// cfg seats differently out of range.
    pub(super) reshaped: bool,
    /// Addresses whose fold dropped a successor the classifier proved this
    /// round, or whose answer stopped being a superset. No seed suppresses
    /// these.
    pub(super) derived_incomplete: Vec<PcodeInsnAddr>,
    /// Addresses that seated a new arm in a mode nothing proved for it: an
    /// unverified answer, not an incomplete one.
    pub(super) assumed_modes: Vec<PcodeInsnAddr>,
}

impl Progress {
    /// Some address's seat differs from the previous round's.
    pub(super) fn changed(&self) -> bool {
        !self.grew.is_empty() || !self.narrowed.is_empty() || self.reshaped
    }
}

/// One address's classifications in one round, before any touches the seats.
struct Staged {
    /// The anchors' answers, merged.
    targets: ResolvedTargets,
    /// The ISA-mode bit flowing into the branch.
    flowing: bool,
    /// Every successor the classifications name, before any mode filter.
    /// `None` is the `LinkRegister` successor, which a merge with targets
    /// drops.
    reported: FxHashSet<Option<u64>>,
    /// Every `(target, mode)` the classifications prove, so a contradiction
    /// survives any number of pairwise merges.
    proved: FxHashSet<(u64, bool)>,
}

impl Staged {
    fn new(flowing: bool) -> Self {
        Self {
            targets: ResolvedTargets::Withdrawn,
            flowing,
            reported: FxHashSet::default(),
            proved: FxHashSet::default(),
        }
    }

    /// Unions one anchor's answer in.
    fn add(&mut self, targets: ResolvedTargets) {
        for (target, bit) in visit(targets.successors()) {
            self.reported.insert(target);
            if let (Some(target), Some(bit)) = (target, bit) {
                self.proved.insert((target, bit));
            }
        }
        self.targets = match &self.targets {
            ResolvedTargets::Withdrawn => targets,
            prev => merge_derived(prev, &targets, self.flowing),
        };
    }
}

/// The loop's answers, over the caller's.
///
/// A seed may sit at the instruction's machine start ([`seated_in`]); the
/// loop keys its own answers at the anchor, so withdrawing one anchor leaves
/// a sibling `BranchIndirect` its seed.
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

    /// Whether the loop holds an answer at `site` with an arm to seat,
    /// ignoring seeds.
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

    /// Withdraws and returns each site in `narrowed` losing ground for the
    /// second time.
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
    /// anchors. Anchors sharing an address union within a round; across
    /// rounds the new set replaces the seat.
    ///
    /// # Errors
    ///
    /// A classified node that is no anchor (internal-consistency violation).
    pub(super) fn fold(
        &mut self,
        anchors: &[FlowingAnchor],
        resolutions: impl IntoIterator<Item = (NodeId, ResolvedTargets)>,
    ) -> Result<Progress> {
        let mut progress = Progress::default();
        for (addr, staged) in self.stage(anchors, resolutions)? {
            self.apply(addr, staged, &mut progress);
        }
        Ok(progress)
    }

    /// The round's classifications per address. A frozen site takes none.
    fn stage(
        &self,
        anchors: &[FlowingAnchor],
        resolutions: impl IntoIterator<Item = (NodeId, ResolvedTargets)>,
    ) -> Result<FxHashMap<PcodeInsnAddr, Staged>> {
        let by_node: FxHashMap<NodeId, (PcodeInsnAddr, bool)> = anchors
            .iter()
            .map(|&(addr, node, flowing)| (node, (addr, flowing)))
            .collect();
        let mut staged: FxHashMap<PcodeInsnAddr, Staged> = FxHashMap::default();
        for (node, targets) in resolutions {
            let &(addr, flowing) = by_node.get(&node).ok_or_else(|| {
                anyhow!("classified IndirectBranch node {node:?} has no recorded pcode address")
            })?;
            if self.frozen.contains(&addr) {
                continue;
            }
            staged
                .entry(addr)
                .or_insert_with(|| Staged::new(flowing))
                .add(targets);
        }
        Ok(staged)
    }

    /// Seats `staged` at `addr`, recording in `progress` how the seat moved
    /// and every successor the round reported yet did not seat.
    fn apply(&mut self, addr: PcodeInsnAddr, staged: Staged, progress: &mut Progress) {
        let Staged {
            targets,
            flowing,
            reported,
            proved,
        } = staged;
        let (targets, contradicted) = drop_contradicted(targets, &proved);
        let seat = self.seat(addr);
        // A re-derivation reports no mode; it must not re-decode old arms.
        let (targets, assumed_mode) = adopt_known_modes(seat, targets, flowing);
        // The seed is unioned in after the mode filters, never subject to
        // them.
        let targets = match self.seed(addr) {
            Some(seed) => union_with_seed(seed, &targets),
            None => targets,
        };
        let seated: FxHashSet<Option<u64>> = visit(targets.successors())
            .map(|(target, _bit)| target)
            .collect();
        let derived_dropped = contradicted || reported.iter().any(|a| !seated.contains(a));
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
            // Settles in one round: an out-of-range `Single` becomes a tail
            // call, which nothing classifies again.
            progress.reshaped = true;
        }
        if derived_dropped || narrowed {
            progress.derived_incomplete.push(addr);
        }
        if assumed_mode {
            progress.assumed_modes.push(addr);
        }
        self.seats.insert(addr, targets);
    }
}

fn is_single(targets: &ResolvedTargets) -> bool {
    matches!(targets, ResolvedTargets::Single(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::test_support::{
        TARGET_VISITS, assert_linear_visits, mid_insn_addr, multiple, pcode_addr, successor_set,
    };

    /// The classifier's answers, by node.
    type Answers = FxHashMap<NodeId, ResolvedTargets>;

    /// An anchor's node: the fold never reads the function, so any id serves.
    fn node(i: u32) -> NodeId {
        NodeId::from_u32(i)
    }

    /// [`SiteTable::fold`] over `seats`, the loop's answers so far; `seats`
    /// comes back folded.
    fn fold_into(
        seats: &mut FxHashMap<PcodeInsnAddr, ResolvedTargets>,
        seeds: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
        anchors: &[FlowingAnchor],
        answers: Answers,
    ) -> Result<Progress> {
        let mut sites = SiteTable::new(seeds);
        sites.seats = std::mem::take(seats);
        let progress = sites.fold(anchors, answers);
        *seats = sites.seats;
        progress
    }

    /// One fold at `pcode_addr(0x1000)`, its one anchor with `flowing` flowing
    /// in: the loop holding `prev`, the caller seeding `seed`, the classifier
    /// answering `next`. Returns the progress and the folded seat.
    fn fold_at(
        prev: Option<ResolvedTargets>,
        next: ResolvedTargets,
        seed: Option<ResolvedTargets>,
        flowing: bool,
    ) -> (Progress, ResolvedTargets) {
        let addr = pcode_addr(0x1000);
        let mut known: FxHashMap<_, _> = prev.into_iter().map(|p| (addr, p)).collect();
        let seeds: FxHashMap<_, _> = seed.into_iter().map(|s| (addr, s)).collect();
        let progress = fold_into(
            &mut known,
            &seeds,
            &[(addr, node(0), flowing)],
            std::iter::once((node(0), next)).collect(),
        )
        .expect("fold");
        (progress, known.remove(&addr).expect("folded"))
    }

    /// [`fold_at`] with the base ISA mode flowing in.
    fn round(
        prev: Option<ResolvedTargets>,
        next: ResolvedTargets,
        seed: Option<ResolvedTargets>,
    ) -> (Progress, ResolvedTargets) {
        fold_at(prev, next, seed, false)
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
                &[(site, node(0), false)],
                [(node(0), multiple(&[(0x2000, None)]))],
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
                &[(second, node(0), false)],
                [(node(0), multiple(&[(0x5000, None)]))],
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
                &[(anchor, node(0), false)],
                [(node(0), multiple(&[(0x2000, None), (0x3000, None)]))],
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
        let table =
            |arms: [u64; 2]| ResolvedTargets::table(arms.map(strider_cfg::ResolvedTarget::from));
        let (progress, folded) =
            round(Some(table([0x2000, 0x3000])), table([0x2000, 0x3000]), None);
        assert!(
            !progress.changed(),
            "identical re-classification must not report growth"
        );
        assert_eq!(folded, table([0x2000, 0x3000]));

        let (progress, folded) = round(Some(folded), table([0x2000, 0x2004]), None);
        assert!(
            progress.changed(),
            "an improved classification must be applied"
        );
        assert!(
            !progress.narrowed.is_empty(),
            "dropping 0x3000 for 0x2004 is not monotone discovery"
        );
        assert_eq!(folded, table([0x2000, 0x2004]));
    }

    #[test]
    fn fold_first_classification_grows() {
        let (progress, folded) = round(None, ResolvedTargets::Single(0x2000.into()), None);
        assert!(
            progress.changed(),
            "a first-time classification must register as growth"
        );
        assert_eq!(progress.grew, vec![pcode_addr(0x1000)]);
        assert!(progress.narrowed.is_empty());
        assert_eq!(folded, ResolvedTargets::Single(0x2000.into()));
    }

    /// A seeded one-arm table and a derived `Single` of the same target name
    /// one successor, but the cfg defers the table out of range and seats the
    /// `Single` as a tail call, so the change of shape is progress.
    #[test]
    fn a_table_turning_single_at_one_target_is_progress() {
        let (progress, folded) = round(
            None,
            ResolvedTargets::Single(0x9000.into()),
            Some(multiple(&[(0x9000, None)])),
        );
        assert_eq!(folded, ResolvedTargets::Single(0x9000.into()));
        assert!(progress.changed(), "the seat changed shape");
    }

    /// Scales ARMS at one site, where
    /// [`fold_does_not_go_quadratic_in_accumulated_targets`]
    /// scales addresses: a mode-less re-derivation of an interworking table
    /// checks every new arm against the seated set.
    #[test]
    fn fold_does_not_go_quadratic_in_arms_at_one_site() {
        use strider_cfg::ResolvedTarget;
        assert_linear_visits("folding seated arms against re-derived ones", |arms| {
            let (progress, _folded) = round(
                Some(ResolvedTargets::table(
                    (0..arms).map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(true))),
                )),
                ResolvedTargets::table(
                    (0..arms * 2).map(|i| ResolvedTarget::new(0x2_0000 + i * 4, None)),
                ),
                None,
            );
            assert_eq!(progress.derived_incomplete, vec![pcode_addr(0x1000)]);
        });
    }

    #[test]
    fn fold_merges_two_anchors_at_same_addr() {
        // Two distinct placeholders sharing one address must union, not let
        // the second insert overwrite the first.
        let addr = pcode_addr(0x1000);
        let mut known = FxHashMap::default();
        fold_into(
            &mut known,
            &FxHashMap::default(),
            &[(addr, node(0), false), (addr, node(1), false)],
            [
                (node(0), ResolvedTargets::Single(0x2000.into())),
                (node(1), ResolvedTargets::Single(0x3000.into())),
            ]
            .into_iter()
            .collect(),
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
        let addr = pcode_addr(0x1000);
        let mut known = FxHashMap::default();
        let progress = fold_into(
            &mut known,
            &FxHashMap::default(),
            &[(addr, node(0), false), (addr, node(1), false)],
            [
                (node(0), ResolvedTargets::LinkRegister),
                (node(1), ResolvedTargets::Single(0x2000.into())),
            ]
            .into_iter()
            .collect(),
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
        let addr = pcode_addr(0x1000);
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
            let answers: Answers = std::iter::once((
                node(0),
                ResolvedTargets::table(vec![0x2000.into(), 0x2004.into()]),
            ))
            .collect();
            TARGET_VISITS.with(|v| v.set(0));
            let progress = fold_into(
                &mut known,
                &FxHashMap::default(),
                &[(addr, node(0), false)],
                answers,
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
        let (_progress, folded) = round(
            Some(ResolvedTargets::table(vec![
                ResolvedTarget::new(0x2000, Some(true)),
                ResolvedTarget::new(0x2004, Some(true)),
            ])),
            ResolvedTargets::table(vec![
                ResolvedTarget::new(0x2000, None),
                ResolvedTarget::new(0x2004, None),
                ResolvedTarget::new(0x2008, None),
            ]),
            None,
        );
        assert_eq!(
            folded.arms(),
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
        let (_progress, folded) = round(
            None,
            ResolvedTargets::Single(ResolvedTarget::new(0x4000, Some(true))),
            Some(multiple(&[(0x2000, None), (0x3000, None)])),
        );
        assert_eq!(
            folded.arms(),
            &[
                ResolvedTarget::new(0x2000, None),
                ResolvedTarget::new(0x3000, None),
                ResolvedTarget::new(0x4000, Some(true)),
            ],
            "seeding may only ADD edges, whatever the classifier proves about the mode",
        );
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
        // The site already proved Thumb at 0x2000; the round re-derives it
        // mode-less and adds 0x3000.
        let fold = |flowing: bool| {
            let (_progress, folded) = fold_at(
                Some(multiple(&[(0x2000, Some(true))])),
                multiple(&[(0x2000, None), (0x3000, None)]),
                None,
                flowing,
            );
            folded.arms().len()
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
        let anchors: Vec<FlowingAnchor> = nodes.iter().map(|&n| (addr, n, false)).collect();
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
                    .map(|(&n, a)| (n, a.clone()))
                    .collect();
                let mut known = FxHashMap::default();
                let progress = fold_into(&mut known, &seeds, &anchors, resolutions).expect("fold");
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
        let addr = pcode_addr(0x1000);
        let none = FxHashMap::default();
        let mut sites = SiteTable::new(&none);
        sites.freeze(addr, multiple(&[(0x2000, None)]));
        let progress = sites
            .fold(
                &[(addr, node(0), false)],
                [(node(0), multiple(&[(0x2000, None), (0x3000, None)]))],
            )
            .expect("fold");
        assert!(!progress.changed());
        assert_eq!(sites.seat(addr), Some(&multiple(&[(0x2000, None)])));
    }
}
