//! Pure integer arithmetic and comparison opcodes.
//!
//! Covers `IntAdd`, `IntSub`, `IntMul`, `IntAnd`, `IntOr`, `IntXor`,
//! `IntDiv`, `IntSdiv`, `IntRem`, `IntSrem`, `IntLeft`, `IntRight`,
//! `IntSright`, `IntNeg`, `Int2Comp`, plus the comparison ops
//! `IntEqual`, `IntLess`, `IntSless`, `IntCarry`, `IntScarry`,
//! `IntSborrow`, and three lowered-at-lift forms:
//!
//! - `IntNotEqual` → `Xor(IntEqual(a, b), IntConst(1)):I1`
//! - `IntLessEqual(a, b)` → `Xor(IntLess(b, a), IntConst(1)):I1`
//! - `IntSlessEqual(a, b)` → `Xor(IntSless(b, a), IntConst(1)):I1`
//!
//! These three lowerings shrink `IntCmpOp` to its primitive predicates;
//! patterns and passes see one canonical shape per predicate instead of
//! redundant operand-swap-inverse pairs.  Logical negation of an `I1`
//! value is `Xor(_, IntConst(1))` since the former BitNot unary-op was removed
//! in favour of `Xor(_, all_ones)` everywhere.
//!
//! Cast / slice / extract / popcount / lzcount / piece / insert / ptr_*
//! handlers live in [`super::cast`] (they manipulate bit positions
//! rather than computing arithmetic).

use strider_ir::node::ValueId;
use strider_ir::{
    ExtendOp, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp, ValueType, VnTypeExt,
};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, nth_input_or_err, require_output_vn};

/// Verifies that two p-code input varnodes have equal byte-widths.
///
/// Several lowerings (`IntSub`, the three comparison lowerings
/// `IntLessEqual` / `IntSlessEqual` / `IntNotEqual`) require their two
/// inputs to share a width.  Sleigh's contract already guarantees this
/// — but a malformed `.sla` spec or a fuzzer-constructed `Insn` could
/// produce a width disagreement.  The IR builders are strict (they reject
/// a width mismatch), so this check is about surfacing the mismatch with a
/// precise lift-time diagnostic instead of a generic builder error,
/// keeping a real spec bug visible.
fn require_equal_input_widths(a: &rsleigh::Vn, b: &rsleigh::Vn) -> Result<()> {
    if a.size != b.size {
        return Err(anyhow::anyhow!(
            "p-code input width mismatch: lhs={} rhs={} (Sleigh requires equal widths)",
            a.size,
            b.size,
        ));
    }
    Ok(())
}

/// Verifies a single-input p-code op has matching input and output widths.
///
/// Several unary integer opcodes (`IntNeg`, `Int2Comp`, `BoolNeg`, etc.)
/// require the output varnode width to equal the single input width.
/// Sleigh's contract already guarantees this — surfacing a mismatch as a
/// precise lift-time error keeps a real `.sla` bug visible instead of
/// letting it surface later as a generic strict-builder width error.
pub(super) fn require_equal_input_output_width(
    input: &rsleigh::Vn,
    output: &rsleigh::Vn,
) -> Result<()> {
    if input.size != output.size {
        return Err(anyhow::anyhow!(
            "p-code unary op width mismatch: input={} output={} (Sleigh requires equal widths)",
            input.size,
            output.size,
        ));
    }
    Ok(())
}

/// Materialises the all-ones (every bit set) integer constant for `ty`.
///
/// For I1..I128 (including I80) `build_int_const(u128::MAX, ty)` masks
/// `u128::MAX` down to `(2^bit_width) - 1`, which gives the correct
/// all-ones value.  For I256/I512 `build_int_const_limbs` fills every
/// limb with `u64::MAX`.  Used by [`FunctionLifter::handle_int_neg_as_xor`]
/// to lower `IntNeg` (bitwise complement) to `Xor(x, all_ones)`.
fn build_all_ones(
    builder: &mut strider_ir::FunctionBuilder,
    ty: strider_ir::ValueType,
) -> Result<strider_ir::Value> {
    if ty.byte_size() <= 16 {
        // I1..I128 (including I80): build_int_const masks u128::MAX to the width.
        builder.build_int_const(u128::MAX, ty)
    } else if ty == ValueType::I256 {
        builder.build_int_const_limbs(&[u64::MAX; 4], ty)
    } else {
        // I512
        builder.build_int_const_limbs(&[u64::MAX; 8], ty)
    }
}

/// Errors when a sign-extended operand is WIDER than the output width.
///
/// A signed table op (`Sdiv` / `Srem` / `SShiftRight`) routes its value
/// operand through `extend_if_needed(.., SignExtend)`, which cannot narrow a
/// wider-than-output operand.  Reject it here with an op-specific message
/// rather than leaning on the generic extend error.  A narrower operand
/// sign-extends correctly and is left permissive — only the wider direction is
/// rejected.
fn reject_operand_wider_than_output(operand: &rsleigh::Vn, output: &rsleigh::Vn) -> Result<()> {
    if operand.size > output.size {
        return Err(anyhow::anyhow!(
            "p-code signed op width mismatch: operand={} wider than output={} \
             (would silently truncate before the signed operation)",
            operand.size,
            output.size,
        ));
    }
    Ok(())
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Translates a p-code integer unary instruction into an IR unary node and
    /// writes the result to the output varnode.
    pub(super) fn process_int_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntUnaryOp,
    ) -> Result<()> {
        self.lift_int_unary(insn, true, |b, v, t| b.build_int_unary_operation(v, op, t))
    }

    /// Translates a p-code `IntNeg` (Sleigh's bitwise complement `~x`) into
    /// an IR `Xor(x, all_ones)` node and writes the result to the output
    /// varnode.
    ///
    /// The former BitNot unary-op was removed in favour of the canonical
    /// `Xor(x, all_ones)` shape, so the lifter materialises the all-ones
    /// constant of the operand's width and emits the xor inline.  The
    /// all-ones operand is built by [`build_all_ones`], which routes
    /// the wide widths (I80 / I128 / I256 / I512) through the wide-const
    /// path so a SIMD register-wide bitwise complement (YMM → I256, ZMM →
    /// I512) lifts cleanly instead of erroring.
    pub(super) fn handle_int_neg_as_xor(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, true, |b, v, t| {
            let all_ones = build_all_ones(b, t)?;
            b.build_int_binary_operation(v, all_ones, IntBinaryOp::Xor, t)
        })
    }

    /// Translates a p-code integer binary instruction into an IR binary node
    /// and writes the result to the output varnode.
    ///
    /// Note: this dispatched-table site is intentionally permissive about
    /// input widths.  Real-world Sleigh on 64-bit arches legitimately
    /// emits arithmetic ops mixing operand widths (e.g. mixing an 8-byte
    /// register with a 4-byte spill / immediate around integer-promotion
    /// boundaries observed in `abi.c` fixtures); this site explicitly
    /// coerces each operand to the output width (zero- or sign-extending
    /// per the op's signedness) below, before the strict builder call.
    /// The lift-time equality check is reserved for the lowered forms
    /// (`handle_int_sub`, `handle_int_not_equal`, `handle_int_less_equal`,
    /// `handle_int_sless_equal`) whose lowering arithmetic *does* require
    /// matching widths, plus the width-sensitive table ops.  The
    /// signedness-sensitive ones (`Sdiv` / `Srem` / `SShiftRight`) route their
    /// value operand through `extend_if_needed(.., SignExtend)`, which cannot
    /// narrow a wider-than-output operand; the unsigned `Div` / `Rem` narrow
    /// via the zero-extend path's truncate-then-extend.  Either way a wider
    /// operand corrupts a quotient / remainder (their low bits are NOT
    /// width-agnostic), so every division / remainder is guarded loud rather
    /// than mis-lifted.  The bitwise / shift-left / add / mul ops are
    /// width-agnostic in their low bits and stay permissive.
    pub(super) fn process_int_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        // The operand coercion below SILENTLY TRUNCATES an operand WIDER than
        // the output (sign-blind on the signed sign-extend path, value-blind on
        // the unsigned zero-extend path).  For ops whose low bits are NOT
        // width-agnostic — every division / remainder, signed (`Sdiv` / `Srem`)
        // or unsigned (`Div` / `Rem`) — truncating a wider operand corrupts the
        // quotient / remainder, so BOTH operands are guarded.  `SShiftRight`
        // sign-extends only the value (lhs) — its shift count (rhs) zero-extends
        // and may legally be any width (e.g. `sar reg, cl`), so only the value
        // is guarded.  A *narrower* operand extends correctly (the intended
        // semantics), so only the wider-than-output direction is rejected.
        let out_vn = require_output_vn(insn)?;
        match op {
            IntBinaryOp::Sdiv | IntBinaryOp::Srem | IntBinaryOp::Div | IntBinaryOp::Rem => {
                reject_operand_wider_than_output(nth_input_or_err(insn, 0)?, out_vn)?;
                reject_operand_wider_than_output(nth_input_or_err(insn, 1)?, out_vn)?;
            }
            IntBinaryOp::SShiftRight => {
                reject_operand_wider_than_output(nth_input_or_err(insn, 0)?, out_vn)?;
            }
            _ => {}
        }
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_ty = out_vn.int_type()?;
        // The signed ops interpret their operands as signed, so a narrower
        // operand must be SIGN-extended to the op width (via `extend_if_needed`)
        // rather than zero-extended.  `Sdiv` / `Srem` sign-extend both operands;
        // `SShiftRight` (arithmetic shift) sign-extends only the value — its
        // shift count is an unsigned amount.  Every other op zero-extends via
        // `convert_to_int_if_needed` (correct for bitwise / shift-left /
        // unsigned ops, whose low-width result is sign-agnostic).  Equal-width
        // operands (the common case) are unaffected: the coercion is a no-op.
        let (lhs, rhs) = match op {
            IntBinaryOp::Sdiv | IntBinaryOp::Srem => (
                self.builder
                    .extend_if_needed(lhs, out_ty, ExtendOp::SignExtend)?,
                self.builder
                    .extend_if_needed(rhs, out_ty, ExtendOp::SignExtend)?,
            ),
            IntBinaryOp::SShiftRight => (
                self.builder
                    .extend_if_needed(lhs, out_ty, ExtendOp::SignExtend)?,
                self.builder.convert_to_int_if_needed(rhs, out_ty)?,
            ),
            _ => (
                self.builder.convert_to_int_if_needed(lhs, out_ty)?,
                self.builder.convert_to_int_if_needed(rhs, out_ty)?,
            ),
        };
        // Strength-reduce `mul(x, 2^k)` (k ≥ 1) to `shl(x, k)` at construction,
        // so a pattern query sees ONE canonical shape whether the compiler
        // emitted an `imul` or a `shl` for the same scaling.  Sound for
        // signed + unsigned in two's complement (both wrap mod 2^width).
        let result = match op {
            IntBinaryOp::Mul => self.build_mul_or_pow2_shift(lhs, rhs, out_ty)?,
            IntBinaryOp::Div => self.build_udiv_or_pow2_shift(lhs, rhs, out_ty)?,
            _ => self
                .builder
                .build_int_binary_operation(lhs, rhs, op, out_ty)?,
        };
        self.write_vn(out_vn, result)
    }

    /// `udiv(x, 2^k)` with `k ≥ 1` → `ShiftRight(x, k)` (LOGICAL right shift);
    /// any other unsigned divide is built verbatim.  Divide is NOT commutative,
    /// so only the DIVISOR (`rhs`) is tested — and only the unsigned `Div`:
    /// signed `Sdiv` by a power of two rounds toward zero and needs a bias
    /// correction (`(x + (2^k - 1)) >> k` for negatives), so it stays `Sdiv`.
    /// Divisor `1` (`div(x,1)→x`) stays `Div` for ConstantFold's identity.
    fn build_udiv_or_pow2_shift(
        &mut self,
        lhs: ValueId,
        rhs: ValueId,
        out_ty: ValueType,
    ) -> Result<ValueId> {
        if let Some(k) = self.builder.int_const_u128(rhs)
            && k >= 2
            && k.is_power_of_two()
        {
            let amount = self
                .builder
                .build_int_const(u128::from(k.trailing_zeros()), out_ty)?;
            return self.builder.build_int_binary_operation(
                lhs,
                amount,
                IntBinaryOp::ShiftRight,
                out_ty,
            );
        }
        self.builder
            .build_int_binary_operation(lhs, rhs, IntBinaryOp::Div, out_ty)
    }

    /// `mul(x, 2^k)` with `k ≥ 1` → `ShiftLeft(x, k)`; any other multiply is
    /// built verbatim.  `Mul` is commutative, so the constant is accepted on
    /// either side.  Multiplier `1` (`mul(x,1)→x`) and `0` (`mul(x,0)→0`) are
    /// deliberately left as `Mul` for ConstantFold's identities — only true
    /// scaling (`≥ 2`) becomes a shift.  A wider-than-`u128` constant does not
    /// fit `int_const_u128`, so it also stays a `Mul` (acceptable — pow2
    /// scaling constants are small).
    fn build_mul_or_pow2_shift(
        &mut self,
        lhs: ValueId,
        rhs: ValueId,
        out_ty: ValueType,
    ) -> Result<ValueId> {
        for (konst, var) in [(rhs, lhs), (lhs, rhs)] {
            let Some(k) = self.builder.int_const_u128(konst) else {
                continue;
            };
            if k >= 2 && k.is_power_of_two() {
                let amount = self
                    .builder
                    .build_int_const(u128::from(k.trailing_zeros()), out_ty)?;
                return self.builder.build_int_binary_operation(
                    var,
                    amount,
                    IntBinaryOp::ShiftLeft,
                    out_ty,
                );
            }
        }
        self.builder
            .build_int_binary_operation(lhs, rhs, IntBinaryOp::Mul, out_ty)
    }

    /// Translates a p-code integer comparison instruction into an IR
    /// comparison node and writes the boolean result to the output varnode.
    ///
    /// Sleigh can emit comparison operands of differing widths on 64-bit
    /// arches, so the comparison is performed at the **max** of the two
    /// input widths — neither operand is truncated.  The narrower operand
    /// is extended sign-correctly for the predicate: signed comparisons
    /// (`Sless` / `Scarry` / `Sborrow`) sign-extend, the unsigned ones
    /// (`Equal` / `Less` / `Carry`) zero-extend.
    pub(super) fn process_int_cmp_op(&mut self, insn: &rsleigh::Insn, op: IntCmpOp) -> Result<()> {
        let in0_size = nth_input_or_err(insn, 0)?.size;
        let in1_size = nth_input_or_err(insn, 1)?.size;
        // Carry / Scarry / Sborrow are width-RELATIVE (overflow of THIS width),
        // so extending their operands to a wider `cmp_width` would corrupt the
        // flag (a wider add never carries out of the narrow width).  Sleigh
        // always emits these with equal-width operands; enforce that so a
        // malformed mixed-width insn fails loud instead of silently producing a
        // wrong (constant-false) flag.  The value comparisons (Equal / Less /
        // Sless) legitimately take mixed widths — extending the narrower
        // operand is the correct semantics there, so they are not guarded.
        if matches!(op, IntCmpOp::Carry | IntCmpOp::Scarry | IntCmpOp::Sborrow) {
            require_equal_input_widths(nth_input_or_err(insn, 0)?, nth_input_or_err(insn, 1)?)?;
        }
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let cmp_width = strider_ir::ValueType::int_for_byte_size(in0_size.max(in1_size))?;
        let ext_op = match op {
            IntCmpOp::Sless | IntCmpOp::Scarry | IntCmpOp::Sborrow => ExtendOp::SignExtend,
            IntCmpOp::Equal | IntCmpOp::Less | IntCmpOp::Carry => ExtendOp::ZeroExtend,
        };
        let lhs = self.builder.extend_if_needed(lhs, cmp_width, ext_op)?;
        let rhs = self.builder.extend_if_needed(rhs, cmp_width, ext_op)?;
        let result = self
            .builder
            .build_int_cmp_operation(lhs, rhs, op, cmp_width)?;
        self.write_vn(out_vn, result)
    }

    /// Shared lowering for the three negated integer comparisons
    /// (`IntNotEqual`, `IntLessEqual`, `IntSlessEqual`).  Each lowers to
    /// `Xor(IntCmpOp(...), IntConst(1)):I1` — one `IntCmpOp` xor'd with the
    /// I1 all-ones constant — keeping the cmp-op enum free of the
    /// `NotEqual` / `LessEqual` / `SlessEqual` variants.  (the former BitNot unary-op
    /// was removed in favour of `Xor(x, all_ones)`.)
    ///
    /// All three require equal input widths and perform the comparison at
    /// the *input* width (the output is a 1-bit `I1`).  They differ only in
    /// the [`IntCmpOp`] predicate and whether the operands are swapped:
    /// `NotEqual` uses `Equal` without a swap; `LessEqual(a, b)` /
    /// `SlessEqual(a, b)` use `Less` / `Sless` with the operands swapped
    /// (`a <= b` iff `not(b < a)`).
    fn lower_cmp_negated(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntCmpOp,
        swap_operands: bool,
    ) -> Result<()> {
        require_equal_input_widths(nth_input_or_err(insn, 0)?, nth_input_or_err(insn, 1)?)?;
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let cmp_width = nth_input_or_err(insn, 0)?.int_type()?;
        let lhs = self.builder.convert_to_int_if_needed(lhs, cmp_width)?;
        let rhs = self.builder.convert_to_int_if_needed(rhs, cmp_width)?;
        let (cmp_lhs, cmp_rhs) = if swap_operands {
            (rhs, lhs)
        } else {
            (lhs, rhs)
        };
        let cmp = self
            .builder
            .build_int_cmp_operation(cmp_lhs, cmp_rhs, op, cmp_width)?;
        let negated = self.build_logical_not(cmp)?;
        self.write_vn(out_vn, negated)
    }

    /// Builds the canonical logical-NOT of a 1-bit (`I1`) value:
    /// `Xor(x, IntConst(1)):I1`.  Strider canonicalises a bitwise complement
    /// to `Xor(_, all_ones)`, and at `I1` the all-ones constant is `1`, so a
    /// boolean negation is `x ^ 1`.  Shared by the boolean / integer-cmp /
    /// float-cmp negated lowerings so the canonical NOT shape lives in one
    /// place; `x` must already be `I1`.
    pub(super) fn build_logical_not(&mut self, x: strider_ir::Value) -> Result<strider_ir::Value> {
        let one = self.builder.build_boolean_const(true);
        self.builder
            .build_int_binary_operation(x, one, IntBinaryOp::Xor, strider_ir::ValueType::I1)
    }

    /// Lowers `IntNotEqual(a, b)` to `Xor(IntEqual(a, b), IntConst(1)):I1`.
    ///
    /// Matches strider's canonical form (one IntCmpOp + one I1 Xor with the
    /// all-ones I1 constant — i.e. a 1-bit complement — instead of an
    /// IntCmpOp::NotEqual variant, keeping the cmp-op enum smaller).  The
    /// cmp's operand width is the *input* width, NOT the output width: the
    /// output is a 1-bit `I1`, the inputs may be any integer width.
    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Equal, false)
    }

    /// Lowers `IntLessEqual(a, b)` to `Xor(IntLess(b, a), IntConst(1)):I1`.
    ///
    /// Operand swap + boolean-negate: `a <= b` iff not(`b < a`).  Removes
    /// the redundant `IntCmpOp::LessEqual` variant — patterns and passes
    /// see one canonical shape (`Less` plus an optional I1 Xor with 1)
    /// instead of two.
    pub(super) fn handle_int_less_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Less, true)
    }

    /// Lowers `IntSlessEqual(a, b)` to `Xor(IntSless(b, a), IntConst(1)):I1`.
    ///
    /// Signed analogue of [`Self::handle_int_less_equal`].  Same operand
    /// swap, same I1 Xor with 1, but with `IntCmpOp::Sless` for signed
    /// comparison.
    pub(super) fn handle_int_sless_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Sless, true)
    }

    /// Lowers `IntSub(a, b)` to `IntAdd(a, IntUnaryOp::Neg(b))`.
    ///
    /// `a - b ≡ a + (-b)` modulo 2^W; the wrap semantics of `IntAdd`
    /// preserve `IntSub`'s exact bit-pattern result.  Removes the
    /// redundant `IntBinaryOp::Sub` variant — patterns and passes see
    /// one canonical shape (`Add` plus an optional inner `Neg`).
    ///
    /// For constant-RHS subtractions (`a - K`), the produced shape is
    /// `Add(a, Neg(IntConst(K)))`, which `ConstantFold` collapses to
    /// `Add(a, IntConst(-K))` immediately — no persisted node-count
    /// regression.  Variable-RHS subtractions add one `Neg` node.
    pub(super) fn handle_int_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Sleigh's `IntSub` requires `inputs[0].size == inputs[1].size ==
        // output.size`.  Surface any mismatch as a precise lift-time error
        // (the strict builder would otherwise reject it with a generic width
        // error) — a Sleigh spec emitting widths in disagreement is a real
        // bug we want to see, not paper over.  The input-width
        // check is shared with the three comparison lowerings via
        // [`require_equal_input_widths`]; `IntSub` adds the extra
        // output-width check (the comparison lowerings produce a Bool
        // output so the output-width check doesn't apply there).
        require_equal_input_widths(nth_input_or_err(insn, 0)?, nth_input_or_err(insn, 1)?)?;
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        // Sleigh requires the IntSub output width to equal the operand width;
        // reuse the shared input/output-width guard rather than re-rolling it.
        require_equal_input_output_width(nth_input_or_err(insn, 0)?, out_vn)?;
        // `Neg`'s width matches the operand's read width (`out_ty`,
        // since all three sizes agree).
        let lhs = self.builder.convert_to_int_if_needed(lhs, out_ty)?;
        let rhs = self.builder.convert_to_int_if_needed(rhs, out_ty)?;
        let neg_rhs = self
            .builder
            .build_int_unary_operation(rhs, IntUnaryOp::Neg, out_ty)?;
        let sum =
            self.builder
                .build_int_binary_operation(lhs, neg_rhs, IntBinaryOp::Add, out_ty)?;
        self.write_vn(out_vn, sum)
    }
}
