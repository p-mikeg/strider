use ir::node::NodeOutputType;
use ir::{BoolUnaryOp, ExtendOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::error::{ErrorKind, Result};

use super::super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    /// Translates a p-code integer unary instruction into an IR unary node and
    /// writes the result to the output varnode.
    pub(super) fn process_int_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntUnaryOp,
    ) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
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
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
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
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out =
            self.builder
                .build_int_cmp_operation(lhs, rhs, op, insn.inputs[0].size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code zero-extend or sign-extend instruction into an IR
    /// extend node and writes the result to the output varnode.
    pub(super) fn process_extend(&mut self, insn: &rsleigh::Insn, op: ExtendOp) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self
            .builder
            .extend_if_needed(input, out_vn.size.try_into()?, op)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // P-code IntNotEqual is lowered to BoolNeg(IntEqual) for deterministic
        // canonical form (one IntCmpOp, one BoolUnaryOp instead of an
        // IntCmpOp::NotEqual variant — keeps the cmp-op enum smaller).
        //
        // The cmp's operand width is the *input* width, NOT the output width:
        // the output is a 1-byte bool, the inputs may be any integer width.
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
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

    pub(super) fn handle_subpiece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // `Subpiece(value, byte_offset, out_size)`: extracts `out_size` bytes
        // starting at byte `byte_offset` from `value`.
        // Implemented as: right-shift by (byte_offset * 8) bits, then truncate.
        let input = self.read_vn(&insn.inputs[0])?;
        let byte_offset = insn.inputs[1].addr.off;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let shifted = if byte_offset == 0 {
            input
        } else {
            let bit_shift = byte_offset * 8;
            let shift_const = self
                .builder
                .build_int_const(bit_shift, insn.inputs[0].size.try_into()?)?;
            self.builder.build_int_binary_operation(
                input,
                shift_const,
                IntBinaryOp::ShiftRight,
                insn.inputs[0].size.try_into()?,
            )?
        };
        let out = self
            .builder
            .truncate_if_needed(shifted, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_popcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self
            .builder
            .build_popcount(input, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_lzcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self.builder.build_lzcount(input, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_piece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = hi (most significant), inputs[1] = lo (least significant).
        // Lowered to: Or(ShiftLeft(ZeroExtend(hi), lo_bits), ZeroExtend(lo)).
        let hi = self.read_vn(&insn.inputs[0])?;
        let lo = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out_ty: NodeOutputType = out_vn.size.try_into()?;
        let hi_ty = self.builder.get_output_type(hi)?.to_natural_int_type();
        let hi_int = self.builder.convert_to_int_if_needed(hi, hi_ty)?;
        let lo_ty = self.builder.get_output_type(lo)?.to_natural_int_type();
        let lo_int = self.builder.convert_to_int_if_needed(lo, lo_ty)?;
        let lo_bits = lo_ty.bit_width() as u64;
        let hi_wide = self.builder.convert_to_int_if_needed(hi_int, out_ty)?;
        let lo_wide = self.builder.convert_to_int_if_needed(lo_int, out_ty)?;
        let shift_amt = self.builder.build_int_const(lo_bits, out_ty)?;
        let hi_shifted = self.builder.build_int_binary_operation(
            hi_wide,
            shift_amt,
            IntBinaryOp::ShiftLeft,
            out_ty,
        )?;
        let out = self.builder.build_int_binary_operation(
            hi_shifted,
            lo_wide,
            IntBinaryOp::Or,
            out_ty,
        )?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_extract(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = value, inputs[1] = lsb (CONST), inputs[2] = bit_count (CONST)
        // Lowered to: Truncate(ShiftRight(x, lsb), narrow_ty), with an extra
        // And mask when len < narrow_ty.bit_width() to preserve "upper bits zero".
        let input = self.read_vn(&insn.inputs[0])?;
        let lsb = insn.inputs[1].addr.off as u8;
        let len = insn.inputs[2].addr.off as u8;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let narrow_ty: NodeOutputType = out_vn.size.try_into()?;
        let x_nat_ty = self.builder.get_output_type(input)?.to_natural_int_type();
        let x_int = self.builder.convert_to_int_if_needed(input, x_nat_ty)?;
        let shifted = if lsb == 0 {
            x_int
        } else {
            let lsb_const = self.builder.build_int_const(lsb as u64, x_nat_ty)?;
            self.builder.build_int_binary_operation(
                x_int,
                lsb_const,
                IntBinaryOp::ShiftRight,
                x_nat_ty,
            )?
        };
        let narrowed = self.builder.truncate_if_needed(shifted, narrow_ty)?;
        let out = if (len as usize) < narrow_ty.bit_width() {
            let mask_val = if len >= 64 {
                u64::MAX
            } else {
                (1u64 << len) - 1
            };
            let mask = self.builder.build_int_const(mask_val, narrow_ty)?;
            self.builder.build_int_binary_operation(
                narrowed,
                mask,
                IntBinaryOp::And,
                narrow_ty,
            )?
        } else {
            narrowed
        };
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_insert(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = dest, inputs[1] = src, inputs[2] = lsb (CONST), inputs[3] = bit_count (CONST).
        // Lowered to: Or(And(dest, !mask_shifted), ShiftLeft(And(src, mask_raw), lsb)).
        let dest = self.read_vn(&insn.inputs[0])?;
        let src = self.read_vn(&insn.inputs[1])?;
        let lsb = insn.inputs[2].addr.off as u8;
        let len = insn.inputs[3].addr.off as u8;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out_ty: NodeOutputType = out_vn.size.try_into()?;

        let dest_ty = self.builder.get_output_type(dest)?.to_natural_int_type();
        let dest_int = self.builder.convert_to_int_if_needed(dest, dest_ty)?;
        let src_ty = self.builder.get_output_type(src)?.to_natural_int_type();
        let src_int = self.builder.convert_to_int_if_needed(src, src_ty)?;

        let dest_wide = self.builder.convert_to_int_if_needed(dest_int, out_ty)?;
        let src_wide = self.builder.convert_to_int_if_needed(src_int, out_ty)?;

        let mask_raw = if len >= 64 {
            u64::MAX
        } else {
            (1u64 << len) - 1
        };
        let mask_shifted = mask_raw.wrapping_shl(lsb as u32);
        let not_mask_shifted = !mask_shifted;

        let not_m_const = self.builder.build_int_const(not_mask_shifted, out_ty)?;
        let cleared = self.builder.build_int_binary_operation(
            dest_wide,
            not_m_const,
            IntBinaryOp::And,
            out_ty,
        )?;

        let mask_const = self.builder.build_int_const(mask_raw, out_ty)?;
        let src_masked = self.builder.build_int_binary_operation(
            src_wide,
            mask_const,
            IntBinaryOp::And,
            out_ty,
        )?;

        let src_positioned = if lsb == 0 {
            src_masked
        } else {
            let lsb_const = self.builder.build_int_const(lsb as u64, out_ty)?;
            self.builder.build_int_binary_operation(
                src_masked,
                lsb_const,
                IntBinaryOp::ShiftLeft,
                out_ty,
            )?
        };

        let out = self.builder.build_int_binary_operation(
            cleared,
            src_positioned,
            IntBinaryOp::Or,
            out_ty,
        )?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_ptr_add(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let base = self.read_vn(&insn.inputs[0])?;
        let index = self.read_vn(&insn.inputs[1])?;
        let elem_size = insn.inputs[2].addr.off;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out_ty: ir::ValueType = out_vn.size.try_into()?;
        let elem_const = self.builder.build_int_const(elem_size, out_ty)?;
        let scaled = self.builder.build_int_binary_operation(
            index,
            elem_const,
            IntBinaryOp::Mul,
            out_ty,
        )?;
        let out = self.builder.build_int_binary_operation(
            base,
            scaled,
            IntBinaryOp::Add,
            out_ty,
        )?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_ptr_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let base = self.read_vn(&insn.inputs[0])?;
        let index = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self.builder.build_int_binary_operation(
            base,
            index,
            IntBinaryOp::Sub,
            out_vn.size.try_into()?,
        )?;
        self.write_vn(out_vn, out)
    }
}
