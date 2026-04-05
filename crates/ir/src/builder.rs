use std::collections::HashMap;
use crate::function::{BuiltFunctionGraph, FunctionGraph};
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::graph::Graph;
use crate::region::{Region, RegionId};
use cranelift_entity::{PrimaryMap, SecondaryMap, entity_impl};
use smallvec::SmallVec;
use crate::ops::{BoolBinaryOp, BoolUnaryOp, ExtendOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

/// A dense, typed identifier for a tracked variable (varnode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarId(u32);
entity_impl!(VarId);


/// Incrementally constructs a sea-of-nodes IR function graph.
///
/// The builder tracks SSA-style per-region variable state: each variable has
/// exactly one current `NodeOutputId` inside the active region.  Reads and
/// writes go through this mapping so that the graph is always in a consistent
/// state.
pub struct FunctionBuilder {
    pub(crate) function: FunctionGraph,
    pub(crate) regions: PrimaryMap<RegionId, Region>,
    pub(crate) cur_region: Option<RegionId>,
    pub(crate) variables: PrimaryMap<VarId, rsleigh::Vn>,
    pub(crate) variable_to_id: HashMap<rsleigh::Vn, VarId>,
    /// Variables clobbered by any call instruction (everything not callee-saved).
    pub(crate) call_cloberred_variables: Vec<rsleigh::Vn>,
    /// Variables used to pass arguments according to the calling convention.
    pub(crate) arg_passing_vars: Vec<rsleigh::Vn>
}

impl FunctionBuilder {

    /// Returns a reference to the underlying [`FunctionGraph`].
    pub fn body(&self) -> &FunctionGraph {
        &self.function
    }

    /// Returns a mutable reference to the underlying [`FunctionGraph`].
    pub fn body_mut(&mut self) -> &mut FunctionGraph {
        &mut self.function
    }

    pub(crate) fn graph(&self) -> &Graph {
        &self.body().graph
    }

    pub(crate) fn graph_mut(&mut self) -> &mut Graph {
        &mut self.function.graph
    }

    /// Creates a new [`FunctionBuilder`] with the given variable set and
    /// calling-convention registers.
    ///
    /// `all_used_variables` is the complete set of varnodes (registers /
    /// unique temporaries) that appear in the function.  Variables not in
    /// `callee_saved_vars` are recorded as call-clobbered.
    pub fn new(
        all_used_variables: Vec<rsleigh::Vn>,
        arg_passing_vars: &[rsleigh::Vn],
        callee_saved_vars: &[rsleigh::Vn],
        _ret_vars: &[rsleigh::Vn]
    ) -> Self {
        let call_cloberred_variables: Vec<_> = all_used_variables.iter()
            .filter(|v| !callee_saved_vars.contains(v)).copied().collect();
        let mut variables = PrimaryMap::new();
        let mut variable_to_id = HashMap::new();
        for variable in all_used_variables {
            let var_id = variables.push(variable);
            variable_to_id.insert(variable, var_id);
        }
        let arg_passing_vars: Vec<_> = arg_passing_vars.iter().copied()
                    .filter(|vn| variable_to_id.contains_key(vn)).collect();

        let mut fb = FunctionBuilder {
            function: FunctionGraph::new_invalid(),
            regions: PrimaryMap::new(),
            cur_region: None,
            variables,
            variable_to_id,
            arg_passing_vars,
            call_cloberred_variables
        };
        fb.build_entry();
        fb
    }

    /// Creates a node in the graph with the given kind, inputs, and output kinds.
    fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    ) -> NodeId {
        self.graph_mut().create_node(kind, inputs, output_kinds)
    }

    /// Creates a single-output, pure (no side-effect) node and returns its
    /// output id.
    fn build_single_output_pure(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        output_type: NodeOutputType,
    ) -> NodeOutputId {
        let node = self.create_node(kind, inputs, [NodeOutputKind::OutputType(output_type)]);
        self.graph().node_outputs(node)[0]
    }

    /// Retrieves the [`NodeOutputType`] of `output_id`.
    ///
    /// Panics if the output does not carry a value (e.g. it is a control or
    /// memory edge).
    fn get_output_type(&self, output_id: NodeOutputId) -> NodeOutputType {
        self
            .graph()
            .output_kind(output_id)
            .as_value()
            .expect(format!("input {output_id} should be a value").as_str())
    }

    /// Emits a boolean constant node and returns its output id.
    pub fn build_boolean_const(&mut self, val: bool) -> NodeOutputId {
        return self.build_single_output_pure(NodeKind::BoolConst(val),[], NodeOutputType::Bool);
    }

    /// If `output_id` is a constant node, returns its value as a `bool`.
    ///
    /// Returns `None` for non-constant nodes.  An `IntConst` is considered
    /// `true` when non-zero.
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

    /// Converts `output_id` to a boolean output, inserting a `CastToBool`
    /// node if needed.
    ///
    /// If `output_id` is already a `Bool` type it is returned unchanged.
    /// If it is a known constant, the constant is folded into a `BoolConst`.
    /// Otherwise a `CastToBool` node is emitted.
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

    /// Emits a boolean binary operation node and returns its output id.
    ///
    /// Both operands are first converted to `Bool` if needed via
    /// [`convert_to_bool_if_needed`].
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


    /// Emits a boolean unary operation node and returns its output id.
    ///
    /// The operand is first converted to `Bool` if needed.
    pub fn build_boolean_unary_operation(&mut self, input_id: NodeOutputId, op: BoolUnaryOp) -> NodeOutputId {
        assert!(self.graph().output_kind(input_id).is_value());
        // Convert the input to be of boolean type
        let converted_input_id = self.convert_to_bool_if_needed(input_id);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::BoolUnaryOp(op), [converted_input_id], NodeOutputType::Bool);
    }

    /// Emits an integer constant node with the given value and type.
    pub fn build_int_const(&mut self, val: u64, output_type: NodeOutputType) -> NodeOutputId {
        return self.build_single_output_pure(NodeKind::IntConst(val),[], output_type);
    }

    /// Emits a 64-bit unsigned integer constant node.
    pub fn build_uint64_const(&mut self, val: u64) -> NodeOutputId {
        return self.build_int_const(val, NodeOutputType::U64);
    }

    /// If `output_id` is a constant node, returns its value truncated to the
    /// declared [`NodeOutputType`] as an unsigned 64-bit integer.
    ///
    /// Returns `None` for non-constant nodes.
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

    /// If `output_id` is an integer constant, returns its value
    /// sign-extended to `i64` according to the declared [`NodeOutputType`].
    ///
    /// Returns `None` for non-constant nodes and for `Bool` constants.
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

    /// Returns both the unsigned and signed interpretations of `output_id` if
    /// it is an integer constant, or `None` otherwise.
    pub fn get_as_int(&self, output_id: NodeOutputId) -> Option<(u64, i64)> {
        let unsigned_val = self.get_as_unsigned_int(output_id);
        let signed_val = self.get_as_signed_int(output_id);
        if let Some(val) = unsigned_val {
            // If unsigned exists - so should sign and the opposite
            Some((val, signed_val.unwrap()))
        } else {
            None
        }
    }

    /// Truncates `output_id` to `output_type` if it is currently wider.
    ///
    /// - If `output_id` is a known constant the truncation is folded into a
    ///   new constant node of the target type.
    /// - If the current type is already ≤ `output_type` the value is returned
    ///   unchanged.
    /// - Otherwise a `Truncate` node is emitted.
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

    /// Extends `output_id` to `output_type` using zero- or sign-extension.
    ///
    /// - If `output_id` is a known constant the extension is folded directly
    ///   into a new constant: sign-extend preserves the sign, zero-extend
    ///   keeps the raw bits.
    /// - If the current type is already ≥ `output_type` the value is returned
    ///   unchanged.
    /// - Otherwise an `Extend` node is emitted.
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

    /// Converts `output_id` to `output_type`, truncating or zero-extending as
    /// needed.
    ///
    /// If the current type is already an integer, truncation and extension are
    /// applied in sequence.  If the current type is `Bool`, a `CastToInt` node
    /// is emitted instead.
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

    /// Emits an integer binary operation node with automatic type coercion.
    ///
    /// Both operands are converted to `output_type` before the operation so
    /// that all inputs share the same width.
    pub fn build_int_binary_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, op: IntBinaryOp, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type);
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::IntBinaryOp(op), [converted_lhs_id, converted_rhs_id], output_type);
    }

    /// Emits an integer unary operation node with automatic type coercion.
    ///
    /// The operand is converted to `output_type` before the operation.
    pub fn build_int_unary_operation(&mut self, input_id: NodeOutputId, op: IntUnaryOp, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_input_id = self.convert_to_int_if_needed(input_id, output_type);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::IntUnaryOp(op), [converted_input_id], output_type);
    }

    /// Emits an integer comparison node.
    ///
    /// Both operands are coerced to `output_type` and the result is always
    /// `Bool`.
    pub fn build_int_cmp_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, kind: IntCmpOp, output_type: NodeOutputType) -> NodeOutputId {
        // Convert the input to be of int type
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type);
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type);

        // Store the requested operation
        return self.build_single_output_pure(NodeKind::IntCmpOp(kind), [converted_lhs_id, converted_rhs_id], NodeOutputType::Bool);
    }


    /// Resets the graph and emits the function `Entry` and `InitialMemory`
    /// nodes.
    ///
    /// Any previously built graph is discarded.  This is automatically called
    /// from [`FunctionBuilder::new`].
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


    /// Returns the current `NodeOutputId` for `var` in the active region, or
    /// `None` if the variable is not known.
    pub fn read_variable_optional(&self, var: &rsleigh::Vn) -> Option<NodeOutputId> {
        if let Some(variable_id) = self.variable_to_id.get(var) {
            Some(self.read_variable_from_id(*variable_id))
        } else {
            None
        }
    }


    /// Returns the current `NodeOutputId` for `variable` in the active region.
    ///
    /// Panics if the variable is not tracked.
    pub fn read_variable(&self, variable: &rsleigh::Vn) -> NodeOutputId {
        self.read_variable_optional(variable).unwrap()
    }

    /// Wires `region_id` as the function entry: connects the entry control
    /// and memory edges and creates initial variable nodes for every tracked
    /// variable.
    pub fn set_entry_region(&mut self, region_id: RegionId) {
        self.link_control_regions(region_id, self.body().entry_control);
        self.link_memory_regions(region_id, self.body().entry_memory);

        // Create initial variables
        let mut initial_variables = SecondaryMap::new();
        for var_id in self.variables.keys(){
            let var = self.variables[var_id];
            initial_variables[var_id] = self.build_single_output_pure(
                NodeKind::InitialVar(var), [], var.size.into());
        }
        self.link_region_variables(region_id, &initial_variables);
    }

    /// Returns an iterator over all tracked varnodes.
    pub fn variables(&self) -> impl Iterator<Item = &rsleigh::Vn> {
        self.variable_to_id.keys()
    }

    /// Creates a new region in the graph with fresh `ControlState`,
    /// `MemSelector`, and per-variable `ControlSelector` phi nodes.
    ///
    /// All variable values in the new region are initially undefined until
    /// predecessor regions are linked via [`link_regions`] or
    /// [`set_entry_region`].
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

    /// Emits a `ControlSelector` (phi-like) node for `var`.
    ///
    /// `selector` is the `ControlSelector` output of the owning region's
    /// `ControlState` node.  `incoming_values` are additional control edges
    /// from predecessor regions.
    fn build_control_phi(&mut self, var: rsleigh::Vn, selector: NodeOutputId, incoming_values: &[NodeOutputId],
    ) -> NodeOutputId {
        assert!(self.graph().output_kind(selector).is_control_selector());
        assert!(incoming_values.iter().copied().all(|v| self.graph().output_kind(v).is_control()));

        self.build_single_output_pure(NodeKind::ControlSelector(var),
            core::iter::once(selector).chain(incoming_values.iter().copied()),
            var.size.into())
    }

    /// Terminates the current region with a `Return` node.
    ///
    /// `value` is the optional return value.  `ret_vars` lists every register
    /// variable whose final value must be captured in the return node (used by
    /// the calling convention to know which registers are live at exit).
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

    /// Terminates the current region with an unconditional branch to `dest`.
    pub fn build_branch(&mut self, dest: RegionId) {
        let res = self.terminate_cur_region();
        assert!(self.graph().output_kind(res.control).is_control());
        assert!(self.graph().output_kind(res.memory).is_memory());
        self.link_region(dest, res.control, res.memory, res.region_id);
    }

    /// Terminates the current region with a conditional branch.
    ///
    /// Emits an `If` node whose two control outputs flow to `true_region` and
    /// `false_region` respectively.
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

    /// Writes `value` to `variable` in the active region.
    pub fn write_variable(&mut self, variable: &rsleigh::Vn, value: NodeOutputId) {
        self.write_variable_from_id(self.variable_to_id[variable], value);
    }

    /// Terminates the current region with a `Call` node.
    ///
    /// Reads argument registers from the current variable state, emits the
    /// call, then writes back clobbered-variable outputs so subsequent reads
    /// pick up the post-call values.
    pub fn build_call(&mut self, call_address: NodeOutputId) {
        let ctrl = self.cur_region_control();
        let memory = self.cur_region_memory();
        // call args should only be the calling convention ones :) - this won't work for x86 due to inputs being stored on the stack
        // will be fixed in optimization
        let arg_passing: SmallVec<[NodeOutputId; 4]> = self.arg_passing_vars.iter()
            .map(|var| self.read_variable(var)).collect();
        // everything except the call saved is cloberred by default
        let clobbered: SmallVec<[_; 4]> = self.call_cloberred_variables.iter().copied().collect();

        let clobbered_outputs: SmallVec<[_; 4]> =  self.call_cloberred_variables.iter()
            .map(|var| self.read_variable(var)).collect();

        let cloberred_kinds: SmallVec<[NodeOutputKind; 4]> = clobbered_outputs.iter()
            .map(|v| self.graph().output_kind(*v)).collect();

        assert!(arg_passing.iter().copied().all(|v| self.graph().output_kind(v).is_value()));
        assert!(cloberred_kinds.iter().copied().all(|v| v.is_value()));
        assert!(self.graph().output_kind(call_address).is_value());

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory].into_iter().chain(cloberred_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        self.function.call_clobbered.insert(call, clobbered.to_vec().into_boxed_slice());
        let call_outputs: Vec<_> = self.graph().node_outputs(call).into_iter().collect();

        self.advance_cur_region_ctrl(call_outputs[0]);
        self.advance_cur_region_memory(call_outputs[1]);
        // Clobber all variables
        for (variable, new_val_value) in core::iter::zip(clobbered, call_outputs.iter().skip(2)) {
            self.write_variable(&variable, *new_val_value);
        }
    }

    /// Emits a `Store` node writing `data` to `addr` in `space` and advances
    /// the region's memory token.
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

    /// Emits a `Load` node reading from `addr` in `space` and returns the
    /// loaded value output.
    pub fn build_load(&mut self, addr: NodeOutputId, space: rsleigh::VnSpace, output_type: NodeOutputType) -> NodeOutputId {
        let memory = self.cur_region_memory();
        assert!(self.graph().output_kind(memory).is_memory());
        assert!(self.graph().output_kind(addr).is_value());

        self.build_single_output_pure(NodeKind::Load(space), [memory, addr], output_type)
    }

    /// Finalises and returns the completed [`BuiltFunctionGraph`].
    pub fn build(self) -> crate::function::BuiltFunctionGraph {
        BuiltFunctionGraph {
            graph: self.function.graph,
            entry: self.function.entry,
            variables: self.variables,
            call_clobbered: self.function.call_clobbered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{IntBinaryOp, BoolBinaryOp, IntCmpOp, ExtendOp};
    use crate::node::{NodeKind, NodeOutputType};

    /// Build a minimal builder with no variables so tests that do not need
    /// SSA variables remain simple.
    fn empty_builder() -> FunctionBuilder {
        FunctionBuilder::new(vec![], &[], &[], &[])
    }

    // ── get_as_unsigned_int ──────────────────────────────────────────────────

    /// A U8 constant built from a wider raw value must be masked to `u8::MAX`.
    #[test]
    fn get_unsigned_int_truncates_to_declared_width() {
        let mut b = empty_builder();
        // Store u8::MAX + 1 — only the low byte is in-range for U8
        let out = b.build_int_const(u8::MAX as u64 + 1, NodeOutputType::U8);
        // The node was created with kind IntConst(256) but the type is U8,
        // so get_as_unsigned_int must mask it.
        let val = b.get_as_unsigned_int(out);
        assert_eq!(val, Some(0));  // 256 & 0xFF == 0
    }

    /// `get_as_unsigned_int` on a non-const node must return `None`.
    #[test]
    fn get_unsigned_int_is_none_for_non_const() {
        let mut b = empty_builder();
        let lhs = b.build_int_const(1, NodeOutputType::U64);
        let rhs = b.build_int_const(2, NodeOutputType::U64);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64);
        assert_eq!(b.get_as_unsigned_int(add), None);
    }

    // ── get_as_signed_int ────────────────────────────────────────────────────

    /// A U8 value with MSB set (`u8::MAX`) must sign-extend to -1 as i64.
    #[test]
    fn get_signed_int_sign_extends_negative_u8() {
        let mut b = empty_builder();
        let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
        assert_eq!(b.get_as_signed_int(out), Some(-1i64));
    }

    /// A U8 value below the sign bit (`i8::MAX`) must stay positive.
    #[test]
    fn get_signed_int_positive_u8_stays_positive() {
        let mut b = empty_builder();
        let out = b.build_int_const(i8::MAX as u64, NodeOutputType::U8);
        assert_eq!(b.get_as_signed_int(out), Some(i8::MAX as i64));
    }

    // ── truncate_if_needed ───────────────────────────────────────────────────

    /// Truncating a constant folds into a new constant of the target type,
    /// not a Truncate node.
    #[test]
    fn truncate_const_folds_to_const() {
        let mut b = empty_builder();
        let out = b.build_int_const(0xABCD, NodeOutputType::U16);
        let truncated = b.truncate_if_needed(out, NodeOutputType::U8);
        // Must fold to a constant
        let val = b.get_as_unsigned_int(truncated);
        assert_eq!(val, Some(0xCD), "low byte of 0xABCD is 0xCD");
        // No Truncate node should have been emitted
        let node = b.graph().get_node_from_output(truncated);
        assert!(matches!(b.graph().node_kind(node), NodeKind::IntConst(_)));
    }

    /// For a **non-const** value already at the target width (or narrower),
    /// `truncate_if_needed` must return the same output id unchanged.
    /// (Const values are always folded into a new constant node regardless of
    /// direction, so the no-op path only applies to non-const values.)
    #[test]
    fn truncate_noop_when_already_narrow_non_const() {
        let mut b = empty_builder();
        // Build a non-const U8 expression: add(1u8, 2u8)
        let lhs = b.build_int_const(1, NodeOutputType::U8);
        let rhs = b.build_int_const(2, NodeOutputType::U8);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U8);
        // "Truncating" to a wider type must return the same node unchanged
        let result = b.truncate_if_needed(add, NodeOutputType::U16);
        assert_eq!(result, add, "non-const U8 value must not be touched when target is U16");
    }

    /// A non-constant U32 truncated to U8 must emit a Truncate node.
    #[test]
    fn truncate_emits_truncate_node_for_non_const() {
        let mut b = empty_builder();
        let lhs = b.build_int_const(1, NodeOutputType::U32);
        let rhs = b.build_int_const(2, NodeOutputType::U32);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U32);

        let truncated = b.truncate_if_needed(add, NodeOutputType::U8);
        let node = b.graph().get_node_from_output(truncated);
        assert!(
            matches!(b.graph().node_kind(node), NodeKind::Truncate),
            "expected Truncate node, got {:?}", b.graph().node_kind(node)
        );
    }

    // ── extend_if_needed ─────────────────────────────────────────────────────

    /// Zero-extending a constant must fold: the result is a wider constant
    /// with high bits cleared.
    #[test]
    fn zero_extend_const_folds_to_wider_const() {
        let mut b = empty_builder();
        let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
        let extended = b.extend_if_needed(out, NodeOutputType::U32, ExtendOp::ZeroExtend);
        assert_eq!(b.get_as_unsigned_int(extended), Some(u8::MAX as u64));
        let node = b.graph().get_node_from_output(extended);
        assert!(matches!(b.graph().node_kind(node), NodeKind::IntConst(_)));
    }

    /// Sign-extending a negative U8 constant (`u8::MAX` = -1 as i8) must fold
    /// to `u32::MAX` (all bits set) as a wider constant.
    #[test]
    fn sign_extend_const_folds_negative_value() {
        let mut b = empty_builder();
        let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
        let extended = b.extend_if_needed(out, NodeOutputType::U32, ExtendOp::SignExtend);
        assert_eq!(b.get_as_unsigned_int(extended), Some(u32::MAX as u64));
    }

    /// Extending a non-constant must emit an Extend node.
    #[test]
    fn extend_emits_extend_node_for_non_const() {
        let mut b = empty_builder();
        let lhs = b.build_int_const(1, NodeOutputType::U8);
        let rhs = b.build_int_const(2, NodeOutputType::U8);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U8);

        let extended = b.extend_if_needed(add, NodeOutputType::U64, ExtendOp::ZeroExtend);
        let node = b.graph().get_node_from_output(extended);
        assert!(
            matches!(b.graph().node_kind(node), NodeKind::Extend(_)),
            "expected Extend node"
        );
    }

    /// If the value is already the target width, `extend_if_needed` must
    /// return it unchanged.
    #[test]
    fn extend_noop_when_already_wide_enough() {
        let mut b = empty_builder();
        let lhs = b.build_int_const(1, NodeOutputType::U64);
        let rhs = b.build_int_const(2, NodeOutputType::U64);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64);

        let result = b.extend_if_needed(add, NodeOutputType::U64, ExtendOp::ZeroExtend);
        assert_eq!(result, add);
    }

    // ── convert_to_bool_if_needed ─────────────────────────────────────────────

    /// A known zero integer must fold to `BoolConst(false)`.
    #[test]
    fn convert_zero_int_to_bool_folds_to_false() {
        let mut b = empty_builder();
        let zero = b.build_int_const(0, NodeOutputType::U32);
        let result = b.convert_to_bool_if_needed(zero);
        let node = b.graph().get_node_from_output(result);
        assert_eq!(b.graph().node_kind(node), &NodeKind::BoolConst(false));
    }

    /// A known non-zero integer must fold to `BoolConst(true)`.
    #[test]
    fn convert_nonzero_int_to_bool_folds_to_true() {
        let mut b = empty_builder();
        let nonzero = b.build_int_const(99, NodeOutputType::U32);
        let result = b.convert_to_bool_if_needed(nonzero);
        let node = b.graph().get_node_from_output(result);
        assert_eq!(b.graph().node_kind(node), &NodeKind::BoolConst(true));
    }

    /// A value already of `Bool` type must be returned unchanged.
    #[test]
    fn convert_bool_to_bool_is_identity() {
        let mut b = empty_builder();
        let bval = b.build_boolean_const(true);
        let result = b.convert_to_bool_if_needed(bval);
        assert_eq!(result, bval);
    }

    /// A non-constant integer must produce a `CastToBool` node.
    #[test]
    fn convert_non_const_int_emits_cast_to_bool_node() {
        let mut b = empty_builder();
        let lhs = b.build_int_const(1, NodeOutputType::U32);
        let rhs = b.build_int_const(2, NodeOutputType::U32);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U32);

        let result = b.convert_to_bool_if_needed(add);
        let node = b.graph().get_node_from_output(result);
        assert!(
            matches!(b.graph().node_kind(node), NodeKind::CastToBool),
            "expected CastToBool node"
        );
    }

    // ── build_int_binary_operation ────────────────────────────────────────────

    /// Building an Add on two constants of the same type must produce an
    /// `IntBinaryOp(Add)` node (no constant folding at this layer).
    #[test]
    fn build_int_binary_op_produces_binary_op_node() {
        let mut b = empty_builder();
        let lhs = b.build_int_const(3, NodeOutputType::U64);
        let rhs = b.build_int_const(4, NodeOutputType::U64);
        let result = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64);
        let node = b.graph().get_node_from_output(result);
        assert_eq!(b.graph().node_kind(node), &NodeKind::IntBinaryOp(IntBinaryOp::Add));
    }

    /// When the operands differ in width, `build_int_binary_operation` must
    /// insert a coercion node so both reach the target type.
    #[test]
    fn build_int_binary_op_coerces_narrower_operand() {
        let mut b = empty_builder();
        let lhs = b.build_int_const(1, NodeOutputType::U8);
        let rhs = b.build_int_const(2, NodeOutputType::U64);
        let result = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64);
        // The result must be typed as U64
        let kind = b.graph().output_kind(result);
        assert_eq!(kind, NodeOutputKind::OutputType(NodeOutputType::U64));
    }

    // ── build_int_cmp_operation ───────────────────────────────────────────────

    /// A comparison must always produce a `Bool` output regardless of the
    /// operand type.
    #[test]
    fn build_int_cmp_produces_bool_output() {
        let mut b = empty_builder();
        let lhs = b.build_int_const(10, NodeOutputType::U32);
        let rhs = b.build_int_const(20, NodeOutputType::U32);
        let result = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, NodeOutputType::U32);
        let kind = b.graph().output_kind(result);
        assert_eq!(kind, NodeOutputKind::OutputType(NodeOutputType::Bool));
    }

    // ── build_boolean_operation ────────────────────────────────────────────────

    /// Boolean AND of two bool constants must produce a `BoolBinaryOp(And)`
    /// node.
    #[test]
    fn build_boolean_operation_produces_bool_binary_node() {
        let mut b = empty_builder();
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        let result = b.build_boolean_operation(t, f, BoolBinaryOp::And);
        let node = b.graph().get_node_from_output(result);
        assert_eq!(b.graph().node_kind(node), &NodeKind::BoolBinaryOp(BoolBinaryOp::And));
        assert_eq!(b.graph().output_kind(result), NodeOutputKind::OutputType(NodeOutputType::Bool));
    }

    // ── deduplication across build helpers ────────────────────────────────────

    /// Two identical constants must alias to the same output id (graph-level
    /// deduplication).
    #[test]
    fn identical_constants_are_deduplicated() {
        let mut b = empty_builder();
        let a = b.build_int_const(77, NodeOutputType::U32);
        let c = b.build_int_const(77, NodeOutputType::U32);
        assert_eq!(a, c, "same constant must reuse the same node");
    }

    /// Two constants with different values must NOT alias.
    #[test]
    fn different_constants_are_distinct() {
        let mut b = empty_builder();
        let a = b.build_int_const(1, NodeOutputType::U32);
        let c = b.build_int_const(2, NodeOutputType::U32);
        assert_ne!(a, c);
    }
}
