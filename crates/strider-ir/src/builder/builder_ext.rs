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

use crate::IRViewer;
use crate::builder::IRBuilder;
use crate::error::Result;
use crate::node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    NodeKind, ValueId, ValueKind, ValueType,
};

/// The shared `build_*` construction vocabulary, available on every
/// [`IRBuilder`] via the blanket impl below.
///
/// All methods are provided (default) — implementors gain them for free.
/// Build-only: the pure point reads it relies on (`value_type`, the
/// `require_*` family, `validate_value_inputs`, `get_as_*`, `const_value`,
/// `infer_float_type`) live on the [`IRViewer`] supertrait of
/// [`IRBuilder`], so they resolve here for free.  The constructors only call
/// `self.create_node(...)` / [`Self::build_single_output_pure`], so the whole
/// vocabulary is pure with respect to any lift-time scratch the implementor
/// may carry.
pub trait IRBuilderExt: IRBuilder {
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

    // ── integer constructors ─────────────────────────────────────────────

    /// Emits a boolean constant — an `IntConst` of type `I1` (`true`→1,
    /// `false`→0).  Booleans are 1-bit integers; logical operations on them
    /// are ordinary `IntBinaryOp`/`IntUnaryOp` at `I1`.
    fn build_boolean_const(&mut self, val: bool) -> ValueId {
        self.build_single_output_pure(
            NodeKind::IntConst(crate::node::IntPayload::Small(u64::from(val))),
            [],
            ValueType::I1,
        )
    }

    /// Emits an integer constant node.
    ///
    /// `val` is masked to `output_type`'s bit width before storage so the
    /// dedup-cache key sees the same payload for semantically-equal constants —
    /// `build_int_const(0x1FF, I8)` and `build_int_const(0xFF, I8)` dedup to
    /// the same node.  Accepts any value convertible to `u128` — most callers
    /// pass a `u64` literal.
    ///
    /// For output types `I80` and `I128` the value is routed through the
    /// wide-const interner (`IntConst(Wide)` node), keeping inline `IntConst`
    /// payloads ≤ 64 bits.
    ///
    /// # Errors
    ///
    /// Returns an error when `output_type` is not an integer type, or when it
    /// is `I256` / `I512` (use `Self::build_int_const_wide` for those).
    fn build_int_const(&mut self, val: impl Into<u128>, output_type: ValueType) -> Result<ValueId> {
        if !output_type.is_integer() {
            return Err(anyhow!(
                "build_int_const called with non-integer type {output_type:?}"
            ));
        }
        if matches!(output_type, ValueType::I256 | ValueType::I512) {
            return Err(anyhow!(
                "build_int_const({output_type:?}) not supported; \
                 use Self::build_int_const_wide for I256/I512"
            ));
        }
        // Mask `val` to the declared output type's bit width so equal values
        // always produce the same dedup-cache key.
        let masked = val.into() & output_type.bit_mask_u128();
        // I80 and I128 go through the interner so inline IntConst payloads
        // stay ≤ 64 bits.
        match output_type {
            ValueType::I80 => {
                return self.build_int_const_wide(
                    crate::wide_const::WideConstStorage::I80(masked),
                    output_type,
                );
            }
            ValueType::I128 => {
                return self.build_int_const_wide(
                    crate::wide_const::WideConstStorage::I128(masked),
                    output_type,
                );
            }
            _ => {}
        }
        // The type bound (I1..I64) guarantees masked fits in u64.
        #[allow(clippy::cast_possible_truncation)]
        Ok(self.build_single_output_pure(
            NodeKind::IntConst(crate::node::IntPayload::Small(masked as u64)),
            [],
            output_type,
        ))
    }

    /// Builds a wide integer constant — `I80` (10 bytes), `I128` (16 bytes),
    /// `I256` (32 bytes), or `I512` (64 bytes) — interning `value` so equal
    /// values share a `WideConstId` (and hence a `NodeId` under the dedup
    /// cache).
    ///
    /// # Errors
    ///
    /// Returns an error when `output_type` is not one of the wide integer
    /// types, or when `value.byte_size()` doesn't match `output_type`'s byte
    /// size.
    fn build_int_const_wide(
        &mut self,
        value: crate::wide_const::WideConstStorage,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let expected = match output_type {
            ValueType::I80 => 10usize,
            ValueType::I128 => 16usize,
            ValueType::I256 => 32usize,
            ValueType::I512 => 64usize,
            other => {
                return Err(anyhow!(
                    "build_int_const_wide called with non-wide output type {other:?}; \
                     use build_int_const for ≤ I64"
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
        Ok(self.build_single_output_pure(
            NodeKind::IntConst(crate::node::IntPayload::Wide(id)),
            [],
            output_type,
        ))
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
        if let Some(bits) = self.int_const_u128(value)
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
