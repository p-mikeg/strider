use ir::node::{NodeId, NodeKind, NodeOutputType};
use ir::{
    BuiltFunctionGraph, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};
use crate::error::{ErrorKind, Result};
use crate::pipeline::{OptimizationResult, Optimizer};

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
        IntBinaryOp::Or => l | r,
        IntBinaryOp::Xor => l ^ r,
        IntBinaryOp::ShiftLeft => l.wrapping_shl(shift(r)),
        IntBinaryOp::ShiftRight => l.wrapping_shr(shift(r)),
        IntBinaryOp::SShiftRight => {
            let sl = ty.get_signed_int(l)?;
            (sl >> shift(r)) as u64
        }
        IntBinaryOp::Div => {
            if r == 0 {
                return None;
            }
            l / r
        }
        IntBinaryOp::Sdiv => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            if sl == i64::MIN && sr == -1 {
                return None;
            } // overflow
            (sl / sr) as u64
        }
        IntBinaryOp::Rem => {
            if r == 0 {
                return None;
            }
            l % r
        }
        IntBinaryOp::Srem => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            (sl % sr) as u64
        }
    };
    ty.get_unsigned_int(raw)
}

/// Evaluates a comparison on two constant integer values.
fn eval_int_cmp(op: IntCmpOp, l: u64, r: u64, ty: NodeOutputType) -> Result<bool> {
    Ok(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::LessEqual => l <= r,
        IntCmpOp::Sless => {
            ty.get_signed_int(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?
                < ty.get_signed_int(r)
                    .ok_or(ErrorKind::ExpectedIntegerType(ty))?
        }
        IntCmpOp::SlessEqual => {
            ty.get_signed_int(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?
                <= ty
                    .get_signed_int(r)
                    .ok_or(ErrorKind::ExpectedIntegerType(ty))?
        }
        IntCmpOp::Carry => {
            // Carry = unsigned addition overflows the type.
            let max = ty
                .get_unsigned_int(u64::MAX)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as u128;
            (l as u128 + r as u128) > max
        }
        IntCmpOp::Borrow => {
            // Borrow = l < r (unsigned subtraction borrows).
            l < r
        }
        IntCmpOp::Scarry => {
            // Signed overflow of l + r.
            let sl = ty
                .get_signed_int(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as i128;
            let sr = ty
                .get_signed_int(r)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as i128;
            let result = sl + sr;
            let bits = ty.bit_width() as u32;
            let min_val = -(1i128 << (bits - 1));
            let max_val = (1i128 << (bits - 1)) - 1;
            result < min_val || result > max_val
        }
        IntCmpOp::Sborrow => {
            // Signed overflow of l - r.
            let sl = ty
                .get_signed_int(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as i128;
            let sr = ty
                .get_signed_int(r)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as i128;
            let result = sl - sr;
            let bits = ty.bit_width() as u32;
            let min_val = -(1i128 << (bits - 1));
            let max_val = (1i128 << (bits - 1)) - 1;
            result < min_val || result > max_val
        }
    })
}

// ── per-node folding ──────────────────────────────────────────────────────────

/// Applies add/sub reassociation and AND-mask merging rules.
///
/// Rules:
/// - `(x + C1) + C2 → x + (C1 + C2)`
/// - `(x - C1) - C2 → x - (C1 + C2)`
/// - `(x + C1) - C2 → x + (C1 - C2)`
/// - `(a & C1) & C2 → a & (C1 & C2)`
/// - `((a & C1) | (b & C2)) & C3 → (a & (C1 & C3)) | (b & (C2 & C3))`
fn apply_reassoc_and_mask_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::build::{self, cap};
    use pattern::{
        BoxedRule, IntVar, Var, add, and, any_int_const, apply_rules_in_order, boxed_rule,
        int_const_with, or, rewrite_rule, sub, var,
    };

    // (x + C1) + C2 → x + (C1 + C2)
    let (x, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_add_add = boxed_rule(rewrite_rule(
        add(add(var(x), any_int_const(c1)), any_int_const(c2)),
        build::add(cap(x), int_const_with!([c1, c2] => c1.wrapping_add(c2))),
    ));

    // (x - C1) - C2 → x - (C1 + C2)
    let (x, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_sub_sub = boxed_rule(rewrite_rule(
        sub(sub(var(x), any_int_const(c1)), any_int_const(c2)),
        build::sub(cap(x), int_const_with!([c1, c2] => c1.wrapping_add(c2))),
    ));

    // (x + C1) - C2 → x + (C1 - C2)
    let (x, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_add_sub = boxed_rule(rewrite_rule(
        sub(add(var(x), any_int_const(c1)), any_int_const(c2)),
        build::add(cap(x), int_const_with!([c1, c2] => c1.wrapping_sub(c2))),
    ));

    // (x - C1) + C2 → x + (C2 - C1)
    let (x, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_sub_add = boxed_rule(rewrite_rule(
        add(sub(var(x), any_int_const(c1)), any_int_const(c2)),
        build::add(cap(x), int_const_with!([c1, c2] => c2.wrapping_sub(c1))),
    ));

    // (a & C1) & C2 → a & (C1 & C2)
    let (a, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_and_merge = boxed_rule(rewrite_rule(
        and(and(var(a), any_int_const(c1)), any_int_const(c2)),
        build::and(cap(a), int_const_with!([c1, c2] => c1 & c2)),
    ));

    // ((a & C1) | (b & C2)) & C3 → (a & (C1 & C3)) | (b & (C2 & C3))
    let (a, b) = (Var::new(), Var::new());
    let (c1, c2, c3) = (IntVar::new(), IntVar::new(), IntVar::new());
    let rule_and_dist = boxed_rule(rewrite_rule(
        and(
            or(and(var(a), any_int_const(c1)), and(var(b), any_int_const(c2))),
            any_int_const(c3),
        ),
        build::or(
            build::and(cap(a), int_const_with!([c1, c3] => c1 & c3)),
            build::and(cap(b), int_const_with!([c2, c3] => c2 & c3)),
        ),
    ));

    let rules: Vec<BoxedRule> = vec![
        rule_add_add,
        rule_sub_sub,
        rule_add_sub,
        rule_sub_add,
        rule_and_merge,
        rule_and_dist,
    ];
    let changed = apply_rules_in_order(rules)(fg, node)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Applies bitcast identity rules:
/// - `IntBitsToFloat(FloatBitsToInt(x)) → x`
/// - `FloatBitsToInt(IntBitsToFloat(x)) → x`
fn apply_bitcast_extend_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::build::cap;
    use pattern::{
        BoxedRule, Var, apply_rules_in_order, boxed_rule, float_bits_to_int, int_bits_to_float,
        rewrite_rule, var,
    };

    // IntBitsToFloat(FloatBitsToInt(x)) → x
    let x = Var::new();
    let rule_int_float = boxed_rule(rewrite_rule(
        int_bits_to_float(float_bits_to_int(var(x))),
        cap(x),
    ));

    // FloatBitsToInt(IntBitsToFloat(x)) → x
    let x = Var::new();
    let rule_float_int = boxed_rule(rewrite_rule(
        float_bits_to_int(int_bits_to_float(var(x))),
        cap(x),
    ));

    let rules: Vec<BoxedRule> = vec![rule_int_float, rule_float_int];
    let changed = apply_rules_in_order(rules)(fg, node)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Applies single-operand algebraic identities to integer binary operations.
///
/// Rules ported from hand-written arms:
/// - `x + 0 → x`, `x - 0 → x`, `x - x → 0`
/// - `x ^ x → 0`, `x ^ 0 → x`
/// - `x * 0 → 0`, `x * 1 → x`
/// - `x & 0 → 0`, `x & x → x`, `x & all_ones → x`
/// - `x | 0 → x`, `x | x → x`
/// - `x << 0 → x`, `x >> 0 → x`, `x >>> 0 → x`
fn apply_identity_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::build::{cap, int_const_lit};
    use pattern::{
        BoxedRule, Var, add, and, apply_rules_in_order, boxed_rule, int_const, mul, or,
        rewrite_rule, shl, shr, sshr, sub, var, xor,
    };

    let x = Var::new();
    let rules: Vec<BoxedRule> = vec![
        // x + 0 → x  (commutative: also covers 0 + x)
        boxed_rule(rewrite_rule(add(var(x), int_const(0)), cap(x))),
        // x - 0 → x
        boxed_rule(rewrite_rule(sub(var(x), int_const(0)), cap(x))),
        // x - x → 0
        boxed_rule(rewrite_rule(sub(var(x), var(x)), int_const_lit(0))),
        // x ^ x → 0
        boxed_rule(rewrite_rule(xor(var(x), var(x)), int_const_lit(0))),
        // x ^ 0 → x  (commutative)
        boxed_rule(rewrite_rule(xor(var(x), int_const(0)), cap(x))),
        // x * 0 → 0  (commutative)
        boxed_rule(rewrite_rule(mul(var(x), int_const(0)), int_const_lit(0))),
        // x * 1 → x  (commutative)
        boxed_rule(rewrite_rule(mul(var(x), int_const(1)), cap(x))),
        // x & 0 → 0  (commutative)
        boxed_rule(rewrite_rule(and(var(x), int_const(0)), int_const_lit(0))),
        // x & x → x
        boxed_rule(rewrite_rule(and(var(x), var(x)), cap(x))),
        // x | 0 → x  (commutative)
        boxed_rule(rewrite_rule(or(var(x), int_const(0)), cap(x))),
        // x | x → x
        boxed_rule(rewrite_rule(or(var(x), var(x)), cap(x))),
        // x << 0 → x  (non-commutative — only RHS 0 is the identity)
        boxed_rule(rewrite_rule(shl(var(x), int_const(0)), cap(x))),
        // x >> 0 → x  (logical shift right)
        boxed_rule(rewrite_rule(shr(var(x), int_const(0)), cap(x))),
        // x >>> 0 → x  (arithmetic / signed shift right)
        boxed_rule(rewrite_rule(sshr(var(x), int_const(0)), cap(x))),
        // x & all_ones → x  (and commutative: all_ones & x → x)
        // The all-ones mask is type-width-dependent so we use a hand-written closure.
        // Returns pattern::Result<bool> to match BoxedRule's signature.
        Box::new(|fg: &mut BuiltFunctionGraph, node_id: NodeId| -> pattern::Result<bool> {
            let NodeKind::IntBinaryOp(IntBinaryOp::And) = *fg.graph.node_kind(node_id) else {
                return Ok(false);
            };
            let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
            let ty = fg.graph.output_kind(out).as_value_or_err()?;
            let Some(all_ones) = ty.get_unsigned_int(u64::MAX) else {
                return Ok(false);
            };
            let [lhs, rhs] = fg.graph.node_inputs_exact::<2>(node_id)?;
            let lhs_c = fg.int_const_val(lhs);
            let rhs_c = fg.int_const_val(rhs);
            if lhs_c == Some(all_ones) {
                return fg.replace_all_uses(out, rhs).map_err(pattern::Error::from);
            }
            if rhs_c == Some(all_ones) {
                return fg.replace_all_uses(out, lhs).map_err(pattern::Error::from);
            }
            Ok(false)
        }),
    ];

    let changed = apply_rules_in_order(rules)(fg, node)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Applies full constant evaluation for integer binary ops, integer unary ops,
/// integer comparisons, truncate, extend (zero/sign), popcount, lzcount,
/// cast_to_bool, and cast_to_int.
fn apply_const_eval_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::{
        BoolVar, BoxedRule, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, any_bool_const,
        any_int_const, apply_rules_in_order, bool_const_with, boxed_rule, cast_to_bool,
        cast_to_int, int_binary_any, int_cmp_any, int_const_with, int_unary_any, lzcount,
        popcount, rewrite_rule, sign_extend, truncate, zero_extend,
    };

    let rules: Vec<BoxedRule> = vec![
        // 1. IntBinaryOp(op)(IntConst(l), IntConst(r)) =>
        //        int_const(eval_int_binary(op, l, r, ty)?, ty)
        //    `eval_int_binary` returns `None` for div-by-zero / signed
        //    overflow / U128+ masking failures; the closure opts out of the
        //    rewrite in that case via `pattern::Error::skip()`.
        {
            let op = IntBinaryOpVar::new();
            let l = IntVar::new();
            let r = IntVar::new();
            boxed_rule(rewrite_rule(
                int_binary_any(op, any_int_const(l), any_int_const(r)),
                int_const_with!([op, l, r, ty] =>
                    eval_int_binary(op, l, r, ty)
                        .ok_or_else(pattern::Error::skip)?
                ),
            ))
        },
        // 2. IntUnaryOp(op)(IntConst(v)) => int_const(op(v) masked to ty, ty)
        //    Skips when masking fails (U128/U256 — not representable in u64).
        {
            let op = IntUnaryOpVar::new();
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                int_unary_any(op, any_int_const(v)),
                int_const_with!([op, v, ty] => {
                    let raw = match op {
                        IntUnaryOp::Neg => v.wrapping_neg(),
                        IntUnaryOp::Not => !v,
                    };
                    ty.get_unsigned_int(raw).ok_or_else(pattern::Error::skip)?
                }),
            ))
        },
        // 3. IntCmpOp(op)(IntConst(l), IntConst(r)) =>
        //        bool_const(eval_int_cmp(op, l, r, in_ty)?)
        //    `in_ty` = root's first-value-input type, which on an IntCmp is
        //    the LHS operand type — exactly what `eval_int_cmp` expects.
        //    `eval_int_cmp` returns `Result<bool, opt::ErrorKind>`; bridge
        //    that failure through `pattern::Error::rewrite_closure(...)`.
        {
            let op = IntCmpOpVar::new();
            let l = IntVar::new();
            let r = IntVar::new();
            boxed_rule(rewrite_rule(
                int_cmp_any(op, any_int_const(l), any_int_const(r)),
                bool_const_with!([op, l, r, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::Error::skip)?;
                    eval_int_cmp(op, l, r, input_ty)
                        .map_err(pattern::Error::rewrite_closure)?
                }),
            ))
        },
        // 4. Truncate(IntConst(v)) => int_const(v, ty)
        {
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                truncate(any_int_const(v)),
                int_const_with!([v] => v),
            ))
        },
        // 5. ZeroExtend(IntConst(v)) => int_const(v, ty)
        {
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                zero_extend(any_int_const(v)),
                int_const_with!([v] => v),
            ))
        },
        // 6. SignExtend(IntConst(v)) =>
        //        int_const(sign_extend(v, in_ty) masked to ty, ty)
        //    `in_ty` is the narrower input type; `get_signed_int` produces
        //    the sign-extended i64 value, which `get_unsigned_int` then
        //    masks to the wider output width.
        {
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                sign_extend(any_int_const(v)),
                int_const_with!([v, in_ty, ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::Error::skip)?;
                    let signed = input_ty
                        .get_signed_int(v)
                        .ok_or_else(|| {
                            pattern::Error::rewrite_closure(ErrorKind::ExpectedIntegerType(
                                input_ty,
                            ))
                        })?
                        as u64;
                    ty.get_unsigned_int(signed).ok_or_else(pattern::Error::skip)?
                }),
            ))
        },
        // 7. Popcount(IntConst(v)) =>
        //        int_const(masked(v, in_ty).count_ones(), ty)
        {
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                popcount(any_int_const(v)),
                int_const_with!([v, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::Error::skip)?;
                    let masked = input_ty
                        .get_unsigned_int(v)
                        .ok_or_else(|| {
                            pattern::Error::rewrite_closure(ErrorKind::ExpectedIntegerType(
                                input_ty,
                            ))
                        })?;
                    masked.count_ones() as u64
                }),
            ))
        },
        // 8. Lzcount(IntConst(v)) =>
        //        int_const((masked(v, in_ty) << (64 - in_ty.bit_width())).leading_zeros(), ty)
        {
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                lzcount(any_int_const(v)),
                int_const_with!([v, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::Error::skip)?;
                    let masked = input_ty
                        .get_unsigned_int(v)
                        .ok_or_else(|| {
                            pattern::Error::rewrite_closure(ErrorKind::ExpectedIntegerType(
                                input_ty,
                            ))
                        })?;
                    let bits = input_ty.bit_width() as u32;
                    (masked << (64 - bits)).leading_zeros() as u64
                }),
            ))
        },
        // 9. CastToBool(IntConst(v)) => bool_const(v != 0)
        {
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                cast_to_bool(any_int_const(v)),
                bool_const_with!([v] => v != 0),
            ))
        },
        // 10. CastToInt(BoolConst(b)) => int_const(b as u64, ty)
        {
            let b = BoolVar::new();
            boxed_rule(rewrite_rule(
                cast_to_int(any_bool_const(b)),
                int_const_with!([b] => b as u64),
            ))
        },
    ];

    let changed = apply_rules_in_order(rules)(fg, node)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Applies constant evaluation and absorbing-element rules for bool binary ops,
/// bool unary ops, and all float ops.
fn apply_bool_float_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::{
        BoolUnaryOpVar, BoolVar, BoxedRule, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
        FloatVar, any_bool_const, any_float_const, apply_rules_in_order, bool_and, bool_or,
        bool_unary_any, bool_xor, boxed_rule, float_binary_any, float_cmp_any, float_is_nan,
        float_unary_any, rewrite_rule,
    };
    use pattern::{bool_const_with, float_const_with};

    let rules: Vec<BoxedRule> = vec![
        // BAnd(BoolConst(l), BoolConst(r)) => bool_const(l && r)
        {
            let l = BoolVar::new();
            let r = BoolVar::new();
            boxed_rule(rewrite_rule(
                bool_and(any_bool_const(l), any_bool_const(r)),
                bool_const_with!([l, r] => l && r),
            ))
        },
        // BOr(BoolConst(l), BoolConst(r)) => bool_const(l || r)
        {
            let l = BoolVar::new();
            let r = BoolVar::new();
            boxed_rule(rewrite_rule(
                bool_or(any_bool_const(l), any_bool_const(r)),
                bool_const_with!([l, r] => l || r),
            ))
        },
        // BXor(BoolConst(l), BoolConst(r)) => bool_const(l ^ r)
        {
            let l = BoolVar::new();
            let r = BoolVar::new();
            boxed_rule(rewrite_rule(
                bool_xor(any_bool_const(l), any_bool_const(r)),
                bool_const_with!([l, r] => l ^ r),
            ))
        },
        // BAnd(BoolConst(false), x) => bool_const(false)  (absorbing element)
        {
            let l = BoolVar::new();
            let x = pattern::Var::new();
            boxed_rule(rewrite_rule(
                bool_and(any_bool_const(l), pattern::var(x)),
                bool_const_with!([l] => {
                    if !l { false } else {
                        return Err(pattern::Error::skip());
                    }
                }),
            ))
        },
        // BOr(BoolConst(true), x) => bool_const(true)  (absorbing element)
        {
            let l = BoolVar::new();
            let x = pattern::Var::new();
            boxed_rule(rewrite_rule(
                bool_or(any_bool_const(l), pattern::var(x)),
                bool_const_with!([l] => {
                    if l { true } else {
                        return Err(pattern::Error::skip());
                    }
                }),
            ))
        },
        // BoolUnaryOp(op)(BoolConst(v)) => bool_const(!v)
        {
            let op = BoolUnaryOpVar::new();
            let v = BoolVar::new();
            boxed_rule(rewrite_rule(
                bool_unary_any(op, any_bool_const(v)),
                bool_const_with!([op, v] => {
                    use ir::BoolUnaryOp;
                    match op {
                        BoolUnaryOp::Neg => !v,
                    }
                }),
            ))
        },
        // FloatBinaryOp(op)(FloatConst(l), FloatConst(r)) =>
        //     float_const(eval_float_binary(op, l, r, ty)?)
        {
            let op = FloatBinaryOpVar::new();
            let l = FloatVar::new();
            let r = FloatVar::new();
            boxed_rule(rewrite_rule(
                float_binary_any(op, any_float_const(l), any_float_const(r)),
                float_const_with!([op, l, r, ty] =>
                    eval_float_binary(op, l, r, ty)
                        .ok_or_else(pattern::Error::skip)?
                ),
            ))
        },
        // FloatUnaryOp(op)(FloatConst(v)) => float_const(eval_float_unary(op, v, ty)?)
        {
            let op = FloatUnaryOpVar::new();
            let v = FloatVar::new();
            boxed_rule(rewrite_rule(
                float_unary_any(op, any_float_const(v)),
                float_const_with!([op, v, ty] =>
                    eval_float_unary(op, v, ty)
                        .ok_or_else(pattern::Error::skip)?
                ),
            ))
        },
        // FloatCmpOp(op)(FloatConst(l), FloatConst(r)) =>
        //     bool_const(eval_float_cmp(op, l, r, in_ty)?)
        //   `in_ty` = root's first-value-input type (the float operand type).
        {
            let op = FloatCmpOpVar::new();
            let l = FloatVar::new();
            let r = FloatVar::new();
            boxed_rule(rewrite_rule(
                float_cmp_any(op, any_float_const(l), any_float_const(r)),
                bool_const_with!([op, l, r, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::Error::skip)?;
                    eval_float_cmp(op, l, r, input_ty)
                        .ok_or_else(pattern::Error::skip)?
                }),
            ))
        },
        // FloatIsNan(FloatConst(v)) => bool_const(v.is_nan())
        //   `in_ty` = root's first-value-input type (F32 or F64).
        {
            let v = FloatVar::new();
            boxed_rule(rewrite_rule(
                float_is_nan(any_float_const(v)),
                bool_const_with!([v, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::Error::skip)?;
                    match input_ty {
                        ir::node::NodeOutputType::F32 => {
                            f32::from_bits(v as u32).is_nan()
                        }
                        ir::node::NodeOutputType::F64 => {
                            f64::from_bits(v).is_nan()
                        }
                        _ => return Err(pattern::Error::skip()),
                    }
                }),
            ))
        },
    ];

    let changed = apply_rules_in_order(rules)(fg, node)?;
    // CastToFloat lowering is too stateful for a rule (it does graph surgery);
    // handle it separately after the rule sweep.
    let cast_changed = try_lower_cast_to_float(fg, node)?;
    Ok(OptimizationResult::from_changed(changed) | cast_changed)
}

fn try_fold_piece(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::Piece = *fg.graph.node_kind(node_id) else {
        return Ok(OptimizationResult::NoChange);
    };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value_or_err()?;
    let [hi, lo] = fg.graph.node_inputs_exact::<2>(node_id)?;
    let Some(hi_v) = fg.int_const_val( hi) else {
        return Ok(OptimizationResult::NoChange);
    };
    let Some(lo_v) = fg.int_const_val( lo) else {
        return Ok(OptimizationResult::NoChange);
    };
    let lo_kind = fg.graph.output_kind(lo);
    let lo_ty = lo_kind.as_value_or_err()?;
    let lo_bits = lo_ty.bit_width() as u32;
    let lo_mask = lo_ty.get_unsigned_int(u64::MAX).unwrap_or(u64::MAX);
    let result = (hi_v << lo_bits) | (lo_v & lo_mask);
    let Some(masked) = ty.get_unsigned_int(result) else {
        return Ok(OptimizationResult::NoChange);
    };
    let new_out = fg.make_int_const( masked, ty)?;
    Ok(OptimizationResult::from_changed(fg.replace_all_uses( out, new_out)?))
}

fn try_fold_extract(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::Extract { lsb, len } = *fg.graph.node_kind(node_id) else {
        return Ok(OptimizationResult::NoChange);
    };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value_or_err()?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
    let Some(v) = fg.int_const_val( input) else {
        return Ok(OptimizationResult::NoChange);
    };
    let mask = if len >= 64 {
        u64::MAX
    } else {
        (1u64 << len) - 1
    };
    let result = (v >> lsb) & mask;
    let Some(masked) = ty.get_unsigned_int(result) else {
        return Ok(OptimizationResult::NoChange);
    };
    let new_out = fg.make_int_const( masked, ty)?;
    Ok(OptimizationResult::from_changed(fg.replace_all_uses( out, new_out)?))
}

fn try_fold_insert(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    let NodeKind::Insert { lsb, len } = *fg.graph.node_kind(node_id) else {
        return Ok(OptimizationResult::NoChange);
    };
    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value_or_err()?;
    let [dest, src] = fg.graph.node_inputs_exact::<2>(node_id)?;
    let Some(dest_v) = fg.int_const_val( dest) else {
        return Ok(OptimizationResult::NoChange);
    };
    let Some(src_v) = fg.int_const_val( src) else {
        return Ok(OptimizationResult::NoChange);
    };
    let mask = if len >= 64 {
        u64::MAX
    } else {
        (1u64 << len) - 1
    };
    let result = (dest_v & !(mask << lsb)) | ((src_v & mask) << lsb);
    let Some(masked) = ty.get_unsigned_int(result) else {
        return Ok(OptimizationResult::NoChange);
    };
    let new_out = fg.make_int_const( masked, ty)?;
    Ok(OptimizationResult::from_changed(fg.replace_all_uses( out, new_out)?))
}

// ── float constant evaluation ─────────────────────────────────────────────────

/// Evaluates a float binary op on raw bit patterns.  Returns the result as a
/// raw bit pattern, or `None` for undefined operations (should not occur in
/// IEEE 754, but we keep the Option for consistency with the int version).
fn eval_float_binary(
    op: FloatBinaryOp,
    bits_l: u64,
    bits_r: u64,
    ty: NodeOutputType,
) -> Option<u64> {
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
                FloatCmpOp::Equal => l == r,
                FloatCmpOp::NotEqual => l != r,
                FloatCmpOp::Less => l < r,
                FloatCmpOp::LessEqual => l <= r,
            })
        }
        NodeOutputType::F64 => {
            let l = f64::from_bits(bits_l);
            let r = f64::from_bits(bits_r);
            Some(match op {
                FloatCmpOp::Equal => l == r,
                FloatCmpOp::NotEqual => l != r,
                FloatCmpOp::Less => l < r,
                FloatCmpOp::LessEqual => l <= r,
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
                FloatUnaryOp::Neg => -v,
                FloatUnaryOp::Abs => v.abs(),
                FloatUnaryOp::Sqrt => v.sqrt(),
                FloatUnaryOp::Ceil => v.ceil(),
                FloatUnaryOp::Floor => v.floor(),
                FloatUnaryOp::Round => v.round(),
            };
            Some(result.to_bits() as u64)
        }
        NodeOutputType::F64 => {
            let v = f64::from_bits(bits);
            let result = match op {
                FloatUnaryOp::Neg => -v,
                FloatUnaryOp::Abs => v.abs(),
                FloatUnaryOp::Sqrt => v.sqrt(),
                FloatUnaryOp::Ceil => v.ceil(),
                FloatUnaryOp::Floor => v.floor(),
                FloatUnaryOp::Round => v.round(),
            };
            Some(result.to_bits())
        }
        _ => None,
    }
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
fn try_lower_cast_to_float(
    fg: &mut BuiltFunctionGraph,
    node_id: NodeId,
) -> Result<OptimizationResult> {
    if !matches!(*fg.graph.node_kind(node_id), NodeKind::CastToFloat) {
        return Ok(OptimizationResult::NoChange);
    }

    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;

    let out_kind = fg.graph.output_kind(out);
    let in_kind = fg.graph.output_kind(input);
    let out_ty = out_kind.as_value_or_err()?;
    let in_ty = in_kind.as_value_or_err()?;

    // 1. Identity: input already has the target float type.
    if in_ty == out_ty {
        return Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, input)?));
    }

    // 2. Float→float precision change.
    if in_ty.is_float() {
        let new_out = fg.make_float_to_float_node( input, out_ty)?;
        return Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, new_out)?));
    }

    // Input is integer from here.

    // 3. Integer constant → float constant (same bits).
    if let Some(bits) = fg.int_const_val( input) {
        let new_out = fg.make_float_const( bits, out_ty)?;
        return Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, new_out)?));
    }

    // 4. Non-constant integer → explicit IntBitsToFloat.
    let new_out = fg.make_int_bits_to_float_node( input, out_ty)?;
    Ok(OptimizationResult::from_changed(fg.replace_all_uses( out, new_out)?))
}

pub struct ConstantFold;

impl Optimizer for ConstantFold {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let nodes: Vec<_> = function.preorder().collect();
        let mut result = OptimizationResult::NoChange;
        for node_id in nodes {
            result |= apply_identity_rules(function, node_id)?;
            result |= apply_const_eval_rules(function, node_id)?;
            result |= apply_bool_float_rules(function, node_id)?;
            result |= apply_reassoc_and_mask_rules(function, node_id)?;
            result |= apply_bitcast_extend_rules(function, node_id)?;
            result |= try_fold_piece(function, node_id)?;
            result |= try_fold_extract(function, node_id)?;
            result |= try_fold_insert(function, node_id)?;
        }
        Ok(result)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ir::node::{NodeKind, NodeOutputType};
    use ir::{
        BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, FunctionBuilder,
        IntBinaryOp, IntCmpOp,
    };

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
        Ok(b.build()?)
    }

    /// Returns the output id that the Return node receives as its value
    /// argument (input[1]: input[0] is the control edge).
    fn return_value(fg: &ir::BuiltFunctionGraph) -> Result<ir::Value> {
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .ok_or(ErrorKind::NoReturnNode)?;
        Ok(fg.graph.node_inputs(ret)[1])
    }

    /// Returns the `NodeKind` of the node that produces the return value.
    fn return_kind(fg: &ir::BuiltFunctionGraph) -> Result<NodeKind> {
        let val = return_value(fg)?;
        let node = fg.graph.get_node_from_output(val);
        Ok(*fg.graph.node_kind(node))
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
        Ok(())
    }

    #[test]
    fn fold_int_xor_self() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xAB, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(x, x, IntBinaryOp::Xor, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
        Ok(())
    }

    #[test]
    fn fold_int_sub_self() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xAB, NodeOutputType::U64);
            Ok(b.build_int_binary_operation(x, x, IntBinaryOp::Sub, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(3));
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(5));
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
            let inner =
                b.build_int_binary_operation(x, c4, IntBinaryOp::And, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(inner, c7, IntBinaryOp::And, NodeOutputType::U64)?)
        })?;
        // Run to convergence (both-const fold + mask-merge may each fire once).
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        // 0xFF & 4 = 4, 4 & 7 = 4.
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(4));
        Ok(())
    }

    // ── add/sub reassociation with constants ──────────────────────────────────

    /// Fabricates a register varnode for use as a non-constant operand.
    fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
        rsleigh::Vn {
            size,
            addr: rsleigh::VnAddr {
                off,
                space: rsleigh::VnSpace::REGISTER,
            },
        }
    }

    /// Builds a minimal function exposing a single tracked variable via
    /// `read_variable` (which returns a `ControlPhi` output wrapping the
    /// entry's `InitialVar`). The closure receives that non-constant value.
    fn make_fn_with_var<F>(vn: rsleigh::Vn, f: F) -> Result<(ir::BuiltFunctionGraph, ir::Value)>
    where
        F: FnOnce(&mut FunctionBuilder, ir::Value) -> Result<ir::Value>,
    {
        let mut b = FunctionBuilder::new(vec![vn], &[vn], &[], &[])?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let x = b.read_variable(&vn)?;
        let val = f(&mut b, x)?;
        b.build_return(Some(val), &[])?;
        Ok((b.build()?, x))
    }

    /// Asserts the return-value node is `expected_base + expected_const`
    /// (type-masked; operand order irrelevant).
    fn assert_add_with_const(
        fg: &ir::BuiltFunctionGraph,
        expected_base: ir::Value,
        expected_const: u64,
        ty: NodeOutputType,
    ) -> Result<()> {
        let val = return_value(fg)?;
        let node = fg.graph.get_node_from_output(val);
        assert!(
            matches!(
                fg.graph.node_kind(node),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            ),
            "expected outer Add, got {:?}",
            fg.graph.node_kind(node)
        );
        let inputs = fg.graph.node_inputs(node);
        assert_eq!(inputs.len(), 2);
        let l = inputs[0];
        let r = inputs[1];
        let masked = ty
            .get_unsigned_int(expected_const)
            .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
        let const_on = |o: ir::Value| -> bool {
            matches!(
                *fg.graph.node_kind(fg.graph.get_node_from_output(o)),
                NodeKind::IntConst(v) if ty.get_unsigned_int(v) == Some(masked)
            )
        };
        let ok = (l == expected_base && const_on(r)) || (r == expected_base && const_on(l));
        assert!(
            ok,
            "expected `base + {:#x}`; got lhs kind={:?}, rhs kind={:?}",
            masked,
            fg.graph.node_kind(fg.graph.get_node_from_output(l)),
            fg.graph.node_kind(fg.graph.get_node_from_output(r)),
        );
        Ok(())
    }

    /// Asserts the return-value node is `expected_base - expected_const`
    /// (lhs must be the base, rhs must be the constant; Sub is non-commutative).
    fn assert_sub_with_const(
        fg: &ir::BuiltFunctionGraph,
        expected_base: ir::Value,
        expected_const: u64,
        ty: NodeOutputType,
    ) -> Result<()> {
        let val = return_value(fg)?;
        let node = fg.graph.get_node_from_output(val);
        assert!(
            matches!(
                fg.graph.node_kind(node),
                NodeKind::IntBinaryOp(IntBinaryOp::Sub)
            ),
            "expected outer Sub, got {:?}",
            fg.graph.node_kind(node)
        );
        let inputs = fg.graph.node_inputs(node);
        assert_eq!(inputs.len(), 2);
        let l = inputs[0];
        let r = inputs[1];
        let masked = ty
            .get_unsigned_int(expected_const)
            .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
        let const_on_rhs = matches!(
            *fg.graph.node_kind(fg.graph.get_node_from_output(r)),
            NodeKind::IntConst(v) if ty.get_unsigned_int(v) == Some(masked)
        );
        assert!(
            l == expected_base && const_on_rhs,
            "expected `base - {:#x}`; got lhs kind={:?}, rhs kind={:?}",
            masked,
            fg.graph.node_kind(fg.graph.get_node_from_output(l)),
            fg.graph.node_kind(fg.graph.get_node_from_output(r)),
        );
        Ok(())
    }

    #[test]
    fn reassoc_add_add_consts() -> Result<()> {
        // (x + 3) + 4 → x + 7
        let vn = reg_vn(0x1000, 8);
        let (mut fg, x) = make_fn_with_var(vn, |b, x| {
            let c3 = b.build_int_const(3, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let inner =
                b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
        })?;
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_add_with_const(&fg, x, 7, NodeOutputType::U64)?;
        Ok(())
    }

    #[test]
    fn reassoc_add_sub_consts() -> Result<()> {
        // (x - 3) + 4 → x + 1
        let vn = reg_vn(0x1000, 8);
        let (mut fg, x) = make_fn_with_var(vn, |b, x| {
            let c3 = b.build_int_const(3, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let inner =
                b.build_int_binary_operation(x, c3, IntBinaryOp::Sub, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
        })?;
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_add_with_const(&fg, x, 1, NodeOutputType::U64)?;
        Ok(())
    }

    #[test]
    fn reassoc_sub_add_consts_wrapping() -> Result<()> {
        // (x + 3) - 4 → x + (3 - 4)  = x + 0xFFFF_FFFF_FFFF_FFFF
        let vn = reg_vn(0x1000, 8);
        let (mut fg, x) = make_fn_with_var(vn, |b, x| {
            let c3 = b.build_int_const(3, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let inner =
                b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Sub, NodeOutputType::U64)?)
        })?;
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_add_with_const(&fg, x, 0xFFFF_FFFF_FFFF_FFFF, NodeOutputType::U64)?;
        Ok(())
    }

    #[test]
    fn reassoc_sub_sub_consts() -> Result<()> {
        // (x - 3) - 4 → x - 7
        let vn = reg_vn(0x1000, 8);
        let (mut fg, x) = make_fn_with_var(vn, |b, x| {
            let c3 = b.build_int_const(3, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let inner =
                b.build_int_binary_operation(x, c3, IntBinaryOp::Sub, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Sub, NodeOutputType::U64)?)
        })?;
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_sub_with_const(&fg, x, 7, NodeOutputType::U64)?;
        Ok(())
    }

    #[test]
    fn reassoc_add_commuted_inner() -> Result<()> {
        // (3 + x) + 4 → x + 7 (inner Add has const on lhs)
        let vn = reg_vn(0x1000, 8);
        let (mut fg, x) = make_fn_with_var(vn, |b, x| {
            let c3 = b.build_int_const(3, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let inner =
                b.build_int_binary_operation(c3, x, IntBinaryOp::Add, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
        })?;
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_add_with_const(&fg, x, 7, NodeOutputType::U64)?;
        Ok(())
    }

    #[test]
    fn reassoc_add_commuted_outer() -> Result<()> {
        // 4 + (x + 3) → x + 7 (outer Add has const on lhs)
        let vn = reg_vn(0x1000, 8);
        let (mut fg, x) = make_fn_with_var(vn, |b, x| {
            let c3 = b.build_int_const(3, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let inner =
                b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(c4, inner, IntBinaryOp::Add, NodeOutputType::U64)?)
        })?;
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_add_with_const(&fg, x, 7, NodeOutputType::U64)?;
        Ok(())
    }

    #[test]
    fn reassoc_chain_three_subs() -> Result<()> {
        // ((x - 4) - 4) - 4 → x - 12.  Requires the fixed-point loop to
        // compose multiple reassociation steps.
        let vn = reg_vn(0x1000, 8);
        let (mut fg, x) = make_fn_with_var(vn, |b, x| {
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let a = b.build_int_binary_operation(x, c4, IntBinaryOp::Sub, NodeOutputType::U64)?;
            let b_ = b.build_int_binary_operation(a, c4, IntBinaryOp::Sub, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(b_, c4, IntBinaryOp::Sub, NodeOutputType::U64)?)
        })?;
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_sub_with_const(&fg, x, 12, NodeOutputType::U64)?;
        Ok(())
    }

    #[test]
    fn reassoc_chain_three_subs_u32() -> Result<()> {
        // Same chain but at U32: ((x - 4) - 4) - 4 → x - 12.
        let vn = reg_vn(0x1000, 4);
        let (mut fg, x) = make_fn_with_var(vn, |b, x| {
            let c4 = b.build_int_const(4, NodeOutputType::U32);
            let a = b.build_int_binary_operation(x, c4, IntBinaryOp::Sub, NodeOutputType::U32)?;
            let b_ = b.build_int_binary_operation(a, c4, IntBinaryOp::Sub, NodeOutputType::U32)?;
            Ok(b.build_int_binary_operation(b_, c4, IntBinaryOp::Sub, NodeOutputType::U32)?)
        })?;
        let mut changed = true;
        while changed {
            changed = ConstantFold.optimize(&mut fg)?.changed();
        }
        assert_sub_with_const(&fg, x, 12, NodeOutputType::U32)?;
        Ok(())
    }

    #[test]
    fn reassoc_no_fold_without_const() -> Result<()> {
        // (x + y) + z, no constants → untouched.
        let xv = reg_vn(0x1000, 8);
        let yv = reg_vn(0x1008, 8);
        let zv = reg_vn(0x1010, 8);
        let mut b = FunctionBuilder::new(vec![xv, yv, zv], &[xv, yv, zv], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let x = b.read_variable(&xv)?;
        let y = b.read_variable(&yv)?;
        let z = b.read_variable(&zv)?;
        let inner = b.build_int_binary_operation(x, y, IntBinaryOp::Add, NodeOutputType::U64)?;
        let outer =
            b.build_int_binary_operation(inner, z, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_return(Some(outer), &[])?;
        let mut fg = b.build()?;
        let before = return_value(&fg)?;
        // Should not change: no constants anywhere.
        let res = ConstantFold.optimize(&mut fg)?;
        assert!(!res.changed(), "no-const chain should not reassociate");
        assert_eq!(return_value(&fg)?, before);
        Ok(())
    }

    #[test]
    fn distribution_rewrite() -> Result<()> {
        // Build ((a & 0xF0) | (b & 0x0F)) & 0xFF.
        // Rule fires: (a & (0xF0 & 0xFF)) | (b & (0x0F & 0xFF))
        //           = (a & 0xF0) | (b & 0x0F)  — changed=true.
        let av = reg_vn(0x1000, 8);
        let bv = reg_vn(0x1008, 8);
        let mut b = FunctionBuilder::new(vec![av, bv], &[av, bv], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let a = b.read_variable(&av)?;
        let bval = b.read_variable(&bv)?;
        let f0 = b.build_int_const(0xF0, NodeOutputType::U64);
        let f0_ = b.build_int_const(0x0F, NodeOutputType::U64);
        let ff = b.build_int_const(0xFF, NodeOutputType::U64);
        let a_and_f0 =
            b.build_int_binary_operation(a, f0, IntBinaryOp::And, NodeOutputType::U64)?;
        let b_and_0f =
            b.build_int_binary_operation(bval, f0_, IntBinaryOp::And, NodeOutputType::U64)?;
        let or_node =
            b.build_int_binary_operation(a_and_f0, b_and_0f, IntBinaryOp::Or, NodeOutputType::U64)?;
        let outer =
            b.build_int_binary_operation(or_node, ff, IntBinaryOp::And, NodeOutputType::U64)?;
        b.build_return(Some(outer), &[])?;
        let mut fg = b.build()?;
        let changed = ConstantFold.optimize(&mut fg)?.changed();
        assert!(changed, "distribution rule should fire");
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
        let val = return_value(&fg)?;
        // Use int_const_val which masks to the declared type.
        let semantic = fg.int_const_val(val);
        assert_eq!(semantic, Some(0), "0xFF00 truncated to U8 should be 0");
        // No Truncate nodes should exist.
        assert!(
            !fg.all_node_ids()
                .any(|n| matches!(fg.graph.node_kind(n), NodeKind::Truncate)),
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
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
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
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
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
        assert!(matches!(
            return_kind(&fg)?,
            NodeKind::IntBinaryOp(IntBinaryOp::Div)
        ));
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
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
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
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(5));
        Ok(())
    }

    #[test]
    fn fold_popcount_zero() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_int_const(0, NodeOutputType::U64);
            Ok(b.build_popcount(v, NodeOutputType::U64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xABCD));
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
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xBC));
        Ok(())
    }

    #[test]
    fn fold_insert_const() -> Result<()> {
        // insert(0xFF00u16, 0x42u16, lsb=0, len=8) = 0xFF42
        let mut fg = make_fn(|b| {
            let dest = b.build_int_const(0xFF00, NodeOutputType::U16);
            let src = b.build_int_const(0x42, NodeOutputType::U16);
            Ok(b.build_insert(dest, src, 0, 8, NodeOutputType::U16)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFF42));
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
        assert_eq!(
            return_kind(&fg)?,
            NodeKind::FloatConst(7.0f32.to_bits() as u64)
        );
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
        assert_eq!(
            return_kind(&fg)?,
            NodeKind::FloatConst(12.0f32.to_bits() as u64)
        );
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
        assert_eq!(
            return_kind(&fg)?,
            NodeKind::FloatConst(2.5f32.to_bits() as u64)
        );
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
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(7.0f64.to_bits()));
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
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(12.0f64.to_bits()));
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
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.5f64.to_bits()));
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
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
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
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
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
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
        Ok(())
    }

    #[test]
    fn fold_f32_neg_const() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(2.0f32.to_bits() as u64, NodeOutputType::F32);
            Ok(b.build_float_unary_op(v, FloatUnaryOp::Neg, NodeOutputType::F32)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(
            return_kind(&fg)?,
            NodeKind::FloatConst((-2.0f32).to_bits() as u64)
        );
        Ok(())
    }

    #[test]
    fn fold_f64_abs_const() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const((-3.0f64).to_bits(), NodeOutputType::F64);
            Ok(b.build_float_unary_op(v, FloatUnaryOp::Abs, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(3.0f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_f64_sqrt_const() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_unary_op(v, FloatUnaryOp::Sqrt, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.0f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_float_is_nan_true() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(f32::NAN.to_bits() as u64, NodeOutputType::F32);
            Ok(b.build_float_is_nan(v)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
        Ok(())
    }

    #[test]
    fn fold_float_is_nan_false() -> Result<()> {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_is_nan(v)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
        Ok(())
    }

    #[test]
    fn fold_float_mul_by_one_identity() -> Result<()> {
        let mut fg = make_fn(|b| {
            let one = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
            let x = b.build_float_const(2.5f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_binary_op(x, one, FloatBinaryOp::Mul, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.5f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_float_div_by_one_identity() -> Result<()> {
        let mut fg = make_fn(|b| {
            let one = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
            let x = b.build_float_const(2.5f64.to_bits(), NodeOutputType::F64);
            Ok(b.build_float_binary_op(x, one, FloatBinaryOp::Div, NodeOutputType::F64)?)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.5f64.to_bits()));
        Ok(())
    }

    #[test]
    fn fold_bitcast_identity_int_bits_to_float_of_float_bits_to_int() -> Result<()> {
        // IntBitsToFloat(FloatBitsToInt(FloatAdd(1.0, 2.0)))
        // → first, FloatAdd(1.0, 2.0) folds to FloatConst(3.0)
        // → then,  IntBitsToFloat(FloatBitsToInt(FloatConst(3.0))) simplifies to FloatConst(3.0)
        //   via the bitcast-identity: replace uses of IntBitsToFloat with FloatBitsToInt's input.
        let mut fg = make_fn(|b| {
            let a = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
            let b2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
            let sum = b.build_float_binary_op(a, b2, FloatBinaryOp::Add, NodeOutputType::F64)?;
            let as_int = b.build_float_bits_to_int(sum, NodeOutputType::U64)?;
            let back_to_float = b.build_int_bits_to_float(as_int, NodeOutputType::F64)?;
            Ok(back_to_float)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        // Float binary fold: sum → FloatConst(3.0).
        // Bitcast identity fold: IntBitsToFloat(FloatBitsToInt(FloatConst(3.0))) → FloatConst(3.0).
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(3.0f64.to_bits()));
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
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(bits));
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
        assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(bits));
        Ok(())
    }

    #[test]
    fn cast_to_float_int_non_const_lowers_to_int_bits_to_float() -> Result<()> {
        let mut fg = make_fn(|b| {
            let int_a = b.build_int_const(1, NodeOutputType::U32);
            let int_b = b.build_int_const(2, NodeOutputType::U32);
            // Non-const int (Add result).
            let sum =
                b.build_int_binary_operation(int_a, int_b, IntBinaryOp::Add, NodeOutputType::U32)?;
            let cast = b.build_cast_to_float(sum, NodeOutputType::F32);
            Ok(cast)
        })?;
        assert!(ConstantFold.optimize(&mut fg)?.changed());
        // Should lower to IntBitsToFloat.
        assert_eq!(return_kind(&fg)?, NodeKind::IntBitsToFloat);
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
        assert_eq!(return_kind(&fg)?, NodeKind::FloatToFloat);
        Ok(())
    }
}
