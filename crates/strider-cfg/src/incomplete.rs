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
    /// Reached in two ISA modes, per p-code index; the losing edge's path
    /// decodes in the winner's. A direct edge beats a seeded arm (the
    /// `Switch` loses every arm there); otherwise work-queue order decides.
    /// Two arms of one `Switch` at one address in both modes drop both.
    /// Other committed vars are not compared.
    IsaModeConflict => "isa_mode_conflict",
    /// Inside a region but off every boundary (a MIPS delay slot too), or a
    /// region start a later decode stepped over. A direct branch edges to the
    /// containing instruction; a table arm defers its site, or is dropped if
    /// found after the seal. A branch over an x86 `lock` prefix edges to the
    /// locked instruction, unreported.
    InteriorTarget => "interior_target",
    /// No bytes back the instruction here (a MIPS branch with an unmapped
    /// delay slot names itself); the edge leaves through a `TailCall` stub.
    /// Off a seeded arm, falling into them drops the arm unless a call
    /// precedes them (`NoReturn`); a branch into them keeps it. An unmapped
    /// entry fails the build.
    UnmappedTarget => "unmapped_target",
    /// No liftable instruction: Sleigh rejects the bytes, has no p-code for
    /// them, or [`CfgOptions::data_ranges`](crate::CfgOptions::data_ranges)
    /// marks them. A branch or a fall-through into data leaves through a
    /// `TailCall` stub, a call falling into them ends `NoReturn`, and any other
    /// fall-through fails the build. Off a seeded arm the arm is dropped, unless
    /// an earlier edge or fall-through reported the address; then the arm keeps
    /// that stub.
    /// A decode failing for an unnamed reason is named at its start.
    UndecodableTarget => "undecodable_target",
    /// A fall-through, not-taken path or conditional skip ran off a
    /// [`FunctionBound::Size`](crate::FunctionBound::Size) or fragment end
    /// (under `Open`, only a fragment's), named at the first address past it.
    /// It leaves through a `TailCall` stub, or ends a call's region
    /// `NoReturn`. A fall-through off a seeded arm drops the arm instead,
    /// unless a call precedes it.
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

    /// The site reasons are the leading run of `ALL`, and the names are
    /// distinct.
    #[test]
    fn names_are_distinct_and_the_sites_lead() {
        let names: std::collections::BTreeSet<_> = Reason::ALL.map(Reason::name).into();
        assert_eq!(names.len(), Reason::ALL.len());
        let sites: Vec<_> = Reason::ALL.into_iter().filter(|r| r.is_site()).collect();
        assert_eq!(sites, [Reason::UnresolvedBranch, Reason::UnverifiedSeed]);
        assert_eq!(Reason::ALL[..2], sites[..]);
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
