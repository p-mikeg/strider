//! What a CFG, or an analysis over several, cannot vouch for.

use std::collections::BTreeSet;
use std::fmt;

use crate::PcodeInsnAddr;

/// Emits [`Reason`], [`Reason::ALL`] and [`Reason::name`] from one row list.
macro_rules! reasons {
    ($($(#[$doc:meta])* $variant:ident => $name:literal,)+) => {
        /// Why an [`Incompleteness`] names an address. The first two name a
        /// dispatch SITE, the other five a branch TARGET.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Reason {
            $($(#[$doc])* $variant,)+
        }

        impl Reason {
            /// Every reason, in the order one address's entries sort.
            pub const ALL: [Reason; [$(stringify!($variant),)+].len()] =
                [$(Reason::$variant,)+];

            /// The spelling Python and the golden files use.
            #[must_use]
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }
        }
    };
}

reasons! {
    /// An indirect branch with no complete successor set: a dispatch left
    /// without an edge, or (in an analysis) a site whose answer stopped
    /// deriving, kept growing, lost a successor, narrowed or was withdrawn,
    /// and an unseeded return whose target is not provably the caller's.
    UnresolvedBranch => "unresolved_branch",
    /// A seated answer nothing verified: in a build every seat; in an
    /// analysis a `Switch` holding exactly the caller's seed, a `TailCall`
    /// seat, a caller's `Return` seat or seeded p-code `RETURN`, and an arm
    /// whose mode only its siblings' seed proved.
    UnverifiedSeed => "unverified_seed",
    /// Reached in two ISA modes, so the losing edge's path decodes in the
    /// winner's. Per p-code index. Other committed vars are not compared. A
    /// direct edge beats a seeded arm (the `Switch` loses every arm there);
    /// otherwise work-queue order decides, so neither decode is trusted.
    IsaModeConflict => "isa_mode_conflict",
    /// Inside a region but off every boundary (a MIPS delay slot too), or a
    /// region start a later decode stepped over. A direct branch keeps an
    /// inexact edge to the owner; a `Switch` arm is dropped.
    InteriorTarget => "interior_target",
    /// No bytes back the instruction here (a MIPS branch with an unmapped
    /// delay slot names itself); the edge leaves through a `TailCall` stub.
    /// Off a seeded arm, a decode running into them drops the arm unless a
    /// call precedes them, which ends `NoReturn`; a branch from the arm, or
    /// an arm onto an existing stub, keeps it. An unmapped entry fails the
    /// build.
    UnmappedTarget => "unmapped_target",
    /// Holds no instruction: Sleigh rejects the bytes or
    /// [`CfgOptions::data_ranges`](crate::CfgOptions::data_ranges) marks them.
    /// A branch, a fall-through into data, or a zero-p-code-op run a call
    /// falls into leaves through a `TailCall` stub; a call falling into them,
    /// directly or through such a run, ends `NoReturn`.
    /// Any other fall-through into rejected bytes fails the build. Off a
    /// seeded arm, as for [`Self::UnmappedTarget`], the arm is dropped and
    /// named by
    /// [`Cfg::undecodable_seeded_targets`](crate::Cfg::undecodable_seeded_targets);
    /// an arm or region off one whose decode fails for any unnamed reason is
    /// named here at its start.
    UndecodableTarget => "undecodable_target",
    /// A fall-through, not-taken path or conditional instruction's skip of
    /// itself ran off a [`FunctionBound::Size`](crate::FunctionBound::Size)
    /// end, named at the first address past the last decoded instruction. It
    /// leaves through a `TailCall` stub; a call returning there ends
    /// `NoReturn` unless its target is in
    /// [`CfgOptions::no_return_targets`](crate::CfgOptions::no_return_targets).
    /// Off a seeded arm the arm is dropped instead. Never under
    /// [`FunctionBound::Open`](crate::FunctionBound::Open).
    PastFunctionEnd => "past_function_end",
}

impl Reason {
    /// Whether the address is a dispatch site rather than a branch target.
    #[must_use]
    pub(crate) fn is_site(self) -> bool {
        matches!(self, Reason::UnresolvedBranch | Reason::UnverifiedSeed)
    }
}

/// The addresses a CFG or an analysis cannot vouch for, each with its
/// [`Reason`], sorted by address and deduplicated. Empty is complete.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Incompleteness(BTreeSet<(PcodeInsnAddr, Reason)>);

impl Incompleteness {
    /// Whether nothing is named.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every entry, by address and then [`Reason`] order.
    pub fn iter(&self) -> impl Iterator<Item = (PcodeInsnAddr, Reason)> + '_ {
        self.0.iter().copied()
    }

    /// The addresses held for `reason`, ascending.
    pub fn of(&self, reason: Reason) -> impl Iterator<Item = PcodeInsnAddr> + '_ {
        self.iter()
            .filter(move |&(_, r)| r == reason)
            .map(|(addr, _)| addr)
    }

    /// Names `addr` for `reason`; a repeat is a no-op.
    pub fn insert(&mut self, addr: PcodeInsnAddr, reason: Reason) {
        self.0.insert((addr, reason));
    }

    /// Adds `build`'s target reasons; its site reasons are that build's alone.
    pub fn absorb(&mut self, build: &Incompleteness) {
        self.0.extend(build.iter().filter(|(_, r)| !r.is_site()));
    }
}

impl fmt::Display for Incompleteness {
    /// The first eight as `<address>[.<p-code index>] <reason>`, then how
    /// many more.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const SHOWN: usize = 8;
        for (i, (addr, reason)) in self.iter().take(SHOWN).enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{:#x}", addr.machine())?;
            if addr.insn_index != 0 {
                write!(f, ".{}", addr.insn_index)?;
            }
            write!(f, " {}", reason.name())?;
        }
        let rest = self.0.len().saturating_sub(SHOWN);
        if rest > 0 {
            write!(f, " and {rest} more")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Incompleteness, Reason};
    use crate::PcodeInsnAddr;

    fn at(addr: u64) -> PcodeInsnAddr {
        PcodeInsnAddr::at_machine_start(addr)
    }

    #[test]
    fn any_one_reason_makes_a_set_incomplete() {
        assert!(Incompleteness::default().is_empty());
        for reason in Reason::ALL {
            let mut set = Incompleteness::default();
            set.insert(at(0x1000), reason);
            assert!(!set.is_empty(), "{reason:?}");
            assert_eq!(set.of(reason).collect::<Vec<_>>(), [at(0x1000)]);
        }
    }

    /// `ALL` is generated from the roster rows, so it cannot miss a variant;
    /// what the rows still fix is the ORDER, and the doc promises the site
    /// reasons are the leading run of it. Not `ALL == sorted`: `Ord` is
    /// derived from the same row order, so that would hold whatever it is.
    #[test]
    fn the_site_reasons_are_the_leading_run_of_all() {
        let sites = Reason::ALL.iter().filter(|r| r.is_site()).count();
        assert!(sites > 0, "no site reason");
        assert!(
            Reason::ALL[..sites].iter().all(|r| r.is_site()),
            "the site reasons must lead: {:?}",
            Reason::ALL
        );
    }

    #[test]
    fn names_are_distinct_and_the_first_two_are_sites() {
        let names: std::collections::BTreeSet<_> = Reason::ALL.map(Reason::name).into();
        assert_eq!(names.len(), Reason::ALL.len());
        let sites: Vec<_> = Reason::ALL.into_iter().filter(|r| r.is_site()).collect();
        assert_eq!(sites, [Reason::UnresolvedBranch, Reason::UnverifiedSeed]);
    }

    #[test]
    fn of_is_ascending_and_deduplicated() {
        let mut set = Incompleteness::default();
        for addr in [0x3000, 0x1000, 0x3000, 0x2000] {
            set.insert(at(addr), Reason::InteriorTarget);
        }
        set.insert(at(0x1000), Reason::UnresolvedBranch);
        assert_eq!(
            set.of(Reason::InteriorTarget).collect::<Vec<_>>(),
            [at(0x1000), at(0x2000), at(0x3000)]
        );
    }

    /// One round's site verdicts are that round's alone; its target reasons
    /// stand for every later round.
    #[test]
    fn absorb_takes_target_reasons_only() {
        let mut build = Incompleteness::default();
        for reason in Reason::ALL {
            build.insert(at(0x1000), reason);
        }
        let mut acc = Incompleteness::default();
        acc.absorb(&build);
        let got: Vec<Reason> = acc.iter().map(|(_, r)| r).collect();
        let want: Vec<Reason> = Reason::ALL.into_iter().filter(|r| !r.is_site()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn display_names_eight_then_counts_the_rest() {
        let mut set = Incompleteness::default();
        set.insert(
            PcodeInsnAddr {
                machine_addr: 0x1000.into(),
                insn_index: 2,
            },
            Reason::UnresolvedBranch,
        );
        assert_eq!(set.to_string(), "0x1000.2 unresolved_branch");
        for addr in 0..9 {
            set.insert(at(0x2000 + addr), Reason::UnmappedTarget);
        }
        assert_eq!(
            set.to_string(),
            "0x1000.2 unresolved_branch, 0x2000 unmapped_target, 0x2001 unmapped_target, \
             0x2002 unmapped_target, 0x2003 unmapped_target, 0x2004 unmapped_target, \
             0x2005 unmapped_target, 0x2006 unmapped_target and 2 more"
        );
    }
}
