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

/// How a [`Region`] ends, and the single source of truth for its control
/// transfer: edges are unweighted, so consumers asking how a region exits, or
/// which `CondBranch` successor is taken, read this and never the edge.
///
/// `Return`, `TailCall`, `NoReturn` and `UnresolvedIndirectBranch` have no
/// outgoing edge at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionTerminator {
    /// Three cases the IR consumer (`link_region_edges`) treats identically:
    /// decoding fell into an already-discovered region, the region is the
    /// first half of a split, or it closed on an explicit `Branch` opcode.
    Unconditional,
    /// Two outgoing edges; the one whose target region CONTAINS
    /// `true_target` is the taken side, the other the fall-through.
    CondBranch {
        /// A full [`PcodeInsnAddr`], not just a machine address: an
        /// intra-machine-instruction `CBRANCH` can put both successors at the
        /// same machine address with different pcode indices.
        ///
        /// Consumers recover polarity by containment, NOT by matching
        /// `start_addr`, which can sit below a region's first instruction
        /// after a zero-pcode-op-hole `split_region`.
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
    /// `known_targets`.  `handle_switch` reads `target_vn` at the region exit
    /// and emits an If-ladder of `IntCmpOp::Equal + If` per target, chained
    /// through the false branch.
    ///
    /// Every target must be an instruction-start address, guaranteed by the
    /// caller: the builder can only validate against the function address
    /// bounds, since instruction boundaries are known post-decode.  A target
    /// landing mid-instruction cannot be wired, as `region_id_at_start`
    /// misses it.
    Switch {
        /// The `BranchIndirect`'s `inputs[0]`, read at the region exit for the
        /// If-ladder comparison value.
        target_vn: rsleigh::Vn,
        targets: Vec<u64>,
    },
    /// `BranchIndirect` whose target is not yet known.  No outgoing edge.
    ///
    /// The orchestrator's fixed-point loop optimises the lifted IR and
    /// inspects `target_vn`'s producer there; successful classifications land
    /// in `known_targets` and materialise on the next CFG rebuild.  The lifter
    /// emits a placeholder `Return(target_value)` to anchor `target_vn` in the
    /// IR for that analysis.
    UnresolvedIndirectBranch {
        /// The offending `BranchIndirect`'s `inputs[0]`, read at the region
        /// exit for the `ValueId` anchoring the placeholder Return.
        target_vn: rsleigh::Vn,
        /// Keys the `known_targets` map and any unresolved-branch report.
        addr: PcodeInsnAddr,
    },
}

/// A basic block: maximal straight-line pcode with one entry and at most one
/// exit.  Ends on a `Branch`, `CondBranch` or `Return` opcode, or when
/// sequential decoding reaches an already-discovered region's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start_addr: PcodeInsnAddr,
    /// Program order.  Empty in exactly two cases (see `add_region`): an
    /// `Unconditional` region whose single trailing branch was popped, or a
    /// `TailCall` stub for an out-of-bound CondBranch arm.
    pub insns: Vec<RegionInstruction>,
    pub terminator: RegionTerminator,
}

impl Region {
    /// `start_addr <= addr <= last_insn.addr`.
    ///
    /// An empty region owns exactly its `start_addr`.  Returning `false` there
    /// once made `find_region_containing_addr` miss the start-address query,
    /// so the work queue built a duplicate region for the same edge target.
    /// That ownership is also what lets `region_if` resolve a CondBranch's OOB
    /// taken arm by containment.
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        match self.insns.last() {
            Some(last) => self.start_addr <= addr && addr <= last.addr,
            None => self.start_addr == addr,
        }
    }
}

/// Edges are unweighted and record only topology; the transfer kind comes
/// from the source region's [`RegionTerminator`].  `StableDiGraph` keeps
/// `NodeIndex` values valid across the removals and rewiring `split_region`
/// performs.
pub(crate) type RegionGraph = StableDiGraph<Region, ()>;
