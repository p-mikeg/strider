use std::collections::HashMap;

use crate::builder_ext::{BuilderExt, GraphBuilder};
use crate::builder_ext::builder::Builder;
use crate::builder_ext::control::BuiltControl;
use crate::builder_ext::memory::{BuiltMemory};
use crate::dot::GraphDotDumper;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType, Var};
use crate::builder_ext::{FunctionBody, 
        IntBuilderExt, BoolBuilderExt, MemoryBuilderExt, ControlBuilderExt};
use crate::graph::Graph;
use cranelift_entity::{PrimaryMap, SecondaryMap, entity_impl};
use rsleigh::MemReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockId(u32);
entity_impl!(BlockId);


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarId(u32);
entity_impl!(VarId);


#[derive(Debug, Clone, PartialEq, Eq)]
struct Block {
    terminated: bool,
    control_region: BuiltControl,
    memory_region: BuiltMemory,
    last_ctrl: NodeOutputId,
    last_memory: NodeOutputId,
    variables: SecondaryMap<VarId, NodeOutputId>,
    initial_variables: SecondaryMap<VarId, NodeOutputId>,
}

pub struct FunctionBuilder {
    function_body: FunctionBody,
    blocks: PrimaryMap<BlockId, Block>,
    variables: PrimaryMap<VarId, Var>,
    variable_to_id: HashMap<Var, VarId>,
    cur_block: Option<BlockId>,
}

impl Builder for FunctionBuilder {
    fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        self.builder().create_node(kind, inputs, output_kinds)
    }

    fn body(&self) -> &FunctionBody {
        &self.function_body
    }

    fn body_mut(&mut self) -> &mut FunctionBody {
        &mut self.function_body
    }
}

impl FunctionBuilder {
    pub fn new(all_used_variables: Vec<Var>) -> Self {
        let mut variables = PrimaryMap::new();
        let mut variable_to_id = HashMap::new();
        for variable in all_used_variables {
            let var_id = variables.push(variable);
            variable_to_id.insert(variable, var_id);
        }
        FunctionBuilder {
            function_body: FunctionBody::new_invalid(),
            blocks: PrimaryMap::new(),
            cur_block: None,
            variables,
            variable_to_id
        }
    }

    fn cur_block_ctrl(&self) -> NodeOutputId {
        self.blocks[self.require_cur_block()].last_ctrl
    }

    fn cur_block_memory(&self) -> NodeOutputId {
        self.blocks[self.require_cur_block()].last_memory
    }

    fn require_cur_block(&self) -> BlockId {
        let block = self.cur_block.expect("current block not set");
        assert!(
            !self.blocks[block].terminated,
            "attempted to insert into terminated block {}",
            block.as_u32()
        );
        block
    }

    fn graph(&self) -> &Graph {
        &self.body().graph
    }

    fn graph_mut(&mut self) -> &mut Graph {
        &mut self.body_mut().graph
    }

    fn builder(&mut self) -> GraphBuilder<'_> {
        GraphBuilder(&mut self.function_body)
    }

    fn advance_cur_block_ctrl(&mut self, ctrl: NodeOutputId) {
        assert!(self.graph().output_kind(ctrl).is_control());
        let block = self.require_cur_block();
        self.blocks[block].last_ctrl = ctrl;
    }

    fn advance_cur_block_memory(&mut self, memory: NodeOutputId) {
        assert!(self.graph().output_kind(memory).is_memory());
        let block = self.require_cur_block();
        self.blocks[block].last_memory = memory;
    }

    fn terminate_cur_block(&mut self) -> NodeOutputId {
        let cur_block = self.require_cur_block();
        let ctrl = self.blocks[cur_block].last_ctrl;
        self.blocks[cur_block].terminated = true;
        ctrl
    }

    #[inline]
    pub fn cur_block(&self) -> Option<BlockId> {
        self.cur_block
    }

    #[inline]
    pub fn set_block(&mut self, block: BlockId) {
        self.cur_block = Some(block);
    }

    pub fn build_entry(&mut self) {
        // We want a clean state when creating the entry 
        self.function_body = FunctionBody::new_invalid();
        let built_entry = self.builder().build_entry();
        // Store the results
        self.function_body.entry_control = built_entry.control;
        self.function_body.entry_memory = built_entry.memory;
        self.function_body.entry = built_entry.entry;
    }

    pub fn get_entry_control(&self) -> NodeOutputId {
        self.function_body.entry_control
    }

    pub fn get_memory_control(&self) -> NodeOutputId {
        self.function_body.entry_memory
    }

    pub fn link_block(&mut self, block: BlockId, control: NodeOutputId, memory: NodeOutputId, cur_block: BlockId) {
        assert!(self.graph().output_kind(control).is_control());
        assert!(self.graph().output_kind(memory).is_memory());

        let control_region = self.blocks[block].control_region.node;
        let memory_region = self.blocks[block].memory_region.node;

        self.graph_mut().add_node_input(control_region, control);
        self.graph_mut().add_node_input(memory_region, memory);
        // Add a dependency between the the parent variable and the current blocks corresponding variable
        for var_id in self.variables.keys() {
            let block_var = self.blocks[block].initial_variables[var_id];
            let block_var_id = self.graph().get_node_from_output(block_var);
            let cur_block_var = self.blocks[cur_block].variables[var_id];
            self.graph_mut().add_node_input(block_var_id, cur_block_var);
        }  
    }

    pub fn update_block_vars(&mut self, block: BlockId, variables: &SecondaryMap<VarId, NodeOutputId>) {
        // Add a dependency between the the parent variable and the current blocks corresponding variable
        for var_id in self.variables.keys(){
            let block_var = self.blocks[block].variables[var_id];
            let block_var_id = self.graph().get_node_from_output(block_var);
            let cur_var = variables[var_id];
            self.graph_mut().add_node_input(block_var_id, cur_var);
        }  
    }

    pub fn get_node_from_vn(&mut self, variable: &Var) -> NodeOutputId {
        self.get_node_from_vn_optional(variable).unwrap()
    }

    pub fn get_node_from_vn_optional(&mut self, variable: &Var) -> Option<NodeOutputId> {
        if let Some(variable_id) = self.variable_to_id.get(variable) {
            Some(self.blocks[self.require_cur_block()].variables[*variable_id])
        } else {
            None
        }
    }

    pub fn set_entry_block(&mut self, block: BlockId) {
        let control = self.body().entry_control;
        let memory = self.body().entry_memory;

        let control_region = self.blocks[block].control_region.node;
        let memory_region = self.blocks[block].memory_region.node;

        self.graph_mut().add_node_input(control_region, control);
        self.graph_mut().add_node_input(memory_region, memory);

        // Create initial varaibles
        let mut initial_variables = SecondaryMap::new();
        for var_id in self.variables.keys(){
            let var = self.variables[var_id];
            initial_variables[var_id] = self.builder()._build_single_output_pure(
                NodeKind::InitialVar(var), [], var.size.into());
        }

        self.update_block_vars(block, &initial_variables);
    }

    pub fn variables(&self) -> impl Iterator<Item = &rsleigh::Vn> {
        self.variable_to_id.keys()
    }

    pub fn link_blocks(&mut self, parent_block: BlockId, child_block: BlockId) {
        // TODO: write better
        self.link_block(child_block, self.blocks[parent_block].last_ctrl, self.blocks[parent_block].last_memory, parent_block);
    }

    pub fn create_block(&mut self) -> BlockId {
        // When creating a block - 
        // 0. Create a new control flow for the new block
        // 1. Assume all memory is corrupted and must be chosen using the memory region
        // 2. Assume all variables are corrupted and must be chosen using the Control Selector 
        let control_region = self.builder().build_control_node(&[]);
        let memory_region = self.builder().build_memory_phi( &[]);
        let last_ctrl = control_region.control;
        let last_memory = memory_region.memory;

        let mut variables = SecondaryMap::new();
        for var_id in self.variables.keys(){
            let var = self.variables[var_id];
            variables[var_id] = self.builder()
                .build_control_phi(var, control_region.selector, &[]).output;
        }
        let block = Block { 
            terminated: false, control_region, 
            memory_region, last_ctrl, last_memory,
            initial_variables: variables.clone(),
            variables
        };
        let block_id = self.blocks.push(block);
        block_id
    }

    pub fn update_var(&mut self, var: &Var, value: NodeOutputId) {
        let current_block = self.require_cur_block();
        self.blocks[current_block].variables[self.variable_to_id[var]] = value;
    }

    pub fn build_return(&mut self, value: Option<NodeOutputId>, ret_vars: &[rsleigh::Vn]) {
        let return_values: Vec<_> = ret_vars.iter()
            .filter_map(|vn| self.get_node_from_vn_optional(vn)).collect();
        let ctrl = self.terminate_cur_block();
        self.builder().build_return(ctrl, value, return_values.iter().copied());
    }

    pub fn build_branch(&mut self, dest: BlockId) {
        let cur_memory =  self.cur_block_memory();
        let current_block = self.require_cur_block();
        let cur_ctrl = self.terminate_cur_block();
        
        self.link_block(dest, cur_ctrl, cur_memory, current_block);
    }

    pub fn build_if(&mut self, cond: NodeOutputId, true_block: BlockId, false_block: BlockId){
        let cur_memory =  self.cur_block_memory();
        let current_block = self.require_cur_block();
        let cur_ctrl = self.terminate_cur_block();

        let built = self.builder().build_if(cur_ctrl, cond);
        self.link_block(true_block, built.true_ctrl, cur_memory, current_block);
        self.link_block(false_block, built.false_ctrl, cur_memory, current_block);
    }

    pub fn build_call(&mut self, call_address: NodeOutputId, arg_passing_vars: &[Var], callee_saved_vars: &[Var]) {
        // Get all input arguments = caller arguments 
        let ctrl = self.cur_block_ctrl();
        let memory = self.cur_block_memory();
        let block = self.require_cur_block();

        // call args should only be the calling convention ones :)
        let call_args: Vec<NodeOutputId> = arg_passing_vars.iter()
            .filter(|var| self.variable_to_id.contains_key(var))
            .map(|var| self.blocks[block].variables[self.variable_to_id[var]]).collect();

        let built = self.builder().build_call(ctrl, memory, call_address, &call_args);
        
        self.advance_cur_block_ctrl(built.control);

        // Clober memory after call
        let clobbered_memory = self.builder().build_post_call_memory(built.memory);
        self.advance_cur_block_memory(clobbered_memory);

        // Clober all variables 
        let calle_saved_set: std::collections::HashSet<Var> = callee_saved_vars.iter().copied().collect();   
        // clobber all registers that are not callee saved
        for var_id in self.variables.keys(){
            let var = self.variables[var_id];
            if calle_saved_set.contains(&var) {
                continue;
            }
            let post_call_var = self.builder().build_post_call_var(built.ret_val, var);
            self.blocks[block].variables[var_id] = post_call_var;
        }
    }

    pub fn build_store(&mut self, addr: NodeOutputId, data: NodeOutputId, space: rsleigh::VnSpace) {
        let memory = self.cur_block_memory();

        let built = self.builder().build_store(memory, addr, data, space);
        self.advance_cur_block_memory(built.memory);
    }

    pub fn build_load(&mut self, addr: NodeOutputId, space: rsleigh::VnSpace, output_type: NodeOutputType) -> NodeOutputId {
        let memory = self.cur_block_memory();
        self.builder().build_load(memory, addr, space, output_type)
    }

    pub fn dot_dumper<'a, R: MemReader>(&'a self, sleigh: &'a rsleigh::Sleigh<R>) -> crate::dot::GraphDotDumper<'a, R> {
        GraphDotDumper { entry: self.body().entry, graph: self.graph(), sleigh}
    }
}


impl FunctionBody {
    pub fn dot_dumper<'a, R: MemReader>(&'a self, sleigh: &'a rsleigh::Sleigh<R>) -> crate::dot::GraphDotDumper<'a, R> {
        GraphDotDumper { entry: self.entry, graph: &self.graph, sleigh}
    }
}

impl BoolBuilderExt for FunctionBuilder {}
impl IntBuilderExt for FunctionBuilder {}

