//! Pure integer arithmetic and comparison opcodes.
//!
//! Covers `IntAdd`, `IntSub`, `IntMul`, `IntAnd`, `IntOr`, `IntXor`,
//! `IntDiv`, `IntSdiv`, `IntRem`, `IntSrem`, `IntLeft`, `IntRight`,
//! `IntSright`, `IntNeg`, `Int2Comp`, plus the comparison ops
//! `IntEqual`, `IntLess`, `IntSless`, `IntLessEqual`, `IntSlessEqual`,
//! `IntCarry`, `IntScarry`, `IntSborrow`, and `IntNotEqual` (lowered to
//! `BoolNeg(IntEqual)`).
//!
//! Cast / slice / extract / popcount / lzcount / piece / insert / ptr_*
//! handlers live in [`super::cast`] (they manipulate bit positions
//! rather than computing arithmetic).

use ir::{BoolUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::Result;
use crate::ValueLifter;

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Translates a p-code integer unary instruction into an IR unary node and
    /// writes the result to the output varnode.
    pub(super) fn process_int_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntUnaryOp,
    ) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self
            .builder
            .build_int_unary_operation(input, op, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code integer binary instruction into an IR binary node
    /// and writes the result to the output varnode.
    pub(super) fn process_int_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self
            .builder
            .build_int_binary_operation(lhs, rhs, op, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code integer comparison instruction into an IR
    /// comparison node and writes the boolean result to the output varnode.
    pub(super) fn process_int_cmp_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntCmpOp,
    ) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out =
            self.builder
                .build_int_cmp_operation(lhs, rhs, op, insn.inputs[0].size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    /// Lowers `IntNotEqual(a, b)` to `BoolNeg(IntEqual(a, b))`.
    ///
    /// Matches strider's pre-existing canonical form (one IntCmpOp + one
    /// BoolUnaryOp instead of an IntCmpOp::NotEqual variant — keeps the
    /// cmp-op enum smaller).  The cmp's operand width is the *input*
    /// width, NOT the output width: the output is a 1-byte bool, the
    /// inputs may be any integer width.
    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = crate::require_output_vn(insn)?;
        let eq = self.builder.build_int_cmp_operation(
            lhs,
            rhs,
            IntCmpOp::Equal,
            insn.inputs[0].size.try_into()?,
        )?;
        let neq = self
            .builder
            .build_boolean_unary_operation(eq, BoolUnaryOp::Neg)?;
        self.write_vn(out_vn, neq)
    }
}
