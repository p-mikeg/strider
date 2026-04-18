use crate::error::{ErrorKind, Result};
use crate::function::FunctionGraph;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
use crate::region::{Region, RegionId};
use cranelift_entity::{PrimaryMap, SecondaryMap, entity_impl};
use smallvec::SmallVec;
use std::collections::HashMap;

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
    pub(crate) arg_passing_vars: Vec<rsleigh::Vn>,
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
        _ret_vars: &[rsleigh::Vn],
    ) -> Result<Self> {
        // For register varnodes, keep only the largest enclosing register.
        // e.g. if both `rdi` and `edi` are clobbered, drop `edi` because
        // clobbering `rdi` already implies `edi`.
        let all_variables: Vec<_> = all_used_variables
            .iter()
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
            })
            .copied()
            .collect();
        let call_cloberred_variables: Vec<_> = all_variables
            .iter()
            .filter(|v| !callee_saved_vars.contains(v))
            .copied()
            .collect();
        let mut variables = PrimaryMap::new();
        let mut variable_to_id = HashMap::new();
        for variable in all_variables {
            let var_id = variables.push(variable);
            variable_to_id.insert(variable, var_id);
        }
        let arg_passing_vars: Vec<_> = arg_passing_vars
            .iter()
            .copied()
            .filter(|vn| variable_to_id.contains_key(vn))
            .collect();

        let mut fb = FunctionBuilder {
            function: FunctionGraph::new_invalid(),
            regions: PrimaryMap::new(),
            cur_region: None,
            variables,
            variable_to_id,
            arg_passing_vars,
            call_cloberred_variables,
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
    pub fn get_output_type(&self, output_id: NodeOutputId) -> Result<NodeOutputType> {
        let kind = self.graph().output_kind(output_id);
        kind.as_value()
            .ok_or_else(|| ErrorKind::ExpectedValue(output_id, kind).into())
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
            return Err(ErrorKind::ExpectedValue(output_id, output_kind).into());
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

    /// If `output_id` is a constant node, returns its value truncated to the
    /// declared [`NodeOutputType`] as an unsigned 64-bit integer.
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    pub fn get_as_unsigned_int(&self, output_id: NodeOutputId) -> Result<Option<u64>> {
        let output_type = self.get_output_type(output_id)?;
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) if output_type.is_integer() => {
                Ok(output_type.get_unsigned_int(*val))
            }
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
            NodeKind::IntConst(val) if output_type.is_integer() => {
                Ok(output_type.get_signed_int(*val))
            }
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
    pub fn truncate_if_needed(
        &mut self,
        output_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
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
    pub fn extend_if_needed(
        &mut self,
        output_id: NodeOutputId,
        output_type: NodeOutputType,
        op: ExtendOp,
    ) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;

        if let Some((unsigned_val, signed_val)) = self.get_as_int(output_id)? {
            return Ok(match op {
                ExtendOp::SignExtend => self.build_int_const(signed_val as u64, output_type),
                ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            });
        }

        if !output_type.is_integer() {
            return Err(ErrorKind::ExpectedInteger(output_id).into());
        }

        if curr_output_type.byte_size() >= output_type.byte_size() {
            return Ok(output_id);
        }
        Ok(self.build_single_output_pure(NodeKind::Extend(op), [output_id], output_type))
    }

    /// Converts `output_id` to `output_type`, truncating or zero-extending as needed.
    pub fn convert_to_int_if_needed(
        &mut self,
        output_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;
        if curr_output_type.is_integer() {
            let truncate_id = self.truncate_if_needed(output_id, output_type)?;
            let extend_id =
                self.extend_if_needed(truncate_id, output_type, ExtendOp::ZeroExtend)?;
            return Ok(extend_id);
        }
        Ok(self.build_single_output_pure(NodeKind::CastToInt, [output_id], output_type))
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

    /// Emits a `Piece` node: `result = (hi << bit_width(lo)) | lo`.
    /// inputs[0] = hi (most significant), inputs[1] = lo (least significant).
    ///
    /// Non-integer inputs (bool, float) are automatically coerced to integers.
    pub fn build_piece(
        &mut self,
        hi: NodeOutputId,
        lo: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let hi_ty = self.get_output_type(hi)?.to_natural_int_type();
        let hi = self.convert_to_int_if_needed(hi, hi_ty)?;
        let lo_ty = self.get_output_type(lo)?.to_natural_int_type();
        let lo = self.convert_to_int_if_needed(lo, lo_ty)?;
        Ok(self.build_single_output_pure(NodeKind::Piece, [hi, lo], output_type))
    }

    /// Emits an `Insert` node: inserts `len` bits from `src` into `dest` at bit `lsb`.
    /// inputs[0] = dest, inputs[1] = src.
    ///
    /// Non-integer inputs are automatically coerced to integers.
    pub fn build_insert(
        &mut self,
        dest: NodeOutputId,
        src: NodeOutputId,
        lsb: u8,
        len: u8,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let dest_ty = self.get_output_type(dest)?.to_natural_int_type();
        let dest = self.convert_to_int_if_needed(dest, dest_ty)?;
        let src_ty = self.get_output_type(src)?.to_natural_int_type();
        let src = self.convert_to_int_if_needed(src, src_ty)?;
        Ok(self.build_single_output_pure(NodeKind::Insert { lsb, len }, [dest, src], output_type))
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

    /// If `output_id` is a `FloatConst` node, returns its raw bit pattern.
    /// Returns `Ok(None)` for non-constant nodes.
    pub fn get_as_float_bits(&self, output_id: NodeOutputId) -> Result<Option<u64>> {
        let output_type = self.get_output_type(output_id)?;
        if !output_type.is_float() {
            return Ok(None);
        }
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::FloatConst(bits) => Ok(Some(*bits)),
            _ => Ok(None),
        }
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

    /// If `input` is not already `float_ty`, wraps it in a `CastToFloat` node.
    fn cast_to_float_if_needed(
        &mut self,
        input: NodeOutputId,
        float_ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if self.get_output_type(input)? == float_ty {
            return Ok(input);
        }
        Ok(self.build_cast_to_float(input, float_ty))
    }

    /// Infers the float type to use for a value that may be int or float.
    /// If the value is already a float type, that type is used.
    /// For integers, maps byte size: ≤4 → F32, otherwise → F64.
    fn infer_float_type(&self, input: NodeOutputId) -> Result<NodeOutputType> {
        let ty = self.get_output_type(input)?;
        if ty.is_float() {
            return Ok(ty);
        }
        Ok(if ty.byte_size() <= 4 {
            NodeOutputType::F32
        } else {
            NodeOutputType::F64
        })
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
        let &id = self
            .variable_to_id
            .get(variable)
            .ok_or(ErrorKind::VariableNotFound(*variable))?;
        self.read_variable_from_id(id)
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
            initial_variables[var_id] =
                self.build_single_output_pure(NodeKind::InitialVar(var), [], output_type);
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
        let memory_node = self.create_node(NodeKind::MemPhi, [], [NodeOutputKind::Memory]);
        let [memory] = self.graph().node_outputs_exact(memory_node)?;

        let control_node = self.create_node(
            NodeKind::ControlState,
            [],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
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
    fn build_control_phi(
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
        for &v in &ret_inputs {
            let kind = self.graph().output_kind(v);
            if !kind.is_value() {
                return Err(ErrorKind::ExpectedValue(v, kind).into());
            }
        }

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

    /// Writes `value` to `variable` in the active region.
    pub fn write_variable(&mut self, variable: &rsleigh::Vn, value: NodeOutputId) -> Result<()> {
        let var_id = *self
            .variable_to_id
            .get(variable)
            .ok_or(ErrorKind::VariableNotFound(*variable))?;
        self.write_variable_from_id(var_id, value)
    }

    /// Terminates the current region with a `Call` node.
    pub fn build_call(&mut self, call_address: NodeOutputId) -> Result<()> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        let arg_passing: SmallVec<[NodeOutputId; 4]> = self
            .arg_passing_vars
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;
        let clobbered: SmallVec<[_; 4]> = self.call_cloberred_variables.iter().copied().collect();

        let clobbered_outputs: SmallVec<[NodeOutputId; 4]> = self
            .call_cloberred_variables
            .iter()
            .map(|var| self.read_variable(var))
            .collect::<Result<_>>()?;

        let cloberred_kinds: SmallVec<[NodeOutputKind; 4]> = clobbered_outputs
            .iter()
            .map(|v| self.graph().output_kind(*v))
            .collect();

        for &v in &arg_passing {
            let kind = self.graph().output_kind(v);
            if !kind.is_value() {
                return Err(ErrorKind::ExpectedValue(v, kind).into());
            }
        }
        for k in &cloberred_kinds {
            if !k.is_value() {
                return Err(ErrorKind::ExpectedValue(NodeOutputId::default(), *k).into());
            }
        }
        let addr_kind = self.graph().output_kind(call_address);
        if !addr_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(call_address, addr_kind).into());
        }

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory]
            .into_iter()
            .chain(cloberred_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        let call_outputs: Vec<_> = self.graph().node_outputs(call).into_iter().collect();

        self.advance_cur_region_ctrl(call_outputs[0])?;
        self.advance_cur_region_memory(call_outputs[1])?;
        for (variable, new_val) in core::iter::zip(clobbered, call_outputs.iter().skip(2)) {
            self.write_variable(&variable, *new_val)?;
        }
        Ok(())
    }

    /// Emits a `CallOther` (user-defined op) node and advances the control
    /// and memory chain of the active region.
    ///
    /// `args` are additional arguments to the intrinsic (may be empty).
    /// `output_ty` is `Some` when the source instruction has an output varnode
    /// and `None` when the intrinsic produces no value (e.g. `syscall` without
    /// an explicit return).  Memory is always treated as clobbered.
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
    ) -> Result<Option<NodeOutputId>> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        for &v in args {
            let kind = self.graph().output_kind(v);
            if !kind.is_value() {
                return Err(ErrorKind::ExpectedValue(v, kind).into());
            }
        }

        let mut output_kinds: SmallVec<[NodeOutputKind; 3]> = SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        if let Some(ty) = output_ty {
            output_kinds.push(NodeOutputKind::OutputType(ty));
        }

        let inputs = [ctrl, memory].into_iter().chain(args.iter().copied());
        let node = self.create_node(NodeKind::CallOther { user_op_id }, inputs, output_kinds);
        let outputs: SmallVec<[NodeOutputId; 3]> =
            self.graph().node_outputs(node).into_iter().collect();
        self.advance_cur_region_ctrl(outputs[0])?;
        self.advance_cur_region_memory(outputs[1])?;
        Ok(outputs.get(2).copied())
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
        for &r in refs {
            let kind = self.graph().output_kind(r);
            if !kind.is_value() {
                return Err(ErrorKind::ExpectedValue(r, kind).into());
            }
        }
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
        for &a in args {
            let kind = self.graph().output_kind(a);
            if !kind.is_value() {
                return Err(ErrorKind::ExpectedValue(a, kind).into());
            }
        }
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

    /// Finalises and returns the completed [`BuiltFunctionGraph`], after running
    /// structural validation on the built graph.
    pub fn build(self) -> crate::Result<crate::function::BuiltFunctionGraph> {
        let built = crate::function::BuiltFunctionGraph {
            graph: self.function.graph,
            entry: self.function.entry,
            variables: self.variables,
            call_clobbered: self.call_cloberred_variables.into_boxed_slice(),
        };
        crate::validate::validate(&built.graph, built.entry)?;
        Ok(built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};
    use crate::ops::{BoolBinaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};

    /// Build a minimal builder with no variables so tests that do not need
    /// SSA variables remain simple.
    fn empty_builder() -> Result<FunctionBuilder> {
        FunctionBuilder::new(vec![], &[], &[], &[])
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
        assert_eq!(val, Some(0)); // 256 & 0xFF == 0
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
        assert_eq!(
            result, add,
            "non-const U8 value must not be touched when target is U16"
        );
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
            "expected Truncate node, got {:?}",
            b.graph().node_kind(node)
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
        let result =
            b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;
        let node = b.graph().get_node_from_output(result);
        assert_eq!(
            b.graph().node_kind(node),
            &NodeKind::IntBinaryOp(IntBinaryOp::Add)
        );
        Ok(())
    }

    /// When the operands differ in width, `build_int_binary_operation` must
    /// insert a coercion node so both reach the target type.
    #[test]
    fn build_int_binary_op_coerces_narrower_operand() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_int_const(1, NodeOutputType::U8);
        let rhs = b.build_int_const(2, NodeOutputType::U64);
        let result =
            b.build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, NodeOutputType::U64)?;
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
        assert_eq!(
            b.graph().node_kind(node),
            &NodeKind::BoolBinaryOp(BoolBinaryOp::And)
        );
        assert_eq!(
            b.graph().output_kind(result),
            NodeOutputKind::OutputType(NodeOutputType::Bool)
        );
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

    // ── Float builder methods ────────────────────────────────────────────────

    #[test]
    fn build_float_const_f32_has_correct_bits() -> Result<()> {
        let mut b = empty_builder()?;
        let bits = 1.0f32.to_bits() as u64;
        let out = b.build_float_const(bits, NodeOutputType::F32);
        let kind = *b.graph().node_kind(b.graph().get_node_from_output(out));
        assert_eq!(kind, NodeKind::FloatConst(bits));
        assert_eq!(
            b.graph().output_kind(out),
            NodeOutputKind::OutputType(NodeOutputType::F32)
        );
        Ok(())
    }

    #[test]
    fn build_float_const_f64_has_correct_bits() -> Result<()> {
        let mut b = empty_builder()?;
        let bits = 1.0f64.to_bits();
        let out = b.build_float_const(bits, NodeOutputType::F64);
        let kind = *b.graph().node_kind(b.graph().get_node_from_output(out));
        assert_eq!(kind, NodeKind::FloatConst(bits));
        assert_eq!(
            b.graph().output_kind(out),
            NodeOutputKind::OutputType(NodeOutputType::F64)
        );
        Ok(())
    }

    #[test]
    fn get_as_float_bits_returns_bits_for_float_const() -> Result<()> {
        let mut b = empty_builder()?;
        let bits = 2.5f64.to_bits();
        let out = b.build_float_const(bits, NodeOutputType::F64);
        assert_eq!(b.get_as_float_bits(out)?, Some(bits));
        Ok(())
    }

    #[test]
    fn get_as_float_bits_returns_none_for_int_const() -> Result<()> {
        let mut b = empty_builder()?;
        let out = b.build_int_const(42, NodeOutputType::U64);
        assert_eq!(b.get_as_float_bits(out)?, None);
        Ok(())
    }

    #[test]
    fn int_bits_to_float_folds_int_const_immediately() -> Result<()> {
        let mut b = empty_builder()?;
        let bits = 1.0f32.to_bits() as u64;
        let int_out = b.build_int_const(bits, NodeOutputType::U32);
        let float_out = b.build_int_bits_to_float(int_out, NodeOutputType::F32)?;
        // Should be a FloatConst, not an IntBitsToFloat node
        let kind = *b
            .graph()
            .node_kind(b.graph().get_node_from_output(float_out));
        assert_eq!(kind, NodeKind::FloatConst(bits));
        Ok(())
    }

    #[test]
    fn float_bits_to_int_folds_float_const_immediately() -> Result<()> {
        let mut b = empty_builder()?;
        let bits = 1.0f64.to_bits();
        let float_out = b.build_float_const(bits, NodeOutputType::F64);
        let int_out = b.build_float_bits_to_int(float_out, NodeOutputType::U64)?;
        // Should be an IntConst, not a FloatBitsToInt node
        let kind = *b.graph().node_kind(b.graph().get_node_from_output(int_out));
        assert_eq!(kind, NodeKind::IntConst(bits));
        Ok(())
    }

    #[test]
    fn build_float_binary_op_produces_correct_node() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
        let rhs = b.build_float_const(2.0f32.to_bits() as u64, NodeOutputType::F32);
        let out = b.build_float_binary_op(lhs, rhs, FloatBinaryOp::Add, NodeOutputType::F32)?;
        let kind = *b.graph().node_kind(b.graph().get_node_from_output(out));
        assert_eq!(kind, NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
        Ok(())
    }

    #[test]
    fn build_float_cmp_op_produces_bool_output() -> Result<()> {
        let mut b = empty_builder()?;
        let lhs = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
        let rhs = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
        let out = b.build_float_cmp_op(lhs, rhs, FloatCmpOp::Less)?;
        assert_eq!(
            b.graph().output_kind(out),
            NodeOutputKind::OutputType(NodeOutputType::Bool)
        );
        Ok(())
    }

    #[test]
    fn build_int_bits_to_float_inserts_node_for_non_const() -> Result<()> {
        let mut b = empty_builder()?;
        let int_val = b.build_int_const(0x3F800000, NodeOutputType::U32);
        let zero = b.build_int_const(0, NodeOutputType::U32);
        // Build an Add(x, 0) so the result is not an IntConst node.
        let non_const = b.build_int_binary_operation(
            int_val,
            zero,
            crate::ops::IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let float_out = b.build_int_bits_to_float(non_const, NodeOutputType::F32)?;
        let kind = *b
            .graph()
            .node_kind(b.graph().get_node_from_output(float_out));
        assert_eq!(kind, NodeKind::IntBitsToFloat);
        Ok(())
    }

    // ── CastToFloat tests ─────────────────────────────────────────────────────

    #[test]
    fn build_cast_to_float_creates_cast_node() -> Result<()> {
        let mut b = empty_builder()?;
        let int_val = b.build_int_const(42, NodeOutputType::U64);
        let cast = b.build_cast_to_float(int_val, NodeOutputType::F64);
        let kind = *b.graph().node_kind(b.graph().get_node_from_output(cast));
        assert_eq!(kind, NodeKind::CastToFloat);
        assert_eq!(b.get_output_type(cast)?, NodeOutputType::F64);
        Ok(())
    }

    #[test]
    fn cast_to_float_if_needed_is_identity_for_same_type() -> Result<()> {
        let mut b = empty_builder()?;
        let float_val = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
        let result = b.cast_to_float_if_needed(float_val, NodeOutputType::F32)?;
        // Should be the same output — no new node inserted.
        assert_eq!(result, float_val);
        Ok(())
    }

    #[test]
    fn build_float_binary_op_with_int_inputs_auto_casts() -> Result<()> {
        let mut b = empty_builder()?;
        let i1 = b.build_int_const(0x3F800000u64, NodeOutputType::U32);
        let i2 = b.build_int_const(0x40000000u64, NodeOutputType::U32);
        // Both inputs are U32 — builder should auto-insert CastToFloat.
        let result = b.build_float_binary_op(i1, i2, FloatBinaryOp::Add, NodeOutputType::F32)?;
        let kind = *b.graph().node_kind(b.graph().get_node_from_output(result));
        assert_eq!(kind, NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
        // Verify inputs are CastToFloat nodes.
        let [lhs, rhs] = b
            .graph()
            .node_inputs_exact::<2>(b.graph().get_node_from_output(result))?;
        let lhs_node = b.graph().get_node_from_output(lhs);
        let rhs_node = b.graph().get_node_from_output(rhs);
        assert_eq!(*b.graph().node_kind(lhs_node), NodeKind::CastToFloat);
        assert_eq!(*b.graph().node_kind(rhs_node), NodeKind::CastToFloat);
        Ok(())
    }

    // ── CallOther / SegmentOp / CPoolRef / New ──────────────────────────────

    /// Helper: build a single-region builder with an active region set.
    fn builder_with_region() -> Result<FunctionBuilder> {
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        Ok(b)
    }

    #[test]
    fn build_call_other_without_output_advances_ctrl_and_memory() -> Result<()> {
        let mut b = builder_with_region()?;
        let ctrl_before = b.cur_region_control()?;
        let mem_before = b.cur_region_memory()?;

        let result = b.build_call_other(7, &[], None)?;
        assert!(result.is_none(), "no output varnode → no value output");

        // Ctrl and memory tokens must advance (be different outputs).
        let ctrl_after = b.cur_region_control()?;
        let mem_after = b.cur_region_memory()?;
        assert_ne!(ctrl_before, ctrl_after);
        assert_ne!(mem_before, mem_after);

        // The node must be a CallOther with the given id.
        let node = b.graph().get_node_from_output(ctrl_after);
        assert_eq!(
            b.graph().node_kind(node),
            &NodeKind::CallOther { user_op_id: 7 }
        );
        Ok(())
    }

    #[test]
    fn build_call_other_with_output_returns_typed_value() -> Result<()> {
        let mut b = builder_with_region()?;
        let arg = b.build_int_const(0x42, NodeOutputType::U64);
        let out = b
            .build_call_other(3, &[arg], Some(NodeOutputType::U32))?
            .ok_or_else(|| ErrorKind::AssertionFailed("output_ty = Some → value output".into()))?;
        assert_eq!(
            b.graph().output_kind(out),
            NodeOutputKind::OutputType(NodeOutputType::U32)
        );
        let node = b.graph().get_node_from_output(out);
        assert_eq!(
            b.graph().node_kind(node),
            &NodeKind::CallOther { user_op_id: 3 }
        );
        Ok(())
    }

    #[test]
    fn build_call_other_rejects_non_value_arg() -> Result<()> {
        let mut b = builder_with_region()?;
        let mem = b.cur_region_memory()?;
        let res = b.build_call_other(0, &[mem], None);
        assert!(matches!(
            res.as_ref().map_err(|e| e.kind()),
            Err(ErrorKind::ExpectedValue(_, _))
        ));
        Ok(())
    }

    #[test]
    fn build_segment_op_produces_pure_node() -> Result<()> {
        let mut b = builder_with_region()?;
        let seg = b.build_int_const(0x10, NodeOutputType::U16);
        let off = b.build_int_const(0x100, NodeOutputType::U32);
        let out = b.build_segment_op(1, seg, off, NodeOutputType::U64)?;
        let node = b.graph().get_node_from_output(out);
        assert_eq!(b.graph().node_kind(node), &NodeKind::SegmentOp { op_id: 1 });
        assert_eq!(
            b.graph().output_kind(out),
            NodeOutputKind::OutputType(NodeOutputType::U64)
        );
        Ok(())
    }

    #[test]
    fn build_segment_op_is_cacheable_across_identical_calls() -> Result<()> {
        let mut b = builder_with_region()?;
        let seg = b.build_int_const(0x10, NodeOutputType::U16);
        let off = b.build_int_const(0x100, NodeOutputType::U32);
        let a = b.build_segment_op(1, seg, off, NodeOutputType::U64)?;
        let c = b.build_segment_op(1, seg, off, NodeOutputType::U64)?;
        assert_eq!(a, c, "SegmentOp is pure → identical calls must dedup");
        Ok(())
    }

    #[test]
    fn build_cpool_ref_produces_typed_node() -> Result<()> {
        let mut b = builder_with_region()?;
        let r0 = b.build_int_const(0xAA, NodeOutputType::U32);
        let r1 = b.build_int_const(0xBB, NodeOutputType::U32);
        let out = b.build_cpool_ref(&[r0, r1], NodeOutputType::U64)?;
        let node = b.graph().get_node_from_output(out);
        assert_eq!(b.graph().node_kind(node), &NodeKind::CPoolRef);
        Ok(())
    }

    #[test]
    fn build_cpool_ref_is_not_deduplicated() -> Result<()> {
        let mut b = builder_with_region()?;
        let r0 = b.build_int_const(0xAA, NodeOutputType::U32);
        let a = b.build_cpool_ref(&[r0], NodeOutputType::U64)?;
        let c = b.build_cpool_ref(&[r0], NodeOutputType::U64)?;
        assert_ne!(
            a, c,
            "CPoolRef is non-cacheable → must yield distinct nodes"
        );
        Ok(())
    }

    #[test]
    fn build_new_produces_typed_node() -> Result<()> {
        let mut b = builder_with_region()?;
        let size = b.build_int_const(32, NodeOutputType::U64);
        let out = b.build_new(&[size], NodeOutputType::U64)?;
        let node = b.graph().get_node_from_output(out);
        assert_eq!(b.graph().node_kind(node), &NodeKind::New);
        Ok(())
    }

    #[test]
    fn build_new_is_not_deduplicated() -> Result<()> {
        let mut b = builder_with_region()?;
        let size = b.build_int_const(32, NodeOutputType::U64);
        let a = b.build_new(&[size], NodeOutputType::U64)?;
        let c = b.build_new(&[size], NodeOutputType::U64)?;
        assert_ne!(a, c, "each allocation must yield a distinct node");
        Ok(())
    }

    #[test]
    fn build_piece_with_float_input_auto_casts() -> Result<()> {
        let mut b = empty_builder()?;
        // Create a float value to pass into piece.
        let float_val = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
        let int_lo = b.build_int_const(0, NodeOutputType::U32);
        // Piece should succeed and insert a CastToInt for the float hi.
        let result = b.build_piece(float_val, int_lo, NodeOutputType::U64)?;
        // Result must be a Piece node.
        let kind = *b.graph().node_kind(b.graph().get_node_from_output(result));
        assert_eq!(kind, NodeKind::Piece);
        Ok(())
    }
}
