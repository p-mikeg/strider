use anyhow::{Result, bail};
use rsleigh::Opcode;
use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};

use crate::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// `region_map` is consulted only by the branch opcodes.
    pub(crate) fn process_insn(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        addr: strider_cfg::PcodeInsnAddr,
        region_map: &super::RegionMap,
    ) -> Result<()> {
        // Every node built here is fingerprinted with this machine address.
        let machine_addr = addr.machine_addr.addr;
        let res = self.with_lift_addr(Some(machine_addr), |s| {
            s.process_insn_inner(region_id, insn, addr, region_map)
        });
        // IR builder errors carry no instruction context of their own.
        res.map_err(|e| {
            e.context(format!(
                "lifting opcode {:?} at machine address {machine_addr:#x}",
                insn.opcode
            ))
        })
    }

    fn process_insn_inner(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        addr: strider_cfg::PcodeInsnAddr,
        region_map: &super::RegionMap,
    ) -> Result<()> {
        match insn.opcode {
            // GHIDRA: Cast is "semantically equivalent to a COPY".
            Opcode::Copy | Opcode::Cast => self.handle_copy(insn)?,
            Opcode::IntSub => self.handle_int_sub(insn)?,
            Opcode::IntLessEqual => self.handle_int_less_equal(insn)?,
            Opcode::IntSlessEqual => self.handle_int_sless_equal(insn)?,
            Opcode::IntNotEqual => self.handle_int_not_equal(insn)?,
            Opcode::Subpiece => self.handle_subpiece(insn)?,
            Opcode::Popcount => self.handle_popcount(insn)?,
            Opcode::Lzcount => self.handle_lzcount(insn)?,
            Opcode::Piece => self.handle_piece(insn)?,
            // Decompiler-internal; SLEIGH lowers bitrange assignments to
            // shift-and-mask instead.
            Opcode::Extract | Opcode::Insert => {
                bail!(
                    "opcode {:?} is a bit-field op no supported sla emits; rsleigh::lift_one is contracted not to emit it",
                    insn.opcode
                );
            }
            // Decompiler-internal pointer arithmetic (PTRSUB is `base +
            // offset`, not a subtraction).
            Opcode::PtrAdd | Opcode::PtrSub => {
                bail!(
                    "opcode {:?} is a decompiler-internal pointer op; rsleigh::lift_one is contracted not to emit it",
                    insn.opcode
                );
            }
            Opcode::FloatSub => self.handle_float_sub(insn)?,
            Opcode::FloatNotEqual => self.handle_float_not_equal(insn)?,
            Opcode::FloatLessEqual => self.handle_float_less_equal(insn)?,
            // `Xor(FloatEqual(x, x), 1):I1`: NaN != NaN.
            Opcode::FloatNan => self.handle_float_nan(insn)?,
            // Bitwise complement: `Xor(x, all_ones)`.
            Opcode::IntNeg => self.handle_int_neg_as_xor(insn)?,
            // `Xor(x, IntConst(1)):I1`.
            Opcode::BoolNeg => self.process_bool_unary_op(insn)?,
            Opcode::FloatInt2Float => self.handle_float_int_to_float(insn)?,
            Opcode::FloatFloat2Float => self.handle_float_float_to_float(insn)?,
            Opcode::FloatTrunc => self.handle_float_trunc(insn)?,
            Opcode::Load => self.handle_load(insn)?,
            // Decompiler-internal: minted by `ActionSegmentize` and the
            // decompiler's SSA construction, never by SLEIGH.
            Opcode::SegmentOp | Opcode::Indirect => {
                bail!(
                    "opcode {:?} is decompiler-internal; rsleigh::lift_one is contracted not to emit it",
                    insn.opcode
                );
            }
            // JVM constant-pool lookup; opaque, variadic refs.
            Opcode::CPoolRef => self.handle_cpool_ref(insn)?,
            // JVM object allocation; opaque.
            Opcode::New => self.handle_new(insn)?,

            // `link_region_edges` wires every unconditional successor.
            Opcode::Nop | Opcode::Branch => {}
            Opcode::CondBranch => self.handle_cond_branch(region_id, insn, region_map)?,
            Opcode::Store => self.handle_store(insn)?,
            // A `BranchIndirect` reaches here only where a `LinkRegister`
            // answer seated the region as a `Return`; other indirect branches
            // get dedicated terminators. Both record a return site.
            Opcode::Return | Opcode::BranchIndirect => self.handle_sleigh_return(insn, addr)?,
            Opcode::Call => self.handle_call(insn)?,
            Opcode::CallIndirect => self.handle_call_indirect(insn)?,
            // Decompiler-internal phi.
            Opcode::MultiEqual => {
                bail!(
                    "opcode {:?} is a decompiler-internal phi; rsleigh::lift_one is contracted not to emit it",
                    insn.opcode
                );
            }
            // CPU intrinsic: inputs[0] is the CONST user-op id.
            Opcode::CallOther => {
                self.handle_call_other(insn, self.ends_no_return_region(region_id, addr))?;
            }

            Opcode::IntAdd => self.process_int_binary_op(insn, IntBinaryOp::Add)?,
            Opcode::IntAnd => self.process_int_binary_op(insn, IntBinaryOp::And)?,
            Opcode::IntXor => self.process_int_binary_op(insn, IntBinaryOp::Xor)?,
            Opcode::IntOr => self.process_int_binary_op(insn, IntBinaryOp::Or)?,
            Opcode::IntDiv => self.process_int_binary_op(insn, IntBinaryOp::Div)?,
            Opcode::IntSdiv => self.process_int_binary_op(insn, IntBinaryOp::Sdiv)?,
            Opcode::IntMul => self.process_int_binary_op(insn, IntBinaryOp::Mul)?,
            Opcode::IntRight => self.process_int_binary_op(insn, IntBinaryOp::ShiftRight)?,
            Opcode::IntSright => self.process_int_binary_op(insn, IntBinaryOp::SShiftRight)?,
            Opcode::IntLeft => self.process_int_binary_op(insn, IntBinaryOp::ShiftLeft)?,
            Opcode::IntRem => self.process_int_binary_op(insn, IntBinaryOp::Rem)?,
            Opcode::IntSrem => self.process_int_binary_op(insn, IntBinaryOp::Srem)?,
            Opcode::IntCarry => self.process_int_cmp_op(insn, IntCmpOp::Carry)?,
            Opcode::IntEqual => self.process_int_cmp_op(insn, IntCmpOp::Equal)?,
            Opcode::IntLess => self.process_int_cmp_op(insn, IntCmpOp::Less)?,
            Opcode::IntSless => self.process_int_cmp_op(insn, IntCmpOp::Sless)?,
            Opcode::IntScarry => self.process_int_cmp_op(insn, IntCmpOp::Scarry)?,
            Opcode::IntSborrow => self.process_int_cmp_op(insn, IntCmpOp::Sborrow)?,
            Opcode::Int2Comp => self.process_int_unary_op(insn, IntUnaryOp::Neg)?,
            Opcode::IntZext => self.process_extend(insn, ExtendOp::ZeroExtend)?,
            Opcode::IntSext => self.process_extend(insn, ExtendOp::SignExtend)?,
            Opcode::BoolAnd => self.process_bool_binary_op(insn, IntBinaryOp::And)?,
            Opcode::BoolOr => self.process_bool_binary_op(insn, IntBinaryOp::Or)?,
            Opcode::BoolXor => self.process_bool_binary_op(insn, IntBinaryOp::Xor)?,
            Opcode::FloatAdd => self.process_float_binary_op(insn, FloatBinaryOp::Add)?,
            Opcode::FloatMul => self.process_float_binary_op(insn, FloatBinaryOp::Mul)?,
            Opcode::FloatDiv => self.process_float_binary_op(insn, FloatBinaryOp::Div)?,
            Opcode::FloatNeg => self.process_float_unary_op(insn, FloatUnaryOp::Neg)?,
            Opcode::FloatAbs => self.process_float_unary_op(insn, FloatUnaryOp::Abs)?,
            Opcode::FloatSqrt => self.process_float_unary_op(insn, FloatUnaryOp::Sqrt)?,
            Opcode::FloatCeil => self.process_float_unary_op(insn, FloatUnaryOp::Ceil)?,
            Opcode::FloatFloor => self.process_float_unary_op(insn, FloatUnaryOp::Floor)?,
            Opcode::FloatRound => self.process_float_unary_op(insn, FloatUnaryOp::Round)?,
            Opcode::FloatEqual => self.process_float_cmp_op(insn, FloatCmpOp::Equal)?,
            Opcode::FloatLess => self.process_float_cmp_op(insn, FloatCmpOp::Less)?,
        }
        Ok(())
    }
}
