//! The mode and merge policy: how two answers for one site combine.

use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{ResolvedTarget, ResolvedTargets};

/// `items`, counting each read in tests: the unit the fold's cost bounds are
/// stated in. Every target read in the fold must go through it, or the cost
/// tests cannot see that read.
pub(super) fn visit<I: IntoIterator>(items: I) -> impl Iterator<Item = I::Item> {
    items.into_iter().inspect(|_| {
        #[cfg(test)]
        super::test_support::TARGET_VISITS.with(|v| v.set(v.get() + 1));
    })
}

/// `targets` with each mode-less entry taking the ISA mode `known` already
/// proved for that address.
///
/// At a site `known` proves a mode DIFFERENT from `flowing_isa_bit`, an
/// address only a mode-less re-derivation reports is dropped rather than
/// seated: `None` reads to the cfg as the mode flowing into the branch, which
/// at an interworking dispatch is the mode being switched away from. The
/// dropped set stays seatable, so the fold reports the site through
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
    let modes: FxHashMap<u64, bool> = visit(known.arms())
        .filter_map(|t| t.isa_bit.map(|bit| (t.addr, bit)))
        .collect();
    if modes.is_empty() || matches!(targets, ResolvedTargets::LinkRegister) {
        return (targets, false);
    }
    let interworking = modes.values().any(|&bit| bit != flowing_isa_bit);
    let known_addrs: FxHashSet<u64> = visit(known.arms()).map(|t| t.addr).collect();
    let mut assumed = false;
    let from_single = matches!(targets, ResolvedTargets::Single(_));
    let kept: Vec<ResolvedTarget> = visit(targets.arms())
        .filter_map(|t| match (t.isa_bit, modes.get(&t.addr)) {
            (Some(_), _) => Some(*t),
            (None, Some(&bit)) => Some(ResolvedTarget::new(t.addr, Some(bit))),
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
    (single_or_table(kept, from_single), assumed)
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

/// Two classifications of one pcode address that BOTH derive from the IR,
/// united, dropping a mode-less address only the non-interworking side knows:
/// an address reported with no mode decodes in `flowing_isa_bit`, which is a
/// guess only against a side proving a DIFFERENT mode.
pub(super) fn merge_derived(
    a: &ResolvedTargets,
    b: &ResolvedTargets,
    flowing_isa_bit: bool,
) -> ResolvedTargets {
    combine(a, b, Some(flowing_isa_bit))
}

/// Two answers for one site united by address. `Withdrawn` is the empty set,
/// so either side `Withdrawn` gives the other; two `LinkRegister`s stay
/// `LinkRegister`; anything else is the unioned targets, deduped by address.
/// `flowing_isa_bit` is the mode flowing into the branch, `None` disabling the
/// interworking drop. Order-independent: two entries for one address keep the
/// ISA mode only where they agree, since a bit contradicted by the other
/// classification cannot be trusted (see [`merge_isa_bit`]).
///
/// `ResolvedTargets` cannot hold "returns AND jumps to X", so mixing
/// `LinkRegister` with a concrete set LOSES the return successor. The fold
/// accounts for it: `None` is one of the successors it tracks, so the site
/// comes back in `Progress::derived_incomplete` instead of converging on the
/// smaller answer.
fn combine(
    a: &ResolvedTargets,
    b: &ResolvedTargets,
    flowing_isa_bit: Option<bool>,
) -> ResolvedTargets {
    match (a, b) {
        (ResolvedTargets::Withdrawn, other) | (other, ResolvedTargets::Withdrawn) => {
            return other.clone();
        }
        (ResolvedTargets::LinkRegister, ResolvedTargets::LinkRegister) => {
            return ResolvedTargets::LinkRegister;
        }
        _ => {}
    }
    let single_addr = |r: &ResolvedTargets| match r {
        ResolvedTargets::Single(t) => Some(t.addr),
        _ => None,
    };
    let single_addrs = [single_addr(a), single_addr(b)];
    let interworks = |side: &[ResolvedTarget]| {
        flowing_isa_bit
            .is_some_and(|flowing| visit(side).any(|t| t.isa_bit.is_some_and(|bit| bit != flowing)))
    };
    let a_interworks = interworks(a.arms());
    let b_interworks = interworks(b.arms());
    let addrs =
        |side: &[ResolvedTarget]| -> FxHashSet<u64> { visit(side).map(|t| t.addr).collect() };
    let a_addrs = addrs(a.arms());
    let b_addrs = addrs(b.arms());
    let guessed = |t: &ResolvedTarget,
                   own_interworks: bool,
                   other_interworks: bool,
                   other_addrs: &FxHashSet<u64>| {
        other_interworks && !own_interworks && t.isa_bit.is_none() && !other_addrs.contains(&t.addr)
    };
    let mut targets: Vec<ResolvedTarget> = visit(a.arms())
        .filter(|t| !guessed(t, a_interworks, b_interworks, &b_addrs))
        .chain(visit(b.arms()).filter(|t| !guessed(t, b_interworks, a_interworks, &a_addrs)))
        .copied()
        .collect();
    targets.sort_by_key(|t| t.addr);
    let mut contradicted: FxHashSet<u64> = FxHashSet::default();
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
    single_or_table(targets, from_single)
}

/// `arms` as one answer: `Single` only when `from_single`, i.e. the lone
/// survivor is a `Single` input's target. The cfg seats an out-of-range
/// `Single` as a tail call but defers an out-of-range arm, so a merge must not
/// change which. Nothing left is `Withdrawn`: once every target has been
/// contradicted the honest answer is "nothing trustworthy here", where staging
/// nothing would leave the previous round's answer seated as if it had never
/// been contradicted.
fn single_or_table(arms: Vec<ResolvedTarget>, from_single: bool) -> ResolvedTargets {
    match arms.as_slice() {
        [single] if from_single => ResolvedTargets::Single(*single),
        _ => ResolvedTargets::table(arms),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::test_support::{assert_linear_visits, multiple};

    #[test]
    fn merge_derived_unions_multiple_targets() {
        assert_eq!(
            merge_derived(
                &multiple(&[(0x1000, None), (0x2000, None)]),
                &multiple(&[(0x2000, None), (0x3000, None)]),
                false,
            ),
            multiple(&[(0x1000, None), (0x2000, None), (0x3000, None)])
        );
    }

    #[test]
    fn merge_derived_two_link_registers_stay_link_register() {
        assert_eq!(
            merge_derived(
                &ResolvedTargets::LinkRegister,
                &ResolvedTargets::LinkRegister,
                false
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
            multiple(&[(0x1000, None), (0x2000, None)])
        );
    }

    /// Two placeholders at one pcode address classifying the SAME target with
    /// OPPOSITE ISA modes. Each address decodes once, so every mode the merge
    /// could pick has been disproved by one side, and "no committed mode" is
    /// not neutral: the cfg reads it as the mode flowing into the branch, which
    /// on an interworking `bx` is the one being switched away from. The address
    /// is dropped instead, leaving `Withdrawn`, which the cfg defers, so the
    /// site is reported unresolved. Order-independent.
    #[test]
    fn merge_derived_drops_a_target_two_classifications_disagree_on() {
        let thumb = ResolvedTargets::Single(ResolvedTarget::new(0x2000, Some(true)));
        let arm = ResolvedTargets::Single(ResolvedTarget::new(0x2000, Some(false)));
        assert_eq!(
            merge_derived(&thumb, &arm, false),
            ResolvedTargets::Withdrawn
        );
        assert_eq!(
            merge_derived(&arm, &thumb, false),
            ResolvedTargets::Withdrawn,
            "order-independent"
        );
    }

    #[test]
    fn single_or_table_is_single_only_for_a_single_survivor() {
        assert_eq!(
            single_or_table(Vec::new(), true),
            ResolvedTargets::Withdrawn
        );
        assert_eq!(
            single_or_table(vec![0x2000.into()], true),
            ResolvedTargets::Single(0x2000.into()),
        );
        assert_eq!(
            single_or_table(vec![0x2000.into()], false),
            multiple(&[(0x2000, None)]),
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
        let seated = multiple(&[(0x2000, Some(true))]);
        let rederived = multiple(&[(0x2000, None), (0x3000, None)]);
        let merged = merge_derived(&seated, &rederived, false);
        assert_eq!(
            merged.arms(),
            &[ResolvedTarget::new(0x2000, Some(true))],
            "the mode-less 0x3000 must not be seated as an arm",
        );
        assert_eq!(
            merge_derived(&rederived, &seated, false).arms(),
            merged.arms(),
            "order-independent",
        );
    }

    /// A classification that commits NO mode does not contradict one that
    /// does; conflating the two erases a seated interworking mode on the next
    /// resolution round.
    #[test]
    fn merge_derived_keeps_a_mode_the_other_side_does_not_report() {
        let thumb = ResolvedTargets::Single(ResolvedTarget::new(0x2000, Some(true)));
        let no_mode = ResolvedTargets::Single(ResolvedTarget::new(0x2000, None));
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
        let a = ResolvedTargets::Single(ResolvedTarget::new(0x2000, Some(true)));
        let b = multiple(&[(0x2000, Some(true)), (0x3000, None)]);
        assert_eq!(
            merge_derived(&a, &b, false),
            multiple(&[(0x2000, Some(true)), (0x3000, None)])
        );
    }

    /// A side proving only the FLOWING mode seats a mode-less arm
    /// identically, so a mode-less classification still widens it: the drop is
    /// for sides that switch ISA.
    #[test]
    fn merge_derived_widens_a_set_whose_proved_mode_is_the_flowing_one() {
        let seated = multiple(&[(0x2000, Some(false))]);
        let rederived = multiple(&[(0x2000, None), (0x3000, None)]);
        assert_eq!(
            merge_derived(&seated, &rederived, false).arms(),
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
        assert_linear_visits("merging contradicted arms", |arms| {
            let thumb = ResolvedTargets::table(
                (0..arms).map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(true))),
            );
            let arm = ResolvedTargets::table(
                (0..arms).map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(false))),
            );
            assert!(merge_derived(&thumb, &arm, false).arms().is_empty());
        });
    }

    /// A merge answers `Single` only when the surviving target is the `Single`
    /// input's own: an arm left over after the `Single`'s target is dropped
    /// stays an arm.
    #[test]
    fn a_surviving_table_arm_stays_an_arm() {
        let single = |addr, bit| ResolvedTargets::Single(ResolvedTarget::new(addr, bit));
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

    /// `Withdrawn` is the empty set to both unions: a `[]` seed or an
    /// all-contradicted merge blocks nothing. Mutation: return
    /// `ResolvedTargets::Withdrawn` from `combine`'s first arm.
    #[test]
    fn withdrawn_is_the_identity_of_both_unions() {
        let w = ResolvedTargets::Withdrawn;
        for t in [
            multiple(&[(0x2000, None), (0x3000, Some(true))]),
            ResolvedTargets::LinkRegister,
            ResolvedTargets::Single(0x4000.into()),
            w.clone(),
        ] {
            assert_eq!(union_with_seed(&w, &t), t, "{t:?}");
            assert_eq!(union_with_seed(&t, &w), t, "{t:?}");
            assert_eq!(merge_derived(&w, &t, false), t, "{t:?}");
            assert_eq!(merge_derived(&t, &w, true), t, "{t:?}");
        }
    }

    /// Order-free and idempotent, so two anchors at one address merge the same
    /// whichever the classifier listed first.
    #[test]
    fn merging_is_commutative_and_idempotent() {
        let answers = [
            ResolvedTargets::LinkRegister,
            ResolvedTargets::Single(ResolvedTarget::new(0x2000, Some(true))),
            multiple(&[(0x2000, None), (0x3000, None)]),
            multiple(&[(0x2000, Some(false)), (0x4000, Some(true))]),
            ResolvedTargets::Withdrawn,
        ];
        for a in &answers {
            for flowing in [false, true] {
                assert_eq!(merge_derived(a, a, flowing), *a, "{a:?}");
                for b in &answers {
                    assert_eq!(
                        merge_derived(a, b, flowing),
                        merge_derived(b, a, flowing),
                        "{a:?} {b:?} flowing {flowing}"
                    );
                }
            }
        }
    }

    #[test]
    fn merge_isa_bit_keeps_what_one_side_proves_and_refuses_a_contradiction() {
        assert_eq!(merge_isa_bit(Some(true), None), Ok(Some(true)));
        assert_eq!(merge_isa_bit(None, Some(false)), Ok(Some(false)));
        assert_eq!(merge_isa_bit(Some(true), Some(true)), Ok(Some(true)));
        assert_eq!(merge_isa_bit(Some(true), Some(false)), Err(Contradiction));
    }
}
