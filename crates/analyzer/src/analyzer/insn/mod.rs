use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
use rsleigh::Opcode;

use crate::error::{ErrorKind, Result};

use super::IrAnalyzer;

mod boolean;
mod control;
mod float;
mod integer;
mod memory;
mod misc;

/// Common boilerplate: require the instruction to have an output varnode and
/// return a borrowed reference to it.  Collapses ~20 inline copies of
/// `insn.output.as_ref().ok_or(ErrorKind::MissingOutputVn(insn.opcode))?`.
pub(super) fn require_output_vn(insn: &rsleigh::Insn) -> Result<&rsleigh::Vn> {
    insn.output
        .as_ref()
        .ok_or_else(|| ErrorKind::MissingOutputVn(insn.opcode).into())
}

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    /// Translates a single p-code instruction `insn` from `region_id` into
    /// one or more IR nodes.
    ///
    /// Matches on the opcode and delegates to the appropriate `process_*`
    /// helper or inline logic.  `region_lookup` resolves a CFG region id to its
    /// IR counterpart; it is called only for branch and conditional-branch
    /// opcodes.  Unimplemented opcodes return an error.
    pub(super) fn process_insn<F>(
        &mut self,
        region_id: cfg::RegionId,
        insn: &rsleigh::Insn,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(cfg::RegionId) -> Result<ir::RegionId>,
    {
        // Coerce the generic closure to a trait object so control-flow helpers
        // in sibling modules don't need to be generic on `F`.
        let region_lookup_dyn: &dyn Fn(cfg::RegionId) -> Result<ir::RegionId> = &region_lookup;
        match insn.opcode {
            Opcode::Nop => {}
            Opcode::BoolNeg => self.process_bool_unary_op(insn, BoolUnaryOp::Neg)?,
            Opcode::BoolAnd => self.process_bool_binary_op(insn, BoolBinaryOp::And)?,
            Opcode::BoolOr => self.process_bool_binary_op(insn, BoolBinaryOp::Or)?,
            Opcode::BoolXor => self.process_bool_binary_op(insn, BoolBinaryOp::Xor)?,
            Opcode::Int2Comp => self.process_int_unary_op(insn, IntUnaryOp::Not)?,
            Opcode::IntNeg => self.process_int_unary_op(insn, IntUnaryOp::Neg)?,
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
            Opcode::IntCarry => self.process_int_cmp_op(insn, IntCmpOp::Carry)?,
            Opcode::IntEqual => self.process_int_cmp_op(insn, IntCmpOp::Equal)?,
            Opcode::IntLess => self.process_int_cmp_op(insn, IntCmpOp::Less)?,
            Opcode::IntSless => self.process_int_cmp_op(insn, IntCmpOp::Sless)?,
            Opcode::IntLessEqual => self.process_int_cmp_op(insn, IntCmpOp::LessEqual)?,
            Opcode::IntRem => self.process_int_binary_op(insn, IntBinaryOp::Rem)?,
            Opcode::IntSrem => self.process_int_binary_op(insn, IntBinaryOp::Srem)?,
            Opcode::IntScarry => self.process_int_cmp_op(insn, IntCmpOp::Scarry)?,
            Opcode::IntSborrow => self.process_int_cmp_op(insn, IntCmpOp::Sborrow)?,
            Opcode::IntSlessEqual => self.process_int_cmp_op(insn, IntCmpOp::SlessEqual)?,
            Opcode::IntSub => self.process_int_binary_op(insn, IntBinaryOp::Sub)?,
            Opcode::IntSext => self.process_extend(insn, ExtendOp::SignExtend)?,
            Opcode::IntZext => self.process_extend(insn, ExtendOp::ZeroExtend)?,
            Opcode::IntNotEqual => self.handle_int_not_equal(insn)?,
            Opcode::Branch => self.handle_branch(region_id, region_lookup_dyn)?,
            Opcode::CondBranch => self.handle_cond_branch(region_id, insn, region_lookup_dyn)?,
            Opcode::Copy => self.handle_copy(insn)?,
            Opcode::Load => self.handle_load(insn)?,
            Opcode::Store => self.handle_store(insn)?,
            // `Return` and `BranchIndirect` share a handler.  The
            // BranchIndirect classification is **only correct for the
            // function-return case** (target = link register, e.g. ARM
            // `bx lr` / `pop {pc}`, MIPS `jr ra`).  Other BranchIndirect
            // sources are misclassified — the analyzer here treats them
            // all as Returns:
            //
            //   * Real tail call (`bx <target>` after computing target):
            //     should be Call + Return.  Our fixtures suppress real
            //     tail calls via `-fno-optimize-sibling-calls`, so this
            //     case doesn't fire here, but external binaries will
            //     lose the call site information.
            //   * Jump table (`ldr pc, [tbl + idx*4]`): should produce
            //     N successor edges, one per case label.  Our fixtures
            //     don't compile any switch as a jump table, so this
            //     case doesn't fire either.
            //   * Computed goto (`goto *ptr`): should be an intra-
            //     function indirect dispatch.  Not present in fixtures.
            //
            // A cleaner future refinement would inspect `insn.inputs[0]`
            // to detect link-register reads vs other targets, but
            // distinguishing the four cases requires data-flow analysis
            // that the per-instruction handler doesn't have.  Left as a
            // known limitation — see `analyzer-known-issues` BUG-5.
            Opcode::Return | Opcode::BranchIndirect => self.handle_return(insn)?,
            Opcode::Call => self.handle_call(insn)?,
            Opcode::CallIndirect => self.handle_call_indirect(insn)?,
            Opcode::Subpiece => self.handle_subpiece(insn)?,
            Opcode::Popcount => self.handle_popcount(insn)?,
            Opcode::Lzcount => self.handle_lzcount(insn)?,
            Opcode::Piece => self.handle_piece(insn)?,
            Opcode::Extract => self.handle_extract(insn)?,
            Opcode::Insert => self.handle_insert(insn)?,
            // PtrAdd: out = base + index * elem_size  (elem_size is a CONST input)
            Opcode::PtrAdd => self.handle_ptr_add(insn)?,
            // PtrSub: out = base - index
            Opcode::PtrSub => self.handle_ptr_sub(insn)?,
            // ── Float arithmetic ──────────────────────────────────────────────
            Opcode::FloatAdd => self.process_float_binary_op(insn, FloatBinaryOp::Add)?,
            Opcode::FloatSub => self.process_float_binary_op(insn, FloatBinaryOp::Sub)?,
            Opcode::FloatMul => self.process_float_binary_op(insn, FloatBinaryOp::Mul)?,
            Opcode::FloatDiv => self.process_float_binary_op(insn, FloatBinaryOp::Div)?,

            // ── Float unary (float → float) ───────────────────────────────────
            Opcode::FloatNeg => self.process_float_unary_op(insn, FloatUnaryOp::Neg)?,
            Opcode::FloatAbs => self.process_float_unary_op(insn, FloatUnaryOp::Abs)?,
            Opcode::FloatSqrt => self.process_float_unary_op(insn, FloatUnaryOp::Sqrt)?,
            Opcode::FloatCeil => self.process_float_unary_op(insn, FloatUnaryOp::Ceil)?,
            Opcode::FloatFloor => self.process_float_unary_op(insn, FloatUnaryOp::Floor)?,
            Opcode::FloatRound => self.process_float_unary_op(insn, FloatUnaryOp::Round)?,

            // ── Float comparisons (→ bool) ────────────────────────────────────
            Opcode::FloatEqual => self.process_float_cmp_op(insn, FloatCmpOp::Equal)?,
            Opcode::FloatNotEqual => self.process_float_cmp_op(insn, FloatCmpOp::NotEqual)?,
            Opcode::FloatLess => self.process_float_cmp_op(insn, FloatCmpOp::Less)?,
            Opcode::FloatLessEqual => self.process_float_cmp_op(insn, FloatCmpOp::LessEqual)?,

            // FloatNan: tests whether input is NaN (unary, → bool).
            // Emitted as FloatCmpOp::NotEqual(x, x) since IEEE 754 guarantees
            // NaN != NaN == true (and x != x is false for all non-NaN x).
            Opcode::FloatNan => self.handle_float_nan(insn)?,

            // ── Float / integer conversions ───────────────────────────────────

            // FloatInt2Float: convert integer value to float (e.g. (float)42)
            Opcode::FloatInt2Float => self.handle_float_int_to_float(insn)?,

            // FloatFloat2Float: change float precision (F32 ↔ F64)
            Opcode::FloatFloat2Float => self.handle_float_float_to_float(insn)?,

            // FloatTrunc: truncate float toward zero to integer (e.g. (int)f)
            Opcode::FloatTrunc => self.handle_float_trunc(insn)?,

            // ── remaining Sleigh opcodes ──────────────────────────────────────

            // Cast: apply a data-type to the output varnode.  GHIDRA docs:
            // "semantically equivalent to a COPY operation".
            Opcode::Cast => self.handle_cast(insn)?,

            // MultiEqual is a decompiler-internal phi; raw p-code should not
            // contain it.  Report instead of guessing semantics.
            Opcode::MultiEqual => {
                return Err(ErrorKind::UnexpectedDecompilerOpcode(insn.opcode).into());
            }

            // CallOther: user-defined CPU intrinsic (cpuid, rdtsc, syscall, …).
            // inputs[0] is a CONST user-op id; remaining inputs are arguments.
            // Clobbers memory.  The instruction's output varnode, if present,
            // receives the intrinsic's result value.
            Opcode::CallOther => self.handle_call_other(insn)?,

            // SegmentOp: segmented-address lookup.
            // inputs[0] = CONST op id, inputs[1] = segment, inputs[2] = offset.
            Opcode::SegmentOp => self.handle_segment_op(insn)?,

            // CPoolRef: JVM constant-pool lookup.  Opaque, variadic refs.
            Opcode::CPoolRef => self.handle_cpool_ref(insn)?,

            // New: JVM object allocation.  Opaque.
            Opcode::New => self.handle_new(insn)?,

            _ => return Err(ErrorKind::UnimplementedOpcode(insn.opcode).into()),
        }
        Ok(())
    }
}
