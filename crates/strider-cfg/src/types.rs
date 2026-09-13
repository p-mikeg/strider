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

/// Identifies one pcode instruction: a machine instruction can lift to
/// several, so the machine address alone is not unique.
///
/// Ordering is lexicographic with `machine_addr` primary.  DO NOT reorder the
/// fields; the derived `Ord` follows declaration order.
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInstruction {
    pub addr: PcodeInsnAddr,
    /// A LOAD / STORE's `inputs[0]` encodes its target space as a raw pointer
    /// into the engine that decoded it, and this type carries no lifetime tying
    /// it to that engine.  `Lifter::check_cfg_space_ids` is the gate; nothing
    /// else may decode that pointer.
    pub insn: rsleigh::Insn,
    /// Byte length of the MACHINE instruction this pcode op came from, so a
    /// region's span can end past its last instruction's start address.  Every
    /// pcode op of one machine instruction repeats it.
    pub len: u32,
}

/// A seeded indirect-branch target that would not decode, with the dispatch
/// site it was an arm of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UndecodableTarget {
    pub site: PcodeInsnAddr,
    pub target: PcodeInsnAddr,
}

/// How a [`Region`] ends.  Edges are unweighted, so the transfer kind lives
/// here and nowhere else.
///
/// `Return`, `TailCall`, `NoReturn` and `UnresolvedIndirectBranch` have no
/// outgoing edge at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionTerminator {
    /// Four cases: the region ended at a zero-pcode-op instruction (`nop`,
    /// `endbr64`, `paciasp`, `bti`, alignment padding), decoding fell into an
    /// already-discovered region, the region is the first half of a split, or
    /// it closed on an explicit `Branch` opcode.
    Unconditional,
    /// Two outgoing edges; the one whose target region CONTAINS
    /// `true_target` is the taken side, the other the fall-through.
    CondBranch {
        /// A full [`PcodeInsnAddr`] rather than a machine address: an
        /// intra-machine-instruction `CBRANCH` can put both successors at the
        /// same machine address with different pcode indices.
        true_target: PcodeInsnAddr,
    },
    Return,
    /// Three emitters: a CallOther that classifies as noreturn (`BUG()`-class
    /// traps such as x86 `ud2` or aarch64 `brk #imm`), a `call` whose target
    /// carries a no-return calling convention, and a `call` or `callind` whose
    /// return address falls outside the function bound.
    NoReturn,
    /// Branch leaving the function range, lowered by the IR layer as
    /// `Call(IntConst(target)) + Return`.
    ///
    /// Two shapes: a region ending in a direct (or `known_targets`-resolved)
    /// jump to an OOB target, and the empty stub `Builder::tail_call_stub`
    /// creates for an edge leaving the decoded bytes.  The stub's `start_addr`
    /// IS the target and it carries no instructions, since nothing outside the
    /// bound and nothing unmapped is decoded; it hangs off a regular successor
    /// edge so the branch it came from survives.
    TailCall {
        /// The callee, with the ISA mode the branch committed for it (an
        /// interworking `bx <const>` to a different-mode function); its `isa_bit`
        /// is `None` when the callee keeps its own entry mode.
        target: crate::ResolvedTarget,
    },
    /// Jump table built from a `ResolvedTargets::Multiple` fed back via
    /// `known_targets`.
    ///
    /// Every target must be an instruction-start address; the builder can
    /// only validate against the function address bounds, since instruction
    /// boundaries are known post-decode.
    Switch {
        /// The `BranchIndirect`'s `inputs[0]`.
        target_vn: rsleigh::Vn,
        /// Each arm with the ISA mode the branch committed (an interworking
        /// `bx`/`jr`-dispatch), else `isa_bit: None`.
        targets: Vec<crate::ResolvedTarget>,
        /// The dispatch instruction, so a seated site stays keyed to its pcode
        /// address and a later resolution round can re-derive and widen it.
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

/// A basic block: maximal straight-line pcode entered only at `start_addr` and
/// left only at the terminator.  Ends on a `Branch`, `CondBranch`, `Return`, or
/// `BranchIndirect` opcode, on a no-return `Call`/`CallOther`/`CallIndirect`,
/// on a zero-pcode-op instruction (`build` segments at every one), or when
/// sequential decoding reaches an already-discovered region's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start_addr: PcodeInsnAddr,
    /// Program order.  Empty in two cases: an `Unconditional` region sealed
    /// at a zero-pcode-op instruction, which is the common one since `build`
    /// segments at every such instruction, or a `TailCall` stub.
    pub insns: Vec<RegionInstruction>,
    /// Byte length of the zero-pcode-op machine instruction an empty region was
    /// sealed at, which no `RegionInstruction` records.  `0` when the region
    /// owns no byte past `start_addr` (a `TailCall` stub).  Unread while
    /// `insns` is non-empty, where the last instruction bounds the span.
    pub empty_span_len: u32,
    pub terminator: RegionTerminator,
}

impl Region {
    /// Index of the pcode op at exactly `addr`.  `insns` holds one entry per
    /// PCODE op, so this is a pcode-op index, not a machine-instruction one.
    ///
    /// `insns` is program order, which for a region is ascending address
    /// order: one forward decode loop fills it and `split_off` preserves the
    /// order, which is what lets this bisect.
    pub(crate) fn insn_index_at(&self, addr: PcodeInsnAddr) -> Option<usize> {
        self.insns
            .binary_search_by(|insn| insn.addr.cmp(&addr))
            .ok()
    }

    /// The ascending order [`Self::insn_index_at`] bisects over.  Checked once
    /// per region mutation, never per query: this is O(len) and
    /// `insn_index_at` runs once per work-queue item and once per switch
    /// target.
    pub(crate) fn insns_are_ascending(&self) -> bool {
        self.insns.windows(2).all(|w| w[0].addr <= w[1].addr)
    }

    /// Whether a pcode op sits at exactly `addr`, i.e. `addr` is a pcode-op
    /// boundary rather than interior bytes.
    pub(crate) fn contains_insn_at(&self, addr: PcodeInsnAddr) -> bool {
        self.insn_index_at(addr).is_some()
    }

    /// Byte length of the span [`Self::contains_addr`] accepts, at least 1: a
    /// region always owns its `start_addr`.
    pub(crate) fn span_len(&self) -> u64 {
        match self.insns.last() {
            Some(last) => last
                .addr
                .machine_addr
                .addr
                // Saturating here is right where `contains_addr` needs
                // `checked`: a span that runs off the top of the address space
                // is as long as the space allows, and this is a LENGTH, not a
                // boundary an ownership test compares against.
                .saturating_add(u64::from(last.len))
                .saturating_sub(self.start_addr.machine_addr.addr),
            None => u64::from(self.empty_span_len),
        }
        .max(1)
    }

    /// `start_addr <= addr < last_insn.addr + last_insn.len`, so a region owns
    /// its last instruction's BYTES: reporting those unowned makes the builder
    /// decode a second region mid-instruction.  At the last instruction's own
    /// machine address the pcode index still bounds the span, a region being
    /// able to end mid-pcode-sequence.
    ///
    /// An empty region owns `start_addr` plus the `empty_span_len` bytes of the
    /// zero-pcode-op instruction it was sealed at; those bytes hold no pcode, so
    /// only their machine addresses are owned.
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        let Some(last) = self.insns.last() else {
            let start = self.start_addr.machine_addr.addr;
            return addr == self.start_addr
                || (addr.machine_addr.addr > start
                    && addr.machine_addr.addr
                        < start.saturating_add(u64::from(self.empty_span_len)));
        };
        if addr < self.start_addr {
            return false;
        }
        if addr.machine_addr == last.addr.machine_addr {
            return addr <= last.addr;
        }
        // `checked_add`, not `saturating`: an instruction whose bytes cross the
        // top of the address space would saturate the end to `u64::MAX` and
        // then report the byte AT `u64::MAX` unowned, which sends the builder
        // to decode a second region inside that instruction. Overflow means
        // the region owns everything at or above its start, which is what
        // `Builder::add_region` already does with `Bound::Unbounded`.
        match last.addr.machine_addr.addr.checked_add(u64::from(last.len)) {
            Some(end) => addr.machine_addr.addr < end,
            None => true,
        }
    }
}

/// `StableDiGraph` keeps `EdgeIndex` values valid across the removals
/// `split_region` performs: it snapshots edge ids into a `Vec` and removes
/// inside the loop, which a plain `Graph` would invalidate by swap-removing.
/// No region is ever removed.
pub(crate) type RegionGraph = StableDiGraph<Region, ()>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{addr, make_region};

    #[test]
    fn machine_insn_addr_from_u64() {
        let a: MachineInsnAddr = 0x1000u64.into();
        assert_eq!(a.addr, 0x1000);
    }

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
        assert_eq!(a.machine_addr.addr, 0x2000);
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
            terminator: RegionTerminator::Unconditional,
        };
        assert!(r.contains_addr(addr(0x1000, 0)));
        assert!(r.contains_addr(addr(0x1003, 0)));
        assert!(!r.contains_addr(addr(0x1004, 0)));
        assert!(!r.contains_addr(addr(0x1000, 1)));
        assert!(!r.contains_addr(addr(0x0fff, 0)));
    }
}
