use petgraph::stable_graph::StableDiGraph;

/// Newtype over `u64` so machine addresses cannot be mixed with plain
/// integers.  Comparison and hashing use the raw address.
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
}

/// How a [`Region`] ends.  Edges are unweighted, so the transfer kind lives
/// here and nowhere else.
///
/// `Return`, `TailCall`, `NoReturn` and `UnresolvedIndirectBranch` have no
/// outgoing edge at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionTerminator {
    /// Four cases: decoding fell into an already-discovered region, the
    /// region is the first half of a split, it closed on an explicit `Branch`
    /// opcode, or a `BranchIndirect` was resolved via `known_targets` to a
    /// single in-range target.
    Unconditional,
    /// Two outgoing edges; the one whose target region CONTAINS
    /// `true_target` is the taken side, the other the fall-through.
    CondBranch {
        /// A full [`PcodeInsnAddr`], not just a machine address: an
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
        target: u64,
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
        targets: Vec<u64>,
    },
    /// `BranchIndirect` whose target is not yet known.  No outgoing edge.
    UnresolvedIndirectBranch {
        /// The offending `BranchIndirect`'s `inputs[0]`.
        target_vn: rsleigh::Vn,
        /// Address of the deferred `BranchIndirect`.
        addr: PcodeInsnAddr,
    },
}

/// A basic block: maximal straight-line pcode with one entry and at most one
/// exit.  Ends on a `Branch`, `CondBranch`, `Return`, or `BranchIndirect`
/// opcode, on a no-return `Call`/`CallOther`, or when sequential decoding
/// reaches an already-discovered region's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start_addr: PcodeInsnAddr,
    /// Program order.  Empty in exactly two cases: an
    /// `Unconditional` region whose single trailing branch was popped, or a
    /// `TailCall` stub for an out-of-bound CondBranch arm.
    pub insns: Vec<RegionInstruction>,
    pub terminator: RegionTerminator,
}

impl Region {
    /// `start_addr <= addr <= last_insn.addr`.
    ///
    /// An empty region owns exactly its `start_addr`.
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        match self.insns.last() {
            Some(last) => self.start_addr <= addr && addr <= last.addr,
            None => self.start_addr == addr,
        }
    }
}

/// `StableDiGraph` keeps `NodeIndex` values valid across the removals and
/// rewiring `split_region` performs.
pub(crate) type RegionGraph = StableDiGraph<Region, ()>;
