use petgraph::stable_graph::StableDiGraph;

/// Newtype over `u64` so machine addresses cannot be mixed with plain
/// integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineInsnAddr {
    pub addr: u64,
}

impl From<u64> for MachineInsnAddr {
    fn from(value: u64) -> Self {
        MachineInsnAddr { addr: value }
    }
}

/// One pcode op: a machine address and an index into its lift.
///
/// The derived `Ord` (machine address first) follows field order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PcodeInsnAddr {
    pub machine_addr: MachineInsnAddr,
    pub insn_index: u64,
}

impl PcodeInsnAddr {
    pub fn at_machine_start(addr: u64) -> Self {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr },
            insn_index: 0,
        }
    }

    /// The machine address, as a plain `u64`.
    #[must_use]
    pub fn machine(self) -> u64 {
        self.machine_addr.addr
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInstruction {
    pub addr: PcodeInsnAddr,
    /// A LOAD / STORE's `inputs[0]` is a space id only the decoding engine
    /// resolves ([`crate::Cfg::space_ids`]).
    pub insn: rsleigh::Insn,
    /// Byte length of the machine instruction this pcode op came from.
    pub len: u32,
}

/// A seeded indirect-branch target that would not decode, with the dispatch
/// site it was an arm of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UndecodableTarget {
    pub site: PcodeInsnAddr,
    pub target: PcodeInsnAddr,
}

/// How a [`Region`] ends; edges are unweighted. `Return`, `TailCall`,
/// `NoReturn` and `UnresolvedIndirectBranch` have no outgoing edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionTerminator {
    /// One successor: a `Branch`, a zero-pcode-op instruction, a fall-through
    /// into an existing region, the first half of a split, or a fall-through
    /// onto a `TailCall` stub (unmapped bytes, data, past the bound).
    Unconditional,
    /// Two outgoing edges; see [`crate::Cfg::region_if`].
    CondBranch {
        /// A full [`PcodeInsnAddr`]: an intra-instruction `CBRANCH` can put
        /// both successors at one machine address.
        true_target: PcodeInsnAddr,
    },
    Return,
    /// A no-return CallOther (`ud2`, `brk #imm`), a call to a
    /// [`crate::CfgOptions::no_return_targets`] entry or returning outside the
    /// bound, or a call whose fall-through holds no code; that last drops
    /// whatever followed the call (a PowerPC TOC restore).
    NoReturn,
    /// A branch leaving the function: a jump to an out-of-range target, or an
    /// empty stub at `start_addr == target` for an edge leaving the decoded
    /// bytes (out of bound, unmapped, not code), hung off a successor edge.
    /// Unreported: a branch into a cold partition missing from
    /// [`crate::CfgOptions::fragment_ranges`] is modelled as a call to an
    /// ABI-conforming callee.
    TailCall {
        /// The callee, with the ISA mode the branch committed (`None`: its own
        /// entry mode).
        target: crate::ResolvedTarget,
    },
    /// A seated jump table. Every target is an instruction start inside the
    /// function bound or a fragment. An arm out of the bound, or off every instruction
    /// boundary of a region decoded before the seal, defers the whole site as
    /// `UnresolvedIndirectBranch`; an off-boundary arm found after the seal is
    /// dropped. Both off-boundary cases are reported as
    /// [`crate::Reason::InteriorTarget`]; an out-of-bound arm only through the
    /// deferred site's `UnresolvedBranch`.
    Switch {
        /// The `BranchIndirect`'s `inputs[0]`.
        target_vn: rsleigh::Vn,
        /// Each arm with the ISA mode the branch committed, if any.
        targets: Vec<crate::ResolvedTarget>,
        /// The dispatch instruction's address.
        addr: crate::PcodeInsnAddr,
    },
    /// `BranchIndirect` whose target is not yet known.  No outgoing edge.
    UnresolvedIndirectBranch {
        /// The offending `BranchIndirect`'s `inputs[0]`.
        target_vn: rsleigh::Vn,
        /// Address of the deferred `BranchIndirect`.
        addr: PcodeInsnAddr,
    },
}

impl RegionTerminator {
    /// The deferral a dispatch at `addr` on `target_vn` seals as: no outgoing
    /// edge, and the resolver re-derives the site.
    pub(crate) fn defer(target_vn: rsleigh::Vn, addr: PcodeInsnAddr) -> Self {
        Self::UnresolvedIndirectBranch { target_vn, addr }
    }

    /// Drops the `Switch` arms at `machine_addr` (one address can be an arm
    /// in both ISA modes), only the first when `first_only`. A table left
    /// with no arm demotes to [`Self::defer`]. Returns whether an arm went.
    pub(crate) fn drop_switch_arms_at(&mut self, machine_addr: u64, first_only: bool) -> bool {
        let Self::Switch {
            target_vn,
            targets,
            addr,
        } = self
        else {
            return false;
        };
        let before = targets.len();
        if first_only {
            if let Some(i) = targets.iter().position(|t| t.addr == machine_addr) {
                targets.remove(i);
            }
        } else {
            targets.retain(|t| t.addr != machine_addr);
        }
        let dropped = targets.len() != before;
        if targets.is_empty() {
            *self = Self::defer(*target_vn, *addr);
        }
        dropped
    }
}

/// A basic block: maximal straight-line pcode entered only at `start_addr` and
/// left only at its [`RegionTerminator`]. Every zero-pcode-op instruction is
/// its own region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start_addr: PcodeInsnAddr,
    /// Program order, ascending. Empty for a zero-pcode-op `Unconditional`
    /// region or a `TailCall` stub.
    pub insns: Vec<RegionInstruction>,
    /// Byte length of an empty region's zero-pcode-op instruction; `0` for a
    /// stub. Unread while `insns` is non-empty.
    pub empty_span_len: u32,
    pub terminator: RegionTerminator,
    /// The ISA mode the region decoded in; `None` with no ISA-mode var, and
    /// for a stub.
    pub(crate) isa_mode: Option<u32>,
}

impl Region {
    /// Index into `insns` of the pcode op at exactly `addr`, by bisection.
    pub(crate) fn insn_index_at(&self, addr: PcodeInsnAddr) -> Option<usize> {
        self.insns
            .binary_search_by(|insn| insn.addr.cmp(&addr))
            .ok()
    }

    /// The ascending order [`Self::insn_index_at`] bisects over. O(len): for
    /// debug checks per mutation.
    pub(crate) fn insns_are_ascending(&self) -> bool {
        self.insns.windows(2).all(|w| w[0].addr <= w[1].addr)
    }

    /// Whether a pcode op sits at exactly `addr`.
    pub(crate) fn contains_insn_at(&self, addr: PcodeInsnAddr) -> bool {
        self.insn_index_at(addr).is_some()
    }

    /// One past the last machine byte this region owns, at least
    /// `start_addr + 1`. `None` past the top of the address space (never
    /// saturated: that would disown the byte at `u64::MAX`).
    pub(crate) fn end(&self) -> Option<u64> {
        let start = self.start_addr.machine();
        let end = match self.insns.last() {
            Some(last) => last.addr.machine().checked_add(u64::from(last.len)),
            None => start.checked_add(u64::from(self.empty_span_len)),
        }?;
        Some(end.max(start.checked_add(1)?))
    }

    /// Byte length of the span [`Self::contains_addr`] accepts, at least 1.
    pub(crate) fn span_len(&self) -> u64 {
        let start = self.start_addr.machine();
        self.end()
            .map_or(u64::MAX - start, |end| end - start)
            .max(1)
    }

    /// `start_addr <= addr < end()`, all of the last instruction's bytes
    /// included; at its own machine address the pcode index still bounds the
    /// span. An empty region owns its `empty_span_len` machine bytes.
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        let below_end = self.end().is_none_or(|end| addr.machine() < end);
        match self.insns.last() {
            None => {
                addr == self.start_addr
                    || (addr.machine_addr > self.start_addr.machine_addr && below_end)
            }
            Some(_) if addr < self.start_addr => false,
            Some(last) if addr.machine_addr == last.addr.machine_addr => addr <= last.addr,
            Some(_) => below_end,
        }
    }
}

/// Stable, so edge ids survive `split_region`'s removals. Region ids are not
/// dense once unreachable regions are dropped.
pub(crate) type RegionGraph = StableDiGraph<Region, ()>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{addr, make_region};

    #[test]
    fn pcode_addr_orders_by_machine_addr_first() {
        // A larger insn_index never outranks a smaller machine address.
        assert!(addr(200, 0) > addr(100, 99));
        assert!(addr(100, 99) < addr(200, 0));
    }

    #[test]
    fn pcode_addr_orders_by_insn_index_when_machine_addr_equal() {
        assert!(addr(100, 1) > addr(100, 0));
        assert!(addr(100, 5) > addr(100, 4));
    }

    #[test]
    fn pcode_addr_at_machine_start_zero_index() {
        let a = PcodeInsnAddr::at_machine_start(0x2000);
        assert_eq!(a.machine(), 0x2000);
        assert_eq!(a.insn_index, 0);
    }

    #[test]
    fn contains_addr_spans_the_region_and_nothing_outside_it() {
        const SPAN: &[(u64, u64)] = &[(0x1000, 0), (0x1010, 0)];
        const PCODE: &[(u64, u64)] = &[(0x1000, 0), (0x1000, 3)];
        for (insns, (machine, index), want) in [
            (SPAN, (0x1000u64, 0u64), true), // start
            (SPAN, (0x1010, 0), true),       // end
            (SPAN, (0x1008, 0), true),       // interior
            (PCODE, (0x1000, 1), true),      // interior of one machine insn's pcode
            (SPAN, (0x0ff8, 0), false),      // before start
            (SPAN, (0x1014, 0), false),      // after end
        ] {
            let r = make_region(insns);
            assert_eq!(
                r.contains_addr(addr(machine, index)),
                want,
                "{machine:#x}+{index} in {insns:x?}"
            );
        }
    }

    #[test]
    fn contains_addr_covers_the_last_instructions_bytes_not_just_its_start() {
        // A region is a hole-free run, so no other region owns the bytes of its
        // last instruction either.
        let mut r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        r.insns.last_mut().unwrap().len = 10;
        assert!(r.contains_addr(addr(0x1015, 0)));
        assert!(r.contains_addr(addr(0x1019, 0)));
        assert!(!r.contains_addr(addr(0x101a, 0)), "the end is exclusive");
        // At the last instruction's own machine address the pcode index still
        // bounds the span: a region can end mid-pcode-sequence.
        assert!(!r.contains_addr(addr(0x1010, 1)));
    }

    #[test]
    fn contains_addr_returns_true_for_empty_region_at_start_addr() {
        // A tail-call stub owns its start address and no byte past it.
        let r = Region {
            start_addr: addr(0x1000, 0),
            insns: Vec::new(),
            empty_span_len: 0,
            isa_mode: None,
            terminator: RegionTerminator::Unconditional,
        };
        assert!(r.contains_addr(addr(0x1000, 0)));
        assert!(!r.contains_addr(addr(0x1000, 1)));
        assert!(!r.contains_addr(addr(0x1001, 0)));
    }

    #[test]
    fn contains_addr_covers_an_empty_regions_zero_pcode_op_instruction() {
        // A region sealed at a four-byte `endbr64` owns all four bytes; they
        // carry no pcode, so only their machine addresses are owned.
        let r = Region {
            start_addr: addr(0x1000, 0),
            insns: Vec::new(),
            empty_span_len: 4,
            isa_mode: None,
            terminator: RegionTerminator::Unconditional,
        };
        assert!(r.contains_addr(addr(0x1000, 0)));
        assert!(r.contains_addr(addr(0x1003, 0)));
        assert!(!r.contains_addr(addr(0x1004, 0)));
        assert!(!r.contains_addr(addr(0x1000, 1)));
        assert!(!r.contains_addr(addr(0x0fff, 0)));
    }
}
