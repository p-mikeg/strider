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
    pub insn: rsleigh::Insn,
    /// Byte length of the MACHINE instruction this pcode op came from, so a
    /// region's span can end past its last instruction's start address.  Every
    /// pcode op of one machine instruction repeats it.
    pub len: u32,
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
    /// Emitted when a CallOther classifies as noreturn: `BUG()`-class traps
    /// such as x86 `ud2` or aarch64 `brk #imm`.
    NoReturn,
    /// Branch leaving the function range, lowered by the IR layer as
    /// `Call(IntConst(target)) + Return`.
    ///
    /// Two shapes: a region ending in a direct (or `known_targets`-resolved)
    /// jump to an OOB target, and the empty stub `Builder::tail_call_stub`
    /// creates per OOB conditional arm.  The stub's `start_addr` IS the OOB
    /// target and it carries no instructions, since nothing outside the bound
    /// is decoded; it hangs off a regular CondBranch edge so the conditional
    /// survives.
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
/// `BranchIndirect` opcode, on a no-return `Call`/`CallOther`, or when
/// sequential decoding reaches an already-discovered region's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start_addr: PcodeInsnAddr,
    /// Program order.  Empty in two cases: an `Unconditional` region sealed
    /// at a zero-pcode-op instruction, which is the common one since `build`
    /// segments at every such instruction, or a `TailCall` stub for an
    /// out-of-bound CondBranch arm.
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
        addr.machine_addr.addr
            < last
                .addr
                .machine_addr
                .addr
                .saturating_add(u64::from(last.len))
    }
}

/// `StableDiGraph` keeps `EdgeIndex` values valid across the removals
/// `split_region` performs: it snapshots edge ids into a `Vec` and removes
/// inside the loop, which a plain `Graph` would invalidate by swap-removing.
/// No region is ever removed.
pub(crate) type RegionGraph = StableDiGraph<Region, ()>;
