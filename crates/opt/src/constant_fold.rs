use ir::{BuiltFunctionGraph, IntBinaryOp, IntUnaryOp, IntCmpOp, BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatUnaryOp, FloatCmpOp};
use ir::node::{NodeId, NodeKind, NodeOutputKind, NodeOutputType};

use crate::error::{Error, Result};
use crate::opt::{OptimizationResult, Optimizer};
use crate::utils::{int_const_val, bool_const_val, float_const_val, make_int_const, make_bool_const, make_float_const, make_int_bits_to_float_node, make_float_to_float_node, replace_all_uses};

// ── integer constant evaluation ───────────────────────────────────────────────

/// Evaluates `op(l, r)` as an integer arithmetic operation, returning the
/// result masked to `ty`, or `None` if the operation is undefined (e.g.
/// division by zero).
fn eval_int_binary(op: IntBinaryOp, l: u64, r: u64, ty: NodeOutputType) -> Option<u64> {
    let bits = ty.bit_width() as u64;
    // Shift amounts are masked to prevent UB; u32 is required by wrapping_shl/shr.
    let shift = |s: u64| -> u32 { (s & (bits - 1)) as u32 };
    let raw: u64 = match op {
        IntBinaryOp::Add => l.wrapping_add(r),
        IntBinaryOp::Sub => l.wrapping_sub(r),
        IntBinaryOp::Mul => l.wrapping_mul(r),
        IntBinaryOp::And => l & r,
        IntBinaryOp::Or  => l | r,
        IntBinaryOp::Xor => l ^ r,
        IntBinaryOp::ShiftLeft  => l.wrapping_shl(shift(r)),
        IntBinaryOp::ShiftRight => l.wrapping_shr(shift(r)),
        IntBinaryOp::SShiftRight => {
            let sl = ty.get_signed_int(l)? as i64;
            (sl >> shift(r)) as u64
        }
        IntBinaryOp::Div => {
            if r == 0 { return None; }
            l / r
        }
        IntBinaryOp::Sdiv => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 { return None; }
            if sl == i64::MIN && sr == -1 { return None; } // overflow
            (sl / sr) as u64
        }
        IntBinaryOp::Rem => {
            if r == 0 { return None; }
            l % r
        }
        IntBinaryOp::Srem => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 { return None; }
            (sl % sr) as u64
        }
    };
    ty.get_unsigned_int(raw)
}

/// Evaluates a comparison on two constant integer values.
fn eval_int_cmp(op: IntCmpOp, l: u64, r: u64, ty: NodeOutputType) -> Result<bool> {
    Ok(match op {
        IntCmpOp::Equal      => l == r,
        IntCmpOp::Less       => l < r,
        IntCmpOp::LessEqual  => l <= r,
        IntCmpOp::Sless      => {
            ty.get_signed_int(l).ok_or(Error::ExpectedIntegerType(ty))?
                < ty.get_signed_int(r).ok_or(Error::ExpectedIntegerType(ty))?
        }
        IntCmpOp::SlessEqual => {
            ty.get_signed_int(l).ok_or(Error::ExpectedIntegerType(ty))?
                <= ty.get_signed_int(r).ok_or(Error::ExpectedIntegerType(ty))?
        }
        IntCmpOp::Carry => {
            // Carry = unsigned addition overflows the type.
            let max = ty.get_unsigned_int(u64::MAX).ok_or(Error::ExpectedIntegerType(ty))? as u128;
            (l as u128 + r as u128) > max
        }
        IntCmpOp::Borrow => {
            // Borrow = l < r (unsigned subtraction borrows).
            l < r
        }
        IntCmpOp::Scarry => {
            // Signed overflow of l + r.
            let sl = ty.get_signed_int(l).ok_or(Error::ExpectedIntegerType(ty))? as i128;
            let sr = ty.get_signed_int(r).ok_or(Error::ExpectedIntegerType(ty))? as i128;
            let result = sl + sr;
            let bits = ty.bit_width() as u32;
            let min_val = -(1i128 << (bits - 1));
            let max_val = (1i128 << (bits - 1)) - 1;
            result < min_val || result > max_val
        }
        IntCmpOp::Sborrow => {
            // Signed overflow of l - r.
            let sl = ty.get_signed_int(l).ok_or(Error::ExpectedIntegerType(ty))? as i128;
            let sr = ty.get_signed_int(r).ok_or(Error::ExpectedIntegerType(ty))? as i128;
            let result = sl - sr;
            let bits = ty.bit_width() as u32;
            let min_val = -(1i128 << (bits - 1));
            let max_val = (1i128 << (bits - 1)) - 1;
            result < min_val || result > max_val
        }
    })
}

// ── per-node folding ──────────────────────────────────────────────────────────

fn try_fold_int_binary(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::IntBinaryOp(op) = kind else { return Ok(OptimizationResult::NoChange); };

    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [lhs, rhs] = fg.graph.node_inputs_exact::<2>(node_id)?;

    let lhs_c = int_const_val(fg, lhs);
    let rhs_c = int_const_val(fg, rhs);
    let all_ones = ty.get_unsigned_int(u64::MAX).ok_or(Error::ExpectedIntegerType(ty))?;

    // Full constant evaluation when both operands are known.
    if let (Some(l), Some(r)) = (lhs_c, rhs_c) {
        if let Some(folded) = eval_int_binary(op, l, r, ty) {
            let new_out = make_int_const(fg, folded, ty)?;
            return replace_all_uses(fg, out, new_out);
        }
    }

    // Algebraic identities and absorbing elements.
    match op {
        IntBinaryOp::Add => {
            if rhs_c == Some(0) {
                return replace_all_uses(fg, out, lhs); // x + 0 → x
            }
            if lhs_c == Some(0) {
                return replace_all_uses(fg, out, rhs); // 0 + x → x
            }
        }
        IntBinaryOp::Sub => {
            if rhs_c == Some(0) {
                return replace_all_uses(fg, out, lhs); // x - 0 → x
            }
            if lhs == rhs {
                let zero = make_int_const(fg, 0, ty)?;
                return replace_all_uses(fg, out, zero); // x - x → 0
            }
        }
        IntBinaryOp::Mul => {
            if lhs_c == Some(0) || rhs_c == Some(0) {
                let zero = make_int_const(fg, 0, ty)?;
                return replace_all_uses(fg, out, zero); // x * 0 → 0
            }
            if lhs_c == Some(1) {
                return replace_all_uses(fg, out, rhs); // 1 * x → x
            }
            if rhs_c == Some(1) {
                return replace_all_uses(fg, out, lhs); // x * 1 → x
            }
        }
        IntBinaryOp::And => {
            // Absorbing: x & 0 → 0
            if lhs_c == Some(0) || rhs_c == Some(0) {
                let zero = make_int_const(fg, 0, ty)?;
                return replace_all_uses(fg, out, zero);
            }
            // Identity: x & all_ones → x
            if lhs_c == Some(all_ones) {
                return replace_all_uses(fg, out, rhs);
            }
            if rhs_c == Some(all_ones) {
                return replace_all_uses(fg, out, lhs);
            }
            // Idempotent: x & x → x
            if lhs == rhs {
                return replace_all_uses(fg, out, lhs);
            }
            // (a & C1) & C2 → a & (C1 & C2)
            if let Some(c2) = rhs_c {
                let lhs_node = fg.graph.get_node_from_output(lhs);
                let lhs_kind = *fg.graph.node_kind(lhs_node);
                if let NodeKind::IntBinaryOp(IntBinaryOp::And) = lhs_kind {
                    let [inner_lhs, inner_rhs] = fg.graph.node_inputs_exact::<2>(lhs_node)?;
                    // Check inner rhs first, then inner lhs.
                    let inner_rhs_c = int_const_val(fg, inner_rhs);
                    if let Some(c1) = inner_rhs_c {
                        let merged = make_int_const(fg, c1 & c2, ty)?;
                        let new_node = fg.graph.create_node(
                            NodeKind::IntBinaryOp(IntBinaryOp::And),
                            [inner_lhs, merged],
                            [NodeOutputKind::OutputType(ty)],
                        );
                        let new_out = fg.graph.node_outputs_exact::<1>(new_node)?[0];
                        return replace_all_uses(fg, out, new_out);
                    }
                    let inner_lhs_c = int_const_val(fg, inner_lhs);
                    if let Some(c1) = inner_lhs_c {
                        let merged = make_int_const(fg, c1 & c2, ty)?;
                        let new_node = fg.graph.create_node(
                            NodeKind::IntBinaryOp(IntBinaryOp::And),
                            [inner_rhs, merged],
                            [NodeOutputKind::OutputType(ty)],
                        );
                        let new_out = fg.graph.node_outputs_exact::<1>(new_node)?[0];
                        return replace_all_uses(fg, out, new_out);
                    }
                }
            }
            // C1 & (a & C2) → a & (C1 & C2) (symmetric case)
            if let Some(c1) = lhs_c {
                let rhs_node = fg.graph.get_node_from_output(rhs);
                let rhs_kind = *fg.graph.node_kind(rhs_node);
                if let NodeKind::IntBinaryOp(IntBinaryOp::And) = rhs_kind {
                    let [inner_lhs, inner_rhs] = fg.graph.node_inputs_exact::<2>(rhs_node)?;
                    let inner_rhs_c = int_const_val(fg, inner_rhs);
                    if let Some(c2) = inner_rhs_c {
                        let merged = make_int_const(fg, c1 & c2, ty)?;
                        let new_node = fg.graph.create_node(
                            NodeKind::IntBinaryOp(IntBinaryOp::And),
                            [inner_lhs, merged],
                            [NodeOutputKind::OutputType(ty)],
                        );
                        let new_out = fg.graph.node_outputs_exact::<1>(new_node)?[0];
                        return replace_all_uses(fg, out, new_out);
                    }
                    let inner_lhs_c = int_const_val(fg, inner_lhs);
                    if let Some(c2) = inner_lhs_c {
                        let merged = make_int_const(fg, c1 & c2, ty)?;
                        let new_node = fg.graph.create_node(
                            NodeKind::IntBinaryOp(IntBinaryOp::And),
                            [inner_rhs, merged],
                            [NodeOutputKind::OutputType(ty)],
                        );
                        let new_out = fg.graph.node_outputs_exact::<1>(new_node)?[0];
                        return replace_all_uses(fg, out, new_out);
                    }
                }
            }
        }
        IntBinaryOp::Or => {
            if lhs_c == Some(0) {
                return replace_all_uses(fg, out, rhs); // 0 | x → x
            }
            if rhs_c == Some(0) {
                return replace_all_uses(fg, out, lhs); // x | 0 → x
            }
            if lhs == rhs {
                return replace_all_uses(fg, out, lhs); // x | x → x
            }
        }
        IntBinaryOp::Xor => {
            if rhs_c == Some(0) {
                return replace_all_uses(fg, out, lhs); // x ^ 0 → x
            }
            if lhs_c == Some(0) {
                return replace_all_uses(fg, out, rhs); // 0 ^ x → x
            }
            if lhs == rhs {
                let zero = make_int_const(fg, 0, ty)?;
                return replace_all_uses(fg, out, zero); // x ^ x → 0
            }
        }
        IntBinaryOp::ShiftLeft | IntBinaryOp::ShiftRight | IntBinaryOp::SShiftRight => {
            if rhs_c == Some(0) {
                return replace_all_uses(fg, out, lhs); // x << 0 → x
            }
        }
        _ => {}
    }

    Ok(OptimizationResult::NoChange)
}

fn try_fold_int_unary(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::IntUnaryOp(op) = kind else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = int_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };

    let raw = match op {
        IntUnaryOp::Neg => v.wrapping_neg(),
        IntUnaryOp::Not => !v,
    };
    let Some(folded) = ty.get_unsigned_int(raw) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_int_const(fg, folded, ty)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_int_cmp(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::IntCmpOp(op) = kind else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [lhs, rhs] = fg.graph.node_inputs_exact::<2>(node_id)?;
    let lhs_kind = fg.graph.output_kind(lhs);
    let input_ty = lhs_kind.as_value().ok_or(Error::ExpectedValueOutput(lhs_kind))?;
    let Some(l) = int_const_val(fg, lhs) else { return Ok(OptimizationResult::NoChange); };
    let Some(r) = int_const_val(fg, rhs) else { return Ok(OptimizationResult::NoChange); };

    let result = eval_int_cmp(op, l, r, input_ty)?;
    let new_out = make_bool_const(fg, result)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_bool_binary(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::BoolBinaryOp(op) = kind else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [lhs, rhs] = fg.graph.node_inputs_exact::<2>(node_id)?;
    let Some(l) = bool_const_val(fg, lhs) else { return Ok(OptimizationResult::NoChange); };
    let Some(r) = bool_const_val(fg, rhs) else { return Ok(OptimizationResult::NoChange); };

    let result = match op {
        BoolBinaryOp::And => l && r,
        BoolBinaryOp::Or  => l || r,
        BoolBinaryOp::Xor => l ^ r,
    };
    let new_out = make_bool_const(fg, result)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_bool_unary(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::BoolUnaryOp(BoolUnaryOp::Neg) = kind else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = bool_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_bool_const(fg, !v)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_truncate(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::Truncate = kind else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let target_ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = int_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };
    let Some(folded) = target_ty.get_unsigned_int(v) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_int_const(fg, folded, target_ty)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_extend(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::Extend(op) = kind else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let target_ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let input_kind = fg.graph.output_kind(input);
    let input_ty = input_kind.as_value().ok_or(Error::ExpectedValueOutput(input_kind))?;
    let Some(v) = int_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };

    let folded = match op {
        ExtendOp::ZeroExtend => v, // already an unsigned value, just reinterpret at wider type
        ExtendOp::SignExtend  => input_ty.get_signed_int(v).ok_or(Error::ExpectedIntegerType(input_ty))? as u64,
    };
    let Some(masked) = target_ty.get_unsigned_int(folded) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_int_const(fg, masked, target_ty)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_cast_to_bool(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::CastToBool = kind else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = int_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_bool_const(fg, v != 0)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_cast_to_int(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::CastToInt = kind else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let target_ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = bool_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_int_const(fg, v as u64, target_ty)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_popcount(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::Popcount = *fg.graph.node_kind(node_id) else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = int_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };
    let input_kind = fg.graph.output_kind(input);
    let input_ty = input_kind.as_value().ok_or(Error::ExpectedValueOutput(input_kind))?;
    let masked = input_ty.get_unsigned_int(v).ok_or(Error::ExpectedIntegerType(input_ty))?;
    let result = masked.count_ones() as u64;
    let new_out = make_int_const(fg, result, ty)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_lzcount(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::Lzcount = *fg.graph.node_kind(node_id) else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = int_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };
    let input_kind = fg.graph.output_kind(input);
    let input_ty = input_kind.as_value().ok_or(Error::ExpectedValueOutput(input_kind))?;
    let masked = input_ty.get_unsigned_int(v).ok_or(Error::ExpectedIntegerType(input_ty))?;
    let bits = input_ty.bit_width() as u32;
    // Shift into the top of a u64 then count leading zeros within the type width.
    let result = (masked << (64 - bits)).leading_zeros() as u64;
    let new_out = make_int_const(fg, result, ty)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_piece(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::Piece = *fg.graph.node_kind(node_id) else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [hi, lo] = fg.graph.node_inputs_exact::<2>(node_id)?;
    let Some(hi_v) = int_const_val(fg, hi) else { return Ok(OptimizationResult::NoChange); };
    let Some(lo_v) = int_const_val(fg, lo) else { return Ok(OptimizationResult::NoChange); };
    let lo_kind = fg.graph.output_kind(lo);
    let lo_ty = lo_kind.as_value().ok_or(Error::ExpectedValueOutput(lo_kind))?;
    let lo_bits = lo_ty.bit_width() as u32;
    let lo_mask = lo_ty.get_unsigned_int(u64::MAX).unwrap_or(u64::MAX);
    let result = (hi_v << lo_bits) | (lo_v & lo_mask);
    let Some(masked) = ty.get_unsigned_int(result) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_int_const(fg, masked, ty)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_extract(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::Extract { lsb, len } = *fg.graph.node_kind(node_id) else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = int_const_val(fg, input) else { return Ok(OptimizationResult::NoChange); };
    let mask = if len >= 64 { u64::MAX } else { (1u64 << len) - 1 };
    let result = (v >> lsb) & mask;
    let Some(masked) = ty.get_unsigned_int(result) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_int_const(fg, masked, ty)?;
    replace_all_uses(fg, out, new_out)
}

fn try_fold_insert(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::Insert { lsb, len } = *fg.graph.node_kind(node_id) else { return Ok(OptimizationResult::NoChange); };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [dest, src] = fg.graph.node_inputs_exact::<2>(node_id)?;
    let Some(dest_v) = int_const_val(fg, dest) else { return Ok(OptimizationResult::NoChange); };
    let Some(src_v)  = int_const_val(fg, src)  else { return Ok(OptimizationResult::NoChange); };
    let mask = if len >= 64 { u64::MAX } else { (1u64 << len) - 1 };
    let result = (dest_v & !(mask << lsb)) | ((src_v & mask) << lsb);
    let Some(masked) = ty.get_unsigned_int(result) else { return Ok(OptimizationResult::NoChange); };
    let new_out = make_int_const(fg, masked, ty)?;
    replace_all_uses(fg, out, new_out)
}

// ── float constant evaluation ─────────────────────────────────────────────────

/// Evaluates a float binary op on raw bit patterns.  Returns the result as a
/// raw bit pattern, or `None` for undefined operations (should not occur in
/// IEEE 754, but we keep the Option for consistency with the int version).
fn eval_float_binary(op: FloatBinaryOp, bits_l: u64, bits_r: u64, ty: NodeOutputType) -> Option<u64> {
    match ty {
        NodeOutputType::F32 => {
            let l = f32::from_bits(bits_l as u32);
            let r = f32::from_bits(bits_r as u32);
            let result = match op {
                FloatBinaryOp::Add => l + r,
                FloatBinaryOp::Sub => l - r,
                FloatBinaryOp::Mul => l * r,
                FloatBinaryOp::Div => l / r,
            };
            Some(result.to_bits() as u64)
        }
        NodeOutputType::F64 => {
            let l = f64::from_bits(bits_l);
            let r = f64::from_bits(bits_r);
            let result = match op {
                FloatBinaryOp::Add => l + r,
                FloatBinaryOp::Sub => l - r,
                FloatBinaryOp::Mul => l * r,
                FloatBinaryOp::Div => l / r,
            };
            Some(result.to_bits())
        }
        _ => None,
    }
}

/// Evaluates a float comparison on raw bit patterns.
fn eval_float_cmp(op: FloatCmpOp, bits_l: u64, bits_r: u64, ty: NodeOutputType) -> Option<bool> {
    match ty {
        NodeOutputType::F32 => {
            let l = f32::from_bits(bits_l as u32);
            let r = f32::from_bits(bits_r as u32);
            Some(match op {
                FloatCmpOp::Equal    => l == r,
                FloatCmpOp::NotEqual => l != r,
                FloatCmpOp::Less     => l < r,
                FloatCmpOp::LessEqual=> l <= r,
            })
        }
        NodeOutputType::F64 => {
            let l = f64::from_bits(bits_l);
            let r = f64::from_bits(bits_r);
            Some(match op {
                FloatCmpOp::Equal    => l == r,
                FloatCmpOp::NotEqual => l != r,
                FloatCmpOp::Less     => l < r,
                FloatCmpOp::LessEqual=> l <= r,
            })
        }
        _ => None,
    }
}

/// Evaluates a float unary op on a raw bit pattern.
fn eval_float_unary(op: FloatUnaryOp, bits: u64, ty: NodeOutputType) -> Option<u64> {
    match ty {
        NodeOutputType::F32 => {
            let v = f32::from_bits(bits as u32);
            let result = match op {
                FloatUnaryOp::Neg   => -v,
                FloatUnaryOp::Abs   => v.abs(),
                FloatUnaryOp::Sqrt  => v.sqrt(),
                FloatUnaryOp::Ceil  => v.ceil(),
                FloatUnaryOp::Floor => v.floor(),
                FloatUnaryOp::Round => v.round(),
            };
            Some(result.to_bits() as u64)
        }
        NodeOutputType::F64 => {
            let v = f64::from_bits(bits);
            let result = match op {
                FloatUnaryOp::Neg   => -v,
                FloatUnaryOp::Abs   => v.abs(),
                FloatUnaryOp::Sqrt  => v.sqrt(),
                FloatUnaryOp::Ceil  => v.ceil(),
                FloatUnaryOp::Floor => v.floor(),
                FloatUnaryOp::Round => v.round(),
            };
            Some(result.to_bits())
        }
        _ => None,
    }
}

// ── per-node float folding ────────────────────────────────────────────────────

fn try_fold_float_binary(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::FloatBinaryOp(op) = kind else { return Ok(OptimizationResult::NoChange); };

    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [lhs, rhs] = fg.graph.node_inputs_exact::<2>(node_id)?;

    let lhs_c = float_const_val(fg, lhs);
    let rhs_c = float_const_val(fg, rhs);

    // Full constant evaluation when both operands are known.
    if let (Some(l), Some(r)) = (lhs_c, rhs_c) {
        if let Some(folded) = eval_float_binary(op, l, r, ty) {
            let new_out = make_float_const(fg, folded, ty)?;
            return replace_all_uses(fg, out, new_out);
        }
    }

    // Safe algebraic identities (no -0.0 or NaN corner cases).
    match op {
        FloatBinaryOp::Mul => {
            // x * 1.0 → x   (valid for all IEEE 754 values including NaN/inf)
            if let Some(r) = rhs_c {
                let is_one = match ty {
                    NodeOutputType::F32 => f32::from_bits(r as u32) == 1.0,
                    NodeOutputType::F64 => f64::from_bits(r) == 1.0,
                    _ => false,
                };
                if is_one { return replace_all_uses(fg, out, lhs); }
            }
            if let Some(l) = lhs_c {
                let is_one = match ty {
                    NodeOutputType::F32 => f32::from_bits(l as u32) == 1.0,
                    NodeOutputType::F64 => f64::from_bits(l) == 1.0,
                    _ => false,
                };
                if is_one { return replace_all_uses(fg, out, rhs); }
            }
        }
        FloatBinaryOp::Div => {
            // x / 1.0 → x
            if let Some(r) = rhs_c {
                let is_one = match ty {
                    NodeOutputType::F32 => f32::from_bits(r as u32) == 1.0,
                    NodeOutputType::F64 => f64::from_bits(r) == 1.0,
                    _ => false,
                };
                if is_one { return replace_all_uses(fg, out, lhs); }
            }
        }
        _ => {}
    }

    Ok(OptimizationResult::NoChange)
}

fn try_fold_float_unary(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::FloatUnaryOp(op) = kind else { return Ok(OptimizationResult::NoChange); };

    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value().ok_or(Error::ExpectedValueOutput(out_kind))?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;

    if let Some(bits) = float_const_val(fg, input) {
        if let Some(folded) = eval_float_unary(op, bits, ty) {
            let new_out = make_float_const(fg, folded, ty)?;
            return replace_all_uses(fg, out, new_out);
        }
    }
    Ok(OptimizationResult::NoChange)
}

fn try_fold_float_cmp(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    let NodeKind::FloatCmpOp(op) = kind else { return Ok(OptimizationResult::NoChange); };

    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [lhs, rhs] = fg.graph.node_inputs_exact::<2>(node_id)?;

    // Determine the type from the inputs, not the (Bool) output.
    let lhs_out_kind = fg.graph.output_kind(lhs);
    let input_ty = lhs_out_kind.as_value().ok_or(Error::ExpectedValueOutput(lhs_out_kind))?;

    if let (Some(l), Some(r)) = (float_const_val(fg, lhs), float_const_val(fg, rhs)) {
        if let Some(result) = eval_float_cmp(op, l, r, input_ty) {
            let new_out = make_bool_const(fg, result)?;
            return replace_all_uses(fg, out, new_out);
        }
    }
    Ok(OptimizationResult::NoChange)
}

fn try_fold_float_is_nan(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::FloatIsNan = *fg.graph.node_kind(node_id) else { return Ok(OptimizationResult::NoChange); };

    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;

    let input_kind = fg.graph.output_kind(input);
    let input_ty = input_kind.as_value().ok_or(Error::ExpectedValueOutput(input_kind))?;

    if let Some(bits) = float_const_val(fg, input) {
        let is_nan = match input_ty {
            NodeOutputType::F32 => f32::from_bits(bits as u32).is_nan(),
            NodeOutputType::F64 => f64::from_bits(bits).is_nan(),
            _ => return Ok(OptimizationResult::NoChange),
        };
        let new_out = make_bool_const(fg, is_nan)?;
        return replace_all_uses(fg, out, new_out);
    }
    Ok(OptimizationResult::NoChange)
}

/// Folds `IntBitsToFloat(FloatBitsToInt(x)) → x` and
/// `FloatBitsToInt(IntBitsToFloat(x)) → x`.
fn try_fold_bitcast_identity(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let kind = *fg.graph.node_kind(node_id);
    match kind {
        NodeKind::IntBitsToFloat => {
            let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
            let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
            let inner = fg.graph.get_node_from_output(input);
            if matches!(*fg.graph.node_kind(inner), NodeKind::FloatBitsToInt) {
                let [inner_input] = fg.graph.node_inputs_exact::<1>(inner)?;
                return replace_all_uses(fg, out, inner_input);
            }
        }
        NodeKind::FloatBitsToInt => {
            let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
            let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
            let inner = fg.graph.get_node_from_output(input);
            if matches!(*fg.graph.node_kind(inner), NodeKind::IntBitsToFloat) {
                let [inner_input] = fg.graph.node_inputs_exact::<1>(inner)?;
                return replace_all_uses(fg, out, inner_input);
            }
        }
        _ => {}
    }
    Ok(OptimizationResult::NoChange)
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Folds constant expressions and applies algebraic identities.
///
/// Handles full constant evaluation for all arithmetic, comparison, boolean,
/// truncation, and extension operations.  Also applies identities such as
/// `x + 0 → x`, `x ^ x → 0`, and nested AND-mask merging `(a & C1) & C2 →
/// a & (C1 & C2)`.
/// Lowers a `CastToFloat` node to the appropriate specific form based on the
/// actual input type:
///
/// - Input is the same float type as output → eliminated (identity).
/// - Input is a different float type → lowered to `FloatToFloat`.
/// - Input is an integer `IntConst(v)` → immediately constant-folded to `FloatConst(v)`.
/// - Input is any other integer type → lowered to `IntBitsToFloat`.
fn try_lower_cast_to_float(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    if !matches!(*fg.graph.node_kind(node_id), NodeKind::CastToFloat) {
        return Ok(OptimizationResult::NoChange);
    }

    let [out]   = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;

    let out_kind  = fg.graph.output_kind(out);
    let in_kind   = fg.graph.output_kind(input);
    let out_ty    = out_kind .as_value().ok_or(crate::error::Error::ExpectedValueOutput(out_kind))?;
    let in_ty     = in_kind  .as_value().ok_or(crate::error::Error::ExpectedValueOutput(in_kind))?;

    // 1. Identity: input already has the target float type.
    if in_ty == out_ty {
        return replace_all_uses(fg, out, input);
    }

    // 2. Float→float precision change.
    if in_ty.is_float() {
        let new_out = make_float_to_float_node(fg, input, out_ty)?;
        return replace_all_uses(fg, out, new_out);
    }

    // Input is integer from here.

    // 3. Integer constant → float constant (same bits).
    if let Some(bits) = int_const_val(fg, input) {
        let new_out = make_float_const(fg, bits, out_ty)?;
        return replace_all_uses(fg, out, new_out);
    }

    // 4. Non-constant integer → explicit IntBitsToFloat.
    let new_out = make_int_bits_to_float_node(fg, input, out_ty)?;
    replace_all_uses(fg, out, new_out)
}

pub struct ConstantFold;

impl Optimizer for ConstantFold {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let nodes: Vec<_> = function.preorder().collect();
        let mut result = OptimizationResult::NoChange;
        for node_id in nodes {
            result |= try_fold_int_binary(function, node_id)?;
            result |= try_fold_int_unary(function, node_id)?;
            result |= try_fold_int_cmp(function, node_id)?;
            result |= try_fold_bool_binary(function, node_id)?;
            result |= try_fold_bool_unary(function, node_id)?;
            result |= try_fold_truncate(function, node_id)?;
            result |= try_fold_extend(function, node_id)?;
            result |= try_fold_cast_to_bool(function, node_id)?;
            result |= try_fold_cast_to_int(function, node_id)?;
            result |= try_fold_popcount(function, node_id)?;
            result |= try_fold_lzcount(function, node_id)?;
            result |= try_fold_piece(function, node_id)?;
            result |= try_fold_extract(function, node_id)?;
            result |= try_fold_insert(function, node_id)?;
            result |= try_fold_float_binary(function, node_id)?;
            result |= try_fold_float_unary(function, node_id)?;
            result |= try_fold_float_cmp(function, node_id)?;
            result |= try_fold_float_is_nan(function, node_id)?;
            result |= try_fold_bitcast_identity(function, node_id)?;
            result |= try_lower_cast_to_float(function, node_id)?;
        }
        Ok(result)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ir::{FunctionBuilder, IntBinaryOp, BoolBinaryOp, IntCmpOp, BoolUnaryOp, FloatBinaryOp, FloatUnaryOp, FloatCmpOp};
    use ir::node::{NodeKind, NodeOutputType};

    /// Builds a minimal single-region function whose return value is produced
    /// by `f`.  All nodes built by `f` are reachable from the entry.
    fn make_fn<F>(f: F) -> Result<ir::BuiltFunctionGraph>
    where
        F: FnOnce(&mut FunctionBuilder) -> Result<ir::Value>,
    {
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let val = f(&mut b)?;
        b.build_return(Some(val), &[])?;
        Ok(b.build())
    }

    /// Returns the output id that the Return node receives as its value
    /// argument (input[1]: input[0] is the control edge).
    fn return_value(fg: &ir::BuiltFunctionGraph) -> ir::Value {
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .expect("no Return node");
        fg.graph.node_inputs(ret)[1]
    }

    /// Returns the `NodeKind` of the node that produces the return value.
    fn return_kind(fg: &ir::BuiltFunctionGraph) -> NodeKind {
        let val = return_value(fg);
        let node = fg.graph.get_node_from_output(val);
        *fg.graph.node_kind(node)
    }

    // ── integer binary folding ────────────────────────────────────────────────

    #[test]
    fn fold_int_add_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let c3 = b.build_int_const(3, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(7));
        Ok(())
    }

    #[test]
    fn fold_int_and_zero() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xFF, NodeOutputType::U64);
            let zero = b.build_int_const(0, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(x, zero, IntBinaryOp::And, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0));
        Ok(())
    }

    #[test]
    fn fold_int_xor_self() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xAB, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(x, x, IntBinaryOp::Xor, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0));
        Ok(())
    }

    #[test]
    fn fold_int_sub_self() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xAB, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(x, x, IntBinaryOp::Sub, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0));
        Ok(())
    }

    #[test]
    fn fold_add_zero_identity() -> Result<()> {
        // x + 0 → x  (x is non-const)
        let mut fg = make_fn(|b| {
            let c1 = b.build_int_const(1, NodeOutputType::U64);
            let c2 = b.build_int_const(2, NodeOutputType::U64);
            let x = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
            let zero = b.build_int_const(0, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(x, zero, IntBinaryOp::Add, NodeOutputType::U64)?)
        })?;
        // After at least one fold pass x+0 should collapse to x, then x folds too.
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_eq!(return_kind(&fg), NodeKind::IntConst(3));
        Ok(())
    }

    #[test]
    fn fold_mul_by_one() -> Result<()> {
        let mut fg = make_fn(|b| {
            let c5 = b.build_int_const(5, NodeOutputType::U64);
            let one = b.build_int_const(1, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(c5, one, IntBinaryOp::Mul, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(5));
        Ok(())
    }

    /// `(x & 4) & 7`  — bit 2 is the only bit reachable by both masks, so the
    /// merged constant is `4 & 7 = 4`.
    #[test]
    fn fold_and_and_masks() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xFF, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let c7 = b.build_int_const(7, NodeOutputType::U64);
            let inner = b.build_int_binary_operation(x, c4, IntBinaryOp::And, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(inner, c7, IntBinaryOp::And, NodeOutputType::U64)?)
        })?;
        // Run to convergence (both-const fold + mask-merge may each fire once).
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        // 0xFF & 4 = 4, 4 & 7 = 4.
        assert_eq!(return_kind(&fg), NodeKind::IntConst(4));
        Ok(())
    }

    // ── truncate / extend ─────────────────────────────────────────────────────

    #[test]
    fn fold_truncate_const() -> Result<()> {
        // The builder's truncate_if_needed already constant-folds inline, so
        // by the time the graph is built there is no Truncate node — just an
        // IntConst with the (possibly unmasked) raw value.
        // Verify that the return value is semantically 0x00 (0xFF00 & 0xFF).
        let fg = make_fn(|b| {
            let wide = b.build_int_const(0xFF00, NodeOutputType::U16);
            Ok(b.truncate_if_needed(wide, NodeOutputType::U8)?)
        })?;
        let val = return_value(&fg);
        // Use int_const_val which masks to the declared type.
        let semantic = crate::utils::int_const_val(&fg, val);
        assert_eq!(semantic, Some(0), "0xFF00 truncated to U8 should be 0");
        // No Truncate nodes should exist.
        assert!(
            !fg.all_node_ids().any(|n| matches!(fg.graph.node_kind(n), NodeKind::Truncate)),
            "builder should have folded the truncate"
        );
        Ok(())
    }

    // ── boolean folding ───────────────────────────────────────────────────────

    #[test]
    fn fold_bool_neg_const() -> Result<()> {
        let mut fg = make_fn(|b| {
            let t = b.build_boolean_const(true);
            Ok(b.build_boolean_unary_operation(t, BoolUnaryOp::Neg)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(false));
        Ok(())
    }

    #[test]
    fn fold_bool_and_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let t = b.build_boolean_const(true);
            let f = b.build_boolean_const(false);
            Ok(b.build_boolean_operation(t, f, BoolBinaryOp::And)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(false));
        Ok(())
    }

    // ── no-fold edge cases ────────────────────────────────────────────────────

    #[test]
    fn no_fold_div_by_zero() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(10, NodeOutputType::U64);
            let zero = b.build_int_const(0, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(x, zero, IntBinaryOp::Div, NodeOutputType::U64)?)
        })?;
        // Should not fold (division by zero is undefined).
        assert!(!ConstantFold.optimize(&mut fg)?.changed());
        assert!(matches!(return_kind(&fg), NodeKind::IntBinaryOp(IntBinaryOp::Div)));
        Ok(())
    }

    #[test]
    fn fold_int_cmp_equal_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let c5 = b.build_int_const(5, NodeOutputType::U64);
            let c5b = b.build_int_const(5, NodeOutputType::U64);
            Ok(b.build_int_cmp_operation(c5, c5b, IntCmpOp::Equal, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(true));
        Ok(())
    }

    #[test]
    fn fold_int_cmp_less_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let c3 = b.build_int_const(3, NodeOutputType::U64);
            let c5 = b.build_int_const(5, NodeOutputType::U64);
            Ok(b.build_int_cmp_operation(c3, c5, IntCmpOp::Less, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(true));
        Ok(())
    }

    // ── Popcount / Lzcount / Piece / Extract / Insert ─────────────────────────

    #[test]
    fn fold_popcount_const() -> Result<()> {
        // popcount(0b10110101) = 5
        let mut fg = make_fn(|b| {
            let v = b.build_int_const(0b10110101, NodeOutputType::U8);
            Ok(b.build_popcount(v, NodeOutputType::U8)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(5));
        Ok(())
    }

    #[test]
    fn fold_popcount_zero() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_int_const(0, NodeOutputType::U64);
            Ok(b.build_popcount(v, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0));
        Ok(())
    }

    #[test]
    fn fold_lzcount_msb_set() -> Result<()> {
        // lzcount(0x80u8) = 0 (MSB is set)
        let mut fg = make_fn(|b| {
            let v = b.build_int_const(0x80, NodeOutputType::U8);
            Ok(b.build_lzcount(v, NodeOutputType::U8)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0));
        Ok(())
    }

    #[test]
    fn fold_lzcount_one() -> Result<()> {
        // lzcount(1u8) = 7 (only bit 0 set in an 8-bit value)
        let mut fg = make_fn(|b| {
            let v = b.build_int_const(1, NodeOutputType::U8);
            Ok(b.build_lzcount(v, NodeOutputType::U8)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(7));
        Ok(())
    }

    #[test]
    fn fold_piece_consts() -> Result<()> {
        // piece(0xABu8, 0xCDu8) → U16 = 0xABCD
        let mut fg = make_fn(|b| {
            let hi = b.build_int_const(0xAB, NodeOutputType::U8);
            let lo = b.build_int_const(0xCD, NodeOutputType::U8);
            Ok(b.build_piece(hi, lo, NodeOutputType::U16)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0xABCD));
        Ok(())
    }

    #[test]
    fn fold_extract_const() -> Result<()> {
        // extract(0xABCDu16, lsb=4, len=8) = (0xABCD >> 4) & 0xFF = 0xBC
        let mut fg = make_fn(|b| {
            let v = b.build_int_const(0xABCD, NodeOutputType::U16);
            Ok(b.build_extract(v, 4, 8, NodeOutputType::U8)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0xBC));
        Ok(())
    }

    #[test]
    fn fold_insert_const() -> Result<()> {
        // insert(0xFF00u16, 0x42u16, lsb=0, len=8) = 0xFF42
        let mut fg = make_fn(|b| {
            let dest = b.build_int_const(0xFF00, NodeOutputType::U16);
            let src  = b.build_int_const(0x42,   NodeOutputType::U16);
            Ok(b.build_insert(dest, src, 0, 8, NodeOutputType::U16)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0xFF42));
        Ok(())
    }

    // ── Float constant folding ────────────────────────────────────────────────

    #[test]
    fn fold_f32_add_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
            let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
            Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Add, NodeOutputType::F32)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(7.0f32.to_bits() as u64));
        Ok(())
    }

    #[test]
    fn fold_f32_mul_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
            let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
            Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Mul, NodeOutputType::F32)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(12.0f32.to_bits() as u64));
        Ok(())
    }

    #[test]
    fn fold_f32_div_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(10.0f32.to_bits() as u64, NodeOutputType::F32);
            let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
            Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Div, NodeOutputType::F32)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(2.5f32.to_bits() as u64));
        Ok(())
    }

    #[test]
    fn fold_f64_add_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(3.0f64.to_bits(), NodeOutputType::F64);
            let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Add, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(7.0f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_f64_mul_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(3.0f64.to_bits(), NodeOutputType::F64);
            let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Mul, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(12.0f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_f64_div_consts() -> Result<()> {
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(10.0f64.to_bits(), NodeOutputType::F64);
            let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Div, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(2.5f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_f32_less_true() -> Result<()> {
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
            let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
            Ok(b.build_float_cmp_op(a, c, FloatCmpOp::Less)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(true));
        Ok(())
    }

    #[test]
    fn fold_f64_equal_true() -> Result<()> {
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
            let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_cmp_op(a, c, FloatCmpOp::Equal)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(true));
        Ok(())
    }

    #[test]
    fn fold_f64_equal_nan_false() -> Result<()> {
        // NaN != NaN per IEEE 754
        let nan = f64::NAN.to_bits();
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(nan, NodeOutputType::F64);
            let c = b.build_float_const(nan, NodeOutputType::F64);
            Ok(b.build_float_cmp_op(a, c, FloatCmpOp::Equal)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(false));
        Ok(())
    }

    #[test]
    fn fold_f32_neg_const() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(2.0f32.to_bits() as u64, NodeOutputType::F32);
            Ok(b.build_float_unary_op(v, FloatUnaryOp::Neg, NodeOutputType::F32)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst((-2.0f32).to_bits() as u64));
        Ok(())
    }

    #[test]
    fn fold_f64_abs_const() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const((-3.0f64).to_bits(), NodeOutputType::F64);
            Ok(b.build_float_unary_op(v, FloatUnaryOp::Abs, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(3.0f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_f64_sqrt_const() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_unary_op(v, FloatUnaryOp::Sqrt, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(2.0f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_float_is_nan_true() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(f32::NAN.to_bits() as u64, NodeOutputType::F32);
            Ok(b.build_float_is_nan(v)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(true));
        Ok(())
    }

    #[test]
    fn fold_float_is_nan_false() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_is_nan(v)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::BoolConst(false));
        Ok(())
    }

    #[test]
    fn fold_float_mul_by_one_identity() -> Result<()> {
        let mut fg = make_fn(|b| {
            let one = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
            let x = b.build_float_const(3.14f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_binary_op(x, one, FloatBinaryOp::Mul, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(3.14f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_float_div_by_one_identity() -> Result<()> {
        let mut fg = make_fn(|b| {
            let one = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
            let x = b.build_float_const(3.14f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_binary_op(x, one, FloatBinaryOp::Div, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(3.14f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_bitcast_identity_int_bits_to_float_of_float_bits_to_int() -> Result<()> {
        // IntBitsToFloat(FloatBitsToInt(FloatAdd(1.0, 2.0)))
        // → first, FloatAdd(1.0, 2.0) folds to FloatConst(3.0)
        // → then,  IntBitsToFloat(FloatBitsToInt(FloatConst(3.0))) simplifies to FloatConst(3.0)
        //   via the bitcast-identity: replace uses of IntBitsToFloat with FloatBitsToInt's input.
        let mut fg = make_fn(|b| {
            let a   = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
            let b2  = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
            let sum = b.build_float_binary_op(a, b2, FloatBinaryOp::Add, NodeOutputType::F64)?;
            let as_int       = b.build_float_bits_to_int(sum, NodeOutputType::U64)?;
            let back_to_float = b.build_int_bits_to_float(as_int, NodeOutputType::F64)?;
            Ok(back_to_float)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        // Float binary fold: sum → FloatConst(3.0).
        // Bitcast identity fold: IntBitsToFloat(FloatBitsToInt(FloatConst(3.0))) → FloatConst(3.0).
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(3.0f64.to_bits()));
        Ok(())
    }

    // ── CastToFloat lowering tests ────────────────────────────────────────────

    #[test]
    fn cast_to_float_int_const_folds_to_float_const() -> Result<()> {
        let bits = 1.0f64.to_bits();
        let mut fg = make_fn(|b| {
            let int_val = b.build_int_const(bits, NodeOutputType::U64);
            let cast = b.build_cast_to_float(int_val, NodeOutputType::F64);
            Ok(cast)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        // CastToFloat(IntConst(bits)) → FloatConst(bits)
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(bits));
        Ok(())
    }

    #[test]
    fn cast_to_float_same_float_type_eliminates() -> Result<()> {
        let bits = 1.0f32.to_bits() as u64;
        let mut fg = make_fn(|b| {
            let float_val = b.build_float_const(bits, NodeOutputType::F32);
            let cast = b.build_cast_to_float(float_val, NodeOutputType::F32);
            Ok(cast)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        // CastToFloat(F32 → F32) → identity (FloatConst)
        assert_eq!(return_kind(&fg), NodeKind::FloatConst(bits));
        Ok(())
    }

    #[test]
    fn cast_to_float_int_non_const_lowers_to_int_bits_to_float() -> Result<()> {
        let mut fg = make_fn(|b| {
            let int_a = b.build_int_const(1, NodeOutputType::U32);
            let int_b = b.build_int_const(2, NodeOutputType::U32);
            // Non-const int (Add result).
            let sum = b.build_int_binary_operation(int_a, int_b, IntBinaryOp::Add, NodeOutputType::U32)?;
            let cast = b.build_cast_to_float(sum, NodeOutputType::F32);
            Ok(cast)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        // Should lower to IntBitsToFloat.
        assert_eq!(return_kind(&fg), NodeKind::IntBitsToFloat);
        Ok(())
    }

    #[test]
    fn cast_to_float_cross_precision_lowers_to_float_to_float() -> Result<()> {
        let mut fg = make_fn(|b| {
            let f32_val = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
            let cast = b.build_cast_to_float(f32_val, NodeOutputType::F64);
            Ok(cast)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        // F32 → F64 should lower to FloatToFloat.
        assert_eq!(return_kind(&fg), NodeKind::FloatToFloat);
        Ok(())
    }
}
