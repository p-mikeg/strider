use petgraph::stable_graph::StableDiGraph;
use std::collections::VecDeque;

/// Classifies the control-flow relationship between two CFG regions.
///
/// Every edge in the [`RegionGraph`] carries one of these four labels.
/// The label determines which outgoing path is taken when execution leaves the
/// source region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionEdgeKind {
    /// Sequential execution: the source region ends without a branch and
    /// execution falls directly into the target region.
    Fallthrough,
    /// Unconditional jump: the source region ends with a pcode `Branch` and
    /// always transfers control to the target.
    Branch,
    /// Conditional branch — taken path: the source region ends with a pcode
    /// `CondBranch` and the branch condition evaluated to *true*.
    IfCaseTrue,
    /// Conditional branch — not-taken path: the source region ends with a
    /// pcode `CondBranch` and the branch condition evaluated to *false*.
    IfCaseFalse,
}

/// A virtual address identifying a native machine instruction.
///
/// This is a newtype wrapper around `u64` that prevents accidental mixing
/// with plain integers.  Comparison and hashing use the raw address value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineInsnAddr {
    /// The raw virtual address of the machine instruction.
    pub addr: u64,
}

impl From<u64> for MachineInsnAddr {
    fn from(value: u64) -> Self {
        MachineInsnAddr { addr: value }
    }
}

/// A fine-grained address that identifies a single pcode instruction.
///
/// One native machine instruction can lift to several pcode instructions.
/// `PcodeInsnAddr` identifies each one by combining the machine-instruction
/// address with an index into the pcode sequence it produces.
///
/// Ordering is lexicographic: `machine_addr` is the primary key and
/// `insn_index` breaks ties.  **Do not reorder the fields** — the `#[derive]`
/// ordering relies on declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PcodeInsnAddr {
    /// Virtual address of the enclosing machine instruction.
    pub machine_addr: MachineInsnAddr,
    /// Zero-based index of this pcode instruction within the machine instruction.
    pub insn_index: u64,
}

/// A single pcode instruction together with its address inside the CFG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInstruction {
    /// Address of this pcode instruction.
    pub addr: PcodeInsnAddr,
    /// The decoded pcode instruction.
    pub insn: rsleigh::Insn,
}

/// A basic block: a maximal straight-line sequence of pcode instructions
/// with a single entry point and (at most) one exit point.
///
/// Regions are the nodes of the [`RegionGraph`].  A region ends when the
/// builder encounters a `Branch`, `CondBranch`, or `Return` pcode opcode, or
/// when sequential execution reaches the start of an already-discovered region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Address of the first pcode instruction in this region.
    pub start_addr: PcodeInsnAddr,
    /// All pcode instructions, in program order.  Never empty.
    pub insns: VecDeque<RegionInstruction>,
    /// `true` when the region ends with an unconditional branch that the
    /// builder classified as a tail call (i.e. a jump to code outside the
    /// current function).
    pub ends_with_tail_call: bool,
}

impl Region {
    /// Returns `true` when `addr` lies within the instruction range of this
    /// region, i.e. `start_addr <= addr <= last_insn.addr`.
    ///
    /// Returns `false` for regions with no instructions (an invariant violation
    /// that `add_region` prevents, but handled gracefully here).
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        match self.insns.back() {
            Some(last) => self.start_addr <= addr && addr <= last.addr,
            None => false,
        }
    }
}

/// The directed graph type used to represent the CFG.
///
/// Nodes are [`Region`]s (basic blocks); edge weights are [`RegionEdgeKind`]
/// values that describe the type of control transfer.  `StableDiGraph` is
/// used so that `NodeIndex` values remain stable when regions are removed or
/// re-wired (e.g. during `split_region`).
pub type RegionGraph = StableDiGraph<Region, RegionEdgeKind>;
