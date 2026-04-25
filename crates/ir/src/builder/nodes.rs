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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when either operand is not a
    /// value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input_id` is not a value
    /// edge.
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

    /// Emits an integer constant node.
    ///
    /// `val` is masked to `output_type`'s bit width before storage.  Accepts
    /// any value convertible to `u128` — most callers pass a `u64` literal.
    ///
    /// # Panics
    ///
    /// Panics if `output_type` is not an integer type, or is `U256` (which
    /// is not yet representable in the u128 storage; no current consumer
    /// produces a U256 IntConst, see plan
    /// `2026-04-25-int-const-u256-and-pattern-width-aware.md`).
    pub fn build_int_const(
        &mut self,
        val: impl Into<u128>,
        output_type: NodeOutputType,
    ) -> NodeOutputId {
        assert!(
            output_type.is_integer(),
            "build_int_const called with non-integer type {output_type:?}"
        );
        assert!(
            !matches!(output_type, NodeOutputType::U256),
            "build_int_const(U256) not yet supported — IntConst storage is u128"
        );
        let val = val.into() & output_type.bit_mask_u128();
        self.build_single_output_pure(NodeKind::IntConst(val), [], output_type)
    }

    /// Emits an integer binary operation node with automatic type coercion.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when either operand is not a
    /// value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input_id` is not a value
    /// edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input_id` is not a value
    /// edge.
    pub fn build_popcount(
        &mut self,
        input_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let input = self.convert_to_int_if_needed(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Popcount, [input], output_type))
    }

    /// Emits a `Lzcount` node that counts leading zero bits in `input_id`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input_id` is not a value
    /// edge.
    pub fn build_lzcount(
        &mut self,
        input_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let input = self.convert_to_int_if_needed(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Lzcount, [input], output_type))
    }

    /// Emits an integer comparison node.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when either operand is not a
    /// value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when either operand is not a
    /// value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input` is not a value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when either operand is not a
    /// value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input` is not a value edge,
    /// [`ErrorKind::ExpectedInteger`] when `input` is a non-integer value, or
    /// [`ErrorKind::ExpectedFloatType`] when `float_type` is not `F32`/`F64`.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input` is not a value edge,
    /// [`ErrorKind::ExpectedFloat`] when `input` is not a float value, or
    /// [`ErrorKind::ExpectedIntegerType`] when `int_type` is not an integer.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input` is not a value edge,
    /// [`ErrorKind::ExpectedFloat`] when `input` is not a float, or
    /// [`ErrorKind::ExpectedFloatType`] when `float_type` is not `F32`/`F64`.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input` is not a value edge,
    /// [`ErrorKind::ExpectedInteger`] when `input` is not an integer, or
    /// [`ErrorKind::ExpectedFloatType`] when `float_type` is not `F32`/`F64`.
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
            // FloatConst stores bits as u64; float types are at most 64 bits wide,
            // so the value fits — u128 payload is masked to the type's width already.
            #[allow(clippy::cast_possible_truncation)]
            return Ok(self.build_float_const(bits as u64, float_type));
        }
        Ok(self.build_single_output_pure(NodeKind::IntBitsToFloat, [input], float_type))
    }

    /// Emits a `FloatBitsToInt` node: reinterprets a float's bit pattern as an
    /// integer of the same width.  If the input is a `FloatConst`, immediately
    /// returns an `IntConst` with the same bit pattern (no extra node created).
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `input` is not a value edge,
    /// [`ErrorKind::ExpectedFloat`] when `input` is not a float, or
    /// [`ErrorKind::ExpectedIntegerType`] when `int_type` is not an integer.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::WrongOutputCount`] if the freshly created `Entry`
    /// or `InitialMemory` nodes do not have their expected single output
    /// (this would indicate a graph-construction bug, not user error).
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::NoCurrentRegion`] / [`ErrorKind::RegionTerminated`]
    /// when there is no active region; [`ErrorKind::VariableNotFound`] when
    /// any element of `ret_vars` is not tracked; [`ErrorKind::ExpectedControl`]
    /// or [`ErrorKind::ExpectedMemory`] if the region's snapshotted ctrl/mem
    /// edges are mistyped (graph-construction bug); or
    /// [`ErrorKind::ExpectedValue`] when `value` or any read return register
    /// is not a value edge.
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
        let mem_kind = self.graph().output_kind(res.memory);
        if !mem_kind.is_memory() {
            return Err(ErrorKind::ExpectedMemory(res.memory, mem_kind).into());
        }
        self.validate_value_inputs(&ret_inputs)?;

        self.create_node(
            NodeKind::Return,
            [res.control, res.memory].into_iter().chain(ret_inputs),
            [],
        );
        Ok(())
    }

    /// Terminates the current region with an unconditional branch to `dest`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::NoCurrentRegion`] / [`ErrorKind::RegionTerminated`]
    /// when there is no active region; [`ErrorKind::ExpectedControl`] /
    /// [`ErrorKind::ExpectedMemory`] when the region's snapshotted edges are
    /// mistyped (graph-construction bug).
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::NoCurrentRegion`] / [`ErrorKind::RegionTerminated`]
    /// when there is no active region; [`ErrorKind::ExpectedValue`] when
    /// `cond` is not a `Bool` value; [`ErrorKind::ExpectedControl`] when the
    /// region's snapshotted control edge is mistyped;
    /// [`ErrorKind::WrongOutputCount`] from the freshly created `If` node.
    pub fn build_if(
        &mut self,
        cond: NodeOutputId,
        true_region: RegionId,
        false_region: RegionId,
    ) -> Result<()> {
        let res = self.terminate_cur_region()?;

        let cond_kind = self.graph().output_kind(cond);
        if !cond_kind.is_bool() {
            return Err(ErrorKind::ExpectedBool(cond).into());
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when either `segment` or `offset`
    /// is not a value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when any element of `refs` is not
    /// a value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when any element of `args` is not
    /// a value edge.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::NoCurrentRegion`] / [`ErrorKind::RegionTerminated`]
    /// when there is no active region; [`ErrorKind::ExpectedMemory`] /
    /// [`ErrorKind::ExpectedValue`] when the memory, address, or data edge is
    /// mistyped.
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
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::NoCurrentRegion`] / [`ErrorKind::RegionTerminated`]
    /// when there is no active region; [`ErrorKind::ExpectedMemory`] when the
    /// memory edge is mistyped; [`ErrorKind::ExpectedValue`] when `addr` is
    /// not a value edge.
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
            if !kind.is_value() {
                return Err(ErrorKind::ExpectedValue(v, kind).into());
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
