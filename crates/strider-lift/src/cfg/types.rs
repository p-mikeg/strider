use petgraph::stable_graph::StableDiGraph;

/// Classifies the control-flow relationship between two CFG regions.
///
/// Every edge in the `RegionGraph` carries one of these four labels.
/// The label determines which outgoing path is taken when execution leaves the
/// source region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionEdgeKind {
    /// Unconditional successor: control always transfers from the source
    /// region to the target.  Covers both sequential fall-through (the
    /// source ends without a branch opcode) and an explicit pcode `Branch`
    /// — the CFG draws the same edge for both, because the IR lifter links
    /// every unconditional successor the same way (via the region linker).
    /// The source [`Region::terminator`] still records which opcode (if
    /// any) ended the region, for callers that care.
    Unconditional,
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
///
/// Construct with `addr.into()` or `MachineInsnAddr::from(addr)`; read
/// via the `addr` field.
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

impl PcodeInsnAddr {
    /// Returns the pcode address pointing at the *first* pcode op of
    /// the machine instruction at `addr` (`insn_index == 0`).
    #[must_use]
    pub fn at_machine_start(addr: u64) -> Self {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr },
            insn_index: 0,
        }
    }
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
/// finalised by [`crate::cfg::Builder`].  The variants line up with the
/// outgoing edges in the `RegionGraph` but also record cases that have
/// no outgoing edge (e.g. `Return`, `TailCall`).
///
/// `Switch` is constructed by `cfg::builder::region_builder` when the
/// indirect-branch resolver classifies a `BranchIndirect` as a
/// jump-table dispatch with a known multi-target set.  See the per-arm
/// doc on [`RegionTerminator::Switch`] for the construction contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionTerminator {
    /// No terminator opcode; control falls into the next region.  This
    /// covers the case where decoding hits the start of an
    /// already-discovered region and the current region is closed out
    /// with a [`RegionEdgeKind::Unconditional`] edge, as well as the
    /// first half of a split region.
    Fallthrough,
    /// Direct unconditional branch, intra-function.  Successor lives on
    /// the [`RegionEdgeKind::Unconditional`] edge (the same edge kind a
    /// `Fallthrough` terminator uses — both are unconditional transfers).
    Branch,
    /// Direct conditional branch.  Successors live on the
    /// [`RegionEdgeKind::IfCaseTrue`] / [`RegionEdgeKind::IfCaseFalse`]
    /// edges.
    CondBranch,
    /// `Return` opcode.  No outgoing edge.
    Return,
    /// Region terminates with no successor.  Emitted by
    /// `cfg::region_builder::process_new_insn` when a CallOther's
    /// `strider_target::call_other_abi::classify` result is `NoReturn` (Linux
    /// `BUG()` / `BUG_ON()`-class traps: x86 `ud2`, aarch64
    /// `brk #imm`).  See
    /// `docs/superpowers/specs/2026-05-05-callother-classification-design.md`.
    NoReturn,
    /// Direct branch whose target lies outside the function range.
    /// The IR layer is expected to lower this as
    /// `Call(IntConst(target)) + Return`.  No outgoing edge.
    TailCall {
        /// Resolved tail-call target machine address.
        target: u64,
    },
    /// Jump table with N statically-known targets.  Constructed by
    /// the cfg builder from a `ResolvedTargets::Multiple` resolution
    /// (which only the strider indirect-resolution fixed-point loop
    /// produces; the cfg-time mini-graph resolver never returns
    /// Multiple).  Strider's `handle_switch` reads
    /// `target_vn` at the region exit and emits an If-ladder of
    /// `IntCmpOp::Equal + If` against each `targets[i]`, chained
    /// through the false-branch.
    ///
    /// `target_value` is an OPTIONAL pinned `NodeOutputId` for the
    /// dispatch value.  When `Some`, strider's `handle_switch` uses
    /// it directly instead of re-reading `target_vn`, pinning the
    /// soundness contract that the comparison value is the SAME
    /// value the IR-level indirect-branch resolver classified.  The cfg builder always sets this to
    /// `None`; it is plumbing for an incremental-rebuild round that
    /// preserves the previous iteration's IR.
    Switch {
        /// The dispatch varnode — the `BranchIndirect`'s
        /// `inputs[0]`.  Strider reads this at the region exit to
        /// obtain the lifted comparison value for the If-ladder.
        target_vn: rsleigh::Vn,
        /// Statically-known dispatch targets.
        targets: Vec<u64>,
        /// OPTIONAL pinned `NodeOutputId` for the dispatch value.
        /// `None` from the cfg builder; populated by the
        /// orchestrator's known-targets feedback path when
        /// available.  When `Some`, strider uses it directly
        /// instead of re-reading `target_vn`.
        target_value: Option<strider_ir::Value>,
    },
    /// `BranchIndirect` whose target the cfg-time mini-graph resolver
    /// (the installed `IndirectResolverFn`, canonical implementation:
    /// `strider_analyze::indirect_resolver::resolve_indirect_target`)
    /// could not prove.
    ///
    /// The region was finalised with this terminator instead of an
    /// error; the strider-level fixed-point loop runs the full
    /// optimizer pipeline over the lifted IR and IR-level indirect-branch resolution
    /// inspects the producer of `target_vn` in the optimised graph.
    /// At fixed point any remaining `UnresolvedIndirectBranch` regions
    /// surface as an "unresolved indirect branch" error.
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
        /// for any unresolved-indirect-branch error raised at fixed
        /// point.
        addr: PcodeInsnAddr,
    },
}

/// A basic block: a maximal straight-line sequence of pcode instructions
/// with a single entry point and (at most) one exit point.
///
/// Regions are the nodes of the `RegionGraph`.  A region ends when the
/// builder encounters a `Branch`, `CondBranch`, or `Return` pcode opcode, or
/// when sequential execution reaches the start of an already-discovered region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Address of the first pcode instruction in this region.
    pub start_addr: PcodeInsnAddr,
    /// All pcode instructions, in program order.  Empty only when the
    /// terminator is `Branch` and arose from the single-instruction
    /// CondBranch-with-OOB-successor fold (see `add_region` in the cfg
    /// builder).  Otherwise non-empty.
    pub insns: Vec<RegionInstruction>,
    /// How this region ends — see [`RegionTerminator`].
    pub terminator: RegionTerminator,
}

impl Region {
    /// Returns `true` when `addr` lies within the instruction range of this
    /// region, i.e. `start_addr <= addr <= last_insn.addr`.
    ///
    /// Empty regions (only valid for `Branch`-terminated post-fold cases —
    /// see [`Region::insns`]) own exactly their `start_addr`.  Returning
    /// `false` for empty regions previously made `find_region_containing_addr`
    /// miss the start-address query, letting the work queue build a duplicate
    /// region for the same edge target.
    #[must_use]
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        match self.insns.last() {
            Some(last) => self.start_addr <= addr && addr <= last.addr,
            None => self.start_addr == addr,
        }
    }
}

/// The directed graph type used to represent the CFG.
///
/// Nodes are [`Region`]s (basic blocks); edge weights are [`RegionEdgeKind`]
/// values that describe the type of control transfer.  `StableDiGraph` is
/// used so that `NodeIndex` values remain stable when regions are removed or
/// re-wired (e.g. during `split_region`).
pub(crate) type RegionGraph = StableDiGraph<Region, RegionEdgeKind>;
