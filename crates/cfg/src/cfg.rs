use crate::error::{Error, Result};
use std::collections::{BTreeMap, HashMap, VecDeque};

use petgraph::{graph::NodeIndex, visit::EdgeRef};
use petgraph::stable_graph::StableDiGraph;
use dot::GraphDotDumper;

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
    IfCaseFalse
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
    pub insn_index: u64
}

/// A single pcode instruction together with its address inside the CFG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInstruction {
    /// Address of this pcode instruction.
    pub addr: PcodeInsnAddr,
    /// The decoded pcode instruction.
    pub insn: rsleigh::Insn
}

/// A completed Control Flow Graph for a single function.
///
/// Produced by [`Builder::build`].  The graph is a [`petgraph::stable_graph::StableDiGraph`]
/// where each node is a [`Region`] (basic block) and each edge is a
/// [`RegionEdgeKind`] (the type of control transfer).
#[derive(Debug)]
pub struct Cfg<R: rsleigh::MemReader> {
    /// The Sleigh context used during construction.  Retained so that
    /// register names can be resolved for visualisation.
    pub sleigh: rsleigh::Sleigh<R>,
    /// The underlying directed graph.  Nodes are regions; edges are labeled
    /// with [`RegionEdgeKind`].
    pub graph: RegionGraph,
    /// The [`NodeIndex`] of the function entry-point region.
    pub entry: NodeIndex,
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
    pub ends_with_tail_call: bool
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
/// used so that [`NodeIndex`] values remain stable when regions are removed or
/// re-wired (e.g. during [`split_region`](Builder::split_region)).
pub type RegionGraph = StableDiGraph<Region, RegionEdgeKind>;

/// Configuration that governs how [`Builder`] builds the CFG.
///
/// Construct via [`OptionsBuilder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Options {
    /// When `Some(n)`, any unconditional branch whose target lies at an
    /// address ≥ `start + n` is treated as a tail call.
    fn_max_size: Option<u64>,
    /// When `false` (the default), unconditional branches whose target
    /// address is *below* the function start are treated as tail calls.
    /// When `true`, such branches are followed normally.
    allow_code_before_start_addr: bool
}
impl Default for Options {
    fn default() -> Self {
        Self { fn_max_size: None, allow_code_before_start_addr: false }
    }
}

/// Builder for [`Options`].
///
/// # Example
/// ```rust,ignore
/// let opts = OptionsBuilder::new()
///     .set_function_max_size(0x1000)
///     .allow_code_before_start_addr()
///     .build();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OptionsBuilder {
    lifter_options: Options
}

impl OptionsBuilder {
    /// Creates an `OptionsBuilder` with all options at their defaults.
    pub fn new() -> Self {
        OptionsBuilder {
            lifter_options: Options::default(),
        }
    }

    /// Sets the maximum size (in bytes) of the function being analysed.
    ///
    /// Any unconditional branch whose target address is ≥ `start_addr + max_size`
    /// will be treated as a tail call.
    pub fn set_function_max_size(mut self, max_size: u64) -> Self {
        self.lifter_options.fn_max_size = Some(max_size);
        self
    }

    /// Allows the CFG builder to follow unconditional branches whose target
    /// address is below the function start address.
    ///
    /// By default such branches are classified as tail calls (they are
    /// assumed to leave the current function).  Enable this option when the
    /// binary layout places shared or out-of-order code before the entry point.
    pub fn allow_code_before_start_addr(mut self) -> Self {
        self.lifter_options.allow_code_before_start_addr = true;
        self
    }

    /// Consumes the builder and returns the final [`Options`].
    pub fn build(self) -> Options {
        self.lifter_options
    }
}

/// Incrementally constructs a [`Cfg`] from a binary entry point.
///
/// The builder uses a work-queue that is seeded with the entry address.
/// Items are popped one at a time; each item triggers decoding of a new
/// region (via [`RegionBuilder`]) or routing of an edge to an existing
/// region.  When a branch target lands in the middle of an already-decoded
/// region, that region is split in two.
///
/// # Usage
/// ```rust,ignore
/// let cfg = Builder::new(sleigh, fn_addr, opts).build()?;
/// ```
pub struct Builder<R: rsleigh::MemReader> {
    sleigh: rsleigh::Sleigh<R>,
    /// Virtual address at which the function entry point begins.
    start_addr: MachineInsnAddr,
    options: Options,
    /// The graph being constructed.
    graph: RegionGraph,
    /// Maps each region's `start_addr` to its [`NodeIndex`].
    /// Used by `find_region_containing_addr` and `split_region`.
    start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>,
    /// Pending addresses to explore, together with the parent edge they
    /// should connect from.  Processed LIFO (depth-first).
    work_queue: VecDeque<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)>
}

impl<R: rsleigh::MemReader> Builder<R> {
    /// Creates a new `Builder` that will construct a CFG starting at
    /// `start_addr` using `sleigh` to disassemble instructions.
    pub fn new(sleigh: rsleigh::Sleigh<R>, start_addr: u64, options: Options) -> Self {
        Self { 
            sleigh,
            start_addr: start_addr.into(),
            options,
            graph: RegionGraph::new(),
            start_addr_to_region_id: BTreeMap::new(),
            work_queue: VecDeque::new()
        }
    }
    /// Inserts `region` into the graph and records its start address in the
    /// lookup map.  Returns the assigned [`NodeIndex`].
    ///
    /// # Errors
    /// Returns [`Error::EmptyRegion`] if `region.insns` is empty.
    fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
        if region.insns.len() == 0 {
            return Err(Error::EmptyRegion(region))
        }

        let start_addr = region.start_addr;
        let region_id = self.graph.add_node(region);
        self.start_addr_to_region_id.insert(start_addr, region_id);
        Ok(region_id)
    }

    /// Finds the region that contains `addr`, if any.
    ///
    /// Uses a BTreeMap range query to find the last region whose
    /// `start_addr <= addr`, then confirms that `addr` also falls within the
    /// region's instruction range via [`Region::contains_addr`].
    fn find_region_containing_addr(&self, addr: PcodeInsnAddr) -> Option<(NodeIndex, &Region)> {
        // Find the last region whose start_addr <= addr
        let (_, &region_id) = self
            .start_addr_to_region_id
            .range(..=addr)
            .next_back()?;

        let region = self.graph.node_weight(region_id)?;
        if region.contains_addr(addr) {
            Some((region_id, region))
        } else {
            None
        }
    }

    /// Returns the pcode address corresponding to the function entry point.
    #[inline]
    fn start_pcode_addr(&self) -> PcodeInsnAddr {
        PcodeInsnAddr{ machine_addr: self.start_addr, insn_index: 0 }
    }

    /// Splits the region identified by `region_id` at `addr`, creating two
    /// regions:
    ///
    /// - **first**: instructions *before* `addr` — gets a new [`NodeIndex`].
    /// - **second**: instructions *from* `addr` onwards — **retains** `region_id`.
    ///
    /// Retaining `region_id` for the second half avoids having to update:
    /// - outgoing edges (children) of the original region, and
    /// - any work-queue entries that still reference `region_id` as a parent.
    ///
    /// The following fixups ARE performed manually:
    /// 1. Incoming edges (parents) are rewired to the first region.
    /// 2. A [`RegionEdgeKind::Fallthrough`] edge is added from first → second.
    /// 3. The `start_addr_to_region_id` map is updated for both halves.
    ///
    /// Returns `region_id` (the second region) on success, or `region_id`
    /// unchanged when `addr` is already the region start (no-op split).
    fn split_region(&mut self, region_id: NodeIndex, addr: PcodeInsnAddr) -> Result<NodeIndex> {
        // The idea here is to swap the region_id to be the **SECOND** region after the split and create a new one for the first one
        // Why? there are 4 things that break when we want to change the region_id
        // 1. The parents of the current region_id should be those of the first region - we will fix it by hand
        // 2. The children of the current region_id should be those of the second region - solved due to replacement
        // 3. The items in the queue that use region_id as parent should point to the second region - solved due to replacement
        // 4. The parent of the popped value from that called the split should also point to the second region

        let second_region = self.graph.node_weight_mut(region_id)
            .ok_or(Error::InvalidRegion(region_id))?;
        let split_index = second_region.insns.iter().position(|insn| insn.addr == addr)
            .ok_or(Error::FailedSplitingRegion(region_id, addr))?;
    
        if split_index == 0 {
            return Ok(region_id);
        }
        // split the insns in 2 based on the split index -  split_off stores the first part in place
        // so we should replace the 2 values
        let second_region_insns = second_region.insns.split_off(split_index);
        let first_region_insns = std::mem::replace(&mut second_region.insns, second_region_insns);
        let second_region_id = region_id;
        let first_region_start_addr = second_region.start_addr;
        second_region.start_addr = addr;


        // We need to update the region location in the mapping to get the correct one when accessed later
        self.start_addr_to_region_id.insert(second_region.start_addr, second_region_id);

        let first_region = self.add_region(Region { 
            start_addr: first_region_start_addr, 
            insns: first_region_insns, 
            ends_with_tail_call: false
        })?;


        // second region inherits all parents of the original region
        let parent_edges: Vec<_> = self.graph
            .edges_directed(second_region_id, petgraph::Incoming)
            .map(|e| (e.id(), e.source(), e.weight().clone()))
            .collect();

        // Move the parent edges to be in the first region instead of the first one
        for (edge_id, parent_id, edge_data) in parent_edges {
            // re-add edge from second_region to the child 
            self.graph.add_edge(parent_id, first_region, edge_data);

            // remove the original edge
            self.graph.remove_edge(edge_id);
        }
        // link the first and the second regions with fallthrough
        self.graph.add_edge(first_region, second_region_id, RegionEdgeKind::Fallthrough);
        Ok(second_region_id)
    }

    /// Routes `addr` to either an existing region or a new one.
    ///
    /// - If a region already contains `addr` at its *start*, just adds the
    ///   incoming edge.
    /// - If a region contains `addr` in its *interior*, calls [`split_region`](Self::split_region)
    ///   to split it and then adds the edge to the second half.
    /// - If no region contains `addr`, calls [`explore_new_region`](Self::explore_new_region).
    fn explore(&mut self, parent_region: Option<(NodeIndex, RegionEdgeKind)>, addr: PcodeInsnAddr) -> Result<()> {
        let existing_region = self.find_region_containing_addr(addr);
        if let Some((region_id, region)) = existing_region {
            // This is the case that someone just referenced our region - add an edge between them
            let (parent_region_id, edge_kind) = parent_region.ok_or(Error::MissingParentEdge)?;
            // We checked and the address is within the current region and needs to start a new region
            // This means we reached here by jumping to the middle of a region and the current region needs to be split in 2
            if region.start_addr != addr {
                // found a jump to the middle of the region. we need to split it.
                let second_region = self.split_region(region_id, addr)?;
                self.graph.add_edge(parent_region_id, second_region, edge_kind);
            } else {
                self.graph.add_edge(parent_region_id, region_id, edge_kind);
            }
        } else {
            // This is not an explored region - explore it
            self.explore_new_region(addr, parent_region)?;
        }
        Ok(())
    }

    /// Creates a [`RegionBuilder`] anchored at `start_addr` and decodes
    /// instructions until the region is complete.
    fn explore_new_region(&mut self, start_addr: PcodeInsnAddr,
            parent_edge: Option<(NodeIndex, RegionEdgeKind)>) -> Result<()> {
        RegionBuilder {
            builder: self, start_addr, insns: VecDeque::new(), parent_edge
        }.build()?;
        Ok(())
    }

    /// Builds and returns the completed [`Cfg`].
    ///
    /// Seeds the work queue with the entry address, processes items until the
    /// queue is empty, then locates the entry region.
    pub fn build(mut self) -> Result<Cfg<R>> {
        self.work_queue.push_back((None, self.start_pcode_addr()));
        while let Some((parent_region, address)) = self.work_queue.pop_back() {
            self.explore(parent_region, address)?;
        }
        let (starting_region, _) = self.find_region_containing_addr(self.start_pcode_addr()).ok_or(Error::FailedCreatingStartRegion)?;

        Ok(Cfg { graph: self.graph, sleigh: self.sleigh, entry: starting_region })
    }
}


/// Outcome of processing a single pcode instruction in [`RegionBuilder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessInsnRes {
    /// The instruction terminated the current region (branch, return, or
    /// fall-through into an already-existing region).
    FinishedProcessing,
    /// The instruction did not terminate the region; decoding continues.
    DidntFinishProcessing
}

/// Builds a single [`Region`] by decoding pcode instructions one at a time.
///
/// Created internally by [`Builder::explore_new_region`]; not part of the
/// public API.  Holds a mutable reference back to the parent [`Builder`] so
/// it can enqueue successor regions and call [`Builder::add_region`].
struct RegionBuilder<'a, R: rsleigh::MemReader> {
    /// Parent builder — used to access the Sleigh context, options, graph,
    /// and work queue.
    builder: &'a mut Builder<R>,
    /// Address of the first instruction this region will contain.
    start_addr: PcodeInsnAddr,
    /// Instructions accumulated so far.
    insns: VecDeque<RegionInstruction>,
    /// The edge from the predecessor region to this one, if any.
    /// `None` only for the function entry region.
    parent_edge: Option<(NodeIndex, RegionEdgeKind)>
}

impl<R: rsleigh::MemReader> RegionBuilder<'_, R> {
    /// Decodes a pcode branch-target varnode into a [`PcodeInsnAddr`].
    ///
    /// Pcode encodes branch targets in two ways:
    /// - **Relative** (`VnSpace::CONST`): the target is a pcode-instruction
    ///   index *offset* within the same machine instruction.
    /// - **Absolute** (default code space): the target is a raw virtual
    ///   address; the pcode index is implicitly 0 (start of machine insn).
    fn decode_branch_target(&mut self, branch_target_var: rsleigh::Vn, branch_insn_addr: PcodeInsnAddr) -> Result<PcodeInsnAddr> {
        let default_code_space = self.builder.sleigh.default_code_space();

        match branch_target_var.addr.space {
            // Relative branch: offset from the current pcode insn index
            rsleigh::VnSpace::CONST => Ok(PcodeInsnAddr {
                machine_addr: branch_insn_addr.machine_addr,
                insn_index: branch_insn_addr.insn_index + branch_target_var.addr.off
            }),
            // Absolute branch: the offset IS the target machine address
            space if space == default_code_space => Ok(PcodeInsnAddr{
                machine_addr: MachineInsnAddr { addr: branch_target_var.addr.off },
                insn_index: 0
            }),
            _ => Err(Error::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr))
        }
    }

    /// Checks whether `branch_target_addr` should be treated as a tail call
    /// using only address-bounds reasoning (no insn_index validation).
    ///
    /// A branch is a tail call if:
    /// - Its target lies *before* the function start AND
    ///   `allow_code_before_start_addr` is `false`, **OR**
    /// - `fn_max_size` is set AND the target lies at or beyond
    ///   `start_addr + fn_max_size`.
    fn is_branch_tail_call_nocheck(&mut self, branch_target_addr: PcodeInsnAddr) -> bool {
        // Only the machine insn address matters for bounds checking; the pcode
        // insn index is irrelevant here.
        let addr = branch_target_addr.machine_addr;

        if addr < self.builder.start_addr && !self.builder.options.allow_code_before_start_addr {
            return true;
        }

        if let Some(fn_max_size) = self.builder.options.fn_max_size {
            if fn_max_size + self.builder.start_addr.addr <= addr.addr {
                return true;
            }
        }
        false
    }

    /// Determines whether `branch_target_addr` is a tail call, validating the
    /// pcode insn index.
    ///
    /// A well-formed tail call must target the *first* pcode instruction of a
    /// machine instruction (`insn_index == 0`).  A branch whose address bounds
    /// indicate a tail call but whose `insn_index != 0` is malformed and
    /// returns [`Error::InvalidTailCall`].
    fn is_branch_tail_call(&mut self, branch_target_addr: PcodeInsnAddr) -> Result<bool> {
        let is_tail_call = self.is_branch_tail_call_nocheck(branch_target_addr);

        if is_tail_call {
            // Tail calls may only jump to the start of a machine insn. They
            // cannot target a specific pcode op inside a machine insn.
            if branch_target_addr.insn_index != 0 {
                return Err(Error::InvalidTailCall(branch_target_addr));
            }
        }

        Ok(is_tail_call)
    }

    /// Processes `insn` as a fresh instruction (not already in any region).
    ///
    /// Appends the instruction to the current region, then acts on the opcode:
    /// - `Branch`: classifies as tail call or enqueues the jump target.
    /// - `CondBranch`: enqueues both the taken and not-taken successors.
    /// - `Return`: ends the region.
    /// - Everything else: returns [`ProcessInsnRes::DidntFinishProcessing`].
    fn process_new_insn(&mut self, insn: &rsleigh::Insn, addr: PcodeInsnAddr, lift_res: &rsleigh::LiftRes) -> Result<ProcessInsnRes> {
        self.insns.push_back(RegionInstruction { addr, insn: insn.to_owned() });

        match insn.opcode {
            rsleigh::Opcode::Branch => {
                let branch_target_addr = self.decode_branch_target(insn.inputs[0], addr)?;
                if self.is_branch_tail_call(branch_target_addr)? {
                    // The tail call marks the end of control flow for this specific path.
                    let _region = self.finish_current_region(true)?;
                    Ok(ProcessInsnRes::FinishedProcessing)
                } else {
                    // We reached the end of the current bb but we know the next address to jump to so enqueue it
                    let region = self.finish_current_region(false)?;
                    self.builder.work_queue.push_back((Some((region, RegionEdgeKind::Branch)), branch_target_addr));
                    Ok(ProcessInsnRes::FinishedProcessing)
                }
            }
            rsleigh::Opcode::CondBranch => {
                let target_addr = self.decode_branch_target(insn.inputs[0], addr)?;

                // We reached the end of the current region
                let region = self.finish_current_region(false)?;

                // Add the true case
                self.builder.work_queue.push_back((Some((region, RegionEdgeKind::IfCaseTrue)), target_addr));
                // The false case requires calculation of the next instruction (is it in the current pcode instr or the next one)
                let next_insn_addr = if addr.insn_index + 1 == lift_res.insns.len() as u64 {
                    PcodeInsnAddr { 
                        machine_addr: MachineInsnAddr {addr: addr.machine_addr.addr + lift_res.machine_insn_len as u64},
                        insn_index: 0 
                    }
                } else {
                    PcodeInsnAddr { 
                        machine_addr: addr.machine_addr,
                        insn_index: addr.insn_index + 1
                    }
                };

                // Add the false case
                self.builder.work_queue.push_back((Some((region, RegionEdgeKind::IfCaseFalse)), next_insn_addr));
                Ok(ProcessInsnRes::FinishedProcessing)
            }
            rsleigh::Opcode::Return => {
                let _region = self.finish_current_region(false)?;
                Ok(ProcessInsnRes::FinishedProcessing)
            }
            _ => Ok(ProcessInsnRes::DidntFinishProcessing)
        }
    }

    /// Finalises the region that has been accumulating instructions.
    ///
    /// Calls [`Builder::add_region`] and, if there is a parent edge, adds
    /// that edge to the graph.  Returns the new region's [`NodeIndex`].
    fn finish_current_region(&mut self, ends_with_tail_call: bool) -> Result<NodeIndex> {
        if self.insns.len() == 0 {
            return Err(Error::NoInstructionsRegionBuilder);
        }
        let region = self.builder.add_region(
            Region { start_addr: self.start_addr, insns: self.insns.to_owned(), ends_with_tail_call }
        )?;
        if let Some((parent_id, edge_kind)) = self.parent_edge {
            self.builder.graph.add_edge(parent_id, region, edge_kind);
        }
        Ok(region)
    }


    /// Processes `insn` at `addr`, first checking whether `addr` is already
    /// the start of a known region.
    ///
    /// If so, the current region has fallen through into an already-explored
    /// region: the current region is finalised and a
    /// [`RegionEdgeKind::Fallthrough`] edge is added to the existing region.
    /// Otherwise delegates to [`process_new_insn`](Self::process_new_insn).
    fn process_insn(&mut self, insn: &rsleigh::Insn, addr: PcodeInsnAddr, lift_res: &rsleigh::LiftRes) -> Result<ProcessInsnRes> {
        let existing_region = self.builder.start_addr_to_region_id.get(&addr);
        // If we already processed the instruction - we fell through to an already processed region
        if let Some(region_id) = existing_region {
            let region_id = *region_id;
            // The parent region falls through to this region
            let region = self.finish_current_region(false)?;
            self.builder.graph.add_edge(region, region_id, RegionEdgeKind::Fallthrough);
            return Ok(ProcessInsnRes::FinishedProcessing);
        }
        self.process_new_insn(insn, addr, lift_res)
    }

    /// Main decode loop: lifts machine instructions one at a time and calls
    /// [`process_insn`](Self::process_insn) for each pcode instruction until
    /// the region is complete.
    ///
    /// # Pcode index accounting
    ///
    /// When a region starts at a non-zero pcode index (because a relative
    /// `CondBranch` branched into the middle of a machine instruction's pcode
    /// sequence), `cur_addr.insn_index` may be > 0 at the top of the first
    /// iteration.  We must add that base offset to the `enumerate` counter so
    /// that every `RegionInstruction` carries the correct
    /// `(machine_addr, insn_index)` pair.  Subsequent machine instructions
    /// always start at pcode index 0, so the offset resets naturally.
    fn build(mut self) -> Result<()> {
        let mut cur_addr = self.start_addr;
        loop {
            let lift_res = self.builder.sleigh.lift_one(cur_addr.machine_addr.addr)
                .map_err(|e| Error::GenericSleighError(format!("{:?}", e)))?;
            // Save the starting pcode index for this machine instruction.
            // For the first machine instruction this may be non-zero when the
            // work queue delivered a mid-instruction entry point.  For all
            // subsequent machine instructions it is always 0.
            let start_pcode_idx = cur_addr.insn_index;
            for (i, insn) in lift_res.insns.iter().skip(start_pcode_idx as usize).enumerate() {
                cur_addr = PcodeInsnAddr {
                    machine_addr: cur_addr.machine_addr,
                    insn_index: start_pcode_idx + i as u64,
                };

                let res = self.process_insn(insn, cur_addr, &lift_res)?;
                if matches!(res, ProcessInsnRes::FinishedProcessing) {
                    return Ok(());
                }
            }
            // We're done exploring a single machine insn, continue to the next one
            cur_addr = PcodeInsnAddr {
                machine_addr: MachineInsnAddr { addr: cur_addr.machine_addr.addr + (lift_res.machine_insn_len as u64) },
                insn_index: 0,
            };
        }
    }
}

/// Type alias for the petgraph [`NodeIndex`] used to identify regions.
pub type RegionId = NodeIndex;

/// The two successors of a conditional-branch region.
///
/// Returned by [`Cfg::region_if`].
pub struct IfRegionState {
    /// Region reached when the branch condition is *true*, if present.
    pub if_true_region: Option<NodeIndex>,
    /// Region reached when the branch condition is *false* (fall-through), if present.
    pub if_false_region: Option<NodeIndex>
}

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Iterates all [`RegionEdgeKind::Fallthrough`] edges as `(src, tgt)` pairs.
    pub fn iterate_fallthroughs(&self) -> impl Iterator<Item = (NodeIndex, NodeIndex)> {
        use petgraph::visit::IntoEdgeReferences;
        self.graph.edge_references()
            .filter(|e| matches!(e.weight(), RegionEdgeKind::Fallthrough))
            .map(|e| (e.source(), e.target()))
    }

    /// Collects all outgoing edges from `region_id` into a map keyed by edge kind.
    ///
    /// # Errors
    /// Returns [`Error::DuplicateEdgeKind`] if the same edge kind appears more
    /// than once on a single region (which would indicate a malformed CFG).
    fn following_regions(&self, region_id: RegionId) -> Result<HashMap<&RegionEdgeKind, NodeIndex>> {
        let mut next_regions = HashMap::new();
        for edge in self.graph.edges_directed(region_id, petgraph::Outgoing) {
            let kind = edge.weight();
            if next_regions.contains_key(kind) {
                return Err(Error::DuplicateEdgeKind(region_id, *kind));
            }
            next_regions.insert(kind, edge.target());
        }
        Ok(next_regions)
    }

    /// Returns the unconditional-branch successor of `region_id`, if any.
    ///
    /// # Errors
    /// Returns an error if the CFG graph is malformed (duplicate edge kinds).
    pub fn region_branch(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        let next_regions = self.following_regions(region_id)?;
        Ok(next_regions.get(&RegionEdgeKind::Branch).copied())
    }

    /// Returns both conditional-branch successors of `region_id`.
    ///
    /// # Errors
    /// Returns an error if the CFG graph is malformed (duplicate edge kinds).
    pub fn region_if(&self, region_id: RegionId) -> Result<IfRegionState> {
        let next_regions = self.following_regions(region_id)?;
        Ok(IfRegionState {
            if_true_region: next_regions.get(&RegionEdgeKind::IfCaseTrue).copied(),
            if_false_region: next_regions.get(&RegionEdgeKind::IfCaseFalse).copied()
        })
    }

    /// Iterates over all [`Region`]s in the CFG (unordered).
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.graph.node_weights()
    }

    /// Iterates over the [`RegionId`] of every region in the CFG (unordered).
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.graph.node_indices()
    }

    /// Returns the pcode instructions contained in `region_id`.
    ///
    /// # Errors
    /// Returns [`Error::InvalidRegion`] when `region_id` does not exist.
    pub fn region_insn(&self, region_id: NodeIndex) -> Result<Vec<rsleigh::Insn>> {
        let region = self.graph.node_weight(region_id).ok_or(Error::InvalidRegion(region_id))?;
        Ok(region.insns.iter().map(|region_insn| region_insn.insn.clone()).collect())
    }

    fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
        let offset = vn.addr.off;
        let size = vn.size;
        match vn.addr.space {
            rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
            rsleigh::VnSpace::REGISTER => {
                let regs = self.sleigh.regs().map_err(|e| Error::SleighError(e))?;
                Ok(regs.vn_to_name(*vn).ok_or(Error::InvalidRegVn(*vn))?.to_string())
            },
            rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
            rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
            _ => unreachable!()
        }
    }

    /// Returns a [`GraphDotDumper`] that can render this CFG as a DOT/HTML file.
    pub fn dot_dumper(&self) -> CfgDotDumper<'_, R> {
        CfgDotDumper(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petgraph::visit::{EdgeRef, IntoEdgeReferences};

    // ── test helpers ──────────────────────────────────────────────────────────

    /// Short constructor for a [`PcodeInsnAddr`].
    fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: machine },
            insn_index: insn,
        }
    }

    /// Returns a minimal Sleigh backed by an empty buffer.
    ///
    /// The resulting Sleigh cannot actually decode instructions (the buffer is
    /// empty) but is sufficient for constructing a [`Builder`] and testing all
    /// methods that do not call `lift_one`.
    fn make_sleigh() -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        rsleigh::Sleigh::new(
            rsleigh::sla_spec::SLA_SPEC_X86,
            rsleigh::pspec::PSPEC_X86,
            reader,
        )
        .expect("failed to create test Sleigh")
    }

    /// Returns a [`Builder`] seeded at `start_addr` with default options.
    fn make_builder(start_addr: u64) -> Builder<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
        Builder::new(make_sleigh(), start_addr, OptionsBuilder::new().build())
    }

    /// Returns a [`Builder`] seeded at `start_addr` with the given `options`.
    fn make_builder_opts(
        start_addr: u64,
        options: Options,
    ) -> Builder<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
        Builder::new(make_sleigh(), start_addr, options)
    }

    /// A minimal dummy pcode instruction (no inputs/outputs, opcode = Copy).
    fn fake_insn() -> rsleigh::Insn {
        rsleigh::Insn { opcode: rsleigh::Opcode::Copy, output: None, inputs: vec![] }
    }

    /// Builds a [`Region`] from a list of `(machine_addr, insn_index)` pairs.
    ///
    /// The first pair is used as `start_addr`; all pairs become instructions.
    /// Panics if `addrs` is empty.
    fn make_region(addrs: &[(u64, u64)]) -> Region {
        assert!(!addrs.is_empty(), "make_region requires at least one address");
        let start = addr(addrs[0].0, addrs[0].1);
        let insns = addrs
            .iter()
            .map(|&(m, i)| RegionInstruction { addr: addr(m, i), insn: fake_insn() })
            .collect();
        Region { start_addr: start, insns, ends_with_tail_call: false }
    }

    /// Builds a [`RegionBuilder`] for `builder` that starts at `start`.
    fn make_region_builder<'a>(
        builder: &'a mut Builder<rsleigh::mem_readers::BufMemReader<Vec<u8>>>,
        start: PcodeInsnAddr,
    ) -> RegionBuilder<'a, rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
        RegionBuilder { builder, start_addr: start, insns: VecDeque::new(), parent_edge: None }
    }

    // ── MachineInsnAddr ───────────────────────────────────────────────────────

    /// `MachineInsnAddr` must implement `From<u64>` and round-trip correctly.
    #[test]
    fn machine_insn_addr_from_u64() {
        let a: MachineInsnAddr = 0x1000u64.into();
        assert_eq!(a.addr, 0x1000);
    }

    /// Addresses derived from different `u64` values must compare correctly.
    #[test]
    fn machine_insn_addr_ordering() {
        let lo: MachineInsnAddr = 0x100u64.into();
        let hi: MachineInsnAddr = 0x200u64.into();
        assert!(lo < hi);
        assert!(hi > lo);
        assert_eq!(lo, lo);
    }

    // ── PcodeInsnAddr ordering ────────────────────────────────────────────────

    /// The primary sort key is the machine address: a higher machine address
    /// sorts after a lower one regardless of pcode instruction index.
    #[test]
    fn pcode_addr_orders_by_machine_addr_first() {
        assert!(addr(200, 0) > addr(100, 99));
    }

    /// When machine addresses are equal the pcode index is the tiebreaker.
    #[test]
    fn pcode_addr_orders_by_insn_index_when_machine_addr_equal() {
        assert!(addr(100, 1) > addr(100, 0));
        assert!(addr(100, 5) > addr(100, 4));
        assert_eq!(addr(100, 3), addr(100, 3));
    }

    /// Ordering must be antisymmetric: `a < b` implies `b > a`.
    #[test]
    fn pcode_addr_ordering_is_antisymmetric() {
        let a = addr(0x400, 2);
        let b = addr(0x400, 5);
        assert!(a < b);
        assert!(b > a);
    }

    /// Equal addresses compare equal under both `==` and `Ord`.
    #[test]
    fn pcode_addr_equality() {
        let a = addr(0x1000, 7);
        let b = addr(0x1000, 7);
        assert_eq!(a, b);
        assert!(!(a < b));
        assert!(!(a > b));
    }

    // ── OptionsBuilder ────────────────────────────────────────────────────────

    /// Defaults: no size limit, backward branches treated as tail calls.
    #[test]
    fn options_builder_defaults() {
        let opts = OptionsBuilder::new().build();
        assert_eq!(opts.fn_max_size, None);
        assert!(!opts.allow_code_before_start_addr);
    }

    /// `set_function_max_size` stores the value without touching the other flag.
    #[test]
    fn options_builder_set_fn_max_size() {
        let opts = OptionsBuilder::new().set_function_max_size(0x1000).build();
        assert_eq!(opts.fn_max_size, Some(0x1000));
        assert!(!opts.allow_code_before_start_addr);
    }

    /// `allow_code_before_start_addr` flips its flag without touching `fn_max_size`.
    #[test]
    fn options_builder_allow_code_before_start_addr() {
        let opts = OptionsBuilder::new().allow_code_before_start_addr().build();
        assert!(opts.allow_code_before_start_addr);
        assert_eq!(opts.fn_max_size, None);
    }

    /// Both options can be set together and are stored independently.
    #[test]
    fn options_builder_both_options_set() {
        let opts = OptionsBuilder::new()
            .set_function_max_size(0x2000)
            .allow_code_before_start_addr()
            .build();
        assert_eq!(opts.fn_max_size, Some(0x2000));
        assert!(opts.allow_code_before_start_addr);
    }

    // ── RegionEdgeKind ────────────────────────────────────────────────────────

    /// All four edge-kind variants must be pairwise distinct.
    #[test]
    fn region_edge_kind_variants_are_distinct() {
        let kinds = [
            RegionEdgeKind::Fallthrough,
            RegionEdgeKind::Branch,
            RegionEdgeKind::IfCaseTrue,
            RegionEdgeKind::IfCaseFalse,
        ];
        for i in 0..kinds.len() {
            for j in (i + 1)..kinds.len() {
                assert_ne!(kinds[i], kinds[j]);
            }
        }
    }

    // ── Region::contains_addr ─────────────────────────────────────────────────

    /// The start address is inside the region.
    #[test]
    fn region_contains_addr_at_start() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(r.contains_addr(addr(0x1000, 0)));
    }

    /// The last instruction address is inside the region.
    #[test]
    fn region_contains_addr_at_end() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(r.contains_addr(addr(0x1010, 0)));
    }

    /// An address that lies strictly between start and end is inside the region.
    /// (`contains_addr` uses the lexicographic range, not the instruction list.)
    #[test]
    fn region_contains_addr_in_interior() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(r.contains_addr(addr(0x1008, 0)));
    }

    /// A pcode-index sub-address between two instructions is inside the region.
    #[test]
    fn region_contains_addr_pcode_interior() {
        // insns at (0x1000,0) and (0x1000,3); index 1 is in between
        let r = make_region(&[(0x1000, 0), (0x1000, 3)]);
        assert!(r.contains_addr(addr(0x1000, 1)));
    }

    /// An address strictly before the start is outside the region.
    #[test]
    fn region_contains_addr_before_start() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(!r.contains_addr(addr(0x0ff8, 0)));
    }

    /// An address strictly after the last instruction is outside the region.
    #[test]
    fn region_contains_addr_after_end() {
        let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
        assert!(!r.contains_addr(addr(0x1014, 0)));
    }

    // ── Builder::add_region ───────────────────────────────────────────────────

    /// Adding a valid region returns a `NodeIndex` and registers the region
    /// in both the graph and the address→id map.
    #[test]
    fn add_region_inserts_into_graph_and_map() {
        let mut b = make_builder(0x1000);
        let r = make_region(&[(0x1000, 0), (0x1004, 0)]);
        let id = b.add_region(r).unwrap();

        assert!(b.graph.node_weight(id).is_some());
        assert_eq!(b.start_addr_to_region_id.get(&addr(0x1000, 0)), Some(&id));
    }

    /// Adding an empty region must return `Error::EmptyRegion`.
    #[test]
    fn add_region_empty_returns_error() {
        let mut b = make_builder(0x1000);
        let empty = Region {
            start_addr: addr(0x1000, 0),
            insns: VecDeque::new(),
            ends_with_tail_call: false,
        };
        assert!(matches!(b.add_region(empty), Err(crate::Error::EmptyRegion(_))));
    }

    /// Adding two non-overlapping regions places both in the graph.
    #[test]
    fn add_region_two_regions_both_present() {
        let mut b = make_builder(0x1000);
        let r1 = make_region(&[(0x1000, 0)]);
        let r2 = make_region(&[(0x1010, 0)]);
        let id1 = b.add_region(r1).unwrap();
        let id2 = b.add_region(r2).unwrap();

        assert_ne!(id1, id2);
        assert_eq!(b.graph.node_count(), 2);
        assert_eq!(b.start_addr_to_region_id[&addr(0x1000, 0)], id1);
        assert_eq!(b.start_addr_to_region_id[&addr(0x1010, 0)], id2);
    }

    // ── Builder::find_region_containing_addr ──────────────────────────────────

    /// Returns `None` when no regions have been added.
    #[test]
    fn find_region_empty_graph() {
        let b = make_builder(0x1000);
        assert!(b.find_region_containing_addr(addr(0x1000, 0)).is_none());
    }

    /// Finds a region when queried exactly at its start address.
    #[test]
    fn find_region_at_start_addr() {
        let mut b = make_builder(0x1000);
        let id = b.add_region(make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
        assert_eq!(b.find_region_containing_addr(addr(0x1000, 0)).map(|(i, _)| i), Some(id));
    }

    /// Finds a region when queried at an interior address.
    #[test]
    fn find_region_at_interior_addr() {
        let mut b = make_builder(0x1000);
        let id = b.add_region(make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
        assert_eq!(b.find_region_containing_addr(addr(0x1008, 0)).map(|(i, _)| i), Some(id));
    }

    /// Finds a region when queried exactly at its last instruction.
    #[test]
    fn find_region_at_last_insn() {
        let mut b = make_builder(0x1000);
        let id = b.add_region(make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
        assert_eq!(b.find_region_containing_addr(addr(0x100f, 0)).map(|(i, _)| i), Some(id));
    }

    /// Returns `None` for an address beyond the region's last instruction.
    #[test]
    fn find_region_beyond_end_returns_none() {
        let mut b = make_builder(0x1000);
        b.add_region(make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
        assert!(b.find_region_containing_addr(addr(0x1020, 0)).is_none());
    }

    /// With two adjacent regions, each query is routed to the correct region.
    #[test]
    fn find_region_two_adjacent_regions_correct_routing() {
        let mut b = make_builder(0x1000);
        let id1 = b.add_region(make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
        let id2 = b.add_region(make_region(&[(0x1010, 0), (0x1020, 0)])).unwrap();

        assert_eq!(b.find_region_containing_addr(addr(0x1004, 0)).map(|(i, _)| i), Some(id1));
        assert_eq!(b.find_region_containing_addr(addr(0x1010, 0)).map(|(i, _)| i), Some(id2));
        assert_eq!(b.find_region_containing_addr(addr(0x1018, 0)).map(|(i, _)| i), Some(id2));
    }

    // ── Builder::split_region ─────────────────────────────────────────────────

    /// Splitting at the region's own start address is a no-op and returns the
    /// original `NodeIndex`.
    #[test]
    fn split_region_at_start_is_noop() {
        let mut b = make_builder(0x1000);
        let id = b.add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)])).unwrap();
        let result = b.split_region(id, addr(0x1000, 0)).unwrap();

        assert_eq!(result, id, "split at start must return original id");
        assert_eq!(b.graph.node_count(), 1, "no new region should be created");
    }

    /// Splitting at an interior address produces two regions.  The second half
    /// keeps the original `NodeIndex`; a new id is created for the first half.
    #[test]
    fn split_region_creates_two_regions() {
        let mut b = make_builder(0x1000);
        let original = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0), (0x100c, 0)]))
            .unwrap();
        let second = b.split_region(original, addr(0x1008, 0)).unwrap();

        // The second half keeps the original NodeIndex
        assert_eq!(second, original);
        assert_eq!(b.graph.node_count(), 2);
    }

    /// After a split the second region starts at the split address and the
    /// first region ends just before it.
    #[test]
    fn split_region_correct_addr_ranges() {
        let mut b = make_builder(0x1000);
        let original = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0), (0x100c, 0)]))
            .unwrap();
        b.split_region(original, addr(0x1008, 0)).unwrap();

        // second half (original id) starts at split point
        assert_eq!(b.graph[original].start_addr, addr(0x1008, 0));
        assert_eq!(b.graph[original].insns.len(), 2);

        // first half starts at the original start
        let first_id = b
            .start_addr_to_region_id[&addr(0x1000, 0)];
        assert_eq!(b.graph[first_id].start_addr, addr(0x1000, 0));
        assert_eq!(b.graph[first_id].insns.len(), 2);
    }

    /// A `Fallthrough` edge must connect the first half to the second half
    /// after the split.
    #[test]
    fn split_region_adds_fallthrough_edge() {
        let mut b = make_builder(0x1000);
        let original = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
            .unwrap();
        b.split_region(original, addr(0x1008, 0)).unwrap();

        let edges: Vec<_> = b.graph.edge_references().collect();
        assert_eq!(edges.len(), 1, "exactly one edge after split");
        assert_eq!(*edges[0].weight(), RegionEdgeKind::Fallthrough);
        assert_eq!(edges[0].target(), original, "edge must point to the second half");
    }

    /// Incoming edges to the original region are rewired to the first half.
    #[test]
    fn split_region_rewires_incoming_edges() {
        let mut b = make_builder(0x1000);
        // Region A (parent)
        let a = b.add_region(make_region(&[(0x0ff0, 0)])).unwrap();
        // Region B (to be split); A → B via Branch
        let b_id = b.add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)])).unwrap();
        b.graph.add_edge(a, b_id, RegionEdgeKind::Branch);

        // Split B at 0x1004
        b.split_region(b_id, addr(0x1004, 0)).unwrap();

        // The original incoming Branch edge must now point to the first half
        let first = b.start_addr_to_region_id[&addr(0x1000, 0)];
        let incoming: Vec<_> = b.graph
            .edges_directed(first, petgraph::Incoming)
            .collect();
        assert_eq!(incoming.len(), 1);
        assert_eq!(*incoming[0].weight(), RegionEdgeKind::Branch);
        assert_eq!(incoming[0].source(), a);

        // The second half (b_id) must NOT have the old Branch incoming edge
        let second_incoming: Vec<_> = b.graph
            .edges_directed(b_id, petgraph::Incoming)
            .filter(|e| *e.weight() == RegionEdgeKind::Branch)
            .collect();
        assert!(second_incoming.is_empty());
    }

    // ── RegionBuilder::is_branch_tail_call_nocheck ────────────────────────────

    /// A target below the function start is a tail call (default options).
    #[test]
    fn tail_call_nocheck_below_start_default_opts() {
        let mut b = make_builder(0x1000);
        let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
        assert!(rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
    }

    /// When `allow_code_before_start_addr` is set, a below-start target is NOT
    /// treated as a tail call.
    #[test]
    fn tail_call_nocheck_below_start_with_allow() {
        let opts = OptionsBuilder::new().allow_code_before_start_addr().build();
        let mut b = make_builder_opts(0x1000, opts);
        let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
        assert!(!rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
    }

    /// A target within the function is never a tail call when no size limit is set.
    #[test]
    fn tail_call_nocheck_within_function_no_limit() {
        let mut b = make_builder(0x1000);
        let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
        assert!(!rb.is_branch_tail_call_nocheck(addr(0x1200, 0)));
    }

    /// A target at exactly `start + fn_max_size` is a tail call.
    #[test]
    fn tail_call_nocheck_at_fn_max_size_boundary() {
        let opts = OptionsBuilder::new().set_function_max_size(0x100).build();
        let mut b = make_builder_opts(0x1000, opts);
        let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
        // 0x1100 == start(0x1000) + max_size(0x100) → tail call
        assert!(rb.is_branch_tail_call_nocheck(addr(0x1100, 0)));
        // 0x10ff is still inside the function
        assert!(!rb.is_branch_tail_call_nocheck(addr(0x10ff, 0)));
    }

    // ── RegionBuilder::is_branch_tail_call ────────────────────────────────────

    /// A tail-call target with `insn_index == 0` is valid and returns `Ok(true)`.
    #[test]
    fn tail_call_valid_insn_index_zero() {
        let mut b = make_builder(0x1000);
        let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
        // target is below start → tail call; insn_index == 0 → valid
        assert!(matches!(rb.is_branch_tail_call(addr(0x0800, 0)), Ok(true)));
    }

    /// A tail-call target with `insn_index != 0` is malformed and returns an error.
    #[test]
    fn tail_call_invalid_insn_index_nonzero() {
        let mut b = make_builder(0x1000);
        let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
        // target is below start → tail call; insn_index != 0 → error
        assert!(matches!(
            rb.is_branch_tail_call(addr(0x0800, 3)),
            Err(crate::Error::InvalidTailCall(_))
        ));
    }

    /// A target inside the function is not a tail call and returns `Ok(false)`,
    /// regardless of `insn_index`.
    #[test]
    fn tail_call_inside_function_returns_false() {
        let mut b = make_builder(0x1000);
        let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
        assert!(matches!(rb.is_branch_tail_call(addr(0x1200, 7)), Ok(false)));
    }
}

pub struct CfgDotDumperState;
pub struct CfgDotDumper<'a, R: rsleigh::MemReader>(&'a Cfg<R>);

impl <'a, R: rsleigh::MemReader> GraphDotDumper for CfgDotDumper<'a, R> {
    type Node = NodeIndex;
    type Error = Error;
    type State = CfgDotDumperState;

    fn create_initial_state(&self) -> Self::State {
        Self::State {}
    }

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        self.0.graph.node_indices()
    }

    fn dump_as_dot(&self, node_id: Self::Node, out: &mut dot::DotEmitter, _state: &mut Self::State) -> Result<()> {
        use std::fmt::Write;

        let dot_id = node_id.index().to_string();
        let node = self.0.graph.node_weight(node_id).ok_or(Error::InvalidRegion(node_id))?;
        let first_insn_index = node.insns.front().ok_or(Error::EmptyRegion(node.clone()))?.addr.insn_index;
        let start_addr = node.start_addr.machine_addr.addr;

        // Build node label once
        let mut label = format!("Instruction(addr={start_addr:#x}, idx={first_insn_index})\n");

        for insn in node.insns.iter() {
            let variables: Vec<String> = insn.insn.output.iter().chain(insn.insn.inputs.iter())
                .map(|vn| self.0.vn_to_name(vn)).collect::<Result<_>>()?;
            let insn_addr = insn.addr.machine_addr.addr;
            write!(&mut label, "\\l{insn_addr:#x}: {:?}", insn.insn.opcode).map_err(|e| Error::FormatError(e))?;
            if variables.len() > 0 {
                write!(&mut label, ", {}", variables.join(", ")).map_err(|e| Error::FormatError(e))?;
            }
        }
        write!(&mut label, "\\l").map_err(|e| Error::FormatError(e))?;

        // Add node
        out.node(&dot_id, &label, "box", &[]);

        // Incoming edges
        for edge in self.0.graph.edges_directed(node_id, petgraph::Incoming) {
            let src_id = edge.source().index().to_string();
            let edge_label = format!("{:?}", edge.weight());
            let edge_style = match edge.weight() {
                RegionEdgeKind::Branch => "bold",
                RegionEdgeKind::Fallthrough => "solid",
                RegionEdgeKind::IfCaseFalse | RegionEdgeKind::IfCaseTrue => "dashed"
            };
            out.edge(
                &src_id,
                &dot_id,
                &[("label", edge_label.as_str()), ("style", edge_style)],
            );
        }

        Ok(())
    }
}