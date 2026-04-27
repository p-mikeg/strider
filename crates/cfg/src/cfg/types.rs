use petgraph::stable_graph::StableDiGraph;

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

/// Classifies how a [`Region`] ends.
///
/// One terminator per region; the value is set when the region is
/// finalised by [`crate::Builder`].  The variants line up with the
/// outgoing edges in the [`RegionGraph`] but also record cases that have
/// no outgoing edge (e.g. `Return`, `TailCall`).
///
/// `Switch` is **reserved** for the future jump-table resolver and is
/// not constructed by the cfg builder today — it is part of the API so
/// that adding jump-table support is a purely additive change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionTerminator {
    /// No terminator opcode; control falls into the next region.  This
    /// covers the case where decoding hits the start of an
    /// already-discovered region and the current region is closed out
    /// with a [`RegionEdgeKind::Fallthrough`] edge, as well as the
    /// first half of a split region.
    Fallthrough,
    /// Direct unconditional branch, intra-function.  Successor lives on
    /// the [`RegionEdgeKind::Branch`] edge.
    Branch,
    /// Direct conditional branch.  Successors live on the
    /// [`RegionEdgeKind::IfCaseTrue`] / [`RegionEdgeKind::IfCaseFalse`]
    /// edges.
    CondBranch,
    /// `Return` opcode (or, in the legacy mapping retained until the
    /// indirect-branch resolver lands, a `BranchIndirect`).  No
    /// outgoing edge.
    Return,
    /// Direct branch whose target lies outside the function range.
    /// The IR layer is expected to lower this as
    /// `Call(IntConst(target)) + Return`.  No outgoing edge.
    TailCall {
        /// Resolved tail-call target machine address.
        target: u64,
    },
    /// FUTURE.  Jump table with N statically-known targets.  Reserved
    /// in the API now so a later resolver upgrade is purely additive.
    /// Not constructed by the current builder.
    Switch {
        /// Statically-known dispatch targets.
        targets: Vec<u64>,
    },
    /// `BranchIndirect` whose target the cfg-time tier-1 resolver
    /// (`indirect_resolve::resolve_indirect_target`) could not prove.
    ///
    /// The region was finalised with this terminator instead of an
    /// error; the strider-level fixed-point loop runs the full
    /// optimizer pipeline over the lifted IR and tier-2 resolution
    /// inspects the producer of `target_vn` in the optimised graph.
    /// At fixed point any remaining `UnresolvedIndirectBranch` regions
    /// surface as `ErrorKind::UnresolvedIndirectBranch(addr)`.
    ///
    /// This variant has **no outgoing edge**: the target is unknown
    /// at cfg-build time.  Strider lifts the region by emitting a
    /// placeholder `Return(target_value)` that anchors `target_vn`
    /// in the IR for analysis.
    UnresolvedIndirectBranch {
        /// The `inputs[0]` varnode of the offending `BranchIndirect`.
        /// Strider reads this varnode at the region exit to obtain
        /// the IR `NodeOutputId` that anchors the placeholder Return.
        target_vn: rsleigh::Vn,
        /// Pcode address of the offending `BranchIndirect`.  Used
        /// as the key for the strider-level `known_targets` map and
        /// for any `ErrorKind::UnresolvedIndirectBranch` raised at
        /// fixed point.
        addr: PcodeInsnAddr,
    },
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
    pub insns: Vec<RegionInstruction>,
    /// How this region ends — see [`RegionTerminator`].
    pub terminator: RegionTerminator,
}

impl Region {
    /// Returns `true` when `addr` lies within the instruction range of this
    /// region, i.e. `start_addr <= addr <= last_insn.addr`.
    ///
    /// Returns `false` for regions with no instructions (an invariant violation
    /// that `add_region` prevents, but handled gracefully here).
    #[must_use]
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        match self.insns.last() {
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
