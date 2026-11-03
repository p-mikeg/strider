//! Three comparisons are lowered at lift time so `IntCmpOp` holds only
//! primitive predicates:
//!
//! - `IntNotEqual(a, b)` -> `Xor(IntEqual(a, b), IntConst(1)):I1`
//! - `IntLessEqual(a, b)` -> `Xor(IntLess(b, a), IntConst(1)):I1`
//! - `IntSlessEqual(a, b)` -> `Xor(IntSless(b, a), IntConst(1)):I1`
//!
//! Logical negation of an `I1` is `Xor(_, IntConst(1))`; bitwise complement is
//! `Xor(_, all_ones)` everywhere.
//!
//! Booleans are the 1-bit integer `I1`, so the boolean ops lower to ordinary
//! integer ops: and/or/xor at `I1`, and `BoolNeg` to `Xor(x, IntConst(1)):I1`.
//!
//! The boolean operands are 1-BYTE varnodes, which read back as `I8`, so the
//! narrowing to `I1` is load-bearing.  It is exact only under the ASSUMPTION
//! that a p-code boolean is 0 or 1: `OpBehaviorBoolOr` and friends are the
//! bitwise ops over the WHOLE varnode (`opbehavior.cc`), so `BOOL_OR(2, 0)` is
//! TRUE there and FALSE here.  The assumption holds across the vendored specs,
//! whose every `&&` / `||` / `!` operand is a comparison, `carry()`/`scarry()`,
//! or a one-bit bitrange the SLEIGH compiler lowers to shift-and-mask
//! (`ARM.sinc:20-22`, `:278`; `AARCH64instructions.sinc:2031-2032`).  A spec
//! carrying a wider value in a boolean needs the boolean ops lowered as `!= 0`
//! at the operand width instead.

use strider_ir::node::ValueId;
use strider_ir::{
    ExtendOp, FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp,
    ValueType, VnTypeExt,
};

use crate::FunctionLifter;
use crate::pcode_util::{Result, nth_input_or_err, require_output_vn};

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
    fn apply(self, b: &mut FunctionBuilder, v: ValueId, ty: ValueType) -> Result<ValueId> {
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

    /// Shared envelope for single-input integer unary ops.
    ///
    /// `enforce_equal_io_width` is on for the `Int2Comp` / `IntNeg`-as-xor
    /// lowerings, which require matching widths, and gates the input coercion
    /// with it: `popcount` / `lzcount` count over the INPUT's width, so
    /// widening their operand would shift the result by the width difference.
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

    pub(super) fn process_int_cmp_op(&mut self, insn: &rsleigh::Insn, op: IntCmpOp) -> Result<()> {
        self.lift_int_cmp(insn, op, false, false)
    }

    /// Compares at the MAX of the two input widths so neither operand is
    /// truncated, extending the narrower one sign-correctly for the predicate.
    /// `negate` wraps the `I1` result in a logical NOT, the lowering of the
    /// three negated comparisons; `swap` covers the `a <= b` iff `not(b < a)`
    /// rewrite.
    fn lift_int_cmp(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntCmpOp,
        negate: bool,
        swap: bool,
    ) -> Result<()> {
        let a = nth_input_or_err(insn, 0)?;
        let b = nth_input_or_err(insn, 1)?;
        // Carry / Scarry / Sborrow are width-RELATIVE: they report overflow of
        // THIS width, so widening their operands makes the flag constant-false
        // (a wider add never carries out of the narrow width).  Sleigh always
        // emits them, and the negated comparisons, equal-width; fail loud if
        // not.  The value comparisons legitimately take mixed widths, so they
        // are unguarded.
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

    /// Canonical logical NOT: `Xor(x, IntConst(1)):I1`, since at `I1` the
    /// all-ones constant of a bitwise complement is `1`.  `x` must already be
    /// `I1`.  Shared by the boolean, integer-cmp and float-cmp lowerings.
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

    /// Lowers `IntSub(a, b)` to `Add(a, Neg(b))`, the one canonical shape
    /// patterns see.  Exact: `a - b` equals `a + (-b)` mod 2^W, and `Add`
    /// wraps identically.
    pub(super) fn handle_int_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Sleigh requires all three widths to agree here, unlike the
        // comparison lowerings, whose I1 output makes only the input check
        // apply.
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
