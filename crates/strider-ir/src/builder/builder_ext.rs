//! The shared IR construction vocabulary. Any [`IRBuilder`] gains every
//! `build_*` constructor for free via the blanket impl — lifter, optimizer
//! editing context, and plain function all build IR the same way.
//!
//! Every constructor here is **pure**: its body bottoms out in
//! `self.create_node(...)` / [`IRBuilderExt::build_single_output_pure`] plus
//! read-only `self.function()` queries, with no dependence on lift-time
//! scratch (the active region, the SSA variable table, the largest-container
//! cache, etc.). The few constructors that DO touch that scratch
//! (`build_store` / `build_load` route through the active region's memory
//! token; `build_entry` / `build_return` / `build_if` / `build_call` and
//! friends terminate or link regions) stay inherent on
//! [`crate::FunctionBuilder`].

use anyhow::anyhow;

use crate::builder::IRBuilder;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use crate::ops::{ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

/// Unified return shape for [`IRBuilderExt::const_value`].
///
/// `Int { val, ty }` carries the raw `u128` payload of an `IntConst`
/// node alongside its declared `ValueType` so callers can decide
/// whether to view it unsigned / signed / mask / etc.  `Float` carries
/// the raw bit pattern of a `FloatConst` — the analyzer never needs
/// the float type for constant folding (`f32` vs `f64` is inferred
/// from the surrounding op), so the type isn't carried here.
#[derive(Debug, Clone, Copy)]
pub enum ConstValue {
    Int { val: u128, ty: ValueType },
    Float { bits: u64 },
}

/// The shared `build_*` construction vocabulary, available on every
/// [`IRBuilder`] via the blanket impl below.
///
/// All methods are provided (default) — implementors gain them for free.
/// The read-only helpers ([`Self::value_type`], the `require_*` family,
/// [`Self::validate_value_inputs`]) only consult `self.function()`, and the
/// constructors only call `self.create_node(...)` /
/// [`Self::build_single_output_pure`], so the whole vocabulary is pure with
/// respect to any lift-time scratch the implementor may carry.
pub trait IRBuilderExt: IRBuilder {
    // ── read accessors ───────────────────────────────────────────────────
    //
    // Structural reads forwarded onto `self.function()`, so every builder
    // (`Function` / `FunctionBuilder` / `EditFunction`) shares one vocabulary
    // for querying a node's input / output edges.

    /// Returns the input value edges of `node` as an iterator.
    fn node_inputs(&self, node: NodeId) -> crate::Inputs<'_> {
        self.function().node_inputs(node)
    }

    /// Returns the output value edges of `node`.
    fn node_outputs(&self, node: NodeId) -> &[ValueId] {
        self.function().node_outputs(node)
    }

    /// Returns the exactly-`N` input value edges of `node`.
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` inputs.
    fn node_inputs_exact<const N: usize>(&self, node: NodeId) -> Result<[ValueId; N]> {
        self.function().graph().node_inputs_exact(node)
    }

    /// Returns the exactly-`N` output value edges of `node`.
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` outputs.
    fn node_outputs_exact<const N: usize>(&self, node: NodeId) -> Result<[ValueId; N]> {
        self.function().node_outputs_exact(node)
    }

    // ── read-only helpers ────────────────────────────────────────────────

    /// Retrieves the [`ValueType`] of `value_id`.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is a control, memory, or
    /// control-phi edge (i.e. not a value edge).
    fn value_type(&self, value_id: ValueId) -> Result<ValueType> {
        let kind = self.function().value_kind(value_id);
        kind.as_value()
            .ok_or_else(|| anyhow!("output {value_id:?} is not a value edge (got {kind:?})"))
    }

    /// Asserts that `value_id` already carries exactly `expected`, returning
    /// it unchanged on success.  The strict counterpart to the coercion
    /// helpers: the value-producing `build_*` constructors call it instead of
    /// silently truncating / extending / bit-casting an operand.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge, or when its
    /// type differs from `expected`.
    fn require_value_type(&self, value_id: ValueId, expected: ValueType) -> Result<ValueId> {
        let actual = self.value_type(value_id)?;
        if actual != expected {
            return Err(anyhow!(
                "operand {value_id:?} has type {actual} but the operation \
                 requires {expected}; the caller must insert the truncate / \
                 extend / bitcast fix-up (builders no longer auto-coerce)"
            ));
        }
        Ok(value_id)
    }

    /// Errors unless `value_id` is a value edge.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a value edge.
    fn require_value_kind(&self, value_id: ValueId) -> Result<()> {
        let kind = self.function().value_kind(value_id);
        if !kind.is_value() {
            return Err(anyhow!("output {value_id:?} is not a value edge (got {kind:?})"));
        }
        Ok(())
    }

    /// Errors unless `value_id` carries a bool value.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a bool value.
    fn require_bool_value(&self, value_id: ValueId) -> Result<()> {
        if !self.function().value_kind(value_id).is_bool() {
            return Err(anyhow!("output {value_id:?} is not a bool value"));
        }
        Ok(())
    }

    /// Errors unless `value_id` is a phi-token edge.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a phi-token edge.
    fn require_phi_token_kind(&self, value_id: ValueId) -> Result<()> {
        if !self.function().value_kind(value_id).is_phi_token() {
            return Err(anyhow!("output {value_id:?} is not a phi-token edge"));
        }
        Ok(())
    }

    /// Errors unless `value_id` carries an integer value.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not an integer value.
    fn require_integer_value(&self, value_id: ValueId) -> Result<()> {
        if !self.value_type(value_id)?.is_integer() {
            return Err(anyhow!("output {value_id:?} is not an integer value"));
        }
        Ok(())
    }

    /// Errors unless `value_id` carries a float value.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a float value.
    fn require_float_value(&self, value_id: ValueId) -> Result<()> {
        if !self.value_type(value_id)?.is_float() {
            return Err(anyhow!("output {value_id:?} is not a float value"));
        }
        Ok(())
    }

    /// Errors unless `ty` is an integer type.
    ///
    /// # Errors
    /// Returns an error when `ty` is not an integer type.
    fn require_integer_type(ty: ValueType) -> Result<()> {
        if !ty.is_integer() {
            return Err(anyhow!("type {ty:?} is not an integer type"));
        }
        Ok(())
    }

    /// Errors unless `ty` is a float type.
    ///
    /// # Errors
    /// Returns an error when `ty` is not a float type.
    fn require_float_type(ty: ValueType) -> Result<()> {
        if !ty.is_float() {
            return Err(anyhow!("type {ty:?} is not a float type"));
        }
        Ok(())
    }

    /// Errors if any element of `inputs` is not a value edge.
    ///
    /// # Errors
    /// Returns an error when any input is not a value edge.
    fn validate_value_inputs(&self, inputs: &[ValueId]) -> Result<()> {
        for &v in inputs {
            self.require_value_kind(v)?;
        }
        Ok(())
    }

    /// Creates a single-output, pure (no side-effect) node and returns its
    /// output id.
    fn build_single_output_pure(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_type: ValueType,
    ) -> ValueId {
        let node = self.create_node(kind, inputs, [ValueKind::Typed(output_type)]);
        self.function().node_outputs(node)[0]
    }

    // ── constant inspection ──────────────────────────────────────────────

    /// Returns the constant value carried by `value_id` if its defining
    /// node is `IntConst` or `FloatConst`; `Ok(None)` otherwise.  The
    /// `get_as_*` helpers below are thin projections off this unified
    /// shape.  Booleans are `IntConst` values typed `I1`.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn const_value(&self, value_id: ValueId) -> Result<Option<ConstValue>> {
        let ty = self.value_type(value_id)?;
        Ok(match self.function().kind_of_value(value_id) {
            NodeKind::IntConst(val) if ty.is_integer() => Some(ConstValue::Int { val: *val, ty }),
            NodeKind::FloatConst(bits) if ty.is_float() => Some(ConstValue::Float { bits: *bits }),
            _ => None,
        })
    }

    /// If `value_id` is a constant node, returns its value truncated to the
    /// declared [`ValueType`] as an unsigned 64-bit integer.
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn get_as_unsigned_int(&self, value_id: ValueId) -> Result<Option<u64>> {
        Ok(self.const_value(value_id)?.and_then(|c| match c {
            ConstValue::Int { val, ty } => {
                ty.get_unsigned_int(val).and_then(|v| u64::try_from(v).ok())
            }
            ConstValue::Float { .. } => None,
        }))
    }

    /// If `value_id` is an integer constant, returns its value
    /// sign-extended to `i64` according to the declared [`ValueType`].
    /// An `I1` boolean folds as `0` / `1` per [`Self::get_as_unsigned_int`].
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn get_as_signed_int(&self, value_id: ValueId) -> Result<Option<i64>> {
        Ok(self.const_value(value_id)?.and_then(|c| match c {
            ConstValue::Int { val, ty } => {
                ty.get_signed_int(val).and_then(|v| i64::try_from(v).ok())
            }
            ConstValue::Float { .. } => None,
        }))
    }

    /// Returns both the unsigned and signed interpretations of `value_id` if
    /// it is an integer constant, or `None` otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn get_as_int(&self, value_id: ValueId) -> Result<Option<(u64, i64)>> {
        Ok(self.get_as_unsigned_int(value_id)?.zip(self.get_as_signed_int(value_id)?))
    }

    /// If `value_id` is a `FloatConst` node, returns its raw bit pattern.
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn get_as_float_bits(&self, value_id: ValueId) -> Result<Option<u64>> {
        Ok(self.const_value(value_id)?.and_then(|c| match c {
            ConstValue::Float { bits } => Some(bits),
            _ => None,
        }))
    }

    // ── width / type coercion ────────────────────────────────────────────

    /// Truncates `value_id` to `output_type` if it is currently wider.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn truncate_if_needed(
        &mut self,
        value_id: ValueId,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;

        if let Some(val) = self.get_as_unsigned_int(value_id)? {
            return self.build_int_const(val, output_type);
        }

        if curr_output_type.bit_width() <= output_type.bit_width() {
            return Ok(value_id);
        }

        Ok(self.build_single_output_pure(NodeKind::Truncate, [value_id], output_type))
    }

    /// Extends `value_id` to `output_type` using zero- or sign-extension.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge, or when
    /// `output_type` is not an integer type and the input is not already a
    /// constant we can fold.
    fn extend_if_needed(
        &mut self,
        value_id: ValueId,
        output_type: ValueType,
        op: ExtendOp,
    ) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;

        if let Some((unsigned_val, signed_val)) = self.get_as_int(value_id)? {
            // signed_val is i64; `i64 as u128` sign-extends to fill the
            // high 64 bits, and build_int_const masks to output_type's
            // width.
            return match op {
                ExtendOp::SignExtend => self.build_int_const(signed_val as u128, output_type),
                ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            };
        }

        if !output_type.is_integer() {
            return Err(anyhow!("output {value_id:?} is not an integer value"));
        }

        // Booleans are I1 (integer); the only non-integer input here would be
        // a float, which cannot be width-extended as an integer — it needs an
        // explicit bitcast (`FloatBitsToInt`) first.
        if !curr_output_type.is_integer() {
            return Err(anyhow!(
                "cannot integer-extend non-integer value {value_id:?} \
                 ({curr_output_type}); a bitcast is required first"
            ));
        }

        if curr_output_type.bit_width() == output_type.bit_width() {
            return Ok(value_id);
        }
        if curr_output_type.bit_width() > output_type.bit_width() {
            // Caller asked to extend a value that is already wider than the
            // target.  Truncate so the returned id always carries
            // `output_type`.
            return self.truncate_if_needed(value_id, output_type);
        }
        Ok(self.build_single_output_pure(NodeKind::Extend(op), [value_id], output_type))
    }

    /// Converts `value_id` to integer `output_type`, truncating or
    /// zero-extending as needed.  Keys on **bit width**, so an `I1` boolean
    /// widens to a wider integer via `ZeroExtend` (true→1, false→0) even
    /// though `I1` and `I8` share a byte size.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge or carries a
    /// non-integer (float) value.
    fn convert_to_int_if_needed(
        &mut self,
        value_id: ValueId,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;
        if !curr_output_type.is_integer() {
            return Err(anyhow!(
                "cannot convert non-integer value {value_id:?} \
                 ({curr_output_type}) to an integer; a bitcast is required first"
            ));
        }
        let truncate_id = self.truncate_if_needed(value_id, output_type)?;
        self.extend_if_needed(truncate_id, output_type, ExtendOp::ZeroExtend)
    }

    /// If `input` is not already `float_ty`, converts it: an integer input is
    /// reinterpreted bit-for-bit via `IntBitsToFloat`, and a float of a
    /// different precision is converted via `FloatToFloat`.
    ///
    /// # Errors
    ///
    /// Returns an error when `input` is not a value edge.
    fn cast_to_float_if_needed(
        &mut self,
        input: ValueId,
        float_ty: ValueType,
    ) -> Result<ValueId> {
        let in_ty = self.value_type(input)?;
        if in_ty == float_ty {
            return Ok(input);
        }
        // There is no `CastToFloat` node: an integer input is reinterpreted
        // bit-for-bit as a float of the same width (`IntBitsToFloat`), and a
        // float input of a different precision is converted (`FloatToFloat`).
        // Register reads are always same-width integers, so the lifter takes
        // the `IntBitsToFloat` arm.
        if in_ty.is_float() {
            self.build_float_to_float(input, float_ty)
        } else {
            self.build_int_bits_to_float(input, float_ty)
        }
    }

    /// Infers the float type to use for a value that may be int or float.
    /// If the value is already a float type, that type is used.
    /// For integers, maps byte size: ≤4 → F32, =8 → F64, =10 → F80.
    ///
    /// The 10-byte case targets x87 ST0/STn registers (which the analyzer
    /// represents as I80 on the int side); inferring F80 keeps the
    /// int→float bit-reinterpret round-trip width-preserving.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a value edge, or for an integer
    /// input whose byte size has no corresponding float type (5, 6, 7, 16,
    /// 32, 64).
    fn infer_float_type(&self, value: ValueId) -> Result<ValueType> {
        let ty = self.value_type(value)?;
        if ty.is_float() {
            return Ok(ty);
        }
        match ty.byte_size() {
            0..=4 => Ok(ValueType::F32),
            8 => Ok(ValueType::F64),
            10 => Ok(ValueType::F80),
            other => Err(anyhow!(
                "infer_float_type: integer byte_size {other} has no corresponding \
                 float type (input type: {ty:?})"
            )),
        }
    }

    // ── integer constructors ─────────────────────────────────────────────

    /// Emits a boolean constant — an `IntConst` of type `I1` (`true`→1,
    /// `false`→0).  Booleans are 1-bit integers; logical operations on them
    /// are ordinary `IntBinaryOp`/`IntUnaryOp` at `I1`.
    fn build_boolean_const(&mut self, val: bool) -> ValueId {
        self.build_single_output_pure(NodeKind::IntConst(u128::from(val)), [], ValueType::I1)
    }

    /// Emits an integer constant node.
    ///
    /// `val` is masked to `output_type`'s bit width before storage so the
    /// dedup-cache key sees the same `IntConst(u128)` payload for
    /// semantically-equal constants — `build_int_const(0x1FF, I8)` and
    /// `build_int_const(0xFF, I8)` dedup to the same node.  Accepts any value
    /// convertible to `u128` — most callers pass a `u64` literal.
    ///
    /// # Errors
    ///
    /// Returns an error when `output_type` is not an integer type, or when it
    /// is `I256` / `I512` (not representable in the `u128` storage that
    /// `IntConst` uses — use `FunctionBuilder::build_int_const_wide` instead).
    fn build_int_const(&mut self, val: impl Into<u128>, output_type: ValueType) -> Result<ValueId> {
        if !output_type.is_integer() {
            return Err(anyhow!(
                "build_int_const called with non-integer type {output_type:?}"
            ));
        }
        if matches!(output_type, ValueType::I256 | ValueType::I512) {
            return Err(anyhow!(
                "build_int_const({output_type:?}) not supported - IntConst storage is u128; \
                 use build_int_const_wide for I256/I512"
            ));
        }
        // Mask `val` to the declared output type's bit width so the
        // dedup-cache key sees the same `IntConst(u128)` payload for
        // semantically-equal constants.
        let masked = val.into() & output_type.bit_mask_u128();
        Ok(self.build_single_output_pure(NodeKind::IntConst(masked), [], output_type))
    }

    /// Emits an integer binary operation node.  **Strict:** both operands
    /// must already carry `output_type` — the caller inserts any
    /// truncate / extend fix-up (the builder no longer auto-coerces).
    ///
    /// # Errors
    ///
    /// Returns an error when an operand is not a value edge or does not
    /// already have type `output_type`.
    fn build_int_binary_operation(
        &mut self,
        lhs_id: ValueId,
        rhs_id: ValueId,
        op: IntBinaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let lhs_id = self.require_value_type(lhs_id, output_type)?;
        let rhs_id = self.require_value_type(rhs_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntBinaryOp(op), [lhs_id, rhs_id], output_type))
    }

    /// Emits an integer unary operation node.  **Strict:** the operand must
    /// already carry `output_type` (the caller inserts any fix-up).
    ///
    /// # Errors
    ///
    /// Returns an error when `input_id` is not a value edge or does not
    /// already have type `output_type`.
    fn build_int_unary_operation(
        &mut self,
        input_id: ValueId,
        op: IntUnaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let value = self.require_value_type(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntUnaryOp(op), [value], output_type))
    }

    /// Emits the canonical lowered shape for `lhs - rhs`:
    /// `Add(lhs, IntUnaryOp::Neg(rhs))`.
    ///
    /// `IntBinaryOp::Sub` is not a primitive in this IR; pcode-lift lowers
    /// `IntSub` opcodes at lift time.  This helper constructs the same shape
    /// from the builder API.
    ///
    /// # Errors
    ///
    /// Returns an error if either operand is not a value edge.
    fn build_sub_as_add_neg(
        &mut self,
        lhs_id: ValueId,
        rhs_id: ValueId,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let neg_rhs = self.build_int_unary_operation(rhs_id, IntUnaryOp::Neg, output_type)?;
        self.build_int_binary_operation(lhs_id, neg_rhs, IntBinaryOp::Add, output_type)
    }

    /// Emits a `Popcount` node that counts set bits in `input_id`.
    ///
    /// # Errors
    ///
    /// Returns an error when `input_id` is not a value edge.
    fn build_popcount(&mut self, input_id: ValueId, output_type: ValueType) -> Result<ValueId> {
        let value = self.require_value_type(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Popcount, [value], output_type))
    }

    /// Emits a `Lzcount` node that counts leading zero bits in `input_id`.
    ///
    /// # Errors
    ///
    /// Returns an error when `input_id` is not a value edge.
    fn build_lzcount(&mut self, input_id: ValueId, output_type: ValueType) -> Result<ValueId> {
        let value = self.require_value_type(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Lzcount, [value], output_type))
    }

    /// Emits an integer comparison node (output `I1`).  **Strict:** both
    /// operands must already carry `operand_type` (the comparison width).
    ///
    /// # Errors
    ///
    /// Returns an error when an operand is not a value edge or does not
    /// already have type `operand_type`.
    fn build_int_cmp_operation(
        &mut self,
        lhs_id: ValueId,
        rhs_id: ValueId,
        kind: IntCmpOp,
        operand_type: ValueType,
    ) -> Result<ValueId> {
        let lhs_id = self.require_value_type(lhs_id, operand_type)?;
        let rhs_id = self.require_value_type(rhs_id, operand_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntCmpOp(kind), [lhs_id, rhs_id], ValueType::I1))
    }

    // ── float constructors ───────────────────────────────────────────────

    /// Emits a float constant node with the given IEEE 754 bit pattern.
    /// `output_type` must be `F32` or `F64`.
    fn build_float_const(&mut self, bits: u64, output_type: ValueType) -> ValueId {
        self.build_single_output_pure(NodeKind::FloatConst(bits), [], output_type)
    }

    /// Emits a float binary operation node.  **Strict:** both operands must
    /// already carry the float `output_type`.
    ///
    /// # Errors
    ///
    /// Returns an error when an operand is not a value edge or does not
    /// already have type `output_type`.
    fn build_float_binary_op(
        &mut self,
        lhs: ValueId,
        rhs: ValueId,
        op: FloatBinaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let lhs = self.require_value_type(lhs, output_type)?;
        let rhs = self.require_value_type(rhs, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatBinaryOp(op), [lhs, rhs], output_type))
    }

    /// Emits a float unary operation node (neg, abs, sqrt, ceil, floor,
    /// round).  **Strict:** the operand must already carry `output_type`.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a value edge or does not already
    /// have type `output_type`.
    fn build_float_unary_op(
        &mut self,
        value: ValueId,
        op: FloatUnaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let coerced = self.require_value_type(value, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatUnaryOp(op), [coerced], output_type))
    }

    /// Emits a float comparison node (output `I1`).  **Strict:** both
    /// operands must already be the same float type.
    ///
    /// # Errors
    ///
    /// Returns an error when an operand is not a float value edge, or when
    /// the operands' float types differ.
    fn build_float_cmp_op(
        &mut self,
        lhs: ValueId,
        rhs: ValueId,
        op: FloatCmpOp,
    ) -> Result<ValueId> {
        let float_ty = self.value_type(lhs)?;
        if !float_ty.is_float() {
            return Err(anyhow!(
                "build_float_cmp_op: lhs {lhs:?} has type {float_ty}, expected a float"
            ));
        }
        let rhs = self.require_value_type(rhs, float_ty)?;
        Ok(self.build_single_output_pure(NodeKind::FloatCmpOp(op), [lhs, rhs], ValueType::I1))
    }

    // ── int / float conversions ──────────────────────────────────────────

    /// Emits an `IntToFloat` node: converts an integer value to the nearest
    /// representable float (like C's `(float)n`).
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not an integer value, or when
    /// `float_type` is not `F32`/`F64`.
    fn build_int_to_float(&mut self, value: ValueId, float_type: ValueType) -> Result<ValueId> {
        self.require_integer_value(value)?;
        Self::require_float_type(float_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntToFloat, [value], float_type))
    }

    /// Emits a `FloatToInt` node: truncates a float toward zero to an integer
    /// (like C's `(int)f`).
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a float value, or when `int_type`
    /// is not an integer.
    fn build_float_to_int(&mut self, value: ValueId, int_type: ValueType) -> Result<ValueId> {
        self.require_float_value(value)?;
        Self::require_integer_type(int_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatToInt, [value], int_type))
    }

    /// Emits a `FloatToFloat` node: converts between float precisions (F32 ↔ F64).
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a float, or when `float_type` is
    /// not `F32`/`F64`.
    fn build_float_to_float(&mut self, value: ValueId, float_type: ValueType) -> Result<ValueId> {
        self.require_float_value(value)?;
        Self::require_float_type(float_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatToFloat, [value], float_type))
    }

    /// Emits an `IntBitsToFloat` node: reinterprets an integer's bit pattern as
    /// a float of the same width.  If the input is an `IntConst`, immediately
    /// returns a `FloatConst` with the same bit pattern (no extra node created).
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not an integer, when `float_type` is
    /// not `F32`/`F64`, or when the input/float widths differ.
    fn build_int_bits_to_float(&mut self, value: ValueId, float_type: ValueType) -> Result<ValueId> {
        self.require_integer_value(value)?;
        Self::require_float_type(float_type)?;
        // A bit-reinterpret preserves width by definition; reject mismatched
        // widths so a wrong-width reinterpret can't silently truncate or
        // zero-pad (e.g. I64 → F32).
        let input_ty = self.value_type(value)?;
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
        if let NodeKind::IntConst(bits) = *self.function().kind_of_value(value)
            && float_type != ValueType::F80
        {
            // FloatConst stores bits as u64; F32/F64 fit, so the value
            // fits — u128 payload is masked to the type's width already.
            #[allow(clippy::cast_possible_truncation)]
            return Ok(self.build_float_const(bits as u64, float_type));
        }
        Ok(self.build_single_output_pure(NodeKind::IntBitsToFloat, [value], float_type))
    }

    /// Emits a `FloatBitsToInt` node: reinterprets a float's bit pattern as an
    /// integer of the same width.  If the input is a `FloatConst`, immediately
    /// returns an `IntConst` with the same bit pattern (no extra node created).
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a float, when `int_type` is not an
    /// integer, or when the input/int widths differ.
    fn build_float_bits_to_int(&mut self, value: ValueId, int_type: ValueType) -> Result<ValueId> {
        self.require_float_value(value)?;
        Self::require_integer_type(int_type)?;
        let input_ty = self.value_type(value)?;
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
        if let NodeKind::FloatConst(bits) = *self.function().kind_of_value(value)
            && input_ty != ValueType::F80
        {
            return self.build_int_const(bits, int_type);
        }
        Ok(self.build_single_output_pure(NodeKind::FloatBitsToInt, [value], int_type))
    }

    // ── opaque / user-defined constructors ───────────────────────────────

    /// Emits a `SegmentOp` node (pure computation: segment + offset → flat
    /// pointer) and returns its value output.
    ///
    /// # Errors
    ///
    /// Returns an error when either `segment` or `offset` is not a value edge.
    fn build_segment_op(
        &mut self,
        op_id: u64,
        segment: ValueId,
        offset: ValueId,
        output_type: ValueType,
    ) -> Result<ValueId> {
        self.validate_value_inputs(&[segment, offset])?;
        Ok(self.build_single_output_pure(
            NodeKind::SegmentOp { op_id },
            [segment, offset],
            output_type,
        ))
    }

    /// Emits a `CPoolRef` node (opaque JVM constant-pool lookup) and returns
    /// its value output.
    ///
    /// # Errors
    ///
    /// Returns an error when any element of `refs` is not a value edge.
    fn build_cpool_ref(&mut self, refs: &[ValueId], output_type: ValueType) -> Result<ValueId> {
        self.validate_value_inputs(refs)?;
        let node = self.create_node(
            NodeKind::CPoolRef,
            refs.iter().copied(),
            [ValueKind::Typed(output_type)],
        );
        let [value] = self.function().node_outputs_exact(node)?;
        Ok(value)
    }

    /// Emits a `New` node (opaque JVM allocation) and returns its value
    /// output.
    ///
    /// # Errors
    ///
    /// Returns an error when any element of `args` is not a value edge.
    fn build_new(&mut self, args: &[ValueId], output_type: ValueType) -> Result<ValueId> {
        self.validate_value_inputs(args)?;
        let node = self.create_node(
            NodeKind::New,
            args.iter().copied(),
            [ValueKind::Typed(output_type)],
        );
        let [value] = self.function().node_outputs_exact(node)?;
        Ok(value)
    }
}

impl<B: IRBuilder + ?Sized> IRBuilderExt for B {}
