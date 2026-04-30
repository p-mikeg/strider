use std::sync::LazyLock;

use ir::node::NodeId;
use ir::{BuiltFunctionGraph, IntUnaryOp};

use crate::error::Result;
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
        BoxedRule, Capture, add, and, any_int_const, boxed_rule, int_const_with, or,
        rewrite_rule, sub, var,
    };

    // (x + C1) + C2 → x + (C1 + C2)
    let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
    let rule_add_add = boxed_rule(rewrite_rule(
        add(add(var(x), any_int_const(c1)), any_int_const(c2)),
        add(var(x), int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2))),
    ));

    // (x - C1) - C2 → x - (C1 + C2)
    let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
    let rule_sub_sub = boxed_rule(rewrite_rule(
        sub(sub(var(x), any_int_const(c1)), any_int_const(c2)),
        sub(var(x), int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2))),
    ));

    // (x + C1) - C2 → x + (C1 - C2)
    let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
    let rule_add_sub = boxed_rule(rewrite_rule(
        sub(add(var(x), any_int_const(c1)), any_int_const(c2)),
        add(var(x), int_const_with!([c1: uint, c2: uint] => c1.wrapping_sub(c2))),
    ));

    // (x - C1) + C2 → x + (C2 - C1)
    let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
    let rule_sub_add = boxed_rule(rewrite_rule(
        add(sub(var(x), any_int_const(c1)), any_int_const(c2)),
        add(var(x), int_const_with!([c1: uint, c2: uint] => c2.wrapping_sub(c1))),
    ));

    // (a & C1) & C2 → a & (C1 & C2)
    let (a, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
    let rule_and_merge = boxed_rule(rewrite_rule(
        and(and(var(a), any_int_const(c1)), any_int_const(c2)),
        and(var(a), int_const_with!([c1: uint, c2: uint] => c1 & c2)),
    ));

    // ((a & C1) | (b & C2)) & C3 → (a & (C1 & C3)) | (b & (C2 & C3))
    let (a, b) = (Capture::new(), Capture::new());
    let (c1, c2, c3) = (Capture::new(), Capture::new(), Capture::new());
    let rule_and_dist = boxed_rule(rewrite_rule(
        and(
            or(and(var(a), any_int_const(c1)), and(var(b), any_int_const(c2))),
            any_int_const(c3),
        ),
        or(
            and(var(a), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
            and(var(b), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
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
        BoxedRule, Capture, boxed_rule, float_bits_to_int, int_bits_to_float, rewrite_rule,
        sign_extend, truncate, var, zero_extend,
    };

    // IntBitsToFloat(FloatBitsToInt(x)) → x
    let x = Capture::new();
    let rule_int_float = boxed_rule(rewrite_rule(
        int_bits_to_float(float_bits_to_int(var(x))),
        var(x),
    ));

    // FloatBitsToInt(IntBitsToFloat(x)) → x
    let x = Capture::new();
    let rule_float_int = boxed_rule(rewrite_rule(
        float_bits_to_int(int_bits_to_float(var(x))),
        var(x),
    ));

    // Truncate(ZeroExtend(x)) → x — when `x`'s type equals the truncate's
    // output type, the round-trip is identity (the extend added zero bits
    // that the truncate cuts away).  Without this rule, register-merge
    // chains in write_reg_vn (which always extend then truncate to land
    // in container width) leave the round-trip in the IR and the pattern
    // matcher's data-flow walk can't cross through Extend/Truncate to find
    // an inner Mul/Add.
    //
    // The width-equality check uses `when_match` on the Bindings: the
    // captured `x`'s output type must equal the rule root's `ty`.
    let zext_round_trip = {
        let x = Capture::new();
        let pat = truncate(zero_extend(var(x))).when_match(move |fg, ty, b| {
            b.get(x)
                .and_then(|out| fg.graph.output_kind(out).as_value())
                .is_some_and(|x_ty| x_ty == ty)
        });
        boxed_rule(rewrite_rule(pat, var(x)))
    };

    // Truncate(SignExtend(x)) → x — same identity at the bit level when
    // widths match (sign-extension's added bits are sign replication; the
    // truncate cuts them off and recovers the original bits).
    let sext_round_trip = {
        let x = Capture::new();
        let pat = truncate(sign_extend(var(x))).when_match(move |fg, ty, b| {
            b.get(x)
                .and_then(|out| fg.graph.output_kind(out).as_value())
                .is_some_and(|x_ty| x_ty == ty)
        });
        boxed_rule(rewrite_rule(pat, var(x)))
    };

    // Narrowing through binop: `Truncate_<W>(IntBinaryOp(op,
    // SignExt_<W→W'>(a), SignExt_<W→W'>(b)))` → `IntBinaryOp_<W>(op, a, b)`
    // for ops where the lower W bits don't depend on the upper bits
    // (Add/Sub/Mul/And/Or/Xor).  MIPS32 lifts `mul a, b` (32×32→64 IntMul
    // on a 64-bit unique varnode) followed by a 32-bit Truncate to get
    // back into integer-register width — without this rule the matcher's
    // data-flow walk for `add(mul(_,_), _)` cannot cross through the
    // Truncate to find the inner Mul.
    //
    // We need separate rules for each (lhs_extend_kind, rhs_extend_kind)
    // permutation because the pattern crate's RHS doesn't currently
    // support reconstructing a non-const node from a captured op variant.
    // The (SignExt, SignExt) case for Mul covers the MIPS32 shape above.
    use pattern::mul as mul_pat;
    let narrow_mul_through_sext = {
        let a = Capture::new();
        let b = Capture::new();
        let pat = truncate(mul_pat(sign_extend(var(a)), sign_extend(var(b)))).when_match(
            move |fg, ty, bnd| {
                bnd.get(a)
                    .and_then(|out| fg.graph.output_kind(out).as_value())
                    .is_some_and(|a_ty| a_ty == ty)
                    && bnd
                        .get(b)
                        .and_then(|out| fg.graph.output_kind(out).as_value())
                        .is_some_and(|b_ty| b_ty == ty)
            },
        );
        boxed_rule(rewrite_rule(pat, mul_pat(var(a), var(b))))
    };

    // Drop the high-bits half of a register-merge Or when truncating to
    // the low half's width.  x86 / x64's `mov $eax, ...` lifts to a
    // write_reg_vn merge:
    //   $rax = ($rax & 0xFFFF_FFFF_0000_0000) | (ZeroExt(low_part) &
    //                                            0x0000_0000_FFFF_FFFF)
    // and downstream reads of $eax produce
    //   Truncate_U32(Or(And(0xFFFF_FFFF_0000_0000, $rax_old),
    //                   And(0x0000_0000_FFFF_FFFF, ZeroExt(low_part))))
    // The first And's mask has zero in the low 32 bits, so its
    // contribution to Truncate_U32(Or(...)) is zero — the truncate
    // collapses to `Truncate_U32(And(0x0000_0000_FFFF_FFFF, ZeroExt(...)))`,
    // which the existing `x & all_ones → x` and round-trip rules then
    // fully simplify.
    //
    // We pin the high-mask check via `when_match`: the captured constant
    // `c`'s low-`W` bits must all be zero, where `W` is the truncate's
    // output bit width.
    use pattern::{and, any_int_const, or};
    // Two rule orientations because the Or's commutative match doesn't
    // generate enough swaps to enumerate "the And side of the Or might
    // be either operand AND the IntConst inside that And might be either
    // operand of the And".
    let mk_drop_high_half = |swap: bool| -> BoxedRule {
        let a = Capture::new();
        let b = Capture::new();
        let c = Capture::new();
        let inner = if swap {
            or(and(any_int_const(c), var(b)), var(a))
        } else {
            or(var(a), and(any_int_const(c), var(b)))
        };
        let pat = truncate(inner).when_match(move |fg, ty, bnd| {
            let Some(c_val) = bnd.get_uint(c, fg) else { return false; };
            let bits = ty.bit_width();
            if bits == 0 || bits >= 128 {
                return false;
            }
            let low_mask: u128 = (1u128 << bits) - 1;
            c_val & low_mask == 0
        });
        boxed_rule(rewrite_rule(pat, truncate(var(a))))
    };

    // `Truncate_<W>(And(low_W_mask, x)) → Truncate_<W>(x)` — the AND's
    // effect of zeroing all bits above W is redundant when the truncate
    // is going to discard those bits anyway.  Two orientations because
    // And is commutative but the matcher's swap doesn't enumerate over
    // `any_int_const` placement.
    let mk_drop_low_mask_under_truncate = |swap: bool| -> BoxedRule {
        let x = Capture::new();
        let c = Capture::new();
        let inner = if swap {
            and(var(x), any_int_const(c))
        } else {
            and(any_int_const(c), var(x))
        };
        let pat = truncate(inner).when_match(move |fg, ty, bnd| {
            let Some(c_val) = bnd.get_uint(c, fg) else { return false; };
            let bits = ty.bit_width();
            if bits == 0 || bits >= 128 {
                return false;
            }
            let low_mask: u128 = (1u128 << bits) - 1;
            // The mask must cover at least the low W bits — anything beyond
            // that is fine since the truncate will drop those bits.
            c_val & low_mask == low_mask
        });
        boxed_rule(rewrite_rule(pat, truncate(var(x))))
    };

    let rules: Vec<BoxedRule> = vec![
        rule_int_float,
        rule_float_int,
        zext_round_trip,
        sext_round_trip,
        narrow_mul_through_sext,
        mk_drop_high_half(false),
        mk_drop_high_half(true),
        mk_drop_low_mask_under_truncate(false),
        mk_drop_low_mask_under_truncate(true),
    ];
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
        BoxedRule, Pat, Capture, add, and, any_int_const, boxed_rule, int_const, mul, neg, or,
        rewrite_rule, shl, shr, sshr, sub, var, xor,
    };

    let x = Capture::new();
    // x & all_ones → x  (commutative). The all-ones mask depends on the
    // output width, so we use `.when_match()` to compare the captured
    // constant against the node's output-type all-ones value.
    let all_ones_rule = {
        let x = Capture::new();
        let c = Capture::new();
        let pat: Pat = and(var(x), any_int_const(c)).into();
        let pat = pat.when_match(move |fg, ty, b| {
            b.get_uint(c, fg) == ty.get_unsigned_int(u128::MAX)
        });
        boxed_rule(rewrite_rule(pat, var(x)))
    };
    // x ^ all_ones → ~x  (commutative).  Clang lowers `~a` to `xor a, -1`
    // on PPC at -O0 (gcc emits the `nor` instruction → IntUnaryOp::Neg);
    // canonicalize so downstream consumers see one shape regardless of
    // compiler choice.
    let xor_all_ones_rule = {
        let x = Capture::new();
        let c = Capture::new();
        let pat: Pat = xor(var(x), any_int_const(c)).into();
        let pat = pat.when_match(move |fg, ty, b| {
            b.get_uint(c, fg) == ty.get_unsigned_int(u128::MAX)
        });
        boxed_rule(rewrite_rule(pat, neg(var(x))))
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
        xor_all_ones_rule,
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
        BoxedRule, Capture, any_bool_const, any_int_const, bool_const_with, boxed_rule,
        cast_to_bool, cast_to_int, int_binary_any, int_cmp_any, int_const_with, int_unary_any,
        lzcount, popcount, rewrite_rule, sign_extend, truncate, zero_extend,
    };

    let rules: Vec<BoxedRule> = vec![
        // 1. IntBinaryOp(op)(IntConst(l), IntConst(r)) =>
        //        int_const(eval_int_binary(op, l, r, ty)?, ty)
        //    `eval_int_binary` returns `None` for div-by-zero / signed
        //    overflow / U128+ masking failures; the closure opts out of the
        //    rewrite in that case via `pattern::skip()`.
        {
            let op = Capture::new();
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                int_binary_any(op, any_int_const(l), any_int_const(r)),
                int_const_with!([op: int_binary_op, l: uint, r: uint, ty] =>
                    eval_int_binary(op, l, r, ty)
                        .ok_or_else(pattern::skip)?
                ),
            ))
        },
        // 2. IntUnaryOp(op)(IntConst(v)) => int_const(op(v) masked to ty, ty)
        {
            let op = Capture::new();
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                int_unary_any(op, any_int_const(v)),
                int_const_with!([op: int_unary_op, v: uint, ty] => {
                    // The IR's enum names follow Sleigh's counter-intuitive
                    // convention (see arithmetic.rs comments and analyzer
                    // insn dispatch):
                    //   `IntUnaryOp::Neg` is BITWISE NOT (Sleigh `IntNeg`).
                    //   `IntUnaryOp::Not` is TWO'S COMPLEMENT (Sleigh `Int2Comp`).
                    let raw = match op {
                        IntUnaryOp::Neg => !v,
                        IntUnaryOp::Not => v.wrapping_neg(),
                    };
                    ty.get_unsigned_int(raw).ok_or_else(pattern::skip)?
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
            let op = Capture::new();
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                int_cmp_any(op, any_int_const(l), any_int_const(r)),
                bool_const_with!([op: int_cmp_op, l: uint, r: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::skip)?;
                    eval_int_cmp(op, l, r, input_ty)?
                }),
            ))
        },
        // 4. Truncate(IntConst(v)) => int_const(v masked to ty, ty)
        //    The wider IntConst's raw value is *not* automatically masked
        //    to the truncate's output width — `make_int_const` stores raw
        //    u64s. Mask explicitly here so we don't plant an unmasked
        //    narrow IntConst into the IR. Skip when ty is U128/U256 (the
        //    truncate output is always narrower than U64 in practice, but
        //    the skip costs nothing and is consistent with other rules).
        {
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                truncate(any_int_const(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(pattern::skip)?
                ),
            ))
        },
        // 5. ZeroExtend(IntConst(v)) => int_const(v, ty)
        {
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                zero_extend(any_int_const(v)),
                int_const_with!([v: uint] => v),
            ))
        },
        // 6. SignExtend(IntConst(v)) =>
        //        int_const(sign_extend(v, in_ty) masked to ty, ty)
        //    `in_ty` is the narrower input type; `get_signed_int` produces
        //    the sign-extended i128 value, which `get_unsigned_int` then
        //    masks to the wider output width.
        {
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                sign_extend(any_int_const(v)),
                int_const_with!([v: uint, in_ty, ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::skip)?;
                    let signed = input_ty
                        .get_signed_int(v)
                        .ok_or_else(|| anyhow::anyhow!("expected integer type, got {input_ty:?}"))?
                        as u128;
                    ty.get_unsigned_int(signed).ok_or_else(pattern::skip)?
                }),
            ))
        },
        // 7. Popcount(IntConst(v)) =>
        //        int_const(masked(v, in_ty).count_ones(), ty)
        {
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                popcount(any_int_const(v)),
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::skip)?;
                    let masked = input_ty
                        .get_unsigned_int(v)
                        .ok_or_else(pattern::skip)?;
                    u128::from(masked.count_ones())
                }),
            ))
        },
        // 8. Lzcount(IntConst(v)) =>
        //        int_const(N if masked == 0 else (masked << (128 - N)).leading_zeros(), ty)
        //    The `masked == 0` case must return the input type's bit width;
        //    shifting by (128 - bits) aligns to the u128's MSB so
        //    `leading_zeros()` gives the correct count within the type's width.
        {
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                lzcount(any_int_const(v)),
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::skip)?;
                    let masked = input_ty
                        .get_unsigned_int(v)
                        .ok_or_else(pattern::skip)?;
                    let bits = input_ty.bit_width() as u32;
                    // Lzcount fold is only computable when the input type
                    // fits in u128.  Wider widths (U256) skip cleanly — the
                    // rule simply doesn't fire and the IR keeps the Lzcount
                    // node as opaque.
                    if bits > 128 {
                        return Err(pattern::skip());
                    }
                    if masked == 0 {
                        u128::from(bits)
                    } else if bits == 128 {
                        u128::from(masked.leading_zeros())
                    } else {
                        u128::from((masked << (128 - bits)).leading_zeros())
                    }
                }),
            ))
        },
        // 9. CastToBool(IntConst(v)) => bool_const(v != 0)
        {
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                cast_to_bool(any_int_const(v)),
                bool_const_with!([v: uint] => v != 0),
            ))
        },
        // 10. CastToInt(BoolConst(b)) => int_const(b as u128, ty)
        {
            let b = Capture::new();
            boxed_rule(rewrite_rule(
                cast_to_int(any_bool_const(b)),
                int_const_with!([b: bool] => u128::from(b)),
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
        BoxedRule, Capture, Pat, any_bool_const, any_float_const, bool_and, bool_or,
        bool_unary_any, bool_xor, boxed_rule, float_binary_any, float_cmp_any, float_unary_any,
        rewrite_rule,
    };
    use pattern::{bool_const_with, float_const_with};

    let rules: Vec<BoxedRule> = vec![
        // BAnd(BoolConst(l), BoolConst(r)) => bool_const(l && r)
        {
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                bool_and(any_bool_const(l), any_bool_const(r)),
                bool_const_with!([l: bool, r: bool] => l && r),
            ))
        },
        // BOr(BoolConst(l), BoolConst(r)) => bool_const(l || r)
        {
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                bool_or(any_bool_const(l), any_bool_const(r)),
                bool_const_with!([l: bool, r: bool] => l || r),
            ))
        },
        // BXor(BoolConst(l), BoolConst(r)) => bool_const(l ^ r)
        {
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                bool_xor(any_bool_const(l), any_bool_const(r)),
                bool_const_with!([l: bool, r: bool] => l ^ r),
            ))
        },
        // BAnd(BoolConst(false), _) => bool_const(false)  (absorbing element).
        // The constraint that the const is the absorbing value lives in the
        // pattern via `.when_match()`, so the rewrite closure is a literal.
        {
            let l = Capture::new();
            let pat: Pat = bool_and(any_bool_const(l), pattern::any()).into();
            let pat = pat.when_match(move |fg, _ty, b| b.get_bool(l, fg) == Some(false));
            boxed_rule(rewrite_rule(pat, bool_const_with!([] => false)))
        },
        // BOr(BoolConst(true), _) => bool_const(true)  (absorbing element)
        {
            let l = Capture::new();
            let pat: Pat = bool_or(any_bool_const(l), pattern::any()).into();
            let pat = pat.when_match(move |fg, _ty, b| b.get_bool(l, fg) == Some(true));
            boxed_rule(rewrite_rule(pat, bool_const_with!([] => true)))
        },
        // BoolUnaryOp(op)(BoolConst(v)) => bool_const(!v)
        {
            let op = Capture::new();
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                bool_unary_any(op, any_bool_const(v)),
                bool_const_with!([op: bool_unary_op, v: bool] => {
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
            let op = Capture::new();
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                float_binary_any(op, any_float_const(l), any_float_const(r)),
                float_const_with!([op: float_binary_op, l: float_bits, r: float_bits, ty] =>
                    eval_float_binary(op, l, r, ty)
                        .ok_or_else(pattern::skip)?
                ),
            ))
        },
        // FloatUnaryOp(op)(FloatConst(v)) => float_const(eval_float_unary(op, v, ty)?)
        {
            let op = Capture::new();
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                float_unary_any(op, any_float_const(v)),
                float_const_with!([op: float_unary_op, v: float_bits, ty] =>
                    eval_float_unary(op, v, ty)
                        .ok_or_else(pattern::skip)?
                ),
            ))
        },
        // FloatCmpOp(op)(FloatConst(l), FloatConst(r)) =>
        //     bool_const(eval_float_cmp(op, l, r, in_ty)?)
        //   `in_ty` = root's first-value-input type (the float operand type).
        {
            let op = Capture::new();
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                float_cmp_any(op, any_float_const(l), any_float_const(r)),
                bool_const_with!([op: float_cmp_op, l: float_bits, r: float_bits, in_ty] => {
                    let input_ty = in_ty.ok_or_else(pattern::skip)?;
                    eval_float_cmp(op, l, r, input_ty)
                        .ok_or_else(pattern::skip)?
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
