use std::collections::HashMap;
use crate::function::{BuiltFunctionGraph, FunctionGraph};
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::graph::Graph;
use crate::region::{Region, RegionId};
use cranelift_entity::{PrimaryMap, SecondaryMap, entity_impl};
use smallvec::SmallVec;
use crate::ops::{BoolBinaryOp, BoolUnaryOp, ExtendOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarId(u32);
entity_impl!(VarId);


pub struct FunctionBuilder {
    pub(crate) function: FunctionGraph,
    pub(crate) regions: PrimaryMap<RegionId, Region>,
    pub(crate) cur_region: Option<RegionId>,    
    pub(crate) variables: PrimaryMap<VarId, rsleigh::Vn>,
    pub(crate) variable_to_id: HashMap<rsleigh::Vn, VarId>,
}

impl FunctionBuilder {

    pub fn body(&self) -> &FunctionGraph {
        &self.function
    }

    pub fn body_mut(&mut self) -> &mut FunctionGraph {
        &mut self.function
    }

    pub(crate) fn graph(&self) -> &Graph {
        &self.body().graph
    }

    pub(crate) fn graph_mut(&mut self) -> &mut Graph {
        &mut self.function.graph
    }

    pub fn new(all_used_variables: Vec<rsleigh::Vn>) -> Self {
        let mut variables = PrimaryMap::new();
        let mut variable_to_id = HashMap::new();
        for variable in all_used_variables {
            let var_id = variables.push(variable);
            variable_to_id.insert(variable, var_id);
        }
        let mut fb = FunctionBuilder {
            function: FunctionGraph::new_invalid(),
            regions: PrimaryMap::new(),
            cur_region: None,
            variables,
            variable_to_id
        };
        fb.build_entry();
        fb
    }

    fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        self.graph_mut().create_node(kind, inputs, output_kinds)
    }

    fn build_single_output_pure(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_type: NodeOutputType,
    ) -> NodeOutputId {
        let node = self.create_node(kind, inputs, [NodeOutputKind::OutputType(output_type)]);
        self.graph().node_outputs(node)[0]
    }

    fn get_output_type(&self, output_id: NodeOutputId) -> NodeOutputType {
        self
            .graph()
            .output_kind(output_id)
            .as_value()
            .expect(format!("input {output_id} should be a value").as_str())
    }

    pub fn build_boolean_const(&mut self, val: bool) -> NodeOutputId {
        return self.build_single_output_pure(NodeKind::BoolConst(val),[], NodeOutputType::Bool);
    }

    pub fn get_as_bool(&mut self, output_id: NodeOutputId) -> Option<bool> {
        let node_id = self.graph().get_node_from_output(output_id);
        let output_type = self.get_output_type(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) => {
                // This is a good sanity that the graph was built correctly
                assert!(output_type.is_integer());
                Some(*val != 0) 
            },
            NodeKind::BoolConst(val) => {
                assert!(output_type.is_bool());
                Some(*val)
            },
            _ => None
        }
    }

    pub fn convert_to_bool_if_needed(&mut self, output_id: NodeOutputId) -> NodeOutputId {
        let output_kind = self.graph().output_kind(output_id);
        // It doesn't make sense to cast phi to bool
        assert!(output_kind.is_value());

        if let Some(bool_val) = self.get_as_bool(output_id) {
            return self.build_boolean_const(bool_val);
        }

        if output_kind.as_value() == Some(NodeOutputType::Bool) {
            return output_id;
        }

        return self.build_single_output_pure(NodeKind::CastToBool, [output_id], NodeOutputType::Bool);
    }

    pub fn build_boolean_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, op: BoolBinaryOp) -> NodeOutputId {
        assert!(self.graph().output_kind(lhs_id).is_value());
        assert!(self.graph().output_kind(rhs_id).is_value());

        // Convert the input to be of boolean type
        let converted_lhs_id = self.convert_to_bool_if_needed(lhs_id);
        let converted_rhs_id = self.convert_to_bool_if_needed(rhs_id);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::BoolBinaryOp(op), 
            [converted_lhs_id, converted_rhs_id], NodeOutputType::Bool);
    }


    pub fn build_boolean_unary_operation(&mut self, input_id: NodeOutputId, op: BoolUnaryOp) -> NodeOutputId {
        assert!(self.graph().output_kind(input_id).is_value());
        // Convert the input to be of boolean type
        let converted_input_id = self.convert_to_bool_if_needed(input_id);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::BoolUnaryOp(op), [converted_input_id], NodeOutputType::Bool);
    }

    pub fn build_int_const(&mut self, val: u64, output_type: NodeOutputType) -> NodeOutputId {
        return self.build_single_output_pure(NodeKind::IntConst(val),[], output_type);
    }

    pub fn build_uint64_const(&mut self, val: u64) -> NodeOutputId {
        return self.build_int_const(val, NodeOutputType::U64);
    }

    pub fn get_as_unsigned_int(&self, output_id: NodeOutputId) -> Option<u64> {
        let node_id = self.graph().get_node_from_output(output_id);
        let output_type = self.get_output_type(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) => {
                // This is a good sanity that the graph was built correctly
                assert!(output_type.is_integer());
                output_type.get_unsigned_int(*val)
            },
            NodeKind::BoolConst(val) => {
                assert!(output_type.is_bool());
                Some(*val as u64)
            },
            _ => None
        }
    }

    pub fn get_as_signed_int(&self, output_id: NodeOutputId) -> Option<i64> {
        let output_type = self.get_output_type(output_id);
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) => {
                // This is a good sanity that the graph was built correctly
                assert!(output_type.is_integer());
                output_type.get_signed_int(*val)
            },
            _ => None
        }
    }

    pub fn get_as_int(&self, output_id: NodeOutputId) -> Option<(u64, i64)> {
        let unsigned_val = self.get_as_unsigned_int(output_id);
        let signed_val = self.get_as_signed_int(output_id);
        if let Some(val) = unsigned_val {
            // If unsigbed exists - so should sign and the opposite
            Some((val, signed_val.unwrap()))
        } else {
            None
        }
    }

    pub fn truncate_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        let curr_output_type = self.get_output_type(output_id);

        // Truncate const values by changing their return type
        if let Some(val) = self.get_as_unsigned_int(output_id) {
            return self.build_int_const(val, output_type);
        }
        
        // No need to truncate values that are already less than the requested amount
        if curr_output_type.byte_size() <= output_type.byte_size() {
            return output_id;
        }

        return self.build_single_output_pure(NodeKind::Truncate, [output_id], output_type);
    }

    pub fn extend_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType, op: ExtendOp) -> NodeOutputId {
        let curr_output_type = self.get_output_type(output_id);

        // If it is a const - we can extend ourselves
        if let Some((unsigned_val, signed_val)) = self.get_as_int(output_id) {
            return match op {
                ExtendOp::SignExtend => self.build_int_const(signed_val as u64, output_type),
                ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            };
        }
        assert!(output_type.is_integer());
        
        // No need to extend values that are already more than the requested amount
        if curr_output_type.byte_size() >= output_type.byte_size() {
            return output_id;
        }
        return self.build_single_output_pure(NodeKind::Extend(op), [output_id], output_type);
    }

    pub fn convert_to_int_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType) -> NodeOutputId {
        let curr_output_type = self.get_output_type(output_id);
        if curr_output_type.is_integer() {
            // In the case we need to truncate or extend the input (u64 to u32 for example)
            let truncate_id = self.truncate_if_needed(output_id, output_type);
            let extend_id = self.extend_if_needed(truncate_id, output_type, ExtendOp::ZeroExtend);
            return extend_id;
        }

        return self.build_single_output_pure(NodeKind::CastToInt, [output_id], output_type);
    }

    pub fn build_int_binary_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, op: IntBinaryOp, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type);
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::IntBinaryOp(op), [converted_lhs_id, converted_rhs_id], output_type);
    }

    pub fn build_int_unary_operation(&mut self, input_id: NodeOutputId, op: IntUnaryOp, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_input_id = self.convert_to_int_if_needed(input_id, output_type);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::IntUnaryOp(op), [converted_input_id], output_type);
    }

    pub fn build_int_cmp_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, kind: IntCmpOp, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type);
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::IntCmpOp(kind), [converted_lhs_id, converted_rhs_id], NodeOutputType::Bool);
    }



    pub fn build_entry(&mut self) {
        // We want a clean state when creating the entry 
        self.function = FunctionGraph::new_invalid();

        self.function.entry = self.create_node(NodeKind::Entry, [], vec![NodeOutputKind::Control]);
        let [control] = self.graph().node_outputs_exact(self.function.entry);
        self.function.entry_control = control;


        let memory_node = self.create_node(NodeKind::InitialMemory, [], vec![NodeOutputKind::Memory]);
        let [memory] = self.graph().node_outputs_exact(memory_node);
        self.function.entry_memory = memory;
    }


    pub fn read_variable_optional(&self, var: &rsleigh::Vn) -> Option<NodeOutputId> {
        if let Some(variable_id) = self.variable_to_id.get(var) {
            Some(self.read_variable_from_id(*variable_id))
        } else {
            None
        }
    }


    pub fn read_variable(&self, variable: &rsleigh::Vn) -> NodeOutputId {
        self.read_variable_optional(variable).unwrap()
    }

    pub fn set_entry_region(&mut self, region_id: RegionId) {
        self.link_control_regions(region_id, self.body().entry_control);
        self.link_memory_regions(region_id, self.body().entry_memory);

        // Create initial varaibles
        let mut initial_variables = SecondaryMap::new();
        for var_id in self.variables.keys(){
            let var = self.variables[var_id];
            initial_variables[var_id] = self.build_single_output_pure(
                NodeKind::InitialVar(var), [], var.size.into());
        }
        self.link_region_variables(region_id, &initial_variables);
    }

    pub fn variables(&self) -> impl Iterator<Item = &rsleigh::Vn> {
        self.variable_to_id.keys()
    }

    pub fn create_region(&mut self) -> RegionId {
        // When creating a region - 
        // 0. Create a new control flow for the new region
        // 1. Assume all memory is corrupted and must be chosen using the memory region
        // 2. Assume all variables are corrupted and must be chosen using the Control Selector 

        let memory_node = self.create_node(
            NodeKind::MemSelector, 
            [],
            [NodeOutputKind::Memory]
        );
        let [memory] = self.graph().node_outputs_exact(memory_node); 

        let control_node = self.create_node(
            NodeKind::ControlState, 
            [],
            [NodeOutputKind::Control, NodeOutputKind::ControlSelector]
        );
        let [control, selector] = self.graph().node_outputs_exact(control_node); 

        let mut variables = SecondaryMap::new();
        for var_id in self.variables.keys(){
            let var = self.variables[var_id];
            variables[var_id] = self.build_control_phi(var, selector, &[]);
        }
        self.create_region_helper(
            control_node,
            control,
            memory_node,
            memory,
            variables
        )
    }

    fn build_control_phi(&mut self, var: rsleigh::Vn, selector: NodeOutputId, incoming_values: &[NodeOutputId],
    ) -> NodeOutputId {
        assert!(self.graph().output_kind(selector).is_control_selector());
        assert!(incoming_values.iter().copied().all(|v| self.graph().output_kind(v).is_control()));

        self.build_single_output_pure(NodeKind::ControlSelector(var), 
            core::iter::once(selector).chain(incoming_values.iter().copied()), 
            var.size.into())
    }

    pub fn build_return(&mut self, value: Option<NodeOutputId>, ret_vars: &[rsleigh::Vn]) {
        let ret_inputs: SmallVec<[NodeOutputId; 4]> = value.into_iter()
            .chain(ret_vars.iter().map(|var| self.read_variable(var))).collect();

        let res = self.terminate_cur_region();

        assert!(self.graph().output_kind(res.control).is_control());
        assert!(ret_inputs.iter().all(|&v| self.graph().output_kind(v).is_value()));

        self.create_node(
            NodeKind::Return,
            core::iter::once(res.control).chain(ret_inputs.into_iter()),
            [],
        );
    }

    pub fn build_branch(&mut self, dest: RegionId) {
        let res = self.terminate_cur_region();
        assert!(self.graph().output_kind(res.control).is_control());
        assert!(self.graph().output_kind(res.memory).is_memory());
        self.link_region(dest, res.control, res.memory, res.region_id);
    }

    pub fn build_if(&mut self, cond: NodeOutputId, true_region: RegionId, false_region: RegionId){
        let res = self.terminate_cur_region();

        assert!(self.graph().output_kind(cond).is_bool());
        assert!(self.graph().output_kind(res.control).is_control());

        let brcond = self.create_node(
            NodeKind::If,
            [res.control, cond],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [true_ctrl_id, false_ctrl_id] = self.graph().node_outputs_exact(brcond);

        self.link_region(true_region, true_ctrl_id, res.memory, res.region_id);
        self.link_region(false_region, false_ctrl_id, res.memory, res.region_id);
    }

    pub fn write_variable(&mut self, variable: &rsleigh::Vn, value: NodeOutputId) {
        self.write_variable_from_id(self.variable_to_id[variable], value);
    }

    pub fn build_call(&mut self, call_address: NodeOutputId, arg_passing_vars: &[rsleigh::Vn], callee_saved_vars: &[rsleigh::Vn]) {
        let ctrl = self.cur_region_control();
        let memory = self.cur_region_memory();

        let arg_passing: SmallVec<[NodeOutputId; 4]> = arg_passing_vars.iter()
            .map(|var| self.read_variable(var)).collect();

        // call args should only be the calling convention ones :)
        let callee_saved: SmallVec<[NodeOutputId; 2]> = callee_saved_vars.iter()
            .map(|var| self.read_variable(var)).collect();
        let callee_saved_kinds: SmallVec<[NodeOutputKind; 2]> = callee_saved.iter()
            .map(|v| self.graph().output_kind(*v)).collect();

        assert!(arg_passing.iter().copied().all(|v| self.graph().output_kind(v).is_value()));
        assert!(callee_saved_kinds.iter().copied().all(|v| v.is_value()));
        assert!(self.graph().output_kind(call_address).is_value());

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory].into_iter().chain(callee_saved_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        let call_outputs: Vec<_> = self.graph().node_outputs(call).into_iter().collect();

        self.advance_cur_region_ctrl(call_outputs[0]);
        self.advance_cur_region_memory(call_outputs[1]);

        // Clober all variables 
        for (variable, new_val_value) in core::iter::zip(callee_saved_vars.iter(), call_outputs.iter().skip(2)) {
            self.write_variable(variable, *new_val_value);
        }
    }

    pub fn build_store(&mut self, addr: NodeOutputId, data: NodeOutputId, space: rsleigh::VnSpace) {
        let memory = self.cur_region_memory();
        assert!(self.graph().output_kind(memory).is_memory());
        assert!(self.graph().output_kind(addr).is_value());
        assert!(self.graph().output_kind(data).is_value());
    
        let node_id = self.create_node(
            NodeKind::Store(space), 
            [memory, addr, data], 
            [NodeOutputKind::Memory]
        );
        let [new_mem] = self.graph().node_outputs_exact(node_id);
        self.advance_cur_region_memory(new_mem);
    }

    pub fn build_load(&mut self, addr: NodeOutputId, space: rsleigh::VnSpace, output_type: NodeOutputType) -> NodeOutputId {
        let memory = self.cur_region_memory();
        assert!(self.graph().output_kind(memory).is_memory());
        assert!(self.graph().output_kind(addr).is_value());

        self.build_single_output_pure(NodeKind::Load(space), [memory, addr], output_type)
    }

    pub fn build(self) -> crate::function::BuiltFunctionGraph {
        BuiltFunctionGraph {
            graph: self.function.graph,
            entry: self.function.entry,
            variables: self.variables
        }
    }
}
