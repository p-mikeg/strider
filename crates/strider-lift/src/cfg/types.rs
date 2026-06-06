use petgraph::stable_graph::StableDiGraph;

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
/// finalised by [`crate::cfg::Builder`].  The terminator is the single
/// source of truth for a region's control transfer: CFG edges are
/// unweighted (`StableDiGraph<Region, ()>`), so consumers that need to
/// know *how* a region exits — and, for a `CondBranch`, *which*
/// successor is the taken side — read the terminator, not the edge.
/// Some variants have no outgoing edge at all (`Return`, `TailCall`,
/// `NoReturn`, `UnresolvedIndirectBranch`).
///
/// `Switch` is constructed by `cfg::builder::region_builder` when the
/// indirect-branch resolver classifies a `BranchIndirect` as a
/// jump-table dispatch with a known multi-target set.  See the per-arm
/// doc on [`RegionTerminator::Switch`] for the construction contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionTerminator {
    /// Region ends without a conditional; the sole outgoing edge is the
    /// unconditional successor.  Constructed in three cases that the
    /// IR-level consumer (`link_region_edges`) treats identically:
    /// decoding fell into an already-discovered region, the region is
    /// the first half of a split, or the region closed on an explicit
    /// `Branch` opcode.
    Unconditional,
    /// Direct conditional branch.  The region has two outgoing edges; the
    /// one whose target region *contains* `true_target` is the taken
    /// (condition-true) side, the other is the fall-through.
    CondBranch {
        /// Address of the taken (condition-true) successor.  Stored as a
        /// full [`PcodeInsnAddr`] — not just a machine address — because an
        /// intra-machine-instruction `CBRANCH` can put the taken and
        /// fall-through successors at the same machine address with
        /// different pcode indices.  Consumers (`Cfg::region_if`,
        /// `CfgDotDumper`) recover the polarity by finding which outgoing
        /// edge's target region *contains* this address (not by matching
        /// `start_addr`, which can sit below a region's first instruction
        /// after a zero-pcode-op-hole `split_region`).
        true_target: PcodeInsnAddr,
    },
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
    /// (produced by the orchestrator's IR-level indirect-resolution
    /// loop and fed back via `known_targets`).  Strider's
    /// `handle_switch` reads
    /// `target_vn` at the region exit and emits an If-ladder of
    /// `IntCmpOp::Equal + If` against each `targets[i]`, chained
    /// through the false-branch.
    Switch {
        /// The dispatch varnode — the `BranchIndirect`'s
        /// `inputs[0]`.  Strider reads this at the region exit to
        /// obtain the lifted comparison value for the If-ladder.
        target_vn: rsleigh::Vn,
        /// Statically-known dispatch targets.
        targets: Vec<u64>,
    },
    /// `BranchIndirect` whose target is not yet known at cfg-build time.
    ///
    /// The region was finalised with this terminator; the strider-level
    /// fixed-point loop runs the full optimizer pipeline over the lifted
    /// IR and IR-level indirect-branch resolution inspects the producer
    /// of `target_vn` in the optimised graph.  Successful classifications
    /// are recorded in `known_targets` and materialised on the next CFG
    /// rebuild.  At fixed point any remaining `UnresolvedIndirectBranch`
    /// regions surface as an "unresolved indirect branch" error.
    ///
    /// This variant has **no outgoing edge**: the target is unknown
    /// at cfg-build time.  Strider lifts the region by emitting a
    /// placeholder `Return(target_value)` that anchors `target_vn`
    /// in the IR for analysis.
    UnresolvedIndirectBranch {
        /// The `inputs[0]` varnode of the offending `BranchIndirect`.
        /// Strider reads this varnode at the region exit to obtain
        /// the IR `ValueId` that anchors the placeholder Return.
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
    /// terminator is `Unconditional` and arose from the
    /// single-instruction CondBranch-with-OOB-successor fold (see
    /// `add_region` in the cfg builder).  Otherwise non-empty.
    pub insns: Vec<RegionInstruction>,
    /// How this region ends — see [`RegionTerminator`].
    pub terminator: RegionTerminator,
}

impl Region {
    /// Returns `true` when `addr` lies within the instruction range of this
    /// region, i.e. `start_addr <= addr <= last_insn.addr`.
    ///
    /// Empty regions (only valid for `Unconditional`-terminated post-fold cases —
    /// see [`Region::insns`]) own exactly their `start_addr`.  Returning
    /// `false` for empty regions previously made `find_region_containing_addr`
    /// miss the start-address query, letting the work queue build a duplicate
    /// region for the same edge target.
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        match self.insns.last() {
            Some(last) => self.start_addr <= addr && addr <= last.addr,
            None => self.start_addr == addr,
        }
    }
}

/// The directed graph type used to represent the CFG.
///
/// Nodes are [`Region`]s (basic blocks); edges are unweighted (`()`) — they
/// record only topology.  The kind of control transfer (and, for a
/// conditional branch, which successor is taken) is read from the source
/// region's [`RegionTerminator`].  `StableDiGraph` is used so that
/// `NodeIndex` values remain stable when regions are removed or re-wired
/// (e.g. during `split_region`).
pub(crate) type RegionGraph = StableDiGraph<Region, ()>;
