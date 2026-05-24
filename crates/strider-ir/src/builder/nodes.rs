use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::error::Result;
use crate::graph::Graph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};
use crate::region::RegionId;

impl FunctionBuilder {
    /// Emits a boolean constant node and returns its output id.
    pub fn build_boolean_const(&mut self, val: bool) -> NodeOutputId {
        self.build_single_output_pure(NodeKind::BoolConst(val), [], NodeOutputType::Bool)
    }

    /// Emits a boolean binary operation node and returns its output id.
    ///
    /// # Errors
    ///
    /// Returns an error when either operand is not a value edge.
    pub fn build_boolean_operation(
        &mut self,
        lhs_id: NodeOutputId,
        rhs_id: NodeOutputId,
        op: BoolBinaryOp,
    ) -> Result<NodeOutputId> {
        self.require_value_kind(lhs_id)?;
        self.require_value_kind(rhs_id)?;
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
    /// Returns an error when `input_id` is not a value edge.
    pub fn build_boolean_unary_operation(
        &mut self,
        input_id: NodeOutputId,
        op: BoolUnaryOp,
    ) -> Result<NodeOutputId> {
        self.require_value_kind(input_id)?;
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
    /// Delegates to [`crate::Graph::make_int_const`] (the single source of
    /// truth for primitive integer-constant construction) and unions any
    /// active `lift_addr` into the returned node's asm-fingerprint.
    ///
    /// # Errors
    ///
    /// Returns an error when `output_type` is not an integer type, or
    /// when it is `U256` / `U512` (not representable in the `u128` storage
    /// that `IntConst` uses — use [`Self::build_int_const_wide`] instead).
    pub fn build_int_const(
        &mut self,
        val: impl Into<u128>,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let addr = self.lift_addr;
        let out = self.graph_mut().make_int_const(val, output_type)?;
        if let Some(addr) = addr {
            let node = self.graph().get_node_from_output(out);
            self.graph_mut().extend_asm_fingerprint(node, &[addr]);
        }
        Ok(out)
    }

    /// Builds an integer constant whose value exceeds `u128` — `U256`
    /// (32 bytes) or `U512` (64 bytes).  Interns `value` via
    /// [`crate::Graph::intern_wide_const`] so two builds with equal
    /// values share the same `WideConstId` (and hence the same
    /// `NodeId` under the dedup cache).
    ///
    /// # Errors
    ///
    /// Returns an error when:
    /// - `output_type` is not `U256` or `U512` (use [`Self::build_int_const`]
    ///   for narrower widths).
    /// - `value.byte_size()` doesn't match `output_type`'s byte size
    ///   (e.g. `U256` storage with `U512` declared output).
    pub fn build_int_const_wide(
        &mut self,
        value: crate::wide_const::WideConstStorage,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let expected = match output_type {
            NodeOutputType::U256 => 32usize,
            NodeOutputType::U512 => 64usize,
            other => {
                return Err(anyhow!(
                    "build_int_const_wide called with non-wide output type {other:?}; \
                     use build_int_const for ≤ U128"
                ));
            }
        };
        if value.byte_size() != expected {
            return Err(anyhow!(
                "WideConstStorage byte_size {} does not match output type {output_type:?} \
                 (expected {expected})",
                value.byte_size()
            ));
        }
        let id = self.graph_mut().intern_wide_const(value);
        Ok(self.build_single_output_pure(NodeKind::IntConstWide(id), [], output_type))
    }

    /// Emits an integer binary operation node with automatic type coercion.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when either operand is not a
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
    /// Returns `ExpectedValue` when `input_id` is not a value
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

    /// Emits the canonical lowered shape for `lhs - rhs`:
    /// `Add(lhs, IntUnaryOp::Neg(rhs))`.
    ///
    /// `IntBinaryOp::Sub` is not a primitive in this IR; pcode-lift lowers
    /// `IntSub` opcodes at lift time.  This helper constructs the same
    /// shape from the builder API, useful in tests and any caller that
    /// needs to synthesise a subtraction without going through pcode-lift.
    /// For constant-RHS subtractions, `ConstantFold` collapses the
    /// `Neg(IntConst(K))` into `IntConst(-K)`, so post-optimisation the
    /// graph typically shows a single `Add(_, IntConst(-K))` node.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` if either operand is not a value edge.
    pub fn build_int_sub(
        &mut self,
        lhs_id: NodeOutputId,
        rhs_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let neg_rhs = self.build_int_unary_operation(rhs_id, IntUnaryOp::Neg, output_type)?;
        self.build_int_binary_operation(lhs_id, neg_rhs, IntBinaryOp::Add, output_type)
    }

    /// Emits a `Popcount` node that counts set bits in `input_id`.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `input_id` is not a value
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
    /// Returns `ExpectedValue` when `input_id` is not a value
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
    /// Returns `ExpectedValue` when either operand is not a
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
    /// Returns `ExpectedValue` when either operand is not a
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
    /// Returns `ExpectedValue` when `input` is not a value edge.
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
    /// Returns `ExpectedValue` when either operand is not a
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
    /// Returns `ExpectedValue` when `input` is not a value edge,
    /// `ExpectedInteger` when `input` is a non-integer value, or
    /// `ExpectedFloatType` when `float_type` is not `F32`/`F64`.
    pub fn build_int_to_float(
        &mut self,
        input: NodeOutputId,
        float_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.require_integer_value(input)?;
        Self::require_float_type(float_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntToFloat, [input], float_type))
    }

    /// Emits a `FloatToInt` node: truncates a float toward zero to an integer
    /// (like C's `(int)f`).
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `input` is not a value edge,
    /// `ExpectedFloat` when `input` is not a float value, or
    /// `ExpectedIntegerType` when `int_type` is not an integer.
    pub fn build_float_to_int(
        &mut self,
        input: NodeOutputId,
        int_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.require_float_value(input)?;
        Self::require_integer_type(int_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatToInt, [input], int_type))
    }

    /// Emits a `FloatToFloat` node: converts between float precisions (F32 ↔ F64).
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `input` is not a value edge,
    /// `ExpectedFloat` when `input` is not a float, or
    /// `ExpectedFloatType` when `float_type` is not `F32`/`F64`.
    pub fn build_float_to_float(
        &mut self,
        input: NodeOutputId,
        float_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.require_float_value(input)?;
        Self::require_float_type(float_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatToFloat, [input], float_type))
    }

    /// Emits an `IntBitsToFloat` node: reinterprets an integer's bit pattern as
    /// a float of the same width.  If the input is an `IntConst`, immediately
    /// returns a `FloatConst` with the same bit pattern (no extra node created).
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `input` is not a value edge,
    /// `ExpectedInteger` when `input` is not an integer, or
    /// `ExpectedFloatType` when `float_type` is not `F32`/`F64`.
    pub fn build_int_bits_to_float(
        &mut self,
        input: NodeOutputId,
        float_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.require_integer_value(input)?;
        Self::require_float_type(float_type)?;
        // Immediate fold: IntConst → FloatConst (same bits).  F80 is
        // 80-bit and `FloatConst`'s payload is `u64`, so the bit pattern
        // doesn't fit — skip the immediate-fold and emit the node
        // unchanged.  The graph keeps the IntBitsToFloat node opaque,
        // which is fine for pattern matching.
        if let NodeKind::IntConst(bits) = *self.graph().kind_of_output(input)
            && float_type != NodeOutputType::F80
        {
            // FloatConst stores bits as u64; F32/F64 fit, so the value
            // fits — u128 payload is masked to the type's width already.
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
    /// Returns `ExpectedValue` when `input` is not a value edge,
    /// `ExpectedFloat` when `input` is not a float, or
    /// `ExpectedIntegerType` when `int_type` is not an integer.
    pub fn build_float_bits_to_int(
        &mut self,
        input: NodeOutputId,
        int_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.require_float_value(input)?;
        Self::require_integer_type(int_type)?;
        let input_ty = self.get_output_type(input)?;
        // Immediate fold: FloatConst → IntConst (same bits).  F80 input
        // is skipped because `FloatConst` only stores 64 bits — even if a
        // FloatConst at F80 type somehow appeared, its u64 payload
        // wouldn't fully represent the 80-bit pattern.  Emit the node
        // unchanged.
        if let NodeKind::FloatConst(bits) = *self.graph().kind_of_output(input)
            && input_ty != NodeOutputType::F80
        {
            return self.build_int_const(bits, int_type);
        }
        Ok(self.build_single_output_pure(NodeKind::FloatBitsToInt, [input], int_type))
    }

    /// Resets the graph and emits the function `Entry` and `InitialMemory` nodes.
    ///
    /// # Errors
    ///
    /// Returns `WrongOutputCount` if the freshly created `Entry`
    /// or `InitialMemory` nodes do not have their expected single output
    /// (this would indicate a graph-construction bug, not user error).
    pub fn build_entry(&mut self) -> Result<()> {
        // Reset the graph to a fresh empty state.  Synthetic test builders
        // call `build_entry` via `new_raw`; resetting in-place keeps the
        // entry/InitialMemory pair as nodes 0/1.
        self.graph = Graph::new();

        self.entry = self.create_node(NodeKind::Entry, [], vec![NodeOutputKind::Control]);
        let [control] = self.graph().node_outputs_exact(self.entry)?;
        self.entry_control = control;

        let memory_node =
            self.create_node(NodeKind::InitialMemory, [], vec![NodeOutputKind::Memory]);
        let [memory] = self.graph().node_outputs_exact(memory_node)?;
        self.entry_memory = memory;
        Ok(())
    }

    /// Terminates the current region with a `Return` node.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `VariableNotFound` when
    /// any element of `ret_vars` is not tracked; `ExpectedControl`
    /// or `ExpectedMemory` if the region's snapshotted ctrl/mem
    /// edges are mistyped (graph-construction bug); or
    /// `ExpectedValue` when `value` or any read return register
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

        self.require_terminator_kinds(&res)?;
        self.validate_value_inputs(&ret_inputs)?;

        self.create_node(
            NodeKind::Return,
            [res.control, res.memory].into_iter().chain(ret_inputs),
            [],
        );
        Ok(())
    }

    /// Terminates the current region with an `IndirectBranch` placeholder
    /// node anchoring `target_value`.  Inputs: `[control, memory,
    /// target_value]`.  Outputs: `[]`.
    ///
    /// Used by the lifter when the CFG terminator is
    /// `RegionTerminator::UnresolvedIndirectBranch`: the value at the
    /// dispatch site is anchored as a value-typed slot on the placeholder
    /// so the indirect-branch resolver can later inspect its producer and
    /// either rewrite the placeholder into a real `Return` or splice in a
    /// `Call`+`Return` pair.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated` when there is no
    /// active region; `ExpectedControl` or `ExpectedMemory` if the
    /// region's snapshotted control/memory edges are mistyped
    /// (graph-construction bug); or `ExpectedValue` when `target_value`
    /// is not a value edge.
    pub fn build_indirect_branch(&mut self, target_value: NodeOutputId) -> Result<()> {
        let res = self.terminate_cur_region()?;

        self.require_terminator_kinds(&res)?;
        self.validate_value_inputs(std::slice::from_ref(&target_value))?;

        self.create_node(
            NodeKind::IndirectBranch,
            [res.control, res.memory, target_value],
            [],
        );
        Ok(())
    }

    /// Terminates the current region with an unconditional branch to `dest`.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `ExpectedControl` /
    /// `ExpectedMemory` when the region's snapshotted edges are
    /// mistyped (graph-construction bug).
    pub fn build_branch(&mut self, dest: RegionId) -> Result<()> {
        let res = self.terminate_cur_region()?;
        self.require_terminator_kinds(&res)?;
        self.link_region(dest, res.control, res.memory, res.region_id)
    }

    /// Terminates the current region with a conditional branch.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `ExpectedValue` when
    /// `cond` is not a `Bool` value; `ExpectedControl` when the
    /// region's snapshotted control edge is mistyped;
    /// `WrongOutputCount` from the freshly created `If` node.
    pub fn build_if(
        &mut self,
        cond: NodeOutputId,
        true_region: RegionId,
        false_region: RegionId,
    ) -> Result<()> {
        let res = self.terminate_cur_region()?;

        self.require_bool_value(cond)?;
        self.require_control_kind(res.control)?;

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
    /// Returns `ExpectedValue` when either `segment` or `offset`
    /// is not a value edge.
    pub fn build_segment_op(
        &mut self,
        op_id: u64,
        segment: NodeOutputId,
        offset: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.validate_value_inputs(&[segment, offset])?;
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
    /// Returns `ExpectedValue` when any element of `refs` is not
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
    /// Returns `ExpectedValue` when any element of `args` is not
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
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `ExpectedMemory` /
    /// `ExpectedValue` when the memory, address, or data edge is
    /// mistyped.
    pub fn build_store(
        &mut self,
        addr: NodeOutputId,
        data: NodeOutputId,
        space: rsleigh::VnSpace,
    ) -> Result<()> {
        let memory = self.cur_region_memory()?;
        self.require_memory_kind(memory)?;
        self.require_value_kind(addr)?;
        self.require_value_kind(data)?;

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
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `ExpectedMemory` when the
    /// memory edge is mistyped; `ExpectedValue` when `addr` is
    /// not a value edge.
    pub fn build_load(
        &mut self,
        addr: NodeOutputId,
        space: rsleigh::VnSpace,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let memory = self.cur_region_memory()?;
        self.require_memory_kind(memory)?;
        self.require_value_kind(addr)?;
        Ok(self.build_single_output_pure(NodeKind::Load(space), [memory, addr], output_type))
    }

    /// Emits a `Phi` node tagged with varnode `var` via the
    /// `phi_var_tag` side-table.
    ///
    /// `phi_token` must be the `PhiToken` output of the owning `Region`.
    /// `incoming_values` are the data inputs, one per predecessor (may be empty
    /// when first created; filled in later via `add_region_predecessor`).
    pub(super) fn build_control_phi(
        &mut self,
        var: rsleigh::Vn,
        phi_token: NodeOutputId,
        incoming_values: &[NodeOutputId],
    ) -> Result<NodeOutputId> {
        self.require_phi_token_kind(phi_token)?;
        self.validate_value_inputs(incoming_values)?;
        let output_type = var.size.try_into()?;
        let out = self.build_single_output_pure(
            NodeKind::Phi,
            core::iter::once(phi_token).chain(incoming_values.iter().copied()),
            output_type,
        );
        let (node_id, _slot) = self.graph().output_definition(out);
        self.graph_mut().set_phi_var_tag(node_id, var);
        Ok(out)
    }
}
