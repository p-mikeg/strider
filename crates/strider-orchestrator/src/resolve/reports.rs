//! What a round's cfg and the loop's seats say about each site: the seats the
//! cfg refused, and the sites reported.

use std::collections::BTreeSet;

use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{Cfg, PcodeInsnAddr, RegionTerminator, ResolvedTargets, UndecodableTarget};
use strider_ir::node::NodeId;

use super::round::Anchors;
use super::sites::SiteTable;
use super::visit;

/// Sites whose seated `Switch` holds FEWER arms than their seat asked for.
///
/// The cfg drops a target it cannot express as an arm (one interior to a
/// region but off every instruction boundary, which it only discovers once the
/// sibling arm it lands in has been decoded) and leaves the rest of the table
/// seated. The fold cannot see that: it compares its own result against the
/// previous round, so the loss reads as convergence. Comparing against the CFG
/// the round actually built is what keeps a converged answer honest.
pub(super) fn seated_arm_losses(cfg: &Cfg, sites: &SiteTable<'_>) -> Vec<PcodeInsnAddr> {
    let mut out = Vec::new();
    for region in cfg.regions() {
        let RegionTerminator::Switch { targets, addr, .. } = &region.terminator else {
            continue;
        };
        let Some(asked) = sites.seat(*addr) else {
            continue;
        };
        let seated: FxHashSet<u64> = targets.iter().map(|t| t.addr).collect();
        if asked.arms().iter().any(|t| !seated.contains(&t.addr)) {
            out.push(*addr);
        }
    }
    out
}

/// The sites whose seat this round's cfg refused, each with the seat it keeps.
///
/// Two ways a target goes bad, and they carry different verdicts.
///
/// A classifier that over-approximates a jump-table bound reaches past the
/// table and names addresses that are not code. The bound is what is wrong, so
/// no arm of that answer is trustworthy and the whole seat of the SITE that
/// named it goes. The same verdict covers an arm whose own target decodes but
/// which reaches code that does not, through a direct branch out of it: the
/// cfg reports the arm, not the address that failed, so the failing address
/// is reported under no reason. The same address seated from another site
/// decodes in that site's own committed context, so its failure there is no
/// verdict here.
///
/// An address two edges reach in different ISA modes is decoded once, in
/// whichever mode won the work queue, so the other edge's arm is not the
/// instruction stream it believes. ANY two edges raise that, direct ones
/// included (`Builder::explore` and `RegionBuilder::note_isa_mode_clash`), so
/// it is no verdict on the rest of the table: only the arm naming the clashing
/// address goes, and the site keeps the arms it still seats.
///
/// The candidates are `anchors`, the sites `undecodable` names and the loop's
/// own seats, never the rest of the caller's table: a binary-wide table holds
/// other functions' sites, and a clash here is no verdict on them.
pub(super) fn refusals(
    undecodable: &[UndecodableTarget],
    clashing: &[PcodeInsnAddr],
    sites: &SiteTable<'_>,
    anchors: impl IntoIterator<Item = PcodeInsnAddr>,
) -> Vec<(PcodeInsnAddr, ResolvedTargets)> {
    let mut bad_arms: FxHashMap<PcodeInsnAddr, FxHashSet<u64>> = FxHashMap::default();
    for bad in undecodable {
        bad_arms
            .entry(bad.site)
            .or_default()
            .insert(bad.target.machine_addr.addr);
    }
    let clashing: FxHashSet<u64> = clashing.iter().map(|a| a.machine_addr.addr).collect();
    if bad_arms.is_empty() && clashing.is_empty() {
        return Vec::new();
    }
    let names = |seat: &ResolvedTargets, bad: &FxHashSet<u64>| {
        seat.arms().iter().any(|t| bad.contains(&t.addr))
    };
    let candidates: BTreeSet<PcodeInsnAddr> = anchors
        .into_iter()
        .chain(bad_arms.keys().copied())
        .chain(sites.seated_sites())
        .collect();
    candidates
        .into_iter()
        .filter_map(|site| {
            let seat = sites.seat(site)?;
            if bad_arms.get(&site).is_some_and(|bad| names(seat, bad)) {
                return Some((site, ResolvedTargets::Withdrawn));
            }
            // Only a table keeps arms here: a `Single` named a clashing
            // target, its only one, and a table left with none is `Withdrawn`.
            names(seat, &clashing).then(|| {
                let kept = seat
                    .arms()
                    .iter()
                    .filter(|t| !clashing.contains(&t.addr))
                    .copied();
                (site, ResolvedTargets::table(kept))
            })
        })
        .collect()
}

/// Whether `site`'s settled successor set is still exactly what the caller
/// seeded, with nothing beyond it. An unseeded site is not covered.
///
/// A seed asserts the site is complete, and that assertion is what suppresses
/// the unclassifiable-`Switch` report. Outgrowing the seed disproves it: the
/// arms past the seed came from a classifier that has since stopped deriving,
/// so nothing vouches for the set that ended up seated.
pub(super) fn seed_still_covers(sites: &SiteTable<'_>, site: PcodeInsnAddr) -> bool {
    let Some(seed) = sites.seed(site) else {
        return false;
    };
    let seeded: FxHashSet<u64> = visit(seed.arms()).map(|t| t.addr).collect();
    sites
        .seat(site)
        .is_none_or(|settled| visit(settled.arms()).all(|t| seeded.contains(&t.addr)))
}

/// Seated `Switch` sites whose arms are exactly what the caller seeded and
/// nothing more, with the classifier silent about them.
///
/// Seating a seed changes the CFG the classifier reads, so a stale or wrong
/// seed can stop the selector deriving and then suppress the report of the arms
/// that cost. Such a site is not "unresolved" (the caller asserted the
/// answer), but nothing verified it, so it is named separately.
pub(super) fn unverified_seeded_sites(
    cfg: &Cfg,
    switches: &Anchors,
    unclassified: &FxHashSet<NodeId>,
    settled: &SiteTable<'_>,
) -> Vec<PcodeInsnAddr> {
    let mut out: Vec<PcodeInsnAddr> = switches
        .iter()
        .filter(|(addr, node)| {
            if !unclassified.contains(node) {
                return false;
            }
            // Anything beyond the seed means the classifier did contribute,
            // and then the site is reported as unresolved instead.
            seed_still_covers(settled, *addr)
        })
        .map(|(addr, _node)| *addr)
        .collect();
    // A `LinkRegister` seed is consumed at CFG-build time: it becomes a
    // `Return`, leaving no placeholder and no `Switch` anchor, so the filter
    // above cannot see it and no other reason names it either. The seat is
    // the only record that a caller's answer replaced whatever the classifier
    // would have derived. Only a CALLER's here: a seat the classifier derived
    // was its own answer, not an unchecked assertion, and `settle_return_sites`
    // re-checks every seat against the round's own IR either way.
    out.extend(
        cfg.link_register_seated()
            .iter()
            .filter(|addr| caller_seeded_return(settled, **addr)),
    );
    // A `TailCall` seat consumes the site the same way: an answer of one
    // target outside the function replaces the dispatch, and any other arm it
    // had is gone with no placeholder and no anchor left behind.
    out.extend_from_slice(cfg.tail_call_seated());
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether the caller seeded `site`'s `Return`. A `[]` seed does not: it
/// withdraws nothing, so a `Return` seated there is the classifier's.
pub(super) fn caller_seeded_return(settled: &SiteTable<'_>, site: PcodeInsnAddr) -> bool {
    matches!(settled.seed(site), Some(ResolvedTargets::LinkRegister))
}

/// The two non-placeholder channels `live_unresolved_branches` merges.
#[derive(Clone, Copy)]
pub(super) struct DerivedChannels<'a> {
    /// Sites still growing when the iteration cap ran out.
    pub(super) budget_exhausted: &'a [PcodeInsnAddr],
    /// Sites whose fold dropped a successor the classifier proved, whose answer
    /// stopped being a superset, or that the loop abandoned.
    pub(super) incomplete_derived: &'a [PcodeInsnAddr],
}

/// Sites the analysis cannot claim a complete successor set for (sorted,
/// deduplicated):
///
/// - a lift-time deferred site whose placeholder is still live. Classification
///   alone does not exclude one: the cfg layer re-emits the placeholder for a
///   target set it cannot seat, and `function` then still holds an
///   `IndirectBranch`.
/// - a seated `Switch` whose selector no longer derives (`unclassified`). Its
///   arms are whatever the round that seated them proved, which may be a
///   proper subset, and seating consumed the placeholder, so this list is its
///   only report.
/// - `budget_exhausted`, the sites still growing when the iteration cap ran out.
/// - `incomplete_derived`, the sites whose fold dropped a successor the
///   classifier proved, whose answer stopped being a superset, or that the loop
///   abandoned.
///
/// A caller seed asserts the site is complete, which suppresses the second of
/// those, the unclassified seated `Switch`, but only while `settled`, the
/// site's final successor set, holds nothing the seed did not name. A site that
/// outgrew its seed disproves the assertion, and the arms past the seed came
/// from a classifier that has since gone silent, so nothing vouches for them.
/// [`unverified_seeded_sites`] applies the same test the other way round, so
/// the `switches` half of the two reports is complementary. The derived
/// channels below are appended unconditionally, so a site can still reach
/// both.
///
/// The seed suppresses nothing else. A live placeholder means the cfg could
/// not seat the seeded set (empty, or a target out of range or interior to an
/// instruction), so the seed did not settle the site. Outgrowing the seed is
/// itself evidence it was incomplete, which leaves `budget_exhausted` standing.
/// And `incomplete_derived` reports what the classifier proved, which a seed is
/// no licence to drop.
pub(super) fn live_unresolved_branches(
    live_indirect: &FxHashSet<NodeId>,
    placeholders: &Anchors,
    switches: &Anchors,
    unclassified: &FxHashSet<NodeId>,
    settled: &SiteTable<'_>,
    derived: DerivedChannels<'_>,
) -> Vec<PcodeInsnAddr> {
    // `live_indirect` holds every reachable placeholder AND seated `Switch` of
    // the round that produced `function`. Intersecting it with `placeholders`
    // selects the live ones.
    let mut out: Vec<PcodeInsnAddr> = placeholders
        .iter()
        .filter(|(_addr, node)| live_indirect.contains(node))
        .map(|(addr, _node)| *addr)
        .collect();
    out.extend(
        switches
            .iter()
            .filter(|(addr, node)| {
                unclassified.contains(node) && !seed_still_covers(settled, *addr)
            })
            .map(|(addr, _node)| *addr),
    );
    // Neither is seed-suppressed: a seed cannot vouch for a site the classifier
    // was still GROWING when the budget ran out, nor for one whose derived
    // answer lost ground.
    out.extend(derived.budget_exhausted.iter().copied());
    out.extend(derived.incomplete_derived);
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::test_support::{
        fn_with_live_indirect_branch, mid_insn_addr, multiple, pcode_addr,
    };

    /// `seeds` with the loop holding `seats` over them.
    fn holding<'s>(
        seeds: &'s FxHashMap<PcodeInsnAddr, ResolvedTargets>,
        seats: &[(PcodeInsnAddr, ResolvedTargets)],
    ) -> SiteTable<'s> {
        let mut sites = SiteTable::new(seeds);
        for (site, targets) in seats {
            sites.freeze(*site, targets.clone());
        }
        sites
    }

    fn no_derived() -> DerivedChannels<'static> {
        DerivedChannels {
            budget_exhausted: &[],
            incomplete_derived: &[],
        }
    }

    /// [`live_unresolved_branches`] with no `Switch` anchors and no seed: the
    /// placeholder half on its own.
    fn live_placeholders(live: &FxHashSet<NodeId>, placeholders: &Anchors) -> Vec<PcodeInsnAddr> {
        let none = FxHashMap::default();
        live_unresolved_branches(
            live,
            placeholders,
            &Vec::new(),
            &FxHashSet::default(),
            &SiteTable::new(&none),
            no_derived(),
        )
    }

    /// The report key is the LIVE placeholder, not the site's classification
    /// state: the cfg layer re-emits a placeholder for a target set it could
    /// not seat, and that site must still be named.
    #[test]
    fn live_unresolved_reports_live_unclassified_branch() {
        let (function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let live: FxHashSet<NodeId> =
            strider_opt::classify_sites(&function, None, &strider_opt::OptOptions::default())
                .expect("classify")
                .sites
                .into_iter()
                .collect();
        assert_eq!(
            live_placeholders(&live, &vec![(addr, node)]),
            vec![addr],
            "a live placeholder must be reported whatever its site's \
             classification state",
        );
    }

    #[test]
    fn live_unresolved_excludes_dead_branch() {
        // A culled placeholder, simulated by pairing the address with a live
        // node that is not an `IndirectBranch`.
        use strider_ir::node::NodeKind;
        use strider_ir::{IRViewer, IRWalker};
        let (function, _node) = fn_with_live_indirect_branch();
        let non_indirect = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Entry))
            .expect("entry node");
        let live: FxHashSet<NodeId> =
            strider_opt::classify_sites(&function, None, &strider_opt::OptOptions::default())
                .expect("classify")
                .sites
                .into_iter()
                .collect();
        assert!(
            live_placeholders(&live, &vec![(pcode_addr(0x1000), non_indirect)]).is_empty(),
            "a dead / non-live IndirectBranch placeholder must not be reported"
        );
    }

    /// A seated `Switch` the classifier can no longer derive keeps its stale,
    /// possibly PARTIAL arm set. Seating consumed its placeholder, so the site
    /// must come back through its anchor or the caller reads an empty report
    /// next to a table missing arms.
    #[test]
    fn unclassifiable_seated_switch_is_reported_unresolved() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unclassified: FxHashSet<NodeId> = std::iter::once(node).collect();
        let none = FxHashMap::default();
        assert_eq!(
            live_unresolved_branches(
                &FxHashSet::default(),
                &Vec::new(),
                &vec![(addr, node)],
                &unclassified,
                &SiteTable::new(&none),
                no_derived(),
            ),
            vec![addr],
            "a seated Switch with no derivable arm set must be reported",
        );
    }

    /// A caller seed asserts the site is complete, so the same unclassifiable
    /// `Switch` is the caller's answer, not a gap, while the settled set is
    /// still the seed and nothing else.
    #[test]
    fn caller_seeded_switch_is_not_reported_unresolved() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unclassified: FxHashSet<NodeId> = std::iter::once(node).collect();
        let seed: FxHashMap<_, _> =
            std::iter::once((addr, ResolvedTargets::Single(0x2000.into()))).collect();
        assert!(
            live_unresolved_branches(
                &FxHashSet::default(),
                &Vec::new(),
                &vec![(addr, node)],
                &unclassified,
                &SiteTable::new(&seed),
                no_derived(),
            )
            .is_empty(),
        );
    }

    /// A site that OUTGREW its seed and then stopped classifying. The seed
    /// asserted a set the site has since grown past, so it vouches for none of
    /// the arms beyond it, and the round that proved them is gone.
    /// `unverified_seeded_sites` stays silent here (the settled set is not the
    /// seed), so this is the site's only report.
    #[test]
    fn a_switch_that_outgrew_its_seed_is_reported_unresolved() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unclassified: FxHashSet<NodeId> = std::iter::once(node).collect();
        let seed: FxHashMap<_, _> =
            std::iter::once((addr, ResolvedTargets::Single(0x2000.into()))).collect();
        let sites = holding(
            &seed,
            &[(addr, multiple(&[(0x2000, None), (0x3000, None)]))],
        );
        assert_eq!(
            live_unresolved_branches(
                &FxHashSet::default(),
                &Vec::new(),
                &vec![(addr, node)],
                &unclassified,
                &sites,
                no_derived(),
            ),
            vec![addr],
            "the seed cannot vouch for the arm the site grew past it",
        );
        assert!(
            !seed_still_covers(&sites, addr),
            "the shared test `unverified_seeded_sites` reads the other way \
             round must agree, or the site lands in both reports or neither",
        );
    }

    /// A seed answers for the site; it says nothing about what the classifier
    /// derives there, so a derived loss is reported even at a seeded site.
    #[test]
    fn caller_seed_does_not_suppress_a_derived_loss() {
        let addr = pcode_addr(0x1000);
        let seed: FxHashMap<_, _> =
            std::iter::once((addr, ResolvedTargets::Single(0x2000.into()))).collect();
        assert_eq!(
            live_unresolved_branches(
                &FxHashSet::default(),
                &Vec::new(),
                &Vec::new(),
                &FxHashSet::default(),
                &SiteTable::new(&seed),
                DerivedChannels {
                    budget_exhausted: &[],
                    incomplete_derived: &[addr],
                },
            ),
            vec![addr],
        );
    }

    /// A seeded arm at `target` that would not decode, seated from `site`.
    fn undecodable(site: PcodeInsnAddr, target: u64) -> UndecodableTarget {
        UndecodableTarget {
            site,
            target: pcode_addr(target),
        }
    }

    fn seeded(
        entries: &[(PcodeInsnAddr, ResolvedTargets)],
    ) -> FxHashMap<PcodeInsnAddr, ResolvedTargets> {
        entries.iter().cloned().collect()
    }

    #[test]
    fn an_undecodable_target_withdraws_the_whole_site() {
        let site = mid_insn_addr(0x1000);
        let seeds = seeded(&[(site, multiple(&[(0x2000, None), (0x9000, None)]))]);
        assert_eq!(
            refusals(
                &[undecodable(site, 0x9000)],
                &[],
                &SiteTable::new(&seeds),
                [site]
            ),
            vec![(site, ResolvedTargets::Withdrawn)],
            "an over-approximated table bound is no verdict on one arm, so no \
             arm of that answer survives",
        );
    }

    /// One site's undecodable arm is no verdict on another site naming the
    /// same address: each decodes in its own committed context.
    #[test]
    fn an_undecodable_target_refuses_only_the_site_that_named_it() {
        let bad = mid_insn_addr(0x1000);
        let other = pcode_addr(0x1100);
        let seeds = seeded(&[
            (bad, multiple(&[(0x2000, None), (0x9000, None)])),
            (other, multiple(&[(0x9000, None)])),
        ]);
        assert_eq!(
            refusals(
                &[undecodable(bad, 0x9000)],
                &[],
                &SiteTable::new(&seeds),
                [bad, other]
            ),
            vec![(bad, ResolvedTargets::Withdrawn)],
        );
    }

    /// An ISA clash is raised by ANY two edges reaching one address, direct
    /// ones included, so it says nothing about the table's other arms.
    #[test]
    fn an_isa_clash_drops_only_the_clashing_arm() {
        let site = mid_insn_addr(0x1000);
        let seeds = seeded(&[(
            site,
            multiple(&[(0x2000, None), (0x3000, None), (0x4000, None)]),
        )]);
        assert_eq!(
            refusals(&[], &[pcode_addr(0x3000)], &SiteTable::new(&seeds), [site]),
            vec![(site, multiple(&[(0x2000, None), (0x4000, None)]))],
        );
    }

    /// The arm a clash leaves is still a table arm: the cfg seats an
    /// out-of-range `Single` as a tail call but defers an out-of-range arm.
    #[test]
    fn an_isa_clash_leaves_a_one_arm_table_an_arm() {
        let site = mid_insn_addr(0x1000);
        let seeds = seeded(&[(site, multiple(&[(0x2000, None), (0x3000, None)]))]);
        assert_eq!(
            refusals(&[], &[pcode_addr(0x3000)], &SiteTable::new(&seeds), [site]),
            vec![(site, multiple(&[(0x2000, None)]))],
        );
    }

    /// A caller can only spell the machine address, so a clash on its seat
    /// is refused at the `BRANCHIND`'s own anchor.
    #[test]
    fn an_isa_clash_on_a_machine_start_seed_is_refused_at_the_anchor() {
        let anchor = mid_insn_addr(0x1000);
        let seeds = seeded(&[(
            pcode_addr(0x1000),
            multiple(&[(0x2000, None), (0x3000, None)]),
        )]);
        assert_eq!(
            refusals(
                &[],
                &[pcode_addr(0x3000)],
                &SiteTable::new(&seeds),
                [anchor]
            ),
            vec![(anchor, multiple(&[(0x2000, None)]))],
        );
    }

    /// Each `BranchIndirect` of the instruction is refused at its own
    /// anchor, not once at the seed's key. Mutation: map each anchor to
    /// `PcodeInsnAddr::at_machine_start` before collecting the candidates.
    #[test]
    fn two_anchors_over_one_machine_start_seed_are_both_refused() {
        let first = mid_insn_addr(0x1000);
        let second = PcodeInsnAddr {
            insn_index: 5,
            ..first
        };
        let seeds = seeded(&[(
            pcode_addr(0x1000),
            multiple(&[(0x2000, None), (0x3000, None)]),
        )]);
        let kept = multiple(&[(0x2000, None)]);
        assert_eq!(
            refusals(
                &[],
                &[pcode_addr(0x3000)],
                &SiteTable::new(&seeds),
                [second, first]
            ),
            vec![(first, kept.clone()), (second, kept)],
        );
    }

    #[test]
    fn an_isa_clash_on_every_arm_withdraws_the_site() {
        let site = pcode_addr(0x1000);
        let seeds = seeded(&[(
            site,
            multiple(&[(0x3000, Some(false)), (0x3000, Some(true))]),
        )]);
        assert_eq!(
            refusals(&[], &[pcode_addr(0x3000)], &SiteTable::new(&seeds), [site]),
            vec![(site, ResolvedTargets::Withdrawn)],
        );
    }

    /// Mutation: replace `names(seat, &clashing)` with `true`.
    #[test]
    fn a_site_naming_no_bad_target_is_left_alone() {
        let site = mid_insn_addr(0x1000);
        let seeds = seeded(&[
            (site, multiple(&[(0x2000, None)])),
            (pcode_addr(0x5000), ResolvedTargets::LinkRegister),
        ]);
        assert!(
            refusals(
                &[undecodable(pcode_addr(0x7000), 0x9000)],
                &[pcode_addr(0x8000)],
                &SiteTable::new(&seeds),
                [site, pcode_addr(0x5000)],
            )
            .is_empty()
        );
    }

    /// A seed for a site no anchor of this round names is not refused, even
    /// when its table names the clash: in a binary-wide table it is another
    /// function's site. Mutation (two lines): make `SiteTable::seeds`
    /// `pub(super)` and chain `.chain(sites.seeds.keys().copied())` onto the
    /// candidates.
    #[test]
    fn a_seed_no_anchor_names_is_not_refused() {
        let site = mid_insn_addr(0x1000);
        let seeds = seeded(&[
            (site, multiple(&[(0x2000, None)])),
            (pcode_addr(0x9_0000), multiple(&[(0x3000, None)])),
        ]);
        assert!(refusals(&[], &[pcode_addr(0x3000)], &SiteTable::new(&seeds), [site]).is_empty());
    }

    /// The loop's own seats are candidates even where no anchor of this round
    /// names them.
    #[test]
    fn a_loop_seat_is_refused_without_an_anchor() {
        let site = mid_insn_addr(0x1000);
        let none = FxHashMap::default();
        let sites = holding(
            &none,
            &[(site, multiple(&[(0x2000, None), (0x3000, None)]))],
        );
        assert_eq!(
            refusals(&[], &[pcode_addr(0x3000)], &sites, []),
            vec![(site, multiple(&[(0x2000, None)]))],
        );
    }
}
