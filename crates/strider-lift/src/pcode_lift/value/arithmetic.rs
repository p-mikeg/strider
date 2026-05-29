//! Pure integer arithmetic and comparison opcodes.
//!
//! Covers `IntAdd`, `IntSub`, `IntMul`, `IntAnd`, `IntOr`, `IntXor`,
//! `IntDiv`, `IntSdiv`, `IntRem`, `IntSrem`, `IntLeft`, `IntRight`,
//! `IntSright`, `IntNeg`, `Int2Comp`, plus the comparison ops
//! `IntEqual`, `IntLess`, `IntSless`, `IntCarry`, `IntScarry`,
//! `IntSborrow`, and three lowered-at-lift forms:
//!
//! - `IntNotEqual` → `BitNot(IntEqual)` at `I1`
//! - `IntLessEqual(a, b)` → `BitNot(IntLess(b, a))` at `I1`
//! - `IntSlessEqual(a, b)` → `BitNot(IntSless(b, a))` at `I1`
//!
//! These three lowerings shrink `IntCmpOp` to its primitive predicates;
//! patterns and passes see one canonical shape per predicate instead of
//! redundant operand-swap-inverse pairs.
//!
//! Cast / slice / extract / popcount / lzcount / piece / insert / ptr_*
//! handlers live in [`super::cast`] (they manipulate bit positions
//! rather than computing arithmetic).

use strider_ir::{ExtendOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::pcode_lift::Result;
use crate::pcode_lift::ValueLifter;

/// Verifies that two p-code input varnodes have equal byte-widths.
///
/// Several lowerings (`IntSub`, the three comparison lowerings
/// `IntLessEqual` / `IntSlessEqual` / `IntNotEqual`) require their two
/// inputs to share a width.  Sleigh's contract already guarantees this
/// — but a malformed `.sla` spec or a fuzzer-constructed `Insn` would
/// produce a width disagreement that the IR's
/// `build_int_binary_operation` / `build_int_cmp_operation` would
/// silently width-adapt.  Surfacing the mismatch as a lift-time error
/// keeps a real spec bug visible instead of papering over it.
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
/// Sleigh's contract already guarantees this — surfacing a mismatch as
/// a lift-time error keeps a real `.sla` bug visible instead of letting
/// `build_int_unary_operation`'s width adaptation silently sign- /
/// zero-extend the operand.
pub(super) fn require_equal_input_output_width(input: &rsleigh::Vn, output: &rsleigh::Vn) -> Result<()> {
    if input.size != output.size {
        return Err(anyhow::anyhow!(
            "p-code unary op width mismatch: input={} output={} (Sleigh requires equal widths)",
            input.size,
            output.size,
        ));
    }
    Ok(())
}

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Translates a p-code integer unary instruction into an IR unary node and
    /// writes the result to the output varnode.
    pub(super) fn process_int_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntUnaryOp,
    ) -> Result<()> {
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        require_equal_input_output_width(crate::pcode_lift::nth_input_or_err(insn, 0)?, out_vn)?;
        let input = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let out_ty = strider_ir::ValueType::int_for_byte_size(out_vn.size)?;
        let input = self.builder.convert_to_int_if_needed(input, out_ty)?;
        let out = self.builder.build_int_unary_operation(input, op, out_ty)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code integer binary instruction into an IR binary node
    /// and writes the result to the output varnode.
    ///
    /// Note: this dispatched-table site is intentionally permissive about
    /// input widths.  Real-world Sleigh on 64-bit arches legitimately
    /// emits arithmetic ops mixing operand widths (e.g. mixing an 8-byte
    /// register with a 4-byte spill / immediate around integer-promotion
    /// boundaries observed in `abi.c` fixtures); the IR's
    /// `build_int_binary_operation` width-adapts via zero-extension.
    /// The lift-time equality check is reserved for the lowered forms
    /// (`handle_int_sub`, `handle_int_not_equal`, `handle_int_less_equal`,
    /// `handle_int_sless_equal`) whose lowering arithmetic *does* require
    /// matching widths.
    pub(super) fn process_int_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out_ty = strider_ir::ValueType::int_for_byte_size(out_vn.size)?;
        // The signed ops interpret their operands as signed, so a narrower
        // operand must be SIGN-extended to the op width rather than
        // zero-extended (`build_int_binary_operation`'s default coercion).
        // `Sdiv` / `Srem` sign-extend both operands; `SShiftRight` (arithmetic
        // shift) sign-extends only the value — its shift count is an unsigned
        // amount.  Every other op keeps the default zero-extension (correct
        // for bitwise / shift-left / unsigned ops, whose low-width result is
        // sign-agnostic).  Equal-width operands (the common case) are
        // unaffected: the extension is a no-op.
        let (lhs, rhs) = match op {
            IntBinaryOp::Sdiv | IntBinaryOp::Srem => (
                self.builder.extend_if_needed(lhs, out_ty, ExtendOp::SignExtend)?,
                self.builder.extend_if_needed(rhs, out_ty, ExtendOp::SignExtend)?,
            ),
            IntBinaryOp::SShiftRight => (
                self.builder.extend_if_needed(lhs, out_ty, ExtendOp::SignExtend)?,
                self.builder.convert_to_int_if_needed(rhs, out_ty)?,
            ),
            _ => (
                self.builder.convert_to_int_if_needed(lhs, out_ty)?,
                self.builder.convert_to_int_if_needed(rhs, out_ty)?,
            ),
        };
        let out = self.builder.build_int_binary_operation(lhs, rhs, op, out_ty)?;
        self.write_vn(out_vn, out)
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
    pub(super) fn process_int_cmp_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntCmpOp,
    ) -> Result<()> {
        let in0_size = crate::pcode_lift::nth_input_or_err(insn, 0)?.size;
        let in1_size = crate::pcode_lift::nth_input_or_err(insn, 1)?.size;
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let cmp_width = strider_ir::ValueType::int_for_byte_size(in0_size.max(in1_size))?;
        let ext_op = match op {
            IntCmpOp::Sless | IntCmpOp::Scarry | IntCmpOp::Sborrow => ExtendOp::SignExtend,
            IntCmpOp::Equal | IntCmpOp::Less | IntCmpOp::Carry => ExtendOp::ZeroExtend,
        };
        let lhs = self.builder.extend_if_needed(lhs, cmp_width, ext_op)?;
        let rhs = self.builder.extend_if_needed(rhs, cmp_width, ext_op)?;
        let out = self.builder.build_int_cmp_operation(lhs, rhs, op, cmp_width)?;
        self.write_vn(out_vn, out)
    }

    /// Shared lowering for the three negated integer comparisons
    /// (`IntNotEqual`, `IntLessEqual`, `IntSlessEqual`).  Each lowers to
    /// `BitNot(IntCmpOp(...))` at `I1` — one `IntCmpOp` wrapped in an
    /// `IntUnaryOp::BitNot` at `I1` — keeping the cmp-op enum free of the
    /// `NotEqual` / `LessEqual` / `SlessEqual` variants.
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
        require_equal_input_widths(
            crate::pcode_lift::nth_input_or_err(insn, 0)?,
            crate::pcode_lift::nth_input_or_err(insn, 1)?,
        )?;
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let cmp_width = strider_ir::ValueType::int_for_byte_size(crate::pcode_lift::nth_input_or_err(insn, 0)?.size)?;
        let lhs = self.builder.convert_to_int_if_needed(lhs, cmp_width)?;
        let rhs = self.builder.convert_to_int_if_needed(rhs, cmp_width)?;
        let (cmp_lhs, cmp_rhs) = if swap_operands { (rhs, lhs) } else { (lhs, rhs) };
        let cmp = self
            .builder
            .build_int_cmp_operation(cmp_lhs, cmp_rhs, op, cmp_width)?;
        let negated = self
            .builder
            .build_int_unary_operation(cmp, IntUnaryOp::BitNot, strider_ir::ValueType::I1)?;
        self.write_vn(out_vn, negated)
    }

    /// Lowers `IntNotEqual(a, b)` to `BitNot(IntEqual(a, b))` at `I1`.
    ///
    /// Matches strider's pre-existing canonical form (one IntCmpOp + one
    /// `IntUnaryOp::BitNot` at `I1` — the 1-bit `BitNot` shape — instead of an
    /// IntCmpOp::NotEqual variant, keeping the cmp-op enum smaller).  The
    /// cmp's operand width is the *input* width, NOT the output width: the
    /// output is a 1-bit `I1`, the inputs may be any integer width.
    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Equal, false)
    }

    /// Lowers `IntLessEqual(a, b)` to `BitNot(IntLess(b, a))` at `I1`.
    ///
    /// Operand swap + boolean-negate: `a <= b` iff not(`b < a`).  Removes
    /// the redundant `IntCmpOp::LessEqual` variant — patterns and passes
    /// see one canonical shape (`Less` plus an optional `BitNot`) instead
    /// of two.
    pub(super) fn handle_int_less_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Less, true)
    }

    /// Lowers `IntSlessEqual(a, b)` to `BitNot(IntSless(b, a))` at `I1`.
    ///
    /// Signed analogue of [`Self::handle_int_less_equal`].  Same operand
    /// swap, same `BitNot` wrap, but with `IntCmpOp::Sless` for signed
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
        // output.size`.  Surface any mismatch as a lift-time error rather
        // than silently coercing in `build_int_binary_operation`'s width
        // adaptation — a Sleigh spec emitting widths in disagreement is
        // a real bug we want to see, not paper over.  The input-width
        // check is shared with the three comparison lowerings via
        // [`require_equal_input_widths`]; `IntSub` adds the extra
        // output-width check (the comparison lowerings produce a Bool
        // output so the output-width check doesn't apply there).
        require_equal_input_widths(
            crate::pcode_lift::nth_input_or_err(insn, 0)?,
            crate::pcode_lift::nth_input_or_err(insn, 1)?,
        )?;
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out_ty = strider_ir::ValueType::int_for_byte_size(out_vn.size)?;
        let in0_size = crate::pcode_lift::nth_input_or_err(insn, 0)?.size;
        if in0_size != out_vn.size {
            return Err(anyhow::anyhow!(
                "IntSub width mismatch: inputs={} out={} (Sleigh requires equal widths)",
                in0_size,
                out_vn.size,
            ));
        }
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
