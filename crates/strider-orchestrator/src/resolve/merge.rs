//! The mode and merge policy: how two answers for one site combine.

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;
use strider_cfg::ResolvedTargets;

/// `items`, counting each read in tests: the unit the fold's cost bounds are
/// stated in. Every target read in the fold must go through it, or the cost
/// tests cannot see that read.
pub(super) fn visit<I: IntoIterator>(items: I) -> impl Iterator<Item = I::Item> {
    items.into_iter().inspect(|_| {
        #[cfg(test)]
        super::test_support::TARGET_VISITS.with(|v| v.set(v.get() + 1));
    })
}

/// Two classifications proved different ISA modes for one target.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Contradiction;

/// The caller's `seed` unioned with the loop's answer `derived` at one site,
/// by address and under no mode filter: a seed carries a mode only if the
/// caller built one.
pub(super) fn union_with_seed(
    seed: &ResolvedTargets,
    derived: &ResolvedTargets,
) -> ResolvedTargets {
    combine(seed, derived, None)
}

/// `targets` with each mode-less entry taking the ISA mode `known` already
/// proved for that address.
///
/// At a site `known` proves a mode DIFFERENT from `flowing_isa_bit`, an
/// address only a mode-less re-derivation reports is dropped rather than
/// seated: `None` reads to the cfg as the mode flowing into the branch, which
/// at an interworking dispatch is the mode being switched away from. The
/// dropped set stays seatable, so the caller reports the site through
/// `Progress::derived_incomplete` rather than relying on the cfg to re-defer
/// it.
/// Where every proved mode IS the flowing one, seating `None` decodes
/// identically, so the set still widens. Only the arms already evaluated
/// proved that mode, and they are no evidence about an arm nobody has
/// evaluated, so a later round widening the index can seat an address whose
/// committed mode was never checked. Dropping it would cost every same-mode
/// dispatch its arms, so it is kept and the second return value reports the
/// site through `Progress::assumed_modes`: the arm set is whole, which is an
/// UNVERIFIED answer rather than an incomplete one.
pub(super) fn adopt_known_modes(
    known: Option<&ResolvedTargets>,
    targets: ResolvedTargets,
    flowing_isa_bit: bool,
) -> (ResolvedTargets, bool) {
    let Some(known) = known else {
        return (targets, false);
    };
    let known_targets = concrete_targets(known);
    let modes: FxHashMap<u64, bool> = visit(known_targets)
        .filter_map(|t| t.isa_bit.map(|bit| (t.addr, bit)))
        .collect();
    if modes.is_empty() || matches!(targets, ResolvedTargets::LinkRegister) {
        return (targets, false);
    }
    let interworking = modes.values().any(|&bit| bit != flowing_isa_bit);
    let known_addrs: rustc_hash::FxHashSet<u64> = visit(known_targets).map(|t| t.addr).collect();
    let mut assumed = false;
    let from_single = matches!(targets, ResolvedTargets::Single(_));
    let kept: Vec<strider_cfg::ResolvedTarget> = visit(concrete_targets(&targets))
        .filter_map(|t| match (t.isa_bit, modes.get(&t.addr)) {
            (Some(_), _) => Some(*t),
            (None, Some(&bit)) => Some(strider_cfg::ResolvedTarget::new(t.addr, Some(bit))),
            (None, None) if known_addrs.contains(&t.addr) => Some(*t),
            // New to a mode-committing site, and the site does not interwork so
            // the flowing mode decodes it the same way the proved arms decode.
            // Kept, and flagged: nobody evaluated THIS arm's mode.
            (None, None) => (!interworking).then(|| {
                assumed = true;
                *t
            }),
        })
        .collect();
    (resolved_from(kept, from_single), assumed)
}

/// A resolved target as the convergence check sees it: `(address, isa_bit)`,
/// with `None` address for `LinkRegister`.
pub(super) type TargetKey = (Option<u64>, Option<bool>);

/// Whether one address's successor set lost ground between two rounds: the
/// ADDRESS projection is no longer a superset, or a target's proved mode flips
/// to the other ISA. A mode-less target taking a proved mode at an unchanged
/// address set is growth, and reading it as a loss would abandon a site that
/// was merely refining.
pub(super) fn narrows(prev: &BTreeSet<TargetKey>, next: &BTreeSet<TargetKey>) -> bool {
    let addrs = |keys: &BTreeSet<TargetKey>| -> BTreeSet<Option<u64>> {
        keys.iter().map(|(addr, _bit)| *addr).collect()
    };
    if !addrs(next).is_superset(&addrs(prev)) {
        return true;
    }
    // A bit is a bool, so "next commits the other mode at this address" is a
    // point lookup.
    let next_bits: rustc_hash::FxHashSet<(Option<u64>, bool)> = next
        .iter()
        .filter_map(|(addr, bit)| bit.map(|b| (*addr, b)))
        .collect();
    prev.iter()
        .any(|(addr, bit)| bit.is_some_and(|prev_bit| next_bits.contains(&(*addr, !prev_bit))))
}

/// Successor set of a `ResolvedTargets`.
pub(super) fn targets_of(r: &ResolvedTargets) -> impl Iterator<Item = TargetKey> + '_ {
    // `isa_bit` is in the key because it changes the DECODE: a target
    // reclassified from Thumb to ARM at one address would otherwise read as
    // converged, leaving regions decoded in the superseded mode.
    let (head, tail): (Option<TargetKey>, &[strider_cfg::ResolvedTarget]) = match r {
        ResolvedTargets::LinkRegister => (Some((None, None)), &[]),
        ResolvedTargets::Single(t) => (Some((Some(t.addr), t.isa_bit)), &[]),
        ResolvedTargets::Multiple(ts) => (None, ts.as_slice()),
    };
    head.into_iter()
        .chain(visit(tail).map(|t| (Some(t.addr), t.isa_bit)))
}

/// The concrete targets of a classification (empty for `LinkRegister`).
pub(super) fn concrete_targets(r: &ResolvedTargets) -> &[strider_cfg::ResolvedTarget] {
    match r {
        ResolvedTargets::LinkRegister => &[],
        ResolvedTargets::Single(t) => std::slice::from_ref(t),
        ResolvedTargets::Multiple(ts) => ts.as_slice(),
    }
}

/// Union two classifications of one pcode address that BOTH derive from the
/// IR, dropping a mode-less address only the non-interworking side knows: an
/// address reported with no mode decodes in `flowing_isa_bit`, which is a guess
/// only against a side proving a DIFFERENT mode.
pub(super) fn merge_derived(
    a: &ResolvedTargets,
    b: &ResolvedTargets,
    flowing_isa_bit: bool,
) -> ResolvedTargets {
    combine(a, b, Some(flowing_isa_bit))
}

/// Two answers for one site united by address. Two `LinkRegister`s stay
/// `LinkRegister`; anything else widens to `Single` or `Multiple` over the
/// unioned targets, deduped by address. `flowing_isa_bit` is the mode flowing
/// into the branch, `None` disabling the interworking drop. Order-independent: two entries for one address keep the ISA mode only
/// where they agree, since a bit contradicted by the other classification
/// cannot be trusted (see [`merge_isa_bit`]).
///
/// `ResolvedTargets` cannot hold "returns AND jumps to X", so mixing
/// `LinkRegister` with a concrete set LOSES the return successor.
/// `SiteTable::fold` accounts for it: `None` is one of the successors
/// `reported` tracks, so the site comes back in `Progress::derived_incomplete`
/// instead of converging on the smaller answer.
fn combine(
    a: &ResolvedTargets,
    b: &ResolvedTargets,
    flowing_isa_bit: Option<bool>,
) -> ResolvedTargets {
    if matches!(a, ResolvedTargets::LinkRegister) && matches!(b, ResolvedTargets::LinkRegister) {
        return ResolvedTargets::LinkRegister;
    }
    let single_addr = |r: &ResolvedTargets| match r {
        ResolvedTargets::Single(t) => Some(t.addr),
        _ => None,
    };
    let single_addrs = [single_addr(a), single_addr(b)];
    let a_targets = concrete_targets(a);
    let b_targets = concrete_targets(b);
    let interworks = |side: &[strider_cfg::ResolvedTarget]| {
        flowing_isa_bit
            .is_some_and(|flowing| visit(side).any(|t| t.isa_bit.is_some_and(|bit| bit != flowing)))
    };
    let a_interworks = interworks(a_targets);
    let b_interworks = interworks(b_targets);
    let addrs = |side: &[strider_cfg::ResolvedTarget]| -> rustc_hash::FxHashSet<u64> {
        visit(side).map(|t| t.addr).collect()
    };
    let a_addrs = addrs(a_targets);
    let b_addrs = addrs(b_targets);
    let guessed = |t: &strider_cfg::ResolvedTarget,
                   own_interworks: bool,
                   other_interworks: bool,
                   other_addrs: &rustc_hash::FxHashSet<u64>| {
        other_interworks && !own_interworks && t.isa_bit.is_none() && !other_addrs.contains(&t.addr)
    };
    let mut targets: Vec<strider_cfg::ResolvedTarget> = visit(a_targets)
        .filter(|t| !guessed(t, a_interworks, b_interworks, &b_addrs))
        .chain(visit(b_targets).filter(|t| !guessed(t, b_interworks, a_interworks, &a_addrs)))
        .copied()
        .collect();
    targets.sort_by_key(|t| t.addr);
    let mut contradicted: rustc_hash::FxHashSet<u64> = rustc_hash::FxHashSet::default();
    targets.dedup_by(|later, kept| {
        if later.addr != kept.addr {
            return false;
        }
        match merge_isa_bit(kept.isa_bit, later.isa_bit) {
            Ok(bit) => kept.isa_bit = bit,
            // Two classifications proved DIFFERENT modes for one address.
            // Decoding it once in either is a coin flip, and the decode-once
            // CFG cannot hold both, so drop it; the fold reports the site,
            // which stays seatable on its remaining arms.
            Err(Contradiction) => {
                contradicted.insert(kept.addr);
            }
        }
        true
    });
    targets.retain(|t| !contradicted.contains(&t.addr));
    let from_single = matches!(targets.as_slice(), [t] if single_addrs.contains(&Some(t.addr)));
    resolved_from(targets, from_single)
}

/// The ISA mode two classifications of one target agree on. A side committing
/// no mode reports nothing rather than denying the other's, so a mode-less
/// `Switch` re-derivation cannot erase a seated interworking mode. Commutative,
/// so `merge_derived` reads the same either way round.
fn merge_isa_bit(a: Option<bool>, b: Option<bool>) -> Result<Option<bool>, Contradiction> {
    match (a, b) {
        (Some(x), Some(y)) if x != y => Err(Contradiction),
        (x, y) => Ok(x.or(y)),
    }
}

/// `targets` as a [`ResolvedTargets`], and the ONE place an EMPTY one is built.
///
/// `strider_cfg` documents `Multiple` as non-empty and defends: it seats no
/// `Switch` for an empty set, re-emitting the placeholder, so the site comes
/// back as a live unresolved branch. That is the honest answer once every
/// target has been contradicted (the enum spells no "nothing trustworthy
/// here") where staging nothing would leave the previous round's answer
/// seated as if it had never been contradicted.
///
/// `Single` only when `from_single`, i.e. the survivor is a `Single` input's
/// target: the cfg seats an out-of-range `Single` as a tail call but defers an
/// out-of-range arm, so a merge must not change which.
pub(super) fn resolved_from(
    targets: Vec<strider_cfg::ResolvedTarget>,
    from_single: bool,
) -> ResolvedTargets {
    match targets.as_slice() {
        [single] if from_single => ResolvedTargets::Single(*single),
        _ => ResolvedTargets::Multiple(targets),
    }
}

/// One address's induced successor set, the unit the convergence check
/// compares. `target = None` is a `LinkRegister` resolution.
pub(super) fn target_keys(resolved: &ResolvedTargets) -> BTreeSet<TargetKey> {
    targets_of(resolved).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::test_support::{assert_linear_visits, multiple};

    #[test]
    fn target_keys_of_link_register_is_the_unknown_successor() {
        let keys = target_keys(&ResolvedTargets::LinkRegister);
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&(None, None)));
    }

    #[test]
    fn target_keys_of_single_resolution_is_one_successor() {
        let expected: BTreeSet<TargetKey> = std::iter::once((Some(0x2000), None)).collect();
        assert_eq!(
            target_keys(&ResolvedTargets::Single(0x2000.into())),
            expected
        );
    }

    #[test]
    fn target_keys_of_multiple_resolution_is_n_successors() {
        let keys = target_keys(&ResolvedTargets::Multiple(
            vec![0x2000, 0x3000, 0x4000]
                .into_iter()
                .map(Into::into)
                .collect(),
        ));
        assert_eq!(keys.len(), 3);
    }

    /// A set, so a repeated target is one successor and the comparison the
    /// convergence check makes is order-independent.
    #[test]
    fn target_keys_dedups_duplicate_targets_in_multiple() {
        let keys = target_keys(&ResolvedTargets::Multiple(
            vec![0x2000, 0x2000, 0x2000]
                .into_iter()
                .map(Into::into)
                .collect(),
        ));
        assert_eq!(keys.len(), 1);
        let reversed = target_keys(&ResolvedTargets::Multiple(
            vec![0x3000, 0x2000].into_iter().map(Into::into).collect(),
        ));
        let forward = target_keys(&ResolvedTargets::Multiple(
            vec![0x2000, 0x3000].into_iter().map(Into::into).collect(),
        ));
        assert_eq!(forward, reversed);
    }

    #[test]
    fn merge_derived_unions_multiple_targets() {
        let merged = merge_derived(
            &ResolvedTargets::Multiple(vec![0x1000, 0x2000].into_iter().map(Into::into).collect()),
            &ResolvedTargets::Multiple(vec![0x2000, 0x3000].into_iter().map(Into::into).collect()),
            false,
        );
        assert_eq!(
            merged,
            ResolvedTargets::Multiple(
                vec![0x1000, 0x2000, 0x3000]
                    .into_iter()
                    .map(Into::into)
                    .collect()
            )
        );
    }

    #[test]
    fn merge_derived_two_link_registers_stay_link_register() {
        assert_eq!(
            merge_derived(
                &ResolvedTargets::LinkRegister,
                &ResolvedTargets::LinkRegister,
                false,
            ),
            ResolvedTargets::LinkRegister
        );
    }

    #[test]
    fn merge_derived_single_plus_single_widens_to_multiple() {
        assert_eq!(
            merge_derived(
                &ResolvedTargets::Single(0x1000.into()),
                &ResolvedTargets::Single(0x2000.into()),
                false,
            ),
            ResolvedTargets::Multiple(vec![0x1000, 0x2000].into_iter().map(Into::into).collect())
        );
    }

    /// Two placeholders at one pcode address classifying the SAME target with
    /// OPPOSITE ISA modes. Each address decodes once, so every mode the merge
    /// could pick has been disproved by one side, and "no committed
    /// mode" is not neutral: the cfg reads it as the mode flowing into the
    /// branch, which on an interworking `bx` is the one being switched away
    /// from. The address is dropped instead, leaving the empty [`resolved_from`]
    /// builds, so the site fails the cfg's seatable guard and is reported
    /// unresolved. Order-independent.
    #[test]
    fn merge_derived_drops_a_target_two_classifications_disagree_on() {
        let thumb = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true)));
        let arm = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(false)));
        let empty = ResolvedTargets::Multiple(vec![]);
        assert_eq!(merge_derived(&thumb, &arm, false), empty);
        assert_eq!(
            merge_derived(&arm, &thumb, false),
            empty,
            "order-independent"
        );
    }

    /// The single place an EMPTY `Multiple` is built. `strider_cfg` documents
    /// that value as unseatable, which is the answer wanted here: the cfg
    /// re-emits the placeholder and the site is reported unresolved.
    #[test]
    fn resolved_from_builds_the_unseatable_empty_multiple() {
        assert_eq!(
            resolved_from(Vec::new(), true),
            ResolvedTargets::Multiple(vec![])
        );
        assert_eq!(
            resolved_from(vec![0x2000.into()], true),
            ResolvedTargets::Single(0x2000.into()),
        );
        assert_eq!(
            resolved_from(vec![0x2000.into()], false),
            ResolvedTargets::Multiple(vec![0x2000.into()]),
        );
    }

    /// Two anchors at ONE pcode address in one round, one of them a `Switch`
    /// re-derivation with no mode to evaluate, which reports `None` for every
    /// address it discovers. The cfg reads that as "inherit the branch's
    /// mode", a guess on an interworking dispatch, so the mode-less side does
    /// not contribute addresses of its own. Across ROUNDS the seated set is not
    /// merged but re-derived, and widening there is
    /// [`adopt_known_modes`]'s job.
    #[test]
    fn merge_derived_does_not_widen_a_mode_bearing_set_with_mode_less_arms() {
        let seated =
            ResolvedTargets::Multiple(vec![strider_cfg::ResolvedTarget::new(0x2000, Some(true))]);
        let rederived = ResolvedTargets::Multiple(vec![
            strider_cfg::ResolvedTarget::new(0x2000, None),
            strider_cfg::ResolvedTarget::new(0x3000, None),
        ]);
        let merged = merge_derived(&seated, &rederived, false);
        assert_eq!(
            concrete_targets(&merged),
            &[strider_cfg::ResolvedTarget::new(0x2000, Some(true))],
            "the mode-less 0x3000 must not be seated as an arm",
        );
        assert_eq!(
            concrete_targets(&merge_derived(&rederived, &seated, false)),
            concrete_targets(&merged),
            "order-independent",
        );
    }

    /// A classification that commits NO mode does not contradict one that
    /// does; conflating the two erases a seated interworking mode on the next
    /// resolution round.
    #[test]
    fn merge_derived_keeps_a_mode_the_other_side_does_not_report() {
        let thumb = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true)));
        let no_mode = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, None));
        assert_eq!(merge_derived(&thumb, &no_mode, false), thumb);
        assert_eq!(
            merge_derived(&no_mode, &thumb, false),
            thumb,
            "order-independent"
        );
    }

    /// Agreeing bits are kept: the collapse above must not throw away a mode
    /// both classifications assert.
    #[test]
    fn merge_derived_agreeing_isa_bits_are_kept() {
        let a = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true)));
        let b = ResolvedTargets::Multiple(vec![
            strider_cfg::ResolvedTarget::new(0x2000, Some(true)),
            strider_cfg::ResolvedTarget::new(0x3000, None),
        ]);
        assert_eq!(
            merge_derived(&a, &b, false),
            ResolvedTargets::Multiple(vec![
                strider_cfg::ResolvedTarget::new(0x2000, Some(true)),
                strider_cfg::ResolvedTarget::new(0x3000, None),
            ])
        );
    }

    /// A side proving only the FLOWING mode seats a mode-less arm
    /// identically, so a mode-less classification still widens it: the drop is
    /// for sides that switch ISA.
    #[test]
    fn merge_derived_widens_a_set_whose_proved_mode_is_the_flowing_one() {
        use strider_cfg::ResolvedTarget;
        let seated = ResolvedTargets::Multiple(vec![ResolvedTarget::new(0x2000, Some(false))]);
        let rederived = ResolvedTargets::Multiple(vec![
            ResolvedTarget::new(0x2000, None),
            ResolvedTarget::new(0x3000, None),
        ]);
        assert_eq!(
            concrete_targets(&merge_derived(&seated, &rederived, false)),
            &[
                ResolvedTarget::new(0x2000, Some(false)),
                ResolvedTarget::new(0x3000, None),
            ],
        );
    }

    /// Two classifications of one address, each proving a mode the other
    /// contradicts on every arm.
    #[test]
    fn merge_derived_does_not_go_quadratic_in_arm_count() {
        use strider_cfg::ResolvedTarget;
        assert_linear_visits("merging contradicted arms", |arms| {
            let thumb = ResolvedTargets::Multiple(
                (0..arms)
                    .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(true)))
                    .collect(),
            );
            let arm = ResolvedTargets::Multiple(
                (0..arms)
                    .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(false)))
                    .collect(),
            );
            let merged = merge_derived(&thumb, &arm, false);
            assert!(concrete_targets(&merged).is_empty());
        });
    }

    /// A merge answers `Single` only when the surviving target is the `Single`
    /// input's own: an arm left over after the `Single`'s target is dropped
    /// stays an arm.
    #[test]
    fn a_surviving_table_arm_stays_an_arm() {
        let single =
            |addr, bit| ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(addr, bit));
        assert_eq!(
            union_with_seed(
                &single(0x2000, Some(true)),
                &multiple(&[(0x2000, Some(false)), (0x3000, None)]),
            ),
            multiple(&[(0x3000, None)]),
            "the Single's target dropped as contradicted",
        );
        assert_eq!(
            merge_derived(
                &single(0x2000, None),
                &multiple(&[(0x3000, Some(true))]),
                false
            ),
            multiple(&[(0x3000, Some(true))]),
            "the Single's mode-less target dropped as guessed",
        );
        assert_eq!(
            union_with_seed(&single(0x2000, None), &multiple(&[(0x2000, None)])),
            single(0x2000, None),
            "the Single's own target survives",
        );
        assert_eq!(
            union_with_seed(&multiple(&[(0x2000, None)]), &multiple(&[(0x2000, None)])),
            multiple(&[(0x2000, None)]),
            "two one-arm tables",
        );
    }

    #[test]
    fn merge_isa_bit_keeps_what_one_side_proves_and_refuses_a_contradiction() {
        assert_eq!(merge_isa_bit(Some(true), None), Ok(Some(true)));
        assert_eq!(merge_isa_bit(None, Some(false)), Ok(Some(false)));
        assert_eq!(merge_isa_bit(Some(true), Some(true)), Ok(Some(true)));
        assert_eq!(merge_isa_bit(Some(true), Some(false)), Err(Contradiction));
    }
}
