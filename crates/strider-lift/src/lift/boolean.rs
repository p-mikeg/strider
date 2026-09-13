//! Booleans are the 1-bit integer `I1`, so these lower to ordinary integer
//! ops: and/or/xor at `I1`, and `BoolNeg` to `Xor(x, IntConst(1)):I1`.
//!
//! The operands are 1-BYTE varnodes, which read back as `I8`, so the narrowing
//! to `I1` below is load-bearing.  It is exact only under the ASSUMPTION that a
//! p-code boolean is 0 or 1: `OpBehaviorBoolOr` and friends are the bitwise ops
//! over the WHOLE varnode (`opbehavior.cc`), so `BOOL_OR(2, 0)` is TRUE there
//! and FALSE here.  The assumption holds across the vendored specs, whose every
//! `&&` / `||` / `!` operand is a comparison, `carry()`/`scarry()`, or a
//! one-bit bitrange the SLEIGH compiler lowers to shift-and-mask
//! (`ARM.sinc:20-22`, `:278`; `AARCH64instructions.sinc:2031-2032`).  A spec
//! carrying a wider value in a boolean needs these lowered as `!= 0` at the
//! operand width instead.

use strider_ir::{IRBuilderExt, IntBinaryOp, ValueType};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
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
}
