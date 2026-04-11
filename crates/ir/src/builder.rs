use std::collections::HashMap;
use crate::function::{BuiltFunctionGraph, FunctionGraph};
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::graph::Graph;
use crate::region::{Region, RegionId};
use crate::error::{Error, Result};
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
    ) -> Result<Self> {
        // For register varnodes, keep only the largest enclosing register.
        // e.g. if both `rdi` and `edi` are clobbered, drop `edi` because
        // clobbering `rdi` already implies `edi`.
        let all_variables: Vec<_> = all_used_variables.iter()
            .filter(|v| {
                if v.addr.space != rsleigh::VnSpace::REGISTER {
                    return true;
                }
                !all_used_variables.iter().any(|other| {
                    other != *v
                        && other.addr.space == rsleigh::VnSpace::REGISTER
                        && other.addr.off <= v.addr.off
                        && other.addr.off + other.size as u64 >= v.addr.off + v.size as u64
                        && other.size > v.size
                })
            }).copied().collect();
        let call_cloberred_variables: Vec<_> = all_variables.iter()
            .filter(|v| !callee_saved_vars.contains(v)).copied().collect();
        let mut variables = PrimaryMap::new();
        let mut variable_to_id = HashMap::new();
        for variable in all_variables {
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
        fb.build_entry()?;
        Ok(fb)
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
    /// Returns an error if the output does not carry a value (e.g. it is a
    /// control or memory edge).
    fn get_output_type(&self, output_id: NodeOutputId) -> Result<NodeOutputType> {
        let kind = self.graph().output_kind(output_id);
        kind.as_value().ok_or(Error::ExpectedValue(output_id, kind))
    }

    /// Emits a boolean constant node and returns its output id.
    pub fn build_boolean_const(&mut self, val: bool) -> NodeOutputId {
        self.build_single_output_pure(NodeKind::BoolConst(val), [], NodeOutputType::Bool)
    }

    /// If `output_id` is a constant node, returns its value as a `bool`.
    ///
    /// Returns `Ok(None)` for non-constant nodes.  An `IntConst` is considered
    /// `true` when non-zero.  Returns an error if the output is not a value.
    pub fn get_as_bool(&self, output_id: NodeOutputId) -> Result<Option<bool>> {
        let output_type = self.get_output_type(output_id)?;
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) if output_type.is_integer() => Ok(Some(*val != 0)),
            NodeKind::BoolConst(val) if output_type.is_bool() => Ok(Some(*val)),
            _ => Ok(None),
        }
    }

    /// Converts `output_id` to a boolean output, inserting a `CastToBool`
    /// node if needed.
    pub fn convert_to_bool_if_needed(&mut self, output_id: NodeOutputId) -> Result<NodeOutputId> {
        let output_kind = self.graph().output_kind(output_id);
        if !output_kind.is_value() {
            return Err(Error::ExpectedValue(output_id, output_kind));
        }

        if let Some(bool_val) = self.get_as_bool(output_id)? {
            return Ok(self.build_boolean_const(bool_val));
        }

        if output_kind.as_value() == Some(NodeOutputType::Bool) {
            return Ok(output_id);
        }

        Ok(self.build_single_output_pure(NodeKind::CastToBool, [output_id], NodeOutputType::Bool))
    }

    /// Emits a boolean binary operation node and returns its output id.
    pub fn build_boolean_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, op: BoolBinaryOp) -> Result<NodeOutputId> {
        let lhs_kind = self.graph().output_kind(lhs_id);
        if !lhs_kind.is_value() {
            return Err(Error::ExpectedValue(lhs_id, lhs_kind));
        }
        let rhs_kind = self.graph().output_kind(rhs_id);
        if !rhs_kind.is_value() {
            return Err(Error::ExpectedValue(rhs_id, rhs_kind));
        }
        let converted_lhs_id = self.convert_to_bool_if_needed(lhs_id)?;
        let converted_rhs_id = self.convert_to_bool_if_needed(rhs_id)?;
        Ok(self.build_single_output_pure(NodeKind::BoolBinaryOp(op),
            [converted_lhs_id, converted_rhs_id], NodeOutputType::Bool))
    }

    /// Emits a boolean unary operation node and returns its output id.
    pub fn build_boolean_unary_operation(&mut self, input_id: NodeOutputId, op: BoolUnaryOp) -> Result<NodeOutputId> {
        let kind = self.graph().output_kind(input_id);
        if !kind.is_value() {
            return Err(Error::ExpectedValue(input_id, kind));
        }
        let converted_input_id = self.convert_to_bool_if_needed(input_id)?;
        Ok(self.build_single_output_pure(NodeKind::BoolUnaryOp(op), [converted_input_id], NodeOutputType::Bool))
    }

    /// Emits an integer constant node with the given value and type.
    pub fn build_int_const(&mut self, val: u64, output_type: NodeOutputType) -> NodeOutputId {
        self.build_single_output_pure(NodeKind::IntConst(val), [], output_type)
    }

    /// Emits a 64-bit unsigned integer constant node.
    pub fn build_uint64_const(&mut self, val: u64) -> NodeOutputId {
        self.build_int_const(val, NodeOutputType::U64)
    }

    /// If `output_id` is a constant node, returns its value truncated to the
    /// declared [`NodeOutputType`] as an unsigned 64-bit integer.
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    pub fn get_as_unsigned_int(&self, output_id: NodeOutputId) -> Result<Option<u64>> {
        let output_type = self.get_output_type(output_id)?;
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) if output_type.is_integer() => Ok(output_type.get_unsigned_int(*val)),
            NodeKind::BoolConst(val) if output_type.is_bool() => Ok(Some(*val as u64)),
            _ => Ok(None),
        }
    }

    /// If `output_id` is an integer constant, returns its value
    /// sign-extended to `i64` according to the declared [`NodeOutputType`].
    ///
    /// Returns `Ok(None)` for non-constant nodes and for `Bool` constants.
    pub fn get_as_signed_int(&self, output_id: NodeOutputId) -> Result<Option<i64>> {
        let output_type = self.get_output_type(output_id)?;
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) if output_type.is_integer() => Ok(output_type.get_signed_int(*val)),
            _ => Ok(None),
        }
    }

    /// Returns both the unsigned and signed interpretations of `output_id` if
    /// it is an integer constant, or `None` otherwise.
    pub fn get_as_int(&self, output_id: NodeOutputId) -> Result<Option<(u64, i64)>> {
        let unsigned_val = self.get_as_unsigned_int(output_id)?;
        let signed_val = self.get_as_signed_int(output_id)?;
        match (unsigned_val, signed_val) {
            (Some(u), Some(s)) => Ok(Some((u, s))),
            _ => Ok(None),
        }
    }

    /// Truncates `output_id` to `output_type` if it is currently wider.
    pub fn truncate_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;

        if let Some(val) = self.get_as_unsigned_int(output_id)? {
            return Ok(self.build_int_const(val, output_type));
        }

        if curr_output_type.byte_size() <= output_type.byte_size() {
            return Ok(output_id);
        }

        Ok(self.build_single_output_pure(NodeKind::Truncate, [output_id], output_type))
    }

    /// Extends `output_id` to `output_type` using zero- or sign-extension.
    pub fn extend_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType, op: ExtendOp) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;

        if let Some((unsigned_val, signed_val)) = self.get_as_int(output_id)? {
            return Ok(match op {
                ExtendOp::SignExtend => self.build_int_const(signed_val as u64, output_type),
                ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            });
        }

        if !output_type.is_integer() {
            return Err(Error::ExpectedInteger(output_id));
        }

        if curr_output_type.byte_size() >= output_type.byte_size() {
            return Ok(output_id);
        }
        Ok(self.build_single_output_pure(NodeKind::Extend(op), [output_id], output_type))
    }

    /// Converts `output_id` to `output_type`, truncating or zero-extending as needed.
    pub fn convert_to_int_if_needed(&mut self, output_id: NodeOutputId, output_type: NodeOutputType) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;
        if curr_output_type.is_integer() {
            let truncate_id = self.truncate_if_needed(output_id, output_type)?;
            let extend_id = self.extend_if_needed(truncate_id, output_type, ExtendOp::ZeroExtend)?;
            return Ok(extend_id);
        }
        Ok(self.build_single_output_pure(NodeKind::CastToInt, [output_id], output_type))
    }

    /// Emits an integer binary operation node with automatic type coercion.
    pub fn build_int_binary_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, op: IntBinaryOp, output_type: NodeOutputType) -> Result<NodeOutputId> {
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type)?;
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntBinaryOp(op), [converted_lhs_id, converted_rhs_id], output_type))
    }

    /// Emits an integer unary operation node with automatic type coercion.
    pub fn build_int_unary_operation(&mut self, input_id: NodeOutputId, op: IntUnaryOp, output_type: NodeOutputType) -> Result<NodeOutputId> {
        let converted_input_id = self.convert_to_int_if_needed(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntUnaryOp(op), [converted_input_id], output_type))
    }

    /// Emits an integer comparison node.
    pub fn build_int_cmp_operation(&mut self, lhs_id: NodeOutputId, rhs_id: NodeOutputId, kind: IntCmpOp, output_type: NodeOutputType) -> Result<NodeOutputId> {
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type)?;
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntCmpOp(kind), [converted_lhs_id, converted_rhs_id], NodeOutputType::Bool))
    }

    /// Resets the graph and emits the function `Entry` and `InitialMemory` nodes.
    pub fn build_entry(&mut self) -> Result<()> {
        self.function = FunctionGraph::new_invalid();

        self.function.entry = self.create_node(NodeKind::Entry, [], vec![NodeOutputKind::Control]);
        let [control] = self.graph().node_outputs_exact(self.function.entry)?;
        self.function.entry_control = control;

        let memory_node = self.create_node(NodeKind::InitialMemory, [], vec![NodeOutputKind::Memory]);
        let [memory] = self.graph().node_outputs_exact(memory_node)?;
        self.function.entry_memory = memory;
        Ok(())
    }

    /// Returns the current `NodeOutputId` for `var` in the active region, or
    /// `None` if the variable is not known.
    pub fn read_variable_optional(&self, var: &rsleigh::Vn) -> Result<Option<NodeOutputId>> {
        if let Some(variable_id) = self.variable_to_id.get(var) {
            Ok(Some(self.read_variable_from_id(*variable_id)?))
        } else {
            Ok(None)
        }
    }

    /// Returns the current `NodeOutputId` for `variable` in the active region.
    ///
    /// Returns an error if the variable is not tracked or no region is active.
    pub fn read_variable(&self, variable: &rsleigh::Vn) -> Result<NodeOutputId> {
        self.variable_to_id
            .get(variable)
            .ok_or(Error::VariableNotFound(*variable))
            .and_then(|&id| self.read_variable_from_id(id))
    }

    /// Wires `region_id` as the function entry: connects the entry control
    /// and memory edges and creates initial variable nodes for every tracked
    /// variable.
    pub fn set_entry_region(&mut self, region_id: RegionId) -> Result<()> {
        let entry_control = self.body().entry_control;
        let entry_memory = self.body().entry_memory;
        self.link_control_regions(region_id, entry_control)?;
        self.link_memory_regions(region_id, entry_memory)?;

        // Create initial variables
        let var_ids: Vec<_> = self.variables.keys().collect();
        let mut initial_variables = SecondaryMap::new();
        for var_id in var_ids {
            let var = self.variables[var_id];
            let output_type = var.size.try_into()?;
            initial_variables[var_id] = self.build_single_output_pure(
                NodeKind::InitialVar(var), [], output_type);
        }
        self.link_region_variables(region_id, &initial_variables)
    }

    /// Returns an iterator over all tracked varnodes.
    pub fn variables(&self) -> impl Iterator<Item = &rsleigh::Vn> {
        self.variable_to_id.keys()
    }

    /// Creates a new region in the graph with fresh `ControlState`,
    /// `MemPhi`, and per-variable `ControlPhi` nodes.
    pub fn create_region(&mut self) -> Result<RegionId> {
        let memory_node = self.create_node(
            NodeKind::MemPhi,
            [],
            [NodeOutputKind::Memory]
        );
        let [memory] = self.graph().node_outputs_exact(memory_node)?;

        let control_node = self.create_node(
            NodeKind::ControlState,
            [],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi]
        );
        let [control, phi_token] = self.graph().node_outputs_exact(control_node)?;

        // Wire the ControlPhi dispatch token as MemPhi.inputs[0], mirroring how
        // ControlPhi nodes are linked.  This gives MemPhi a direct back-reference to
        // its ControlState so that dead-branch elimination and redundant-phi removal
        // can treat MemPhi and ControlPhi identically (same positional logic, same
        // automatic discovery via output_uses(cs_phi_out)).
        self.graph_mut().add_node_input(memory_node, phi_token)?;

        let var_ids: Vec<_> = self.variables.keys().collect();
        let mut variables = SecondaryMap::new();
        for var_id in var_ids {
            let var = self.variables[var_id];
            variables[var_id] = self.build_control_phi(var, phi_token, &[])?;
        }
        self.create_region_helper(control_node, control, memory_node, memory, variables)
    }

    /// Emits a `ControlPhi` node for varnode `var`.
    ///
    /// `phi_token` must be the `ControlPhi` output of the owning `ControlState`.
    /// `incoming_values` are the data inputs, one per predecessor (may be empty
    /// when first created; filled in later via `add_region_predecessor`).
    fn build_control_phi(&mut self, var: rsleigh::Vn, phi_token: NodeOutputId, incoming_values: &[NodeOutputId]) -> Result<NodeOutputId> {
        let phi_token_kind = self.graph().output_kind(phi_token);
        if !phi_token_kind.is_control_phi() {
            return Err(Error::ExpectedControlPhi(phi_token));
        }
        for &v in incoming_values {
            let kind = self.graph().output_kind(v);
            if !kind.is_control() {
                return Err(Error::ExpectedControl(v, kind));
            }
        }
        let output_type = var.size.try_into()?;
        Ok(self.build_single_output_pure(NodeKind::ControlPhi(var),
            core::iter::once(phi_token).chain(incoming_values.iter().copied()),
            output_type))
    }

    /// Terminates the current region with a `Return` node.
    pub fn build_return(&mut self, value: Option<NodeOutputId>, ret_vars: &[rsleigh::Vn]) -> Result<()> {
        let mut ret_inputs: SmallVec<[NodeOutputId; 4]> = SmallVec::new();
        if let Some(v) = value {
            ret_inputs.push(v);
        }
        for var in ret_vars {
            ret_inputs.push(self.read_variable(var)?);
        }

        let res = self.terminate_cur_region()?;

        let ctrl_kind = self.graph().output_kind(res.control);
        if !ctrl_kind.is_control() {
            return Err(Error::ExpectedControl(res.control, ctrl_kind));
        }
        for &v in &ret_inputs {
            let kind = self.graph().output_kind(v);
            if !kind.is_value() {
                return Err(Error::ExpectedValue(v, kind));
            }
        }

        self.create_node(
            NodeKind::Return,
            core::iter::once(res.control).chain(ret_inputs.into_iter()),
            [],
        );
        Ok(())
    }

    /// Terminates the current region with an unconditional branch to `dest`.
    pub fn build_branch(&mut self, dest: RegionId) -> Result<()> {
        let res = self.terminate_cur_region()?;
        let ctrl_kind = self.graph().output_kind(res.control);
        if !ctrl_kind.is_control() {
            return Err(Error::ExpectedControl(res.control, ctrl_kind));
        }
        let mem_kind = self.graph().output_kind(res.memory);
        if !mem_kind.is_memory() {
            return Err(Error::ExpectedMemory(res.memory, mem_kind));
        }
        self.link_region(dest, res.control, res.memory, res.region_id)
    }

    /// Terminates the current region with a conditional branch.
    pub fn build_if(&mut self, cond: NodeOutputId, true_region: RegionId, false_region: RegionId) -> Result<()> {
        let res = self.terminate_cur_region()?;

        let cond_kind = self.graph().output_kind(cond);
        if !cond_kind.is_bool() {
            return Err(Error::ExpectedValue(cond, cond_kind));
        }
        let ctrl_kind = self.graph().output_kind(res.control);
        if !ctrl_kind.is_control() {
            return Err(Error::ExpectedControl(res.control, ctrl_kind));
        }

        let brcond = self.create_node(
            NodeKind::If,
            [res.control, cond],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [true_ctrl_id, false_ctrl_id] = self.graph().node_outputs_exact(brcond)?;

        self.link_region(true_region, true_ctrl_id, res.memory, res.region_id)?;
        self.link_region(false_region, false_ctrl_id, res.memory, res.region_id)
    }

    /// Writes `value` to `variable` in the active region.
    pub fn write_variable(&mut self, variable: &rsleigh::Vn, value: NodeOutputId) -> Result<()> {
        let var_id = *self.variable_to_id.get(variable)
            .ok_or(Error::VariableNotFound(*variable))?;
        self.write_variable_from_id(var_id, value)
    }

    /// Terminates the current region with a `Call` node.
    pub fn build_call(&mut self, call_address: NodeOutputId) -> Result<()> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        let arg_passing: SmallVec<[NodeOutputId; 4]> = self.arg_passing_vars.iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        let clobbered: SmallVec<[_; 4]> = self.call_cloberred_variables.iter().copied().collect();

        let clobbered_outputs: SmallVec<[NodeOutputId; 4]> = self.call_cloberred_variables.iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;

        let cloberred_kinds: SmallVec<[NodeOutputKind; 4]> = clobbered_outputs.iter()
            .map(|v| self.graph().output_kind(*v)).collect();

        for &v in &arg_passing {
            let kind = self.graph().output_kind(v);
            if !kind.is_value() {
                return Err(Error::ExpectedValue(v, kind));
            }
        }
        for k in &cloberred_kinds {
            if !k.is_value() {
                return Err(Error::ExpectedValue(NodeOutputId::default(), *k));
            }
        }
        let addr_kind = self.graph().output_kind(call_address);
        if !addr_kind.is_value() {
            return Err(Error::ExpectedValue(call_address, addr_kind));
        }

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory].into_iter().chain(cloberred_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        let call_outputs: Vec<_> = self.graph().node_outputs(call).into_iter().collect();

        self.advance_cur_region_ctrl(call_outputs[0])?;
        self.advance_cur_region_memory(call_outputs[1])?;
        for (variable, new_val) in core::iter::zip(clobbered, call_outputs.iter().skip(2)) {
            self.write_variable(&variable, *new_val)?;
        }
        Ok(())
    }

    /// Emits a `Store` node writing `data` to `addr` in `space` and advances
    /// the region's memory token.
    pub fn build_store(&mut self, addr: NodeOutputId, data: NodeOutputId, space: rsleigh::VnSpace) -> Result<()> {
        let memory = self.cur_region_memory()?;
        let mem_kind = self.graph().output_kind(memory);
        if !mem_kind.is_memory() {
            return Err(Error::ExpectedMemory(memory, mem_kind));
        }
        let addr_kind = self.graph().output_kind(addr);
        if !addr_kind.is_value() {
            return Err(Error::ExpectedValue(addr, addr_kind));
        }
        let data_kind = self.graph().output_kind(data);
        if !data_kind.is_value() {
            return Err(Error::ExpectedValue(data, data_kind));
        }

        let node_id = self.create_node(
            NodeKind::Store(space),
            [memory, addr, data],
            [NodeOutputKind::Memory]
        );
        let [new_mem] = self.graph().node_outputs_exact(node_id)?;
        self.advance_cur_region_memory(new_mem)
    }

    /// Emits a `Load` node reading from `addr` in `space` and returns the
    /// loaded value output.
    pub fn build_load(&mut self, addr: NodeOutputId, space: rsleigh::VnSpace, output_type: NodeOutputType) -> Result<NodeOutputId> {
        let memory = self.cur_region_memory()?;
        let mem_kind = self.graph().output_kind(memory);
        if !mem_kind.is_memory() {
            return Err(Error::ExpectedMemory(memory, mem_kind));
        }
        let addr_kind = self.graph().output_kind(addr);
        if !addr_kind.is_value() {
            return Err(Error::ExpectedValue(addr, addr_kind));
        }
        Ok(self.build_single_output_pure(NodeKind::Load(space), [memory, addr], output_type))
    }

    /// Finalises and returns the completed [`BuiltFunctionGraph`].
    pub fn build(self) -> crate::function::BuiltFunctionGraph {
        BuiltFunctionGraph {
            graph: self.function.graph,
            entry: self.function.entry,
            variables: self.variables,
            call_clobbered: self.call_cloberred_variables.into_boxed_slice(),
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
    fn empty_builder() -> Result<FunctionBuilder> {
        Ok(FunctionBuilder::new(vec![], &[], &[], &[])?)
    }

    // ── get_as_unsigned_int ──────────────────────────────────────────────────

    /// A U8 constant built from a wider raw value must be masked to `u8::MAX`.
    #[test]
    fn get_unsigned_int_truncates_to_declared_width() -> Result<()> {
        let mut b = empty_builder()?;
        // Store u8::MAX + 1 — only the low byte is in-range for U8
        let out = b.build_int_const(u8::MAX as u64 + 1, NodeOutputType::U8);
        // The node was created with kind IntConst(256) but the type is U8,
        // so get_as_unsigned_int must mask it.
        let val = b.get_as_unsigned_int(out)?;
        assert_eq!(val, Some(0));  // 256 & 0xFF == 0
        Ok(())
    }

    /// `get_as_unsigned_int` on a non-const node must return `None`.
    #[test]
    fn get_unsigned_int_is_none_for_non_const() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(1, NodeOutputType::U64);
        let rhs = b.build_int_const(2, NodeOutputType::U64);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;
        assert_eq!(b.get_as_unsigned_int(add)?, None);
        Ok(())
    }

    // ── get_as_signed_int ────────────────────────────────────────────────────

    /// A U8 value with MSB set (`u8::MAX`) must sign-extend to -1 as i64.
    #[test]
    fn get_signed_int_sign_extends_negative_u8() -> Result<()> {
        let mut b = empty_builder()?;
        let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
        assert_eq!(b.get_as_signed_int(out)?, Some(-1i64));
        Ok(())
    }

    /// A U8 value below the sign bit (`i8::MAX`) must stay positive.
    #[test]
    fn get_signed_int_positive_u8_stays_positive() -> Result<()> {
        let mut b = empty_builder()?;
        let out = b.build_int_const(i8::MAX as u64, NodeOutputType::U8);
        assert_eq!(b.get_as_signed_int(out)?, Some(i8::MAX as i64));
        Ok(())
    }

    // ── truncate_if_needed ───────────────────────────────────────────────────

    /// Truncating a constant folds into a new constant of the target type,
    /// not a Truncate node.
    #[test]
    fn truncate_const_folds_to_const() -> Result<()> {
        let mut b = empty_builder()?;
        let out = b.build_int_const(0xABCD, NodeOutputType::U16);
        let truncated = b.truncate_if_needed(out, NodeOutputType::U8)?;
        // Must fold to a constant
        let val = b.get_as_unsigned_int(truncated)?;
        assert_eq!(val, Some(0xCD), "low byte of 0xABCD is 0xCD");
        // No Truncate node should have been emitted
        let node = b.graph().get_node_from_output(truncated);
        assert!(matches!(b.graph().node_kind(node), NodeKind::IntConst(_)));
        Ok(())
    }

    /// For a **non-const** value already at the target width (or narrower),
    /// `truncate_if_needed` must return the same output id unchanged.
    /// (Const values are always folded into a new constant node regardless of
    /// direction, so the no-op path only applies to non-const values.)
    #[test]
    fn truncate_noop_when_already_narrow_non_const() -> Result<()> {
        let mut b = empty_builder()?;
        // Build a non-const U8 expression: add(1u8, 2u8)
        let lhs = b.build_int_const(1, NodeOutputType::U8);
        let rhs = b.build_int_const(2, NodeOutputType::U8);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U8)?;
        // "Truncating" to a wider type must return the same node unchanged
        let result = b.truncate_if_needed(add, NodeOutputType::U16)?;
        assert_eq!(result, add, "non-const U8 value must not be touched when target is U16");
        Ok(())
    }

    /// A non-constant U32 truncated to U8 must emit a Truncate node.
    #[test]
    fn truncate_emits_truncate_node_for_non_const() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(1, NodeOutputType::U32);
        let rhs = b.build_int_const(2, NodeOutputType::U32);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U32)?;

        let truncated = b.truncate_if_needed(add, NodeOutputType::U8)?;
        let node = b.graph().get_node_from_output(truncated);
        assert!(
            matches!(b.graph().node_kind(node), NodeKind::Truncate),
            "expected Truncate node, got {:?}", b.graph().node_kind(node)
        );
        Ok(())
    }

    // ── extend_if_needed ─────────────────────────────────────────────────────

    /// Zero-extending a constant must fold: the result is a wider constant
    /// with high bits cleared.
    #[test]
    fn zero_extend_const_folds_to_wider_const() -> Result<()> {
        let mut b = empty_builder()?;
        let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
        let extended = b.extend_if_needed(out, NodeOutputType::U32, ExtendOp::ZeroExtend)?;
        assert_eq!(b.get_as_unsigned_int(extended)?, Some(u8::MAX as u64));
        let node = b.graph().get_node_from_output(extended);
        assert!(matches!(b.graph().node_kind(node), NodeKind::IntConst(_)));
        Ok(())
    }

    /// Sign-extending a negative U8 constant (`u8::MAX` = -1 as i8) must fold
    /// to `u32::MAX` (all bits set) as a wider constant.
    #[test]
    fn sign_extend_const_folds_negative_value() -> Result<()> {
        let mut b = empty_builder()?;
        let out = b.build_int_const(u8::MAX as u64, NodeOutputType::U8);
        let extended = b.extend_if_needed(out, NodeOutputType::U32, ExtendOp::SignExtend)?;
        assert_eq!(b.get_as_unsigned_int(extended)?, Some(u32::MAX as u64));
        Ok(())
    }

    /// Extending a non-constant must emit an Extend node.
    #[test]
    fn extend_emits_extend_node_for_non_const() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(1, NodeOutputType::U8);
        let rhs = b.build_int_const(2, NodeOutputType::U8);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U8)?;

        let extended = b.extend_if_needed(add, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
        let node = b.graph().get_node_from_output(extended);
        assert!(
            matches!(b.graph().node_kind(node), NodeKind::Extend(_)),
            "expected Extend node"
        );
        Ok(())
    }

    /// If the value is already the target width, `extend_if_needed` must
    /// return it unchanged.
    #[test]
    fn extend_noop_when_already_wide_enough() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(1, NodeOutputType::U64);
        let rhs = b.build_int_const(2, NodeOutputType::U64);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;

        let result = b.extend_if_needed(add, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
        assert_eq!(result, add);
        Ok(())
    }

    // ── convert_to_bool_if_needed ─────────────────────────────────────────────

    /// A known zero integer must fold to `BoolConst(false)`.
    #[test]
    fn convert_zero_int_to_bool_folds_to_false() -> Result<()> {
        let mut b = empty_builder()?;
        let zero = b.build_int_const(0, NodeOutputType::U32);
        let result = b.convert_to_bool_if_needed(zero)?;
        let node = b.graph().get_node_from_output(result);
        assert_eq!(b.graph().node_kind(node), &NodeKind::BoolConst(false));
        Ok(())
    }

    /// A known non-zero integer must fold to `BoolConst(true)`.
    #[test]
    fn convert_nonzero_int_to_bool_folds_to_true() -> Result<()> {
        let mut b = empty_builder()?;
        let nonzero = b.build_int_const(99, NodeOutputType::U32);
        let result = b.convert_to_bool_if_needed(nonzero)?;
        let node = b.graph().get_node_from_output(result);
        assert_eq!(b.graph().node_kind(node), &NodeKind::BoolConst(true));
        Ok(())
    }

    /// A value already of `Bool` type must be returned unchanged.
    #[test]
    fn convert_bool_to_bool_is_identity() -> Result<()> {
        let mut b = empty_builder()?;
        let bval = b.build_boolean_const(true);
        let result = b.convert_to_bool_if_needed(bval)?;
        assert_eq!(result, bval);
        Ok(())
    }

    /// A non-constant integer must produce a `CastToBool` node.
    #[test]
    fn convert_non_const_int_emits_cast_to_bool_node() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(1, NodeOutputType::U32);
        let rhs = b.build_int_const(2, NodeOutputType::U32);
        let add = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U32)?;

        let result = b.convert_to_bool_if_needed(add)?;
        let node = b.graph().get_node_from_output(result);
        assert!(
            matches!(b.graph().node_kind(node), NodeKind::CastToBool),
            "expected CastToBool node"
        );
        Ok(())
    }

    // ── build_int_binary_operation ────────────────────────────────────────────

    /// Building an Add on two constants of the same type must produce an
    /// `IntBinaryOp(Add)` node (no constant folding at this layer).
    #[test]
    fn build_int_binary_op_produces_binary_op_node() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(3, NodeOutputType::U64);
        let rhs = b.build_int_const(4, NodeOutputType::U64);
        let result = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;
        let node = b.graph().get_node_from_output(result);
        assert_eq!(b.graph().node_kind(node), &NodeKind::IntBinaryOp(IntBinaryOp::Add));
        Ok(())
    }

    /// When the operands differ in width, `build_int_binary_operation` must
    /// insert a coercion node so both reach the target type.
    #[test]
    fn build_int_binary_op_coerces_narrower_operand() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(1, NodeOutputType::U8);
        let rhs = b.build_int_const(2, NodeOutputType::U64);
        let result = b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;
        // The result must be typed as U64
        let kind = b.graph().output_kind(result);
        assert_eq!(kind, NodeOutputKind::OutputType(NodeOutputType::U64));
        Ok(())
    }

    // ── build_int_cmp_operation ───────────────────────────────────────────────

    /// A comparison must always produce a `Bool` output regardless of the
    /// operand type.
    #[test]
    fn build_int_cmp_produces_bool_output() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(10, NodeOutputType::U32);
        let rhs = b.build_int_const(20, NodeOutputType::U32);
        let result = b.build_int_cmp_operation(lhs, rhs, IntCmpOp::Less, NodeOutputType::U32)?;
        let kind = b.graph().output_kind(result);
        assert_eq!(kind, NodeOutputKind::OutputType(NodeOutputType::Bool));
        Ok(())
    }

    // ── build_boolean_operation ────────────────────────────────────────────────

    /// Boolean AND of two bool constants must produce a `BoolBinaryOp(And)`
    /// node.
    #[test]
    fn build_boolean_operation_produces_bool_binary_node() -> Result<()> {
        let mut b = empty_builder()?;
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        let result = b.build_boolean_operation(t, f, BoolBinaryOp::And)?;
        let node = b.graph().get_node_from_output(result);
        assert_eq!(b.graph().node_kind(node), &NodeKind::BoolBinaryOp(BoolBinaryOp::And));
        assert_eq!(b.graph().output_kind(result), NodeOutputKind::OutputType(NodeOutputType::Bool));
        Ok(())
    }

    // ── deduplication across build helpers ────────────────────────────────────

    /// Two identical constants must alias to the same output id (graph-level
    /// deduplication).
    #[test]
    fn identical_constants_are_deduplicated() -> Result<()> {
        let mut b = empty_builder()?;
        let a = b.build_int_const(77, NodeOutputType::U32);
        let c = b.build_int_const(77, NodeOutputType::U32);
        assert_eq!(a, c, "same constant must reuse the same node");
        Ok(())
    }

    /// Two constants with different values must NOT alias.
    #[test]
    fn different_constants_are_distinct() -> Result<()> {
        let mut b = empty_builder()?;
        let a = b.build_int_const(1, NodeOutputType::U32);
        let c = b.build_int_const(2, NodeOutputType::U32);
        assert_ne!(a, c);
        Ok(())
    }
}
