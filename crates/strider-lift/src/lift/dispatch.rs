//! P-code instruction dispatch.
//!
//! Owns the single opcode-keyed match (`process_insn_inner`) that routes
//! every p-code instruction — value-producing, control-flow, call, and
//! store — to the appropriate per-opcode-family handler.  The handlers
//! themselves live in the sibling by-family modules (`arithmetic`,
//! `boolean`, `cast`, `float`, `integer`, `memory`, `misc`, `control`,
//! `call`).  Every opcode hits exactly one direct `match` arm; the trivial
//! integer / bool / float binary, comparison, unary, and extend families
//! forward to a `process_*_op` builder call with the corresponding IR op.

use anyhow::{Result, bail};
use rsleigh::Opcode;
use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};

use crate::lift::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Translates a single p-code instruction `insn` from `region_id` into
    /// one or more IR nodes.
    ///
    /// Matches on the opcode and delegates to the appropriate `process_*`
    /// helper or inline logic.  `region_map` resolves a CFG region id to its
    /// IR counterpart (via [`super::ir_region_of`]); it is consulted only for
    /// branch and conditional-branch opcodes.  Unimplemented opcodes return
    /// an error.
    pub(crate) fn process_insn(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        addr: strider_cfg::PcodeInsnAddr,
        region_map: &super::RegionMap,
    ) -> Result<()> {
        // Funnel: every IR node born from this pcode insn picks up the
        // parent machine-instruction address in its asm-fingerprint
        // side-table (see `with_lift_addr`).
        let machine_addr = addr.machine_addr.addr;
        let res = self.with_lift_addr(Some(machine_addr), |s| {
            s.process_insn_inner(region_id, insn, region_map)
        });
        // Attach the offending machine instruction's address + opcode to any
        // lift failure.  Width / shape errors raised deep in the IR builders
        // (e.g. an unsupported odd-byte varnode width via `int_for_byte_size`)
        // otherwise carry no asm context, so a failed whole-function lift
        // can't be tied back to a specific instruction.
        res.map_err(|e| {
            e.context(format!(
                "lifting opcode {:?} at machine address {machine_addr:#x}",
                insn.opcode
            ))
        })
    }

    /// Single opcode-keyed dispatch.  Every p-code opcode hits exactly one
    /// arm: value-producing opcodes call their family handler, control /
    /// call / store opcodes call theirs, and the trivial integer / bool /
    /// float binary / comparison / unary / extend families forward to a
    /// `process_*_op` builder call with the corresponding IR op.  An opcode
    /// the lifter does not model bails.
    fn process_insn_inner(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        region_map: &super::RegionMap,
    ) -> Result<()> {
        // `handle_branch` / `handle_cond_branch` resolve successor regions
        // through `region_map` (via `super::ir_region_of`).
        match insn.opcode {
            // ── Value-producing opcodes ──────────────────────────────────────
            // Cast: apply a data-type to the output varnode.  GHIDRA docs:
            // "semantically equivalent to a COPY operation", so it shares the
            // `Copy` handler verbatim (read input 0, write to the output vn).
            Opcode::Copy | Opcode::Cast => self.handle_copy(insn)?,
            Opcode::IntSub => self.handle_int_sub(insn)?,
            Opcode::IntLessEqual => self.handle_int_less_equal(insn)?,
            Opcode::IntSlessEqual => self.handle_int_sless_equal(insn)?,
            Opcode::IntNotEqual => self.handle_int_not_equal(insn)?,
            Opcode::Subpiece => self.handle_subpiece(insn)?,
            Opcode::Popcount => self.handle_popcount(insn)?,
            Opcode::Lzcount => self.handle_lzcount(insn)?,
            Opcode::Piece => self.handle_piece(insn)?,
            Opcode::Extract => self.handle_extract(insn)?,
            Opcode::Insert => self.handle_insert(insn)?,
            // PtrAdd / PtrSub are decompiler-internal pointer-arithmetic
            // opcodes (CPUI_PTRADD / CPUI_PTRSUB) that `rsleigh::Sleigh::lift_one`
            // does not emit — raw SLEIGH lifting uses INT_ADD/INT_MULT directly.
            // Surfacing one means rsleigh's contract changed; fail closed (as
            // for MULTIEQUAL) rather than guess semantics — especially since
            // CPUI_PTRSUB is `base + offset`, not a subtraction.
            Opcode::PtrAdd | Opcode::PtrSub => {
                bail!(
                    "opcode {:?} is a decompiler-internal pointer op; rsleigh::lift_one is contracted not to emit it",
                    insn.opcode
                );
            }
            Opcode::FloatSub => self.handle_float_sub(insn)?,
            Opcode::FloatNotEqual => self.handle_float_not_equal(insn)?,
            Opcode::FloatLessEqual => self.handle_float_less_equal(insn)?,
            // FloatNan: tests whether input is NaN (unary, → bool).  Lowered to
            // Xor(FloatEqual(x, x), 1):I1 since IEEE 754 guarantees NaN != NaN.
            Opcode::FloatNan => self.handle_float_nan(insn)?,
            // IntNeg: bitwise complement (`~x`).  Lowered to `Xor(x, all_ones)` —
            // the former BitNot unary-op was removed in favour of the canonical Xor shape.
            Opcode::IntNeg => self.handle_int_neg_as_xor(insn)?,
            // BoolNeg: logical NOT of a 1-bit value.  Lowered to
            // `Xor(x, IntConst(1)):I1` for the same reason.
            Opcode::BoolNeg => self.process_bool_unary_op(insn)?,
            // ── Float / integer conversions ───────────────────────────────────
            Opcode::FloatInt2Float => self.handle_float_int_to_float(insn)?,
            Opcode::FloatFloat2Float => self.handle_float_float_to_float(insn)?,
            Opcode::FloatTrunc => self.handle_float_trunc(insn)?,
            Opcode::Load => self.handle_load(insn)?,
            // SegmentOp: segmented-address lookup.
            Opcode::SegmentOp => self.handle_segment_op(insn)?,
            // CPoolRef: JVM constant-pool lookup.  Opaque, variadic refs.
            Opcode::CPoolRef => self.handle_cpool_ref(insn)?,
            // New: JVM object allocation.  Opaque.
            Opcode::New => self.handle_new(insn)?,

            // ── Control-flow / call / store opcodes ──────────────────────────
            Opcode::Nop => {}
            Opcode::Branch => self.handle_branch()?,
            Opcode::CondBranch => self.handle_cond_branch(region_id, insn, region_map)?,
            Opcode::Store => self.handle_store(insn)?,
            // `Return` and `BranchIndirect` share a handler that emits a
            // calling-convention `Return`.  This is correct for the
            // link-register-return case (e.g. ARM `bx lr`); tail calls /
            // jump tables / computed gotos are routed via dedicated
            // terminators (`Switch`, `UnresolvedIndirectBranch`) that the
            // cfg builder seats from the orchestrator's `known_targets`
            // feedback, both handled in the special-terminator post-pass.
            Opcode::Return | Opcode::BranchIndirect => self.handle_return()?,
            Opcode::Call => self.handle_call(insn)?,
            Opcode::CallIndirect => self.handle_call_indirect(insn)?,
            // GHIDRA's MULTIEQUAL is a decompiler-internal phi that
            // `rsleigh::Sleigh::lift_one` does not emit.  Surfacing it
            // here means rsleigh's contract changed; surface as an
            // error rather than guessing semantics.
            Opcode::MultiEqual => {
                bail!(
                    "opcode {:?} is a decompiler-internal phi; rsleigh::lift_one is contracted not to emit it",
                    insn.opcode
                );
            }
            // CallOther: user-defined CPU intrinsic (cpuid, rdtsc, syscall, …).
            // inputs[0] is a CONST user-op id; remaining inputs are arguments.
            // Clobbers memory.  The instruction's output varnode, if present,
            // receives the intrinsic's result value.
            Opcode::CallOther => self.handle_call_other(insn)?,

            // ── Trivial forwarding arms ──────────────────────────────────────
            // Integer / bool / float binary, comparison, unary, and extend
            // ops that simply forward to a `process_*_op` builder call with
            // the corresponding IR op.  Adding a new opcode in this family is
            // a one-line match-arm edit.
            //
            // Integer binary:
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
            // Integer comparison:
            Opcode::IntCarry => self.process_int_cmp_op(insn, IntCmpOp::Carry)?,
            Opcode::IntEqual => self.process_int_cmp_op(insn, IntCmpOp::Equal)?,
            Opcode::IntLess => self.process_int_cmp_op(insn, IntCmpOp::Less)?,
            Opcode::IntSless => self.process_int_cmp_op(insn, IntCmpOp::Sless)?,
            Opcode::IntScarry => self.process_int_cmp_op(insn, IntCmpOp::Scarry)?,
            Opcode::IntSborrow => self.process_int_cmp_op(insn, IntCmpOp::Sborrow)?,
            // Integer unary: only `Int2Comp` (two's-complement negate, `-x`)
            // — `IntNeg` (bitwise complement `~x`) is lowered above to
            // `Xor(x, all_ones)` via `handle_int_neg_as_xor`.
            Opcode::Int2Comp => self.process_int_unary_op(insn, IntUnaryOp::Neg)?,
            // Integer extend:
            Opcode::IntZext => self.process_extend(insn, ExtendOp::ZeroExtend)?,
            Opcode::IntSext => self.process_extend(insn, ExtendOp::SignExtend)?,
            // Boolean binary: booleans are `I1`, so logical and/or/xor are
            // integer and/or/xor at `I1`.
            Opcode::BoolAnd => self.process_bool_binary_op(insn, IntBinaryOp::And)?,
            Opcode::BoolOr => self.process_bool_binary_op(insn, IntBinaryOp::Or)?,
            Opcode::BoolXor => self.process_bool_binary_op(insn, IntBinaryOp::Xor)?,
            // Float binary:
            Opcode::FloatAdd => self.process_float_binary_op(insn, FloatBinaryOp::Add)?,
            Opcode::FloatMul => self.process_float_binary_op(insn, FloatBinaryOp::Mul)?,
            Opcode::FloatDiv => self.process_float_binary_op(insn, FloatBinaryOp::Div)?,
            // Float unary:
            Opcode::FloatNeg => self.process_float_unary_op(insn, FloatUnaryOp::Neg)?,
            Opcode::FloatAbs => self.process_float_unary_op(insn, FloatUnaryOp::Abs)?,
            Opcode::FloatSqrt => self.process_float_unary_op(insn, FloatUnaryOp::Sqrt)?,
            Opcode::FloatCeil => self.process_float_unary_op(insn, FloatUnaryOp::Ceil)?,
            Opcode::FloatFloor => self.process_float_unary_op(insn, FloatUnaryOp::Floor)?,
            Opcode::FloatRound => self.process_float_unary_op(insn, FloatUnaryOp::Round)?,
            // Float comparison:
            Opcode::FloatEqual => self.process_float_cmp_op(insn, FloatCmpOp::Equal)?,
            Opcode::FloatLess => self.process_float_cmp_op(insn, FloatCmpOp::Less)?,

            // An opcode the lifter does not model bails.
            _ => bail!("unimplemented p-code opcode {:?}", insn.opcode),
        }
        Ok(())
    }
}
