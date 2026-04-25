use std::sync::LazyLock;

use ir::node::NodeId;
use ir::{BuiltFunctionGraph, IntUnaryOp};

use crate::error::{ErrorKind, Result};
use crate::pipeline::OptimizationResult;

use super::eval_float::{eval_float_binary, eval_float_cmp, eval_float_unary};
use super::eval_int::{eval_int_binary, eval_int_cmp};
use super::try_lower_cast_to_float;

// ── per-node folding ──────────────────────────────────────────────────────────

/// Builds the rule vec for [`apply_reassoc_and_mask_rules`].
///
/// Called once from [`REASSOC_AND_MASK_RULES`]'s `LazyLock` initializer.
fn build_reassoc_and_mask_rules() -> Vec<pattern::BoxedRule> {
    use pattern::{
        BoxedRule, IntVar, Var, add, and, any_int_const, boxed_rule, int_const_with, or,
        rewrite_rule, sub, var,
    };

    // (x + C1) + C2 → x + (C1 + C2)
    let (x, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_add_add = boxed_rule(rewrite_rule(
        add(add(var(x), any_int_const(c1)), any_int_const(c2)),
        add(var(x), int_const_with!([c1, c2] => c1.wrapping_add(c2))),
    ));

    // (x - C1) - C2 → x - (C1 + C2)
    let (x, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_sub_sub = boxed_rule(rewrite_rule(
        sub(sub(var(x), any_int_const(c1)), any_int_const(c2)),
        sub(var(x), int_const_with!([c1, c2] => c1.wrapping_add(c2))),
    ));

    // (x + C1) - C2 → x + (C1 - C2)
    let (x, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_add_sub = boxed_rule(rewrite_rule(
        sub(add(var(x), any_int_const(c1)), any_int_const(c2)),
        add(var(x), int_const_with!([c1, c2] => c1.wrapping_sub(c2))),
    ));

    // (x - C1) + C2 → x + (C2 - C1)
    let (x, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_sub_add = boxed_rule(rewrite_rule(
        add(sub(var(x), any_int_const(c1)), any_int_const(c2)),
        add(var(x), int_const_with!([c1, c2] => c2.wrapping_sub(c1))),
    ));

    // (a & C1) & C2 → a & (C1 & C2)
    let (a, c1, c2) = (Var::new(), IntVar::new(), IntVar::new());
    let rule_and_merge = boxed_rule(rewrite_rule(
        and(and(var(a), any_int_const(c1)), any_int_const(c2)),
        and(var(a), int_const_with!([c1, c2] => c1 & c2)),
    ));

    // ((a & C1) | (b & C2)) & C3 → (a & (C1 & C3)) | (b & (C2 & C3))
    let (a, b) = (Var::new(), Var::new());
    let (c1, c2, c3) = (IntVar::new(), IntVar::new(), IntVar::new());
    let rule_and_dist = boxed_rule(rewrite_rule(
        and(
            or(and(var(a), any_int_const(c1)), and(var(b), any_int_const(c2))),
            any_int_const(c3),
        ),
        or(
            and(var(a), int_const_with!([c1, c3] => c1 & c3)),
            and(var(b), int_const_with!([c2, c3] => c2 & c3)),
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
    rules
}

static REASSOC_AND_MASK_RULES: LazyLock<Vec<pattern::BoxedRule>> =
    LazyLock::new(build_reassoc_and_mask_rules);

/// Applies add/sub reassociation and AND-mask merging rules.
///
/// Rules:
/// - `(x + C1) + C2 → x + (C1 + C2)`
/// - `(x - C1) - C2 → x - (C1 + C2)`
/// - `(x + C1) - C2 → x + (C1 - C2)`
/// - `(a & C1) & C2 → a & (C1 & C2)`
/// - `((a & C1) | (b & C2)) & C3 → (a & (C1 & C3)) | (b & (C2 & C3))`
pub(super) fn apply_reassoc_and_mask_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::apply_rules_in_order;
    let changed = apply_rules_in_order(&REASSOC_AND_MASK_RULES)(fg, node)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Builds the rule vec for [`apply_bitcast_extend_rules`].
fn build_bitcast_extend_rules() -> Vec<pattern::BoxedRule> {
    use pattern::{
        BoxedRule, Var, boxed_rule, float_bits_to_int, int_bits_to_float, rewrite_rule, var,
    };

    // IntBitsToFloat(FloatBitsToInt(x)) → x
    let x = Var::new();
    let rule_int_float = boxed_rule(rewrite_rule(
        int_bits_to_float(float_bits_to_int(var(x))),
        var(x),
    ));

    // FloatBitsToInt(IntBitsToFloat(x)) → x
    let x = Var::new();
    let rule_float_int = boxed_rule(rewrite_rule(
        float_bits_to_int(int_bits_to_float(var(x))),
        var(x),
    ));

    let rules: Vec<BoxedRule> = vec![rule_int_float, rule_float_int];
    rules
}

static BITCAST_EXTEND_RULES: LazyLock<Vec<pattern::BoxedRule>> =
    LazyLock::new(build_bitcast_extend_rules);

/// Applies bitcast identity rules:
/// - `IntBitsToFloat(FloatBitsToInt(x)) → x`
/// - `FloatBitsToInt(IntBitsToFloat(x)) → x`
pub(super) fn apply_bitcast_extend_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::apply_rules_in_order;
    let changed = apply_rules_in_order(&BITCAST_EXTEND_RULES)(fg, node)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Builds the rule vec for [`apply_identity_rules`].
fn build_identity_rules() -> Vec<pattern::BoxedRule> {
    use pattern::{
        BoxedRule, IntVar, Pat, Var, add, and, any_int_const, boxed_rule, int_const, mul, or,
        rewrite_rule, shl, shr, sshr, sub, var, xor,
    };

    let x = Var::new();
    // x & all_ones → x  (commutative). The all-ones mask depends on the
    // output width, so we use `.when_match()` to compare the captured
    // constant against the node's output-type all-ones value.
    let all_ones_rule = {
        let x = Var::new();
        let c = IntVar::new();
        let pat: Pat = and(var(x), any_int_const(c)).into();
        let pat = pat.when_match(move |_fg, ty, b| {
            b.get_int(c) == ty.get_unsigned_int(u64::MAX)
        });
        boxed_rule(rewrite_rule(pat, var(x)))
    };

    let rules: Vec<BoxedRule> = vec![
        // x + 0 → x  (commutative: also covers 0 + x)
        boxed_rule(rewrite_rule(add(var(x), int_const(0)), var(x))),
        // x - 0 → x
        boxed_rule(rewrite_rule(sub(var(x), int_const(0)), var(x))),
        // x - x → 0
        boxed_rule(rewrite_rule(sub(var(x), var(x)), int_const(0))),
        // x ^ x → 0
        boxed_rule(rewrite_rule(xor(var(x), var(x)), int_const(0))),
        // x ^ 0 → x  (commutative)
        boxed_rule(rewrite_rule(xor(var(x), int_const(0)), var(x))),
        // x * 0 → 0  (commutative)
        boxed_rule(rewrite_rule(mul(var(x), int_const(0)), int_const(0))),
        // x * 1 → x  (commutative)
        boxed_rule(rewrite_rule(mul(var(x), int_const(1)), var(x))),
        // x & 0 → 0  (commutative)
        boxed_rule(rewrite_rule(and(var(x), int_const(0)), int_const(0))),
        // x & x → x
        boxed_rule(rewrite_rule(and(var(x), var(x)), var(x))),
        // x | 0 → x  (commutative)
        boxed_rule(rewrite_rule(or(var(x), int_const(0)), var(x))),
        // x | x → x
        boxed_rule(rewrite_rule(or(var(x), var(x)), var(x))),
        // x << 0 → x  (non-commutative — only RHS 0 is the identity)
        boxed_rule(rewrite_rule(shl(var(x), int_const(0)), var(x))),
        // x >> 0 → x  (logical shift right)
        boxed_rule(rewrite_rule(shr(var(x), int_const(0)), var(x))),
        // x >>> 0 → x  (arithmetic / signed shift right)
        boxed_rule(rewrite_rule(sshr(var(x), int_const(0)), var(x))),
        all_ones_rule,
    ];
    rules
}

static IDENTITY_RULES: LazyLock<Vec<pattern::BoxedRule>> = LazyLock::new(build_identity_rules);

/// Applies single-operand algebraic identities to integer binary operations.
///
/// Rules ported from hand-written arms:
/// - `x + 0 → x`, `x - 0 → x`, `x - x → 0`
/// - `x ^ x → 0`, `x ^ 0 → x`
/// - `x * 0 → 0`, `x * 1 → x`
/// - `x & 0 → 0`, `x & x → x`, `x & all_ones → x`
/// - `x | 0 → x`, `x | x → x`
/// - `x << 0 → x`, `x >> 0 → x`, `x >>> 0 → x`
pub(super) fn apply_identity_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::apply_rules_in_order;
    let changed = apply_rules_in_order(&IDENTITY_RULES)(fg, node)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Builds the rule vec for [`apply_const_eval_rules`].
fn build_const_eval_rules() -> Vec<pattern::BoxedRule> {
    use pattern::{
        BoolVar, BoxedRule, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, any_bool_const,
        any_int_const, bool_const_with, boxed_rule, cast_to_bool, cast_to_int, int_binary_any,
        int_cmp_any, int_const_with, int_unary_any, lzcount, popcount, rewrite_rule, sign_extend,
        truncate, zero_extend,
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
    rules
}

static CONST_EVAL_RULES: LazyLock<Vec<pattern::BoxedRule>> = LazyLock::new(build_const_eval_rules);

/// Applies full constant evaluation for integer binary ops, integer unary ops,
/// integer comparisons, truncate, extend (zero/sign), popcount, lzcount,
/// cast_to_bool, and cast_to_int.
pub(super) fn apply_const_eval_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::apply_rules_in_order;
    let changed = apply_rules_in_order(&CONST_EVAL_RULES)(fg, node)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Builds the rule vec for [`apply_bool_float_rules`].
fn build_bool_float_rules() -> Vec<pattern::BoxedRule> {
    use pattern::{
        BoolUnaryOpVar, BoolVar, BoxedRule, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
        FloatVar, any_bool_const, any_float_const, bool_and, bool_or, bool_unary_any, bool_xor,
        boxed_rule, float_binary_any, float_cmp_any, float_unary_any, rewrite_rule,
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
        // BAnd(BoolConst(false), _) => bool_const(false)  (absorbing element)
        {
            let l = BoolVar::new();
            boxed_rule(rewrite_rule(
                bool_and(any_bool_const(l), pattern::any()),
                bool_const_with!([l] => {
                    if !l { false } else {
                        return Err(pattern::Error::skip());
                    }
                }),
            ))
        },
        // BOr(BoolConst(true), _) => bool_const(true)  (absorbing element)
        {
            let l = BoolVar::new();
            boxed_rule(rewrite_rule(
                bool_or(any_bool_const(l), pattern::any()),
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
    ];
    rules
}

static BOOL_FLOAT_RULES: LazyLock<Vec<pattern::BoxedRule>> = LazyLock::new(build_bool_float_rules);

/// Applies constant evaluation and absorbing-element rules for bool binary ops,
/// bool unary ops, and all float ops.
pub(super) fn apply_bool_float_rules(
    fg: &mut BuiltFunctionGraph,
    node: NodeId,
) -> Result<OptimizationResult> {
    use pattern::apply_rules_in_order;
    let changed = apply_rules_in_order(&BOOL_FLOAT_RULES)(fg, node)?;
    // CastToFloat lowering is too stateful for a rule (it does graph surgery);
    // handle it separately after the rule sweep.
    let cast_changed = try_lower_cast_to_float(fg, node)?;
    Ok(OptimizationResult::from_changed(changed) | cast_changed)
}
