use super::FunctionBuilder;
use crate::error::{ErrorKind, Result};
use crate::function::FunctionGraph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};
use crate::region::RegionId;
use smallvec::SmallVec;

impl FunctionBuilder {
    /// Emits a boolean constant node and returns its output id.
    pub fn build_boolean_const(&mut self, val: bool) -> NodeOutputId {
        self.build_single_output_pure(NodeKind::BoolConst(val), [], NodeOutputType::Bool)
    }

    /// Emits a boolean binary operation node and returns its output id.
    pub fn build_boolean_operation(
        &mut self,
        lhs_id: NodeOutputId,
        rhs_id: NodeOutputId,
        op: BoolBinaryOp,
    ) -> Result<NodeOutputId> {
        let lhs_kind = self.graph().output_kind(lhs_id);
        if !lhs_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(lhs_id, lhs_kind).into());
        }
        let rhs_kind = self.graph().output_kind(rhs_id);
        if !rhs_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(rhs_id, rhs_kind).into());
        }
        let converted_lhs_id = self.convert_to_bool_if_needed(lhs_id)?;
        let converted_rhs_id = self.convert_to_bool_if_needed(rhs_id)?;
        Ok(self.build_single_output_pure(
            NodeKind::BoolBinaryOp(op),
            [converted_lhs_id, converted_rhs_id],
            NodeOutputType::Bool,
        ))
    }

    /// Emits a boolean unary operation node and returns its output id.
    pub fn build_boolean_unary_operation(
        &mut self,
        input_id: NodeOutputId,
        op: BoolUnaryOp,
    ) -> Result<NodeOutputId> {
        let kind = self.graph().output_kind(input_id);
        if !kind.is_value() {
            return Err(ErrorKind::ExpectedValue(input_id, kind).into());
        }
        let converted_input_id = self.convert_to_bool_if_needed(input_id)?;
        Ok(self.build_single_output_pure(
            NodeKind::BoolUnaryOp(op),
            [converted_input_id],
            NodeOutputType::Bool,
        ))
    }

    /// Emits an integer constant node with the given value and type.
    ///
    /// # Panics
    /// Panics if `output_type` is `U128` or `U256` — constants are stored as
    /// `u64` and cannot correctly represent values of those widths.
    pub fn build_int_const(&mut self, val: u64, output_type: NodeOutputType) -> NodeOutputId {
        assert!(
            !matches!(output_type, NodeOutputType::U128 | NodeOutputType::U256),
            "cannot build an IntConst of type {output_type}: constants are stored as u64"
        );
        self.build_single_output_pure(NodeKind::IntConst(val), [], output_type)
    }

    /// Emits a 64-bit unsigned integer constant node.
    pub fn build_uint64_const(&mut self, val: u64) -> NodeOutputId {
        self.build_int_const(val, NodeOutputType::U64)
    }

    /// Emits an integer binary operation node with automatic type coercion.
    pub fn build_int_binary_operation(
        &mut self,
        lhs_id: NodeOutputId,
        rhs_id: NodeOutputId,
        op: IntBinaryOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type)?;
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type)?;
        Ok(self.build_single_output_pure(
            NodeKind::IntBinaryOp(op),
            [converted_lhs_id, converted_rhs_id],
            output_type,
        ))
    }

    /// Emits an integer unary operation node with automatic type coercion.
    pub fn build_int_unary_operation(
        &mut self,
        input_id: NodeOutputId,
        op: IntUnaryOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let converted_input_id = self.convert_to_int_if_needed(input_id, output_type)?;
        Ok(self.build_single_output_pure(
            NodeKind::IntUnaryOp(op),
            [converted_input_id],
            output_type,
        ))
    }

    /// Emits a `Popcount` node that counts set bits in `input_id`.
    pub fn build_popcount(
        &mut self,
        input_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let input = self.convert_to_int_if_needed(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Popcount, [input], output_type))
    }

    /// Emits a `Lzcount` node that counts leading zero bits in `input_id`.
    pub fn build_lzcount(
        &mut self,
        input_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let input = self.convert_to_int_if_needed(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Lzcount, [input], output_type))
    }

    /// Emits an integer comparison node.
    pub fn build_int_cmp_operation(
        &mut self,
        lhs_id: NodeOutputId,
        rhs_id: NodeOutputId,
        kind: IntCmpOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let converted_lhs_id = self.convert_to_int_if_needed(lhs_id, output_type)?;
        let converted_rhs_id = self.convert_to_int_if_needed(rhs_id, output_type)?;
        Ok(self.build_single_output_pure(
            NodeKind::IntCmpOp(kind),
            [converted_lhs_id, converted_rhs_id],
            NodeOutputType::Bool,
        ))
    }

    // ── Float helpers ─────────────────────────────────────────────────────────

    /// Emits a float constant node with the given IEEE 754 bit pattern.
    /// `output_type` must be `F32` or `F64`.
    pub fn build_float_const(&mut self, bits: u64, output_type: NodeOutputType) -> NodeOutputId {
        self.build_single_output_pure(NodeKind::FloatConst(bits), [], output_type)
    }

    /// Generic cast of any value (int or float) to `float_type` (F32 or F64).
    ///
    /// Never fails — accepts any input type.  The optimizer lowers the node to
    /// `IntBitsToFloat`, `FloatToFloat`, or an identity depending on the actual
    /// input type at optimization time.
    pub fn build_cast_to_float(
        &mut self,
        input: NodeOutputId,
        float_type: NodeOutputType,
    ) -> NodeOutputId {
        self.build_single_output_pure(NodeKind::CastToFloat, [input], float_type)
    }

    /// Emits a float binary operation node.
    ///
    /// Inputs that are not already `output_type` are automatically wrapped in a
    /// `CastToFloat` node (int inputs, or float inputs of a different precision).
    pub fn build_float_binary_op(
        &mut self,
        lhs: NodeOutputId,
        rhs: NodeOutputId,
        op: FloatBinaryOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let lhs = self.cast_to_float_if_needed(lhs, output_type)?;
        let rhs = self.cast_to_float_if_needed(rhs, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatBinaryOp(op), [lhs, rhs], output_type))
    }

    /// Emits a float unary operation node (neg, abs, sqrt, ceil, floor, round).
    ///
    /// If `input` is not already `output_type`, a `CastToFloat` node is inserted.
    pub fn build_float_unary_op(
        &mut self,
        input: NodeOutputId,
        op: FloatUnaryOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let input = self.cast_to_float_if_needed(input, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatUnaryOp(op), [input], output_type))
    }

    /// Emits a float comparison node; produces a `Bool` output.
    ///
    /// The float type is inferred from the inputs (existing float type, or
    /// mapped from integer byte size).  Both inputs are cast if needed.
    pub fn build_float_cmp_op(
        &mut self,
        lhs: NodeOutputId,
        rhs: NodeOutputId,
        op: FloatCmpOp,
    ) -> Result<NodeOutputId> {
        let float_ty = self.infer_float_type(lhs)?;
        let lhs = self.cast_to_float_if_needed(lhs, float_ty)?;
        let rhs = self.cast_to_float_if_needed(rhs, float_ty)?;
        Ok(self.build_single_output_pure(
            NodeKind::FloatCmpOp(op),
            [lhs, rhs],
            NodeOutputType::Bool,
        ))
    }

    /// Emits an `IntToFloat` node: converts an integer value to the nearest
    /// representable float (like C's `(float)n`).
    pub fn build_int_to_float(
        &mut self,
        input: NodeOutputId,
        float_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if !self.get_output_type(input)?.is_integer() {
            return Err(ErrorKind::ExpectedInteger(input).into());
        }
        if !float_type.is_float() {
            return Err(ErrorKind::ExpectedFloatType(float_type).into());
        }
        Ok(self.build_single_output_pure(NodeKind::IntToFloat, [input], float_type))
    }

    /// Emits a `FloatToInt` node: truncates a float toward zero to an integer
    /// (like C's `(int)f`).
    pub fn build_float_to_int(
        &mut self,
        input: NodeOutputId,
        int_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if !self.get_output_type(input)?.is_float() {
            return Err(ErrorKind::ExpectedFloat(input).into());
        }
        if !int_type.is_integer() {
            return Err(ErrorKind::ExpectedIntegerType(int_type).into());
        }
        Ok(self.build_single_output_pure(NodeKind::FloatToInt, [input], int_type))
    }

    /// Emits a `FloatToFloat` node: converts between float precisions (F32 ↔ F64).
    pub fn build_float_to_float(
        &mut self,
        input: NodeOutputId,
        float_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if !self.get_output_type(input)?.is_float() {
            return Err(ErrorKind::ExpectedFloat(input).into());
        }
        if !float_type.is_float() {
            return Err(ErrorKind::ExpectedFloatType(float_type).into());
        }
        Ok(self.build_single_output_pure(NodeKind::FloatToFloat, [input], float_type))
    }

    /// Emits an `IntBitsToFloat` node: reinterprets an integer's bit pattern as
    /// a float of the same width.  If the input is an `IntConst`, immediately
    /// returns a `FloatConst` with the same bit pattern (no extra node created).
    pub fn build_int_bits_to_float(
        &mut self,
        input: NodeOutputId,
        float_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if !self.get_output_type(input)?.is_integer() {
            return Err(ErrorKind::ExpectedInteger(input).into());
        }
        if !float_type.is_float() {
            return Err(ErrorKind::ExpectedFloatType(float_type).into());
        }
        // Immediate fold: IntConst → FloatConst (same bits).
        let node_id = self.graph().get_node_from_output(input);
        if let NodeKind::IntConst(bits) = *self.graph().node_kind(node_id) {
            return Ok(self.build_float_const(bits, float_type));
        }
        Ok(self.build_single_output_pure(NodeKind::IntBitsToFloat, [input], float_type))
    }

    /// Emits a `FloatBitsToInt` node: reinterprets a float's bit pattern as an
    /// integer of the same width.  If the input is a `FloatConst`, immediately
    /// returns an `IntConst` with the same bit pattern (no extra node created).
    pub fn build_float_bits_to_int(
        &mut self,
        input: NodeOutputId,
        int_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if !self.get_output_type(input)?.is_float() {
            return Err(ErrorKind::ExpectedFloat(input).into());
        }
        if !int_type.is_integer() {
            return Err(ErrorKind::ExpectedIntegerType(int_type).into());
        }
        // Immediate fold: FloatConst → IntConst (same bits).
        let node_id = self.graph().get_node_from_output(input);
        if let NodeKind::FloatConst(bits) = *self.graph().node_kind(node_id) {
            return Ok(self.build_int_const(bits, int_type));
        }
        Ok(self.build_single_output_pure(NodeKind::FloatBitsToInt, [input], int_type))
    }

    /// Resets the graph and emits the function `Entry` and `InitialMemory` nodes.
    pub fn build_entry(&mut self) -> Result<()> {
        self.function = FunctionGraph::new_invalid();

        self.function.entry = self.create_node(NodeKind::Entry, [], vec![NodeOutputKind::Control]);
        let [control] = self.graph().node_outputs_exact(self.function.entry)?;
        self.function.entry_control = control;

        let memory_node =
            self.create_node(NodeKind::InitialMemory, [], vec![NodeOutputKind::Memory]);
        let [memory] = self.graph().node_outputs_exact(memory_node)?;
        self.function.entry_memory = memory;
        Ok(())
    }

    /// Terminates the current region with a `Return` node.
    pub fn build_return(
        &mut self,
        value: Option<NodeOutputId>,
        ret_vars: &[rsleigh::Vn],
    ) -> Result<()> {
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
            return Err(ErrorKind::ExpectedControl(res.control, ctrl_kind).into());
        }
        self.validate_value_inputs(&ret_inputs)?;

        self.create_node(
            NodeKind::Return,
            core::iter::once(res.control).chain(ret_inputs),
            [],
        );
        Ok(())
    }

    /// Terminates the current region with an unconditional branch to `dest`.
    pub fn build_branch(&mut self, dest: RegionId) -> Result<()> {
        let res = self.terminate_cur_region()?;
        let ctrl_kind = self.graph().output_kind(res.control);
        if !ctrl_kind.is_control() {
            return Err(ErrorKind::ExpectedControl(res.control, ctrl_kind).into());
        }
        let mem_kind = self.graph().output_kind(res.memory);
        if !mem_kind.is_memory() {
            return Err(ErrorKind::ExpectedMemory(res.memory, mem_kind).into());
        }
        self.link_region(dest, res.control, res.memory, res.region_id)
    }

    /// Terminates the current region with a conditional branch.
    pub fn build_if(
        &mut self,
        cond: NodeOutputId,
        true_region: RegionId,
        false_region: RegionId,
    ) -> Result<()> {
        let res = self.terminate_cur_region()?;

        let cond_kind = self.graph().output_kind(cond);
        if !cond_kind.is_bool() {
            return Err(ErrorKind::ExpectedValue(cond, cond_kind).into());
        }
        let ctrl_kind = self.graph().output_kind(res.control);
        if !ctrl_kind.is_control() {
            return Err(ErrorKind::ExpectedControl(res.control, ctrl_kind).into());
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

    /// Emits a `SegmentOp` node (pure computation: segment + offset → flat
    /// pointer) and returns its value output.
    pub fn build_segment_op(
        &mut self,
        op_id: u64,
        segment: NodeOutputId,
        offset: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let seg_kind = self.graph().output_kind(segment);
        if !seg_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(segment, seg_kind).into());
        }
        let off_kind = self.graph().output_kind(offset);
        if !off_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(offset, off_kind).into());
        }
        Ok(self.build_single_output_pure(
            NodeKind::SegmentOp { op_id },
            [segment, offset],
            output_type,
        ))
    }

    /// Emits a `CPoolRef` node (opaque JVM constant-pool lookup) and returns
    /// its value output.  `refs` holds the opaque reference inputs as emitted
    /// by Sleigh.
    pub fn build_cpool_ref(
        &mut self,
        refs: &[NodeOutputId],
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.validate_value_inputs(refs)?;
        let node = self.create_node(
            NodeKind::CPoolRef,
            refs.iter().copied(),
            [NodeOutputKind::OutputType(output_type)],
        );
        let [out] = self.graph().node_outputs_exact(node)?;
        Ok(out)
    }

    /// Emits a `New` node (opaque JVM allocation) and returns its value
    /// output.  `args` are the raw Sleigh inputs (typically a size).
    pub fn build_new(
        &mut self,
        args: &[NodeOutputId],
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.validate_value_inputs(args)?;
        let node = self.create_node(
            NodeKind::New,
            args.iter().copied(),
            [NodeOutputKind::OutputType(output_type)],
        );
        let [out] = self.graph().node_outputs_exact(node)?;
        Ok(out)
    }

    /// Emits a `Store` node writing `data` to `addr` in `space` and advances
    /// the region's memory token.
    pub fn build_store(
        &mut self,
        addr: NodeOutputId,
        data: NodeOutputId,
        space: rsleigh::VnSpace,
    ) -> Result<()> {
        let memory = self.cur_region_memory()?;
        let mem_kind = self.graph().output_kind(memory);
        if !mem_kind.is_memory() {
            return Err(ErrorKind::ExpectedMemory(memory, mem_kind).into());
        }
        let addr_kind = self.graph().output_kind(addr);
        if !addr_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(addr, addr_kind).into());
        }
        let data_kind = self.graph().output_kind(data);
        if !data_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(data, data_kind).into());
        }

        let node_id = self.create_node(
            NodeKind::Store(space),
            [memory, addr, data],
            [NodeOutputKind::Memory],
        );
        let [new_mem] = self.graph().node_outputs_exact(node_id)?;
        self.advance_cur_region_memory(new_mem)
    }

    /// Emits a `Load` node reading from `addr` in `space` and returns the
    /// loaded value output.
    pub fn build_load(
        &mut self,
        addr: NodeOutputId,
        space: rsleigh::VnSpace,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let memory = self.cur_region_memory()?;
        let mem_kind = self.graph().output_kind(memory);
        if !mem_kind.is_memory() {
            return Err(ErrorKind::ExpectedMemory(memory, mem_kind).into());
        }
        let addr_kind = self.graph().output_kind(addr);
        if !addr_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(addr, addr_kind).into());
        }
        Ok(self.build_single_output_pure(NodeKind::Load(space), [memory, addr], output_type))
    }

    /// Emits a `ControlPhi` node for varnode `var`.
    ///
    /// `phi_token` must be the `ControlPhi` output of the owning `ControlState`.
    /// `incoming_values` are the data inputs, one per predecessor (may be empty
    /// when first created; filled in later via `add_region_predecessor`).
    pub(super) fn build_control_phi(
        &mut self,
        var: rsleigh::Vn,
        phi_token: NodeOutputId,
        incoming_values: &[NodeOutputId],
    ) -> Result<NodeOutputId> {
        let phi_token_kind = self.graph().output_kind(phi_token);
        if !phi_token_kind.is_control_phi() {
            return Err(ErrorKind::ExpectedControlPhi(phi_token).into());
        }
        for &v in incoming_values {
            let kind = self.graph().output_kind(v);
            if !kind.is_control() {
                return Err(ErrorKind::ExpectedControl(v, kind).into());
            }
        }
        let output_type = var.size.try_into()?;
        Ok(self.build_single_output_pure(
            NodeKind::ControlPhi(var),
            core::iter::once(phi_token).chain(incoming_values.iter().copied()),
            output_type,
        ))
    }
}
