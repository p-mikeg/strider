//! What a CFG, or an analysis over several, cannot vouch for.

use std::collections::BTreeSet;
use std::fmt;

use crate::PcodeInsnAddr;

/// Why an [`Incompleteness`] names an address.
///
/// The first two name a dispatch SITE, the key
/// [`CfgOptions::known_targets`](crate::CfgOptions::known_targets) seeds; the
/// other five a branch TARGET, which a direct edge reaches too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    /// An indirect branch with no complete successor set. A CFG build names
    /// every dispatch it left without an edge. An analysis names a live
    /// placeholder, a seated `Switch` whose selector stopped deriving, a site
    /// still growing at the round cap, one whose answer lost a proved
    /// successor, narrowed or was withdrawn, and a return whose target is not
    /// provably the caller's and the caller did not seed.
    UnresolvedBranch,
    /// A seated answer nothing verified. A CFG build names every seat. An
    /// analysis names a `Switch` holding exactly the caller's seed, every
    /// `TailCall` seat, a `Return` seat the caller made (where a failed
    /// re-check of it lands too), a p-code `RETURN` the caller seeded whose
    /// target is not provably the caller's, and an arm seated on a mode only
    /// its siblings' seed proved.
    UnverifiedSeed,
    /// Reached in two ISA modes: one region owns the bytes, so the losing
    /// edge's path decodes in the other mode. Held per p-code index, so a
    /// clash noted mid-instruction is its own entry.
    ///
    /// The ISA MODE only. Two edges that agree on the mode but disagree on
    /// another committed var (an edge into an ARM IT block, on `condit`) are
    /// not reported: the first arrival owns the bytes. An edge the entry
    /// reaches without a seeded arm wins over an arm, which is dropped; any
    /// other pair is won by work-queue order, so neither decode is trusted.
    IsaModeConflict,
    /// Interior to a region but off every boundary one can start at (a MIPS
    /// delay slot too), or a region start a later decode stepped over. A
    /// direct branch keeps an inexact edge to the owning region; a `Switch`
    /// arm is dropped.
    InteriorTarget,
    /// No bytes back it: the edge leaves through an empty `TailCall` stub. The
    /// address is the instruction that could not be read whole, so a MIPS
    /// branch whose delay slot is unmapped names itself. An unmapped seeded
    /// arm is named with no stub: the arm is dropped. An unmapped ENTRY fails
    /// the whole build instead.
    UnmappedTarget,
    /// Holds no instruction: bytes Sleigh rejects, or
    /// [`CfgOptions::data_ranges`](crate::CfgOptions::data_ranges) marks. A
    /// branch leaves through a `TailCall` stub; a fall-through past a call
    /// ends that call as `NoReturn`. Any other fall-through into data leaves
    /// through a stub too; one into bytes Sleigh rejects fails the whole
    /// build. Bytes reached off a seeded arm are named here with no stub: the
    /// arm is dropped instead, and
    /// [`Cfg::undecodable_seeded_targets`](crate::Cfg::undecodable_seeded_targets)
    /// names it. So is the start of an arm, or of a region off one, whose
    /// decode fails for a reason no other variant names.
    UndecodableTarget,
    /// Where a fall-through ran off a
    /// [`FunctionBound::Size`](crate::FunctionBound::Size) end, at the first
    /// address after the last instruction decoded: the end itself unless that
    /// instruction straddles it. That is a sequential fall-through, a
    /// conditional branch's not-taken path, or a conditional instruction's
    /// skip of itself (`if (!cc) goto inst_next`: ARM, Thumb IT, MIPS
    /// branch-likely, x86 `rep`). The region leaves through an empty `TailCall`
    /// stub there, as a branch out of range does. A call returning there ends
    /// its region `NoReturn` and is reported, unless its target is in
    /// [`CfgOptions::no_return_targets`](crate::CfgOptions::no_return_targets).
    /// Typical causes: an entry point falling into the next symbol, a slot
    /// patched at run time, or a size too short. Off a seeded arm a sequential
    /// fall-through is named here with no stub: the arm is dropped instead, and
    /// [`Cfg::undecodable_seeded_targets`](crate::Cfg::undecodable_seeded_targets)
    /// names it. Never under [`FunctionBound::Open`](crate::FunctionBound::Open):
    /// only an address below the entry is out of range there, and a
    /// fall-through moves up.
    PastFunctionEnd,
}

impl Reason {
    /// Every reason, in the order one address's entries sort.
    pub const ALL: [Reason; 7] = [
        Reason::UnresolvedBranch,
        Reason::UnverifiedSeed,
        Reason::IsaModeConflict,
        Reason::InteriorTarget,
        Reason::UnmappedTarget,
        Reason::UndecodableTarget,
        Reason::PastFunctionEnd,
    ];

    /// The spelling Python and the golden files use.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Reason::UnresolvedBranch => "unresolved_branch",
            Reason::UnverifiedSeed => "unverified_seed",
            Reason::IsaModeConflict => "isa_mode_conflict",
            Reason::InteriorTarget => "interior_target",
            Reason::UnmappedTarget => "unmapped_target",
            Reason::UndecodableTarget => "undecodable_target",
            Reason::PastFunctionEnd => "past_function_end",
        }
    }

    /// Whether the address is a dispatch site rather than a branch target.
    #[must_use]
    pub fn is_site(self) -> bool {
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

    /// Adds `build`'s target reasons. Its site reasons answer for that build
    /// alone, so they are left out.
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
            write!(f, "{:#x}", addr.machine_addr.addr)?;
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
