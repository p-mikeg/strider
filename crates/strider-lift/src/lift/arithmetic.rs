//! Three comparisons are lowered at lift time so `IntCmpOp` holds only
//! primitive predicates:
//!
//! - `IntNotEqual(a, b)` -> `Xor(IntEqual(a, b), IntConst(1)):I1`
//! - `IntLessEqual(a, b)` -> `Xor(IntLess(b, a), IntConst(1)):I1`
//! - `IntSlessEqual(a, b)` -> `Xor(IntSless(b, a), IntConst(1)):I1`
//!
//! Logical negation of an `I1` is `Xor(_, IntConst(1))`; bitwise complement is
//! `Xor(_, all_ones)` everywhere.

use strider_ir::{ExtendOp, IRBuilderExt, IntBinaryOp, IntCmpOp, IntUnaryOp, ValueType, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, nth_input_or_err, require_output_vn};

/// Sleigh's contract guarantees `a` and `b` equal widths wherever this runs.
/// The lift resizes operands to one width before the builder sees them, so
/// this is the only check: without it a malformed `.sla` lifts silently.
/// `what` names the op in the error.
pub(super) fn require_same_size(a: &rsleigh::Vn, b: &rsleigh::Vn, what: &str) -> Result<()> {
    if a.size != b.size {
        return Err(anyhow::anyhow!(
            "p-code {what} width mismatch: {} against {} (Sleigh requires equal widths)",
            a.size,
            b.size,
        ));
    }
    Ok(())
}

/// Only the wider direction is rejected: a narrower operand extends correctly,
/// which is the intended semantics.
fn reject_operand_wider_than_output(operand: &rsleigh::Vn, output: &rsleigh::Vn) -> Result<()> {
    if operand.size > output.size {
        return Err(anyhow::anyhow!(
            "p-code op width mismatch: operand={} wider than output={} \
             (would silently truncate the operand)",
            operand.size,
            output.size,
        ));
    }
    Ok(())
}

/// A width rule on one raw p-code operand, checked before it is read.
#[derive(Clone, Copy)]
enum WidthGuard {
    Any,
    /// Coercion would truncate a wider operand, and the op's low bits do not
    /// survive that.
    NotWider,
    /// The lowering agrees with p-code only at the output width:
    /// `SShiftRight`'s sign fill lands at the operand's.
    Equal,
}

impl WidthGuard {
    fn check(self, insn: &rsleigh::Insn, slot: usize, out: &rsleigh::Vn) -> Result<()> {
        match self {
            Self::Any => Ok(()),
            Self::NotWider => reject_operand_wider_than_output(nth_input_or_err(insn, slot)?, out),
            Self::Equal => require_same_size(nth_input_or_err(insn, slot)?, out, "signed shift"),
        }
    }
}

/// How one operand is brought to the output width.
#[derive(Clone, Copy)]
enum Coerce {
    /// Truncate a wider operand, zero-extend a narrower one: what an unsigned
    /// op reads, and for `Add`, `Mul`, the bitwise ops and the left shift the
    /// result's low bits do not depend on the fill.
    Convert,
    /// Sign-extend a narrower operand; never truncates.
    SignExtend,
}

impl Coerce {
    fn apply(
        self,
        b: &mut strider_ir::FunctionBuilder,
        v: strider_ir::Value,
        ty: ValueType,
    ) -> Result<strider_ir::Value> {
        match self {
            Self::Convert => b.convert_to_int_if_needed(v, ty),
            Self::SignExtend => b.extend_if_needed(v, ty, ExtendOp::SignExtend),
        }
    }
}

/// How one raw p-code operand reaches the output width: the guard is checked
/// on the raw operand, then the coercion applied to the value read.
#[derive(Clone, Copy)]
struct OperandPolicy {
    guard: WidthGuard,
    coerce: Coerce,
}

const PLAIN: OperandPolicy = OperandPolicy {
    guard: WidthGuard::Any,
    coerce: Coerce::Convert,
};

/// Each operand's policy, `[lhs, rhs]`. A shift's rhs is its count: p-code
/// leaves it any width (`sar reg, cl`), so it is saturated, then converted,
/// and zero-extends even under `SShiftRight`.
fn binary_lift_policy(op: IntBinaryOp) -> [OperandPolicy; 2] {
    use IntBinaryOp as Op;
    let unsigned = OperandPolicy {
        guard: WidthGuard::NotWider,
        coerce: Coerce::Convert,
    };
    let signed = OperandPolicy {
        guard: WidthGuard::NotWider,
        coerce: Coerce::SignExtend,
    };
    match op {
        Op::Add | Op::And | Op::Or | Op::Xor | Op::Mul | Op::ShiftLeft | Op::ShiftRight => {
            [PLAIN, PLAIN]
        }
        Op::Div | Op::Rem => [unsigned, unsigned],
        Op::Sdiv | Op::Srem => [signed, signed],
        // The value sign-extends to the OUTPUT width, while
        // `OpBehaviorIntSright` fills at the INPUT width (sizein=4/sizeout=8
        // over 0xFFFFFFFF gives GHIDRA 0xFFFFFFFF, this lowering
        // 0xFFFFFFFFFFFFFFFF): the two agree only at equal widths.
        Op::SShiftRight => [
            OperandPolicy {
                guard: WidthGuard::Equal,
                coerce: Coerce::SignExtend,
            },
            PLAIN,
        ],
    }
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    pub(super) fn process_int_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntUnaryOp,
    ) -> Result<()> {
        self.lift_int_unary(insn, true, |b, v, t| b.build_int_unary_operation(v, op, t))
    }

    /// Sleigh's `IntNeg` is bitwise complement `~x`, which canonicalises to
    /// `Xor(x, all_ones)` at any width, a SIMD-wide (YMM, ZMM) one included.
    pub(super) fn handle_int_neg_as_xor(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, true, |b, v, t| {
            let all_ones = b.build_all_ones(t)?;
            b.build_int_binary_operation(v, all_ones, IntBinaryOp::Xor, t)
        })
    }

    /// A shift count wider than the output, clamped so it still reads as
    /// out-of-range after truncation.
    ///
    /// P-code tests the FULL count against `8 * sizeout` (`opbehavior.cc`
    /// `OpBehaviorIntRight` / `IntSright` / `IntLeft`), so truncating
    /// `0x1_0000_0000` to `I32` yields 0 and the shift silently does nothing.
    /// x86 SIMD shift-by-register (`psrad xmm, xmm` and friends) is exactly
    /// this shape: a 4-byte lane shifted by the 8-byte count the ISA reads
    /// from `SRC[63:0]`, where an over-large count means "fill with zero / the
    /// sign bit".
    ///
    /// OR-ing `out_bits` in leaves every one of its bits set, so the result is
    /// `>= out_bits` whatever the truncation kept. That holds for the widths
    /// that are not powers of two (`I24`, `I48`, `I80`, ...) as well.
    fn saturate_shift_count(
        &mut self,
        count: strider_ir::Value,
        out_ty: ValueType,
    ) -> Result<strider_ir::Value> {
        let count_ty = strider_ir::IRViewer::value_type(self.builder.function(), count)?;
        if count_ty.bit_width() <= out_ty.bit_width() {
            return Ok(count);
        }
        let out_bits = u128::try_from(out_ty.bit_width()).expect("bit width fits u128");
        let bound = self.builder.build_int_const(out_bits, count_ty)?;
        let in_range = self.builder.build_int_cmp_operation(
            count,
            bound,
            strider_ir::IntCmpOp::Less,
            count_ty,
        )?;
        let one = self.builder.build_int_const(1u128, ValueType::I1)?;
        let too_big = self.builder.build_int_binary_operation(
            in_range,
            one,
            IntBinaryOp::Xor,
            ValueType::I1,
        )?;
        let widened = self
            .builder
            .extend_if_needed(too_big, out_ty, ExtendOp::ZeroExtend)?;
        let all_ones =
            self.builder
                .build_int_unary_operation(widened, strider_ir::IntUnaryOp::Neg, out_ty)?;
        let bound_out = self.builder.build_int_const(out_bits, out_ty)?;
        let addend = self.builder.build_int_binary_operation(
            all_ones,
            bound_out,
            IntBinaryOp::And,
            out_ty,
        )?;
        let narrow = self.builder.truncate_if_needed(count, out_ty)?;
        self.builder
            .build_int_binary_operation(narrow, addend, IntBinaryOp::Or, out_ty)
    }

    /// Permissive about mixed input widths: real Sleigh on 64-bit arches
    /// mixes an 8-byte register with a 4-byte spill or immediate, so each
    /// operand is brought to the output width by [`binary_lift_policy`].
    pub(super) fn process_int_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        let out_vn = require_output_vn(insn)?;
        let policy = binary_lift_policy(op);
        for (slot, p) in policy.iter().enumerate() {
            p.guard.check(insn, slot, out_vn)?;
        }
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_ty = out_vn.int_type()?;
        let rhs = if op.is_shift() {
            self.saturate_shift_count(rhs, out_ty)?
        } else {
            rhs
        };
        let lhs = policy[0].coerce.apply(&mut self.builder, lhs, out_ty)?;
        let rhs = policy[1].coerce.apply(&mut self.builder, rhs, out_ty)?;
        let result = self
            .builder
            .build_int_binary_operation(lhs, rhs, op, out_ty)?;
        self.write_vn(out_vn, result)
    }

    /// Compares at the MAX of the two input widths so neither operand is
    /// truncated, extending the narrower one sign-correctly for the predicate.
    pub(super) fn process_int_cmp_op(&mut self, insn: &rsleigh::Insn, op: IntCmpOp) -> Result<()> {
        let in0_size = nth_input_or_err(insn, 0)?.size;
        let in1_size = nth_input_or_err(insn, 1)?.size;
        // Carry / Scarry / Sborrow are width-RELATIVE: they report overflow of
        // THIS width, so widening their operands makes the flag constant-false
        // (a wider add never carries out of the narrow width).  Sleigh always
        // emits them equal-width; fail loud if not.  The value comparisons
        // legitimately take mixed widths, so they are unguarded.
        if matches!(op, IntCmpOp::Carry | IntCmpOp::Scarry | IntCmpOp::Sborrow) {
            require_same_size(
                nth_input_or_err(insn, 0)?,
                nth_input_or_err(insn, 1)?,
                "input",
            )?;
        }
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let cmp_width = strider_ir::ValueType::int_from_bytes(in0_size.max(in1_size))?;
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

    /// Shared lowering for the three negated comparisons.  The comparison runs
    /// at the INPUT width; the output is a 1-bit `I1`.  `swap_operands` covers
    /// the `a <= b` iff `not(b < a)` rewrite.
    fn lower_cmp_negated(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntCmpOp,
        swap_operands: bool,
    ) -> Result<()> {
        require_same_size(
            nth_input_or_err(insn, 0)?,
            nth_input_or_err(insn, 1)?,
            "input",
        )?;
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

    /// Canonical logical NOT: `Xor(x, IntConst(1)):I1`, since at `I1` the
    /// all-ones constant of a bitwise complement is `1`.  `x` must already be
    /// `I1`.  Shared by the boolean, integer-cmp and float-cmp lowerings.
    pub(super) fn build_logical_not(&mut self, x: strider_ir::Value) -> Result<strider_ir::Value> {
        let one = self.builder.build_boolean_const(true);
        self.builder
            .build_int_binary_operation(x, one, IntBinaryOp::Xor, strider_ir::ValueType::I1)
    }

    /// Lowers to `Xor(IntEqual(a, b), IntConst(1)):I1`.
    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Equal, false)
    }

    /// Lowers to `Xor(IntLess(b, a), IntConst(1)):I1`.
    pub(super) fn handle_int_less_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Less, true)
    }

    /// Lowers to `Xor(IntSless(b, a), IntConst(1)):I1`.
    pub(super) fn handle_int_sless_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Sless, true)
    }

    /// Lowers `IntSub(a, b)` to `Add(a, Neg(b))`, the one canonical shape
    /// patterns see.  Exact: `a - b` equals `a + (-b)` mod 2^W, and `Add`
    /// wraps identically.
    pub(super) fn handle_int_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Sleigh requires all three widths to agree here, unlike the
        // comparison lowerings, whose I1 output makes only the input check
        // apply.
        require_same_size(
            nth_input_or_err(insn, 0)?,
            nth_input_or_err(insn, 1)?,
            "input",
        )?;
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        require_same_size(nth_input_or_err(insn, 0)?, out_vn, "unary op")?;
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
