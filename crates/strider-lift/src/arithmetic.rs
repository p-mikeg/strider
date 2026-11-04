//! Narrowing a 1-byte p-code boolean to `I1` assumes it is 0 or 1 (p-code's
//! boolean ops act on the whole varnode, so `BOOL_OR(2, 0)` differs). Every
//! vendored spec feeds them comparisons, carry flags or one-bit bitranges.

use strider_ir::node::ValueId;
use strider_ir::{
    ExtendOp, FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp,
    ValueType, VnTypeExt,
};

use crate::FunctionLifter;
use crate::pcode_util::{Result, nth_input_or_err, require_output_vn};

/// Errs unless `a` and `b` have equal widths, as Sleigh guarantees; the lift
/// resizes operands afterwards, so a malformed `.sla` fails only here.
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

/// A narrower operand extends correctly; only a wider one is rejected.
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
    /// Truncating a wider operand would change the op's low bits.
    NotWider,
    /// `SShiftRight`'s sign fill matches p-code only at equal widths.
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
    /// Truncate a wider operand, zero-extend a narrower one.
    Convert,
    /// Sign-extend a narrower operand; never truncates.
    SignExtend,
}

impl Coerce {
    fn apply(self, b: &mut FunctionBuilder, v: ValueId, ty: ValueType) -> Result<ValueId> {
        match self {
            Self::Convert => b.convert_to_int_if_needed(v, ty),
            Self::SignExtend => b.extend_if_needed(v, ty, ExtendOp::SignExtend),
        }
    }
}

/// The guard checks the raw operand; the coercion applies to the value read.
#[derive(Clone, Copy)]
struct OperandPolicy {
    guard: WidthGuard,
    coerce: Coerce,
}

const PLAIN: OperandPolicy = OperandPolicy {
    guard: WidthGuard::Any,
    coerce: Coerce::Convert,
};

/// `[lhs, rhs]` policies. A shift count may be any width, so it is saturated
/// then converted, zero-extending even under `SShiftRight`.
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
        // p-code fills at the input width, this lowering at the output's.
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

    /// Sleigh's `IntNeg` is `~x`, lowered to `Xor(x, all_ones)`.
    pub(super) fn handle_int_neg_as_xor(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, true, |b, v, t| {
            let all_ones = b.build_all_ones(t)?;
            b.build_int_binary_operation(v, all_ones, IntBinaryOp::Xor, t)
        })
    }

    /// Single-input integer op. `enforce_equal_io_width` requires and coerces
    /// to the output width; off for `popcount` / `lzcount`, which count over
    /// the input's own width.
    pub(super) fn lift_int_unary(
        &mut self,
        insn: &rsleigh::Insn,
        enforce_equal_io_width: bool,
        build: impl FnOnce(&mut FunctionBuilder, ValueId, ValueType) -> Result<ValueId>,
    ) -> Result<()> {
        let out_vn = require_output_vn(insn)?;
        if enforce_equal_io_width {
            require_same_size(nth_input_or_err(insn, 0)?, out_vn, "unary op")?;
        }
        let value = self.read_input(insn, 0)?;
        let out_ty = out_vn.int_type()?;
        let value = if enforce_equal_io_width {
            self.builder.convert_to_int_if_needed(value, out_ty)?
        } else {
            value
        };
        let result = build(&mut self.builder, value, out_ty)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_popcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, false, |b, v, t| b.build_popcount(v, t))
    }

    pub(super) fn handle_lzcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, false, |b, v, t| b.build_lzcount(v, t))
    }

    /// Narrows a shift count wider than the output so an out-of-range count
    /// stays out of range (p-code tests the full count; x86 `psrad xmm, xmm`).
    /// OR-ing in `out_bits` keeps the result `>= out_bits` at any width.
    fn saturate_shift_count(&mut self, count: ValueId, out_ty: ValueType) -> Result<ValueId> {
        let count_ty = self.builder.value_type(count)?;
        if count_ty.bit_width() <= out_ty.bit_width() {
            return Ok(count);
        }
        let out_bits = u128::try_from(out_ty.bit_width()).expect("bit width fits u128");
        let bound = self.builder.build_int_const(out_bits, count_ty)?;
        let in_range =
            self.builder
                .build_int_cmp_operation(count, bound, IntCmpOp::Less, count_ty)?;
        let too_big = self.build_logical_not(in_range)?;
        let widened = self
            .builder
            .extend_if_needed(too_big, out_ty, ExtendOp::ZeroExtend)?;
        let all_ones = self
            .builder
            .build_int_unary_operation(widened, IntUnaryOp::Neg, out_ty)?;
        let addend =
            self.builder
                .build_const_binop(out_bits, all_ones, IntBinaryOp::And, out_ty)?;
        let narrow = self.builder.truncate_if_needed(count, out_ty)?;
        self.builder
            .build_int_binary_operation(narrow, addend, IntBinaryOp::Or, out_ty)
    }

    /// Inputs may differ in width; [`binary_lift_policy`] brings each to the
    /// output's.
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

    pub(super) fn process_int_cmp_op(&mut self, insn: &rsleigh::Insn, op: IntCmpOp) -> Result<()> {
        self.lift_int_cmp(insn, op, false, false)
    }

    /// Compares at the wider input width, extending per the predicate's
    /// signedness. `negate` wraps the result in a logical NOT; `swap` swaps
    /// the operands.
    fn lift_int_cmp(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntCmpOp,
        negate: bool,
        swap: bool,
    ) -> Result<()> {
        let a = nth_input_or_err(insn, 0)?;
        let b = nth_input_or_err(insn, 1)?;
        // The overflow flags are relative to the operand width, so widening
        // would make them false; Sleigh emits them and the negated compares
        // at equal widths.
        if negate || matches!(op, IntCmpOp::Carry | IntCmpOp::Scarry | IntCmpOp::Sborrow) {
            require_same_size(a, b, "input")?;
        }
        let lhs = self.read_vn(a)?;
        let rhs = self.read_vn(b)?;
        let out_vn = require_output_vn(insn)?;
        let cmp_width = ValueType::int_from_bytes(a.size.max(b.size))?;
        let ext_op = match op {
            IntCmpOp::Sless | IntCmpOp::Scarry | IntCmpOp::Sborrow => ExtendOp::SignExtend,
            IntCmpOp::Equal | IntCmpOp::Less | IntCmpOp::Carry => ExtendOp::ZeroExtend,
        };
        let lhs = self.builder.extend_if_needed(lhs, cmp_width, ext_op)?;
        let rhs = self.builder.extend_if_needed(rhs, cmp_width, ext_op)?;
        let (lhs, rhs) = if swap { (rhs, lhs) } else { (lhs, rhs) };
        let cmp = self
            .builder
            .build_int_cmp_operation(lhs, rhs, op, cmp_width)?;
        let result = if negate {
            self.build_logical_not(cmp)?
        } else {
            cmp
        };
        self.write_vn(out_vn, result)
    }

    /// Logical NOT of an `I1`: `Xor(x, IntConst(1)):I1`.
    pub(super) fn build_logical_not(&mut self, x: ValueId) -> Result<ValueId> {
        let one = self.builder.build_boolean_const(true);
        self.builder
            .build_int_binary_operation(x, one, IntBinaryOp::Xor, ValueType::I1)
    }

    pub(super) fn process_bool_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let lhs = self.builder.convert_to_int_if_needed(lhs, ValueType::I1)?;
        let rhs = self.builder.convert_to_int_if_needed(rhs, ValueType::I1)?;
        let result = self
            .builder
            .build_int_binary_operation(lhs, rhs, op, ValueType::I1)?;
        self.write_vn(out_vn, result)
    }

    /// `BoolNeg`, lowered to `Xor(x, IntConst(1)):I1`.
    pub(super) fn process_bool_unary_op(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        let value = self
            .builder
            .convert_to_int_if_needed(value, ValueType::I1)?;
        let result = self.build_logical_not(value)?;
        self.write_vn(out_vn, result)
    }

    /// Lowers to `Xor(IntEqual(a, b), IntConst(1)):I1`.
    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_cmp(insn, IntCmpOp::Equal, true, false)
    }

    /// Lowers to `Xor(IntLess(b, a), IntConst(1)):I1`.
    pub(super) fn handle_int_less_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_cmp(insn, IntCmpOp::Less, true, true)
    }

    /// Lowers to `Xor(IntSless(b, a), IntConst(1)):I1`.
    pub(super) fn handle_int_sless_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_cmp(insn, IntCmpOp::Sless, true, true)
    }

    /// Lowers `IntSub(a, b)` to `Add(a, Neg(b))`.
    pub(super) fn handle_int_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let a = nth_input_or_err(insn, 0)?;
        require_same_size(a, nth_input_or_err(insn, 1)?, "input")?;
        let lhs = self.read_vn(a)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        require_same_size(a, out_vn, "output")?;
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
