use crate::error::{Error, Result};
use std::collections::{BTreeMap, HashMap, VecDeque};

use petgraph::{graph::NodeIndex, visit::EdgeRef};
use petgraph::stable_graph::StableDiGraph;
use dot::GraphDotDumper;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionEdgeKind {
    Fallthrough,
    Branch,
    IfCaseTrue,
    IfCaseFalse
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineInsnAddr {
    pub addr: u64,
}

impl From<u64> for MachineInsnAddr {
    fn from(value: u64) -> Self {
        MachineInsnAddr { addr: value }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PcodeInsnAddr {
    // NOTE: The order here is important - first is the address and only if it matches - check the insn_index 
    pub machine_addr: MachineInsnAddr,
    pub insn_index: u64
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInstruction {
    pub addr: PcodeInsnAddr,
    pub insn: rsleigh::Insn
}

#[derive(Debug)]
pub struct Cfg<R: rsleigh::MemReader> {
    pub sleigh: rsleigh::Sleigh<R>,
    pub graph: RegionGraph,
    pub entry: NodeIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start_addr: PcodeInsnAddr,
    pub insns: VecDeque<RegionInstruction>,
    // does this region end with a pcode branch insn that represents a tail call?
    pub ends_with_tail_call: bool
}

impl Region {
    pub fn contains_addr(&self, addr: PcodeInsnAddr) -> bool {
        self.start_addr <= addr && addr <= self.insns.back().expect("Region instructions can't be empty").addr
    }
}

/// the result type using our error.
pub type RegionGraph = StableDiGraph<Region ,RegionEdgeKind>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Options {
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool
}
impl Default for Options {
    fn default() -> Self {
        Self { fn_max_size: None, allow_code_before_start_addr: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OptionsBuilder {
    lifter_options: Options
}

impl OptionsBuilder {
    pub fn new() -> Self {
        OptionsBuilder {
            lifter_options: Options::default(),
        }
    }

    pub fn set_function_max_size(mut self, max_size: u64) -> Self {
        self.lifter_options.fn_max_size = Some(max_size);
        self
    }

    pub fn allow_code_before_start_addr(mut self) -> Self {
        self.lifter_options.allow_code_before_start_addr = true;
        self
    }

    pub fn build(self) -> Options {
        self.lifter_options
    }
}

pub struct Builder<R: rsleigh::MemReader> {
    sleigh: rsleigh::Sleigh<R>,
    start_addr: MachineInsnAddr,
    options: Options,
    graph: RegionGraph,
    start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>,
    work_queue: VecDeque<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)>
}

impl<R: rsleigh::MemReader>  Builder<R> {
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
    fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
        if region.insns.len() == 0 {
            return Err(Error::EmptyRegion(region))
        }        

        let start_addr = region.start_addr;
        let region_id = self.graph.add_node(region);
        self.start_addr_to_region_id.insert(start_addr, region_id);
        Ok(region_id)
    }

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

    #[inline]
    fn start_pcode_addr(&self) -> PcodeInsnAddr {
        PcodeInsnAddr{ machine_addr: self.start_addr, insn_index: 0 }
    }

    fn split_region(&mut self, region_id: NodeIndex, addr: PcodeInsnAddr) -> Result<NodeIndex> {
        // The idea here is to swap the region_id to be the **SECOND** region after the split and create a new one for the first one
        // Why? there are 4 things that break when we want to change the region_id
        // 1. The parents of the current region_id should be those of the first region - we will fix it by hand
        // 2. The children of the current region_id should be those of the second region - solved due to replacement
        // 3. The items in the queue that use region_id as parent should point to the second region - solved due to replacement
        // 4. The parent of the popped value from that called the split should also point to the second region

        let second_region = self.graph.node_weight_mut(region_id).expect("region id must be valid");
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

    fn explore(&mut self, parent_region: Option<(NodeIndex, RegionEdgeKind)>, addr: PcodeInsnAddr) -> Result<()> {
        let existing_region = self.find_region_containing_addr(addr);
        if let Some((region_id, region)) = existing_region {
            // This is the case that someone just refereneced our region - add an edge between them
            let (parent_region_id, edge_kind) = parent_region.expect("Every item except the first one must have a parent");
            // We checked and the address is within the current region and needs to start a new region
            // This means we reached here by jumpi to the middle of a region and the current region needs to be split in 2
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

    fn explore_new_region(&mut self, start_addr: PcodeInsnAddr, 
            parent_edge: Option<(NodeIndex, RegionEdgeKind)>) -> Result<()> {
        RegionBuilder {
            builder: self, start_addr, insns: VecDeque::new(), parent_edge
        }.build()?;
        Ok(())
    }

    pub fn build(mut self) -> Result<Cfg<R>> {
        self.work_queue.push_back((None, self.start_pcode_addr()));
        while let Some((parent_region, address)) = self.work_queue.pop_back() {
            self.explore(parent_region, address)?;
        }
        let (starting_region, _) = self.find_region_containing_addr(self.start_pcode_addr()).ok_or(Error::FailedCreatingStartRegion)?;

        Ok(Cfg { graph: self.graph, sleigh: self.sleigh, entry: starting_region })
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessInsnRes {
    FinishedProcessing,
    DidntFinishProcessing
}

struct RegionBuilder<'a, R: rsleigh::MemReader> {
    builder: &'a mut Builder<R>,
    start_addr: PcodeInsnAddr,
    insns: VecDeque<RegionInstruction>,
    parent_edge: Option<(NodeIndex, RegionEdgeKind)>
}

impl <R: rsleigh::MemReader> RegionBuilder<'_, R> {
    fn decode_branch_target(&mut self, branch_target_var: rsleigh::Vn, branch_insn_addr: PcodeInsnAddr) -> Result<PcodeInsnAddr> {
        let default_code_space = self.builder.sleigh.default_code_space();

        match branch_target_var.addr.space {
            // The case that the branch is relative
            rsleigh::VnSpace::CONST => Ok(PcodeInsnAddr { 
                machine_addr: branch_insn_addr.machine_addr,
                insn_index: branch_insn_addr.insn_index + branch_target_var.addr.off
            }),
            // The case that the branch is absolute
            space if space == default_code_space => Ok(PcodeInsnAddr{ 
                machine_addr: MachineInsnAddr { addr: branch_target_var.addr.off }, 
                insn_index: 0 
            }),
            _ => Err(Error::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr))
        }
    }

    fn is_branch_tail_call_nocheck(&mut self, branch_target_addr: PcodeInsnAddr) -> bool {
        // Only the machine insn address is relevant here for the bounds checking that we perform.
        // the pcode insn index does not matter.
        let addr = branch_target_addr.machine_addr;

        if addr < self.builder.start_addr && !self.builder.options.allow_code_before_start_addr {
            return true;
        }

        if let Some(fn_max_size) = self.builder.options.fn_max_size {
            if fn_max_size + self.builder.start_addr.addr <= addr.addr {
                return true;
            }
        }
        return false;
    }

    fn is_branch_tail_call(&mut self, branch_target_addr: PcodeInsnAddr) -> Result<bool> {
        let is_tail_call = self.is_branch_tail_call_nocheck(branch_target_addr);

        if is_tail_call {
            // Tail calls may only jump to the start of a machine insn. they can't jump to a specific pcode op inside the
            // target machine insn other than the first pcode op.
            if branch_target_addr.insn_index == 0 {
                return Err(Error::InvalidTailCall(branch_target_addr));
            }
        }

        Ok(is_tail_call)
    }

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


    fn process_insn(&mut self, insn: &rsleigh::Insn, addr: PcodeInsnAddr, lift_res: &rsleigh::LiftRes) -> Result<ProcessInsnRes> {
        let existing_region = self.builder.start_addr_to_region_id.get(&addr);
        // If we already processed the instruction - we fell through to an already processed region
        if let Some(region_id) = existing_region {
            let region_id = region_id.clone();
            // The parent region fallthroughs to this region  
            let region = self.finish_current_region(false)?;
            self.builder.graph.add_edge(region, region_id, RegionEdgeKind::Fallthrough);
            return Ok(ProcessInsnRes::FinishedProcessing);
        }
        return self.process_new_insn(insn, addr, lift_res);
    }
    
    fn build(mut self) -> Result<()> {
        let mut cur_addr = self.start_addr;
        loop {
            let lift_res = self.builder.sleigh.lift_one(cur_addr.machine_addr.addr)
                .map_err(|e| Error::GenericSleighError(format!("{:?}", e)))?;
            for (insn_index, insn) in lift_res.insns.iter().skip(cur_addr.insn_index as usize).enumerate() {
                cur_addr = PcodeInsnAddr { machine_addr: cur_addr.machine_addr, insn_index: insn_index as u64 };

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

pub type RegionId = NodeIndex;


pub struct IfRegionState {
    pub if_true_region: Option<NodeIndex>,
    pub if_false_region: Option<NodeIndex>
}

impl<R: rsleigh::MemReader>  Cfg<R>  {
    pub fn iterate_fallthroughs(&self) -> impl Iterator<Item = (NodeIndex, NodeIndex)> {
        use petgraph::visit::IntoEdgeReferences;
        self.graph.edge_references()
            .filter(|e| matches!(e.weight(), RegionEdgeKind::Fallthrough))
            .map(|e| (e.source(), e.target()))
    }

    fn following_regions(&self, region_id: RegionId) -> HashMap<&RegionEdgeKind, NodeIndex> {
        let mut next_regions = HashMap::new();
        for edge in self.graph.edges_directed(region_id, petgraph::Outgoing) {
            let kind = edge.weight();
            assert!(!next_regions.contains_key(kind), "region {region_id:?} contains more than 1 edge of type {kind:?}");
            next_regions.insert(kind, edge.target());
        }
        next_regions
    }

    pub fn region_branch(&self, region_id: RegionId) -> Option<NodeIndex> {
        let next_regions = self.following_regions(region_id);
        next_regions.get(&RegionEdgeKind::Branch).copied()
    }

    pub fn region_if(&self, region_id: RegionId) -> IfRegionState {
        let next_regions = self.following_regions(region_id);
        IfRegionState {
            if_true_region: next_regions.get(&RegionEdgeKind::IfCaseTrue).copied(),
            if_false_region: next_regions.get(&RegionEdgeKind::IfCaseFalse).copied()
        }
    }

    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.graph.node_weights()
    }

    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.graph.node_indices()
    }

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

    pub fn dot_dumper(&self) -> CfgDotDumper<'_, R> {
        CfgDotDumper(self)
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