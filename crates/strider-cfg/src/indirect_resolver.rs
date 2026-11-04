//! Indirect-branch answers the build seats; classifying them needs the IR,
//! above this crate.

use rustc_hash::FxHashSet;

/// One resolved branch target.
///
/// `isa_bit` is the ISA mode the branch commits for this target on an
/// alternate-ISA arch (ARM Thumb, MIPS16): `Some(true)` alternate, `Some(false)`
/// base, `None` when the branch commits none and the target inherits the mode
/// flowing into the branch. `addr` already has the mode bit masked off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub addr: u64,
    pub isa_bit: Option<bool>,
}

impl ResolvedTarget {
    #[must_use]
    pub fn new(addr: u64, isa_bit: Option<bool>) -> Self {
        Self { addr, isa_bit }
    }
}

/// A bare address with no mode switch (`isa_bit: None`), the common case.
impl From<u64> for ResolvedTarget {
    fn from(addr: u64) -> Self {
        Self {
            addr,
            isa_bit: None,
        }
    }
}

/// The statically-known targets of one `BranchIndirect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTargets {
    /// Return via the link register (ARM/AArch64 `bx lr`).  Seated as a
    /// [`crate::RegionTerminator::Return`].
    LinkRegister,
    /// A constant dispatch, or a caller declaring one. Out of function range it
    /// is a [`crate::RegionTerminator::TailCall`]; in range it is seated as a
    /// one-arm [`crate::RegionTerminator::Switch`], which keeps the selector so
    /// a later round can widen it.
    Single(ResolvedTarget),
    /// A jump table, seated as a [`crate::RegionTerminator::Switch`]; one
    /// out-of-range or off-boundary arm defers the whole site.
    Multiple(Arms),
    /// Nothing trustworthy, and no arms. A union treats it as the empty set;
    /// seated, it defers the site, over a caller's seed at that site too.
    Withdrawn,
}

/// A table's arms: non-empty, sorted by `(addr, isa_bit)`, no exact
/// duplicate. One address in both ISA modes keeps both, a clash the cfg
/// reports. Built by [`ResolvedTargets::table`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arms(Vec<ResolvedTarget>);

impl std::ops::Deref for Arms {
    type Target = [ResolvedTarget];

    fn deref(&self) -> &[ResolvedTarget] {
        &self.0
    }
}

impl ResolvedTargets {
    /// A table over `arms`, sorted and deduplicated: [`Self::Withdrawn`] when
    /// empty.
    #[must_use]
    pub fn table(arms: impl IntoIterator<Item = ResolvedTarget>) -> Self {
        let mut arms: Vec<ResolvedTarget> = arms.into_iter().collect();
        arms.sort_unstable_by_key(|t| (t.addr, t.isa_bit));
        arms.dedup();
        if arms.is_empty() {
            Self::Withdrawn
        } else {
            Self::Multiple(Arms(arms))
        }
    }

    /// A caller's own answer of `targets`: one target is a [`Self::Single`],
    /// which out of range is a tail call rather than a deferring arm.
    #[must_use]
    pub fn seeded(targets: impl IntoIterator<Item = u64>) -> Self {
        let table = Self::table(targets.into_iter().map(|t| ResolvedTarget::new(t, None)));
        match table.arms() {
            [only] => Self::Single(*only),
            _ => table,
        }
    }

    /// The concrete targets: none for `LinkRegister` and `Withdrawn`.
    #[must_use]
    pub fn arms(&self) -> &[ResolvedTarget] {
        match self {
            Self::LinkRegister | Self::Withdrawn => &[],
            Self::Single(target) => std::slice::from_ref(target),
            Self::Multiple(arms) => &arms.0,
        }
    }

    /// Every successor as `(address, isa_bit)`; `(None, None)` is the return
    /// through the link register.
    pub fn successors(&self) -> impl Iterator<Item = (Option<u64>, Option<bool>)> + '_ {
        let ret = matches!(self, Self::LinkRegister).then_some((None, None));
        ret.into_iter()
            .chain(self.arms().iter().map(|t| (Some(t.addr), t.isa_bit)))
    }

    /// Whether `next` loses ground on `self`: an address `self` reaches that
    /// `next` does not, or a proved mode flipping to the other ISA. A mode-less
    /// target taking a mode is growth, not a loss.
    #[must_use]
    pub fn narrows_to(&self, next: &Self) -> bool {
        let addrs: FxHashSet<Option<u64>> = next.successors().map(|(addr, _)| addr).collect();
        let bits: FxHashSet<(Option<u64>, bool)> = next
            .successors()
            .filter_map(|(addr, bit)| bit.map(|bit| (addr, bit)))
            .collect();
        self.successors().any(|(addr, bit)| {
            !addrs.contains(&addr) || bit.is_some_and(|bit| bits.contains(&(addr, !bit)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(addr: u64, bit: Option<bool>) -> ResolvedTarget {
        ResolvedTarget::new(addr, bit)
    }

    /// One seeded target is a `Single`, a repeated one too; none withdraws.
    #[test]
    fn a_seed_of_one_target_is_single() {
        let single = ResolvedTargets::Single(at(0x2000, None));
        assert_eq!(ResolvedTargets::seeded([0x2000]), single);
        assert_eq!(ResolvedTargets::seeded([0x2000, 0x2000]), single);
        assert_eq!(
            ResolvedTargets::seeded([0x3000, 0x2000]),
            ResolvedTargets::table([at(0x2000, None), at(0x3000, None)])
        );
        assert_eq!(ResolvedTargets::seeded([]), ResolvedTargets::Withdrawn);
    }

    #[test]
    fn a_table_of_nothing_is_withdrawn() {
        assert_eq!(ResolvedTargets::table([]), ResolvedTargets::Withdrawn);
        assert!(ResolvedTargets::Withdrawn.arms().is_empty());
        assert_eq!(ResolvedTargets::Withdrawn.successors().count(), 0);
    }

    #[test]
    fn a_table_sorts_and_drops_exact_duplicates() {
        let t = ResolvedTargets::table([at(0x3000, None), at(0x2000, None), at(0x3000, None)]);
        assert_eq!(t.arms(), &[at(0x2000, None), at(0x3000, None)]);
        assert_eq!(
            t,
            ResolvedTargets::table([at(0x2000, None), at(0x3000, None)])
        );
    }

    /// One address in both ISA modes is two arms; the cfg reports the clash.
    #[test]
    fn table_keeps_both_modes_of_one_address() {
        let t = ResolvedTargets::table([at(0x2000, Some(true)), at(0x2000, Some(false))]);
        assert_eq!(t.arms(), &[at(0x2000, Some(false)), at(0x2000, Some(true))]);
    }

    #[test]
    fn successors_name_the_return_and_every_arm() {
        assert_eq!(
            ResolvedTargets::LinkRegister
                .successors()
                .collect::<Vec<_>>(),
            vec![(None, None)]
        );
        assert_eq!(
            ResolvedTargets::Single(at(0x2000, Some(true)))
                .successors()
                .collect::<Vec<_>>(),
            vec![(Some(0x2000), Some(true))]
        );
        assert_eq!(
            ResolvedTargets::table([at(0x4000, None), at(0x2000, None), at(0x3000, None)])
                .successors()
                .collect::<Vec<_>>(),
            vec![
                (Some(0x2000), None),
                (Some(0x3000), None),
                (Some(0x4000), None)
            ]
        );
        assert!(ResolvedTargets::LinkRegister.arms().is_empty());
    }

    #[test]
    fn narrows_to_is_a_lost_address_or_a_flipped_mode() {
        let t = |arms: &[(u64, Option<bool>)]| {
            ResolvedTargets::table(arms.iter().map(|&(a, b)| at(a, b)))
        };
        let base = t(&[(0x2000, None), (0x3000, None)]);
        assert!(base.narrows_to(&t(&[(0x2000, None)])), "a lost address");
        assert!(
            !base.narrows_to(&t(&[(0x2000, None), (0x3000, None), (0x4000, None)])),
            "growth"
        );
        assert!(
            !base.narrows_to(&t(&[(0x2000, Some(true)), (0x3000, None)])),
            "a mode taken"
        );
        assert!(
            t(&[(0x2000, Some(true))]).narrows_to(&t(&[(0x2000, Some(false))])),
            "a mode flipped"
        );
        assert!(
            ResolvedTargets::LinkRegister.narrows_to(&t(&[(0x2000, None)])),
            "the return lost"
        );
        assert!(base.narrows_to(&ResolvedTargets::Withdrawn));
        assert!(!ResolvedTargets::Withdrawn.narrows_to(&base));
    }
}
