//! P-code instruction dispatch.
//!
//! Owns the single opcode-keyed match (`process_insn_inner`) that routes
//! every p-code instruction — value-producing, control-flow, call, and
//! store — to the appropriate per-opcode-family handler.  The handlers
//! themselves live in the sibling by-family modules (`arithmetic`,
//! `boolean`, `cast`, `float`, `integer`, `memory`, `misc`, `control`,
//! `call`).  This module also holds the `OPCODE_TO_*` dispatch tables and
//! the `lookup` helper for the trivial table-driven arms.

use anyhow::{Result, bail};
use rsleigh::Opcode;
use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};

use crate::lift::FunctionLifter;

/// (Opcode, IntBinaryOp) dispatch table for the trivial arms that just
/// forward to `process_int_binary_op`.
static OPCODE_TO_INT_BINARY: &[(Opcode, IntBinaryOp)] = &[
    (Opcode::IntAdd, IntBinaryOp::Add),
    (Opcode::IntAnd, IntBinaryOp::And),
    (Opcode::IntXor, IntBinaryOp::Xor),
    (Opcode::IntOr, IntBinaryOp::Or),
    (Opcode::IntDiv, IntBinaryOp::Div),
    (Opcode::IntSdiv, IntBinaryOp::Sdiv),
    (Opcode::IntMul, IntBinaryOp::Mul),
    (Opcode::IntRight, IntBinaryOp::ShiftRight),
    (Opcode::IntSright, IntBinaryOp::SShiftRight),
    (Opcode::IntLeft, IntBinaryOp::ShiftLeft),
    (Opcode::IntRem, IntBinaryOp::Rem),
    (Opcode::IntSrem, IntBinaryOp::Srem),
];

/// (Opcode, IntCmpOp) dispatch table for the trivial arms that just
/// forward to `process_int_cmp_op`.
static OPCODE_TO_INT_CMP: &[(Opcode, IntCmpOp)] = &[
    (Opcode::IntCarry, IntCmpOp::Carry),
    (Opcode::IntEqual, IntCmpOp::Equal),
    (Opcode::IntLess, IntCmpOp::Less),
    (Opcode::IntSless, IntCmpOp::Sless),
    (Opcode::IntScarry, IntCmpOp::Scarry),
    (Opcode::IntSborrow, IntCmpOp::Sborrow),
];

/// (Opcode, IntUnaryOp) dispatch table.  Only `Int2Comp` (two's-complement
/// negate, `-x`) remains here — rsleigh's `IntNeg` (bitwise complement
/// `~x`) is lowered out-of-table to `Xor(x, all_ones)` via
/// [`FunctionLifter::handle_int_neg_as_xor`] since the former BitNot unary-op was
/// removed.  See `IntUnaryOp` doc-comment.
static OPCODE_TO_INT_UNARY: &[(Opcode, IntUnaryOp)] = &[
    (Opcode::Int2Comp, IntUnaryOp::Neg),
];

/// (Opcode, ExtendOp) dispatch table for the trivial extend arms.
static OPCODE_TO_EXTEND: &[(Opcode, ExtendOp)] = &[
    (Opcode::IntZext, ExtendOp::ZeroExtend),
    (Opcode::IntSext, ExtendOp::SignExtend),
];

/// (Opcode, IntBinaryOp) dispatch table for the boolean binary opcodes.
/// Booleans are `I1`, so logical and/or/xor are integer and/or/xor at `I1`.
static OPCODE_TO_BOOL_BINARY: &[(Opcode, IntBinaryOp)] = &[
    (Opcode::BoolAnd, IntBinaryOp::And),
    (Opcode::BoolOr, IntBinaryOp::Or),
    (Opcode::BoolXor, IntBinaryOp::Xor),
];

// Note: `BoolNeg` (logical NOT of a 1-bit value) is handled out-of-table
// by [`FunctionLifter::process_bool_unary_op`], which lowers it to
// `Xor(x, IntConst(1)):I1` — the former BitNot unary-op no longer exists, so
// a bool not is `Xor(_, all_ones)` at `I1`.

/// (Opcode, FloatBinaryOp) dispatch table.
static OPCODE_TO_FLOAT_BINARY: &[(Opcode, FloatBinaryOp)] = &[
    (Opcode::FloatAdd, FloatBinaryOp::Add),
    (Opcode::FloatMul, FloatBinaryOp::Mul),
    (Opcode::FloatDiv, FloatBinaryOp::Div),
];

/// (Opcode, FloatUnaryOp) dispatch table.
static OPCODE_TO_FLOAT_UNARY: &[(Opcode, FloatUnaryOp)] = &[
    (Opcode::FloatNeg, FloatUnaryOp::Neg),
    (Opcode::FloatAbs, FloatUnaryOp::Abs),
    (Opcode::FloatSqrt, FloatUnaryOp::Sqrt),
    (Opcode::FloatCeil, FloatUnaryOp::Ceil),
    (Opcode::FloatFloor, FloatUnaryOp::Floor),
    (Opcode::FloatRound, FloatUnaryOp::Round),
];

/// (Opcode, FloatCmpOp) dispatch table.
static OPCODE_TO_FLOAT_CMP: &[(Opcode, FloatCmpOp)] = &[
    (Opcode::FloatEqual, FloatCmpOp::Equal),
    (Opcode::FloatLess, FloatCmpOp::Less),
];

fn lookup<T: Copy>(table: &[(Opcode, T)], op: Opcode) -> Option<T> {
    table.iter().find(|(o, _)| *o == op).map(|(_, t)| *t)
}

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Translates a single p-code instruction `insn` from `region_id` into
    /// one or more IR nodes.
    ///
    /// Matches on the opcode and delegates to the appropriate `process_*`
    /// helper or inline logic.  `region_lookup` resolves a CFG region id to its
    /// IR counterpart; it is called only for branch and conditional-branch
    /// opcodes.  Unimplemented opcodes return an error.
    pub(crate) fn process_insn<F>(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        addr: strider_cfg::PcodeInsnAddr,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(strider_cfg::RegionId) -> Result<strider_ir::RegionId>,
    {
        // Funnel: every IR node born from this pcode insn picks up the
        // parent machine-instruction address in its asm-fingerprint
        // side-table.  The set_lift_addr(Some)/set_lift_addr(None)
        // bracket is the funnel.  A closure API would force a `&mut
        // self` plus a `&mut self.builder` split the borrow checker
        // rejects, so we use open-call brackets instead.
        let machine_addr = addr.machine_addr.addr;
        self.builder.set_lift_addr(Some(machine_addr));
        let res = self.process_insn_inner(region_id, insn, region_lookup);
        self.builder.set_lift_addr(None);
        res
    }

    /// Single opcode-keyed dispatch.  Every p-code opcode hits exactly one
    /// arm: value-producing opcodes call their family handler, control /
    /// call / store opcodes call theirs, and the trivial table-driven
    /// families fall through the `other` arm.  An opcode the lifter does
    /// not model bails.
    fn process_insn_inner<F>(
        &mut self,
        region_id: strider_cfg::RegionId,
        insn: &rsleigh::Insn,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(strider_cfg::RegionId) -> Result<strider_ir::RegionId>,
    {
        let lifter = self;
        // `handle_branch` / `handle_cond_branch` take `&dyn Fn(...)`;
        // `&region_lookup` (generic `F: Fn(...)`) coerces to the trait
        // object at the call boundary, so no explicit cast is needed.
        match insn.opcode {
            // ── Value-producing opcodes ──────────────────────────────────────
            Opcode::Copy => lifter.handle_copy(insn)?,
            Opcode::IntSub => lifter.handle_int_sub(insn)?,
            Opcode::IntLessEqual => lifter.handle_int_less_equal(insn)?,
            Opcode::IntSlessEqual => lifter.handle_int_sless_equal(insn)?,
            Opcode::IntNotEqual => lifter.handle_int_not_equal(insn)?,
            Opcode::Subpiece => lifter.handle_subpiece(insn)?,
            Opcode::Popcount => lifter.handle_popcount(insn)?,
            Opcode::Lzcount => lifter.handle_lzcount(insn)?,
            Opcode::Piece => lifter.handle_piece(insn)?,
            Opcode::Extract => lifter.handle_extract(insn)?,
            Opcode::Insert => lifter.handle_insert(insn)?,
            // PtrAdd: out = base + index * elem_size  (elem_size is a CONST input)
            Opcode::PtrAdd => lifter.handle_ptr_add(insn)?,
            // PtrSub: out = base - index
            Opcode::PtrSub => lifter.handle_ptr_sub(insn)?,
            // Cast: apply a data-type to the output varnode.  GHIDRA docs:
            // "semantically equivalent to a COPY operation".
            Opcode::Cast => lifter.handle_cast(insn)?,
            Opcode::FloatSub => lifter.handle_float_sub(insn)?,
            Opcode::FloatNotEqual => lifter.handle_float_not_equal(insn)?,
            Opcode::FloatLessEqual => lifter.handle_float_less_equal(insn)?,
            // FloatNan: tests whether input is NaN (unary, → bool).  Lowered to
            // Xor(FloatEqual(x, x), 1):I1 since IEEE 754 guarantees NaN != NaN.
            Opcode::FloatNan => lifter.handle_float_nan(insn)?,
            // IntNeg: bitwise complement (`~x`).  Lowered to `Xor(x, all_ones)` —
            // the former BitNot unary-op was removed in favour of the canonical Xor shape.
            Opcode::IntNeg => lifter.handle_int_neg_as_xor(insn)?,
            // BoolNeg: logical NOT of a 1-bit value.  Lowered to
            // `Xor(x, IntConst(1)):I1` for the same reason.
            Opcode::BoolNeg => lifter.process_bool_unary_op(insn)?,
            // ── Float / integer conversions ───────────────────────────────────
            Opcode::FloatInt2Float => lifter.handle_float_int_to_float(insn)?,
            Opcode::FloatFloat2Float => lifter.handle_float_float_to_float(insn)?,
            Opcode::FloatTrunc => lifter.handle_float_trunc(insn)?,
            Opcode::Load => lifter.handle_load(insn)?,
            // SegmentOp: segmented-address lookup.
            Opcode::SegmentOp => lifter.handle_segment_op(insn)?,
            // CPoolRef: JVM constant-pool lookup.  Opaque, variadic refs.
            Opcode::CPoolRef => lifter.handle_cpool_ref(insn)?,
            // New: JVM object allocation.  Opaque.
            Opcode::New => lifter.handle_new(insn)?,

            // ── Control-flow / call / store opcodes ──────────────────────────
            Opcode::Nop => {}
            Opcode::Branch => lifter.handle_branch(region_id, &region_lookup)?,
            Opcode::CondBranch => lifter.handle_cond_branch(region_id, insn, &region_lookup)?,
            Opcode::Store => lifter.handle_store(insn)?,
            // `Return` and `BranchIndirect` share a handler that emits a
            // calling-convention `Return`.  This is correct for the
            // link-register-return case (e.g. ARM `bx lr`); tail calls /
            // jump tables / computed gotos are routed via dedicated
            // terminators (`Switch`, `UnresolvedIndirectBranch`) that the
            // cfg builder seats from the orchestrator's `known_targets`
            // feedback, both handled in the special-terminator post-pass.
            Opcode::Return | Opcode::BranchIndirect => lifter.handle_return(insn)?,
            Opcode::Call => lifter.handle_call(insn)?,
            Opcode::CallIndirect => lifter.handle_call_indirect(insn)?,
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
            Opcode::CallOther => lifter.handle_call_other(insn)?,

            // ── Trivial table-driven arms ────────────────────────────────────
            // Integer / bool / float binary, comparison, unary, and extend
            // ops that simply forward to a `process_*_op` builder call.
            // Adding a new opcode in this family is a one-row edit to the
            // corresponding table above.  An opcode matching none of the
            // tables is genuinely unmodelled and bails.
            other => {
                if let Some(op) = lookup(OPCODE_TO_INT_BINARY, other) {
                    lifter.process_int_binary_op(insn, op)?;
                } else if let Some(op) = lookup(OPCODE_TO_INT_CMP, other) {
                    lifter.process_int_cmp_op(insn, op)?;
                } else if let Some(op) = lookup(OPCODE_TO_INT_UNARY, other) {
                    lifter.process_int_unary_op(insn, op)?;
                } else if let Some(op) = lookup(OPCODE_TO_EXTEND, other) {
                    lifter.process_extend(insn, op)?;
                } else if let Some(op) = lookup(OPCODE_TO_BOOL_BINARY, other) {
                    lifter.process_bool_binary_op(insn, op)?;
                } else if let Some(op) = lookup(OPCODE_TO_FLOAT_BINARY, other) {
                    lifter.process_float_binary_op(insn, op)?;
                } else if let Some(op) = lookup(OPCODE_TO_FLOAT_UNARY, other) {
                    lifter.process_float_unary_op(insn, op)?;
                } else if let Some(op) = lookup(OPCODE_TO_FLOAT_CMP, other) {
                    lifter.process_float_cmp_op(insn, op)?;
                } else {
                    bail!("unimplemented p-code opcode {:?}", insn.opcode);
                }
            }
        }
        Ok(())
    }
}
