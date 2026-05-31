use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp};
use crate::region::RegionId;

impl FunctionBuilder {
    /// Emits a boolean constant — an `IntConst` of type `I1` (`true`→1,
    /// `false`→0).  Booleans are 1-bit integers; logical operations on them
    /// are ordinary `IntBinaryOp`/`IntUnaryOp` at `I1`.
    pub fn build_boolean_const(&mut self, val: bool) -> NodeOutputId {
        self.build_single_output_pure(
            NodeKind::IntConst(u128::from(val)),
            [],
            NodeOutputType::I1,
        )
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
    /// when it is `I256` / `I512` (not representable in the `u128` storage
    /// that `IntConst` uses — use [`Self::build_int_const_wide`] instead).
    pub fn build_int_const(
        &mut self,
        val: impl Into<u128>,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let addr = self.lift_addr;
        let out = self.function_mut().make_int_const(val, output_type)?;
        if let Some(addr) = addr {
            let node = self.function().node_for_output(out);
            self.function_mut().extend_asm_fingerprint(node, &[addr]);
        }
        Ok(out)
    }

    /// Emits the all-ones integer constant of `output_type` —
    /// `(2^bit_width) - 1`.  For widths ≤ 128 bits this is an `IntConst`;
    /// for `I256` / `I512` it is an `IntConstWide` carrying a
    /// `WideConstStorage::all_ones` payload (interned).  Asm-fingerprint
    /// plumbing is applied just like [`Self::build_int_const`].
    ///
    /// Used by the pcode lifter to materialise the second operand of
    /// `Xor(x, all_ones)` — the canonical IR form of bitwise complement
    /// (`~x`) since the former BitNot unary-op variant was removed.
    ///
    /// # Errors
    ///
    /// Returns an error when `output_type` is not an integer type.
    pub fn build_all_ones_const(
        &mut self,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let addr = self.lift_addr;
        let out = self.function_mut().make_all_ones_const(output_type)?;
        if let Some(addr) = addr {
            let node = self.function().node_for_output(out);
            self.function_mut().extend_asm_fingerprint(node, &[addr]);
        }
        Ok(out)
    }

    /// Builds an integer constant whose value exceeds `u128` — `I256`
    /// (32 bytes) or `I512` (64 bytes).  Interns `value` via
    /// `crate::Graph::intern_wide_const` so two builds with equal
    /// values share the same `WideConstId` (and hence the same
    /// `NodeId` under the dedup cache).
    ///
    /// # Errors
    ///
    /// Returns an error when:
    /// - `output_type` is not `I256` or `I512` (use [`Self::build_int_const`]
    ///   for narrower widths).
    /// - `value.byte_size()` doesn't match `output_type`'s byte size
    ///   (e.g. `I256` storage with `I512` declared output).
    pub fn build_int_const_wide(
        &mut self,
        value: crate::wide_const::WideConstStorage,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let expected = match output_type {
            NodeOutputType::I256 => 32usize,
            NodeOutputType::I512 => 64usize,
            other => {
                return Err(anyhow!(
                    "build_int_const_wide called with non-wide output type {other:?}; \
                     use build_int_const for ≤ I128"
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
        let id = self.function_mut().intern_wide_const(value);
        Ok(self.build_single_output_pure(NodeKind::IntConstWide(id), [], output_type))
    }

    /// Emits an integer binary operation node.  **Strict:** both operands
    /// must already carry `output_type` — the caller inserts any
    /// truncate / extend fix-up (the builder no longer auto-coerces).
    ///
    /// # Errors
    ///
    /// Returns an error when an operand is not a value edge or does not
    /// already have type `output_type`.
    pub fn build_int_binary_operation(
        &mut self,
        lhs_id: NodeOutputId,
        rhs_id: NodeOutputId,
        op: IntBinaryOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let lhs_id = self.require_value_type(lhs_id, output_type)?;
        let rhs_id = self.require_value_type(rhs_id, output_type)?;
        Ok(self.build_single_output_pure(
            NodeKind::IntBinaryOp(op),
            [lhs_id, rhs_id],
            output_type,
        ))
    }

    /// Emits an integer unary operation node.  **Strict:** the operand must
    /// already carry `output_type` (the caller inserts any fix-up).
    ///
    /// # Errors
    ///
    /// Returns an error when `input_id` is not a value edge or does not
    /// already have type `output_type`.
    pub fn build_int_unary_operation(
        &mut self,
        input_id: NodeOutputId,
        op: IntUnaryOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let input_id = self.require_value_type(input_id, output_type)?;
        Ok(self.build_single_output_pure(
            NodeKind::IntUnaryOp(op),
            [input_id],
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
    pub fn build_sub_as_add_neg(
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
        let input = self.require_value_type(input_id, output_type)?;
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
        let input = self.require_value_type(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Lzcount, [input], output_type))
    }

    /// Emits an integer comparison node.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when either operand is not a
    /// value edge.
    /// Emits an integer comparison node (output `I1`).  **Strict:** both
    /// operands must already carry `operand_type` (the comparison width).
    ///
    /// # Errors
    ///
    /// Returns an error when an operand is not a value edge or does not
    /// already have type `operand_type`.
    pub fn build_int_cmp_operation(
        &mut self,
        lhs_id: NodeOutputId,
        rhs_id: NodeOutputId,
        kind: IntCmpOp,
        operand_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let lhs_id = self.require_value_type(lhs_id, operand_type)?;
        let rhs_id = self.require_value_type(rhs_id, operand_type)?;
        Ok(self.build_single_output_pure(
            NodeKind::IntCmpOp(kind),
            [lhs_id, rhs_id],
            NodeOutputType::I1,
        ))
    }

    // ── Float helpers ─────────────────────────────────────────────────────────

    /// Emits a float constant node with the given IEEE 754 bit pattern.
    /// `output_type` must be `F32` or `F64`.
    pub fn build_float_const(&mut self, bits: u64, output_type: NodeOutputType) -> NodeOutputId {
        self.build_single_output_pure(NodeKind::FloatConst(bits), [], output_type)
    }

    /// Emits a float binary operation node.  **Strict:** both operands must
    /// already carry the float `output_type` (the caller inserts any
    /// `IntBitsToFloat` / `FloatToFloat` fix-up).
    ///
    /// # Errors
    ///
    /// Returns an error when an operand is not a value edge or does not
    /// already have type `output_type`.
    pub fn build_float_binary_op(
        &mut self,
        lhs: NodeOutputId,
        rhs: NodeOutputId,
        op: FloatBinaryOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let lhs = self.require_value_type(lhs, output_type)?;
        let rhs = self.require_value_type(rhs, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatBinaryOp(op), [lhs, rhs], output_type))
    }

    /// Emits a float unary operation node (neg, abs, sqrt, ceil, floor,
    /// round).  **Strict:** the operand must already carry `output_type`.
    ///
    /// # Errors
    ///
    /// Returns an error when `input` is not a value edge or does not already
    /// have type `output_type`.
    pub fn build_float_unary_op(
        &mut self,
        input: NodeOutputId,
        op: FloatUnaryOp,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let input = self.require_value_type(input, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatUnaryOp(op), [input], output_type))
    }

    /// Emits a float comparison node (output `I1`).  **Strict:** both
    /// operands must already be the same float type (the caller inserts any
    /// bit-cast fix-up).
    ///
    /// # Errors
    ///
    /// Returns an error when an operand is not a float value edge, or when
    /// the operands' float types differ.
    pub fn build_float_cmp_op(
        &mut self,
        lhs: NodeOutputId,
        rhs: NodeOutputId,
        op: FloatCmpOp,
    ) -> Result<NodeOutputId> {
        let float_ty = self.get_output_type(lhs)?;
        if !float_ty.is_float() {
            return Err(anyhow!(
                "build_float_cmp_op: lhs {lhs:?} has type {float_ty}, expected a float"
            ));
        }
        let rhs = self.require_value_type(rhs, float_ty)?;
        Ok(self.build_single_output_pure(
            NodeKind::FloatCmpOp(op),
            [lhs, rhs],
            NodeOutputType::I1,
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
        // A bit-reinterpret preserves width by definition; reject mismatched
        // widths so a wrong-width reinterpret can't silently truncate or
        // zero-pad (e.g. I64 → F32).
        let input_ty = self.get_output_type(input)?;
        if input_ty.byte_size() != float_type.byte_size() {
            return Err(anyhow!(
                "IntBitsToFloat width mismatch: input {input_ty:?} ({} bytes) \
                 vs float {float_type:?} ({} bytes)",
                input_ty.byte_size(),
                float_type.byte_size(),
            ));
        }
        // Immediate fold: IntConst → FloatConst (same bits).  F80 is
        // 80-bit and `FloatConst`'s payload is `u64`, so the bit pattern
        // doesn't fit — skip the immediate-fold and emit the node
        // unchanged.  The graph keeps the IntBitsToFloat node opaque,
        // which is fine for pattern matching.
        if let NodeKind::IntConst(bits) = *self.function().kind_of_output(input)
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
        // A bit-reinterpret preserves width by definition; reject mismatched
        // widths so a wrong-width reinterpret can't silently truncate or
        // zero-pad (e.g. F64 → I32).
        if input_ty.byte_size() != int_type.byte_size() {
            return Err(anyhow!(
                "FloatBitsToInt width mismatch: input {input_ty:?} ({} bytes) \
                 vs int {int_type:?} ({} bytes)",
                input_ty.byte_size(),
                int_type.byte_size(),
            ));
        }
        // Immediate fold: FloatConst → IntConst (same bits).  F80 input
        // is skipped because `FloatConst` only stores 64 bits — even if a
        // FloatConst at F80 type somehow appeared, its u64 payload
        // wouldn't fully represent the 80-bit pattern.  Emit the node
        // unchanged.
        if let NodeKind::FloatConst(bits) = *self.function().kind_of_output(input)
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
        // Reset the function to a fresh empty state while preserving
        // the calling-convention metadata `new_raw` already populated.
        // Synthetic test builders call `build_entry` via `new_raw`;
        // resetting in-place keeps the entry/InitialMemory pair as
        // nodes 0/1.
        let cc_metadata = std::mem::take(&mut self.function.cc_metadata);
        self.function = crate::function::Function::new();
        self.function.cc_metadata = cc_metadata;

        let entry_node = self.create_node(NodeKind::Entry, [], vec![NodeOutputKind::Control]);
        self.function.set_entry(entry_node);

        let memory_node =
            self.create_node(NodeKind::InitialMemory, [], vec![NodeOutputKind::Memory]);
        let [memory] = self.function().node_outputs_exact(memory_node)?;
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
        let [true_ctrl_id, false_ctrl_id] = self.function().node_outputs_exact(brcond)?;

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
        let [out] = self.function().node_outputs_exact(node)?;
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
        let [out] = self.function().node_outputs_exact(node)?;
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
        let [new_mem] = self.function().node_outputs_exact(node_id)?;
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
    pub(super) fn build_vn_phi(
        &mut self,
        var: rsleigh::Vn,
        phi_token: NodeOutputId,
        incoming_values: &[NodeOutputId],
    ) -> Result<NodeOutputId> {
        self.require_phi_token_kind(phi_token)?;
        self.validate_value_inputs(incoming_values)?;
        let output_type = NodeOutputType::int_for_byte_size(var.size)?;
        let out = self.build_single_output_pure(
            NodeKind::Phi,
            core::iter::once(phi_token).chain(incoming_values.iter().copied()),
            output_type,
        );
        let (node_id, _slot) = self.function().output_definition(out);
        self.function_mut().set_phi_var_tag(node_id, var);
        Ok(out)
    }
}
