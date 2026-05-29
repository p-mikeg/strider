use std::sync::LazyLock;

use strider_ir::node::NodeId;
use strider_ir::IntUnaryOp;

use crate::opt::error::Result;
use crate::opt::pipeline::OptimizationResult;

use super::eval_float::{eval_float_binary, eval_float_cmp, eval_float_unary};
use super::eval_int::{eval_int_binary, eval_int_cmp};

/// Runs every constant-fold rule group on `node`, OR-ing the per-group
/// `bool` change flags.
///
/// The five `LazyLock` rule statics keep their semantic grouping
/// (identity / const-eval / bool-float / reassoc-and-mask /
/// bitcast-extend) for readers, but the per-group wrapper functions
/// they used to feed were byte-identical boilerplate — this single
/// entry point replaces them.  The bitcast-extend group includes the
/// `IntBitsToFloat`/`FloatBitsToInt` round-trip identities, so int↔float
/// bitcasts are folded inline; there is no separate lowering step.
pub(super) fn apply_all_rules(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    node: NodeId,
) -> Result<OptimizationResult> {
    use crate::pattern::apply_rules_in_order;
    let mut changed = false;
    for group in [
        &*IDENTITY_RULES,
        &*CONST_EVAL_RULES,
        &*BOOL_FLOAT_RULES,
        &*REASSOC_AND_MASK_RULES,
        &*BITCAST_EXTEND_RULES,
    ] {
        if apply_rules_in_order(group)(ctx, node)? {
            changed = true;
        }
    }
    Ok(OptimizationResult::from_changed(changed))
}

// ── per-node folding ──────────────────────────────────────────────────────────

/// Builds the rule vec for [`REASSOC_AND_MASK_RULES`].
fn build_reassoc_and_mask_rules() -> Vec<crate::pattern::BoxedRule> {
    use crate::pattern::macros::int_const_with;
    use crate::pattern::{
        BoxedRule, Capture, add, and, any_int_const, boxed_rule, or,
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

/// Add/sub reassociation and AND-mask merging rules.
///
/// Rules:
/// - `(x + C1) + C2 → x + (C1 + C2)`
/// - `(x - C1) - C2 → x - (C1 + C2)`
/// - `(x + C1) - C2 → x + (C1 - C2)`
/// - `(a & C1) & C2 → a & (C1 & C2)`
/// - `((a & C1) | (b & C2)) & C3 → (a & (C1 & C3)) | (b & (C2 & C3))`
static REASSOC_AND_MASK_RULES: LazyLock<Vec<crate::pattern::BoxedRule>> =
    LazyLock::new(build_reassoc_and_mask_rules);

/// Builds the rule vec for [`BITCAST_EXTEND_RULES`].
fn build_bitcast_extend_rules() -> Vec<crate::pattern::BoxedRule> {
    use crate::pattern::{
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
        let pat = truncate(zero_extend(var(x))).when_match(move |ctx, ty, b| {
            b.get(x)
                .and_then(|out| ctx.output_kind(out).as_value())
                .is_some_and(|x_ty| x_ty == ty)
        });
        boxed_rule(rewrite_rule(pat, var(x)))
    };

    // Truncate(SignExtend(x)) → x — same identity at the bit level when
    // widths match (sign-extension's added bits are sign replication; the
    // truncate cuts them off and recovers the original bits).
    let sext_round_trip = {
        let x = Capture::new();
        let pat = truncate(sign_extend(var(x))).when_match(move |ctx, ty, b| {
            b.get(x)
                .and_then(|out| ctx.output_kind(out).as_value())
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
    use crate::pattern::mul as mul_pat;
    let narrow_mul_through_sext = {
        let a = Capture::new();
        let b = Capture::new();
        let pat = truncate(mul_pat(sign_extend(var(a)), sign_extend(var(b)))).when_match(
            move |ctx, ty, bnd| {
                bnd.get(a)
                    .and_then(|out| ctx.output_kind(out).as_value())
                    .is_some_and(|a_ty| a_ty == ty)
                    && bnd
                        .get(b)
                        .and_then(|out| ctx.output_kind(out).as_value())
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
    use crate::pattern::{and, any_int_const, or};
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
        let pat = truncate(inner).when_match(move |ctx, ty, bnd| {
            let Some(c_val) = bnd.get_uint(c, ctx) else { return false; };
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
        let pat = truncate(inner).when_match(move |ctx, ty, bnd| {
            let Some(c_val) = bnd.get_uint(c, ctx) else { return false; };
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

/// Bitcast, extend/truncate round-trip, and truncate-folding rules:
/// - `IntBitsToFloat(FloatBitsToInt(x)) → x`
/// - `FloatBitsToInt(IntBitsToFloat(x)) → x`
/// - `Truncate(ZeroExtend(x)) → x` / `Truncate(SignExtend(x)) → x` (width-matched)
/// - narrow-mul-through-sext: `Truncate(Mul(SignExt(a), SignExt(b))) → Mul(a, b)`
/// - drop-high-half of a register-merge `Or` under a truncate
/// - drop a redundant low-bits AND mask under a truncate
static BITCAST_EXTEND_RULES: LazyLock<Vec<crate::pattern::BoxedRule>> =
    LazyLock::new(build_bitcast_extend_rules);

/// Builds the rule vec for [`IDENTITY_RULES`].
fn build_identity_rules() -> Vec<crate::pattern::BoxedRule> {
    use crate::pattern::{
        BoxedRule, Pat, Capture, add, and, any_int_const, bit_not, boxed_rule, int_const, mul, or,
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
        let pat = pat.when_match(move |ctx, ty, b| {
            b.get_uint(c, ctx) == ty.get_unsigned_int(u128::MAX)
        });
        boxed_rule(rewrite_rule(pat, var(x)))
    };
    // x ^ all_ones → ~x  (commutative).  Clang lowers `~a` to `xor a, -1`
    // on PPC at -O0 (gcc emits the `nor` instruction → IntUnaryOp::BitNot);
    // canonicalize so downstream consumers see one shape regardless of
    // compiler choice.
    let xor_all_ones_rule = {
        let x = Capture::new();
        let c = Capture::new();
        let pat: Pat = xor(var(x), any_int_const(c)).into();
        let pat = pat.when_match(move |ctx, ty, b| {
            b.get_uint(c, ctx) == ty.get_unsigned_int(u128::MAX)
        });
        boxed_rule(rewrite_rule(pat, bit_not(var(x))))
    };
    // x | all_ones → all_ones  (commutative; absorbing element).  The
    // all-ones value depends on the output width, so match the captured
    // constant against the node's all-ones mask and rewrite to that same
    // constant.  At `I1` this subsumes the boolean `x | true → true`.
    let or_all_ones_rule = {
        let x = Capture::new();
        let c = Capture::new();
        let pat: Pat = or(var(x), any_int_const(c)).into();
        let pat = pat.when_match(move |ctx, ty, b| {
            b.get_uint(c, ctx) == ty.get_unsigned_int(u128::MAX)
        });
        boxed_rule(rewrite_rule(pat, var(c)))
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
        or_all_ones_rule,
    ];
    rules
}

/// Algebraic identities for integer binary operations.
///
/// Rules ported from hand-written arms:
/// - `x + 0 → x`, `x - 0 → x`, `x - x → 0`
/// - `x ^ x → 0`, `x ^ 0 → x`
/// - `x * 0 → 0`, `x * 1 → x`
/// - `x & 0 → 0`, `x & x → x`, `x & all_ones → x`
/// - `x | 0 → x`, `x | x → x`
/// - `x << 0 → x`, `x >> 0 → x`, `x >>> 0 → x`
static IDENTITY_RULES: LazyLock<Vec<crate::pattern::BoxedRule>> = LazyLock::new(build_identity_rules);

/// Builds the rule vec for [`CONST_EVAL_RULES`].
fn build_const_eval_rules() -> Vec<crate::pattern::BoxedRule> {
    use crate::pattern::macros::{bool_const_with, int_const_with};
    use crate::pattern::{
        BoxedRule, Capture, any_int_const, boxed_rule, int_binary_any, int_cmp_any, int_unary_any,
        lzcount, popcount, rewrite_rule, sign_extend, truncate, zero_extend,
    };

    let rules: Vec<BoxedRule> = vec![
        // 1. IntBinaryOp(op)(IntConst(l), IntConst(r)) =>
        //        int_const(eval_int_binary(op, l, r, ty)?, ty)
        //    `eval_int_binary` returns `None` for div-by-zero / signed
        //    overflow / I128+ masking failures; the closure opts out of the
        //    rewrite in that case via `crate::pattern::skip()`.
        {
            let op = Capture::new();
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                int_binary_any(op, any_int_const(l), any_int_const(r)),
                int_const_with!([op: int_binary_op, l: uint, r: uint, ty] =>
                    eval_int_binary(op, l, r, ty)
                        .ok_or_else(crate::pattern::skip)?
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
                    let raw = match op {
                        IntUnaryOp::BitNot => !v,
                        IntUnaryOp::Neg => v.wrapping_neg(),
                    };
                    ty.get_unsigned_int(raw).ok_or_else(crate::pattern::skip)?
                }),
            ))
        },
        // 3. IntCmpOp(op)(IntConst(l), IntConst(r)) =>
        //        bool_const(eval_int_cmp(op, l, r, in_ty)?)
        //    `in_ty` = root's first-value-input type, which on an IntCmp is
        //    the LHS operand type — exactly what `eval_int_cmp` expects.
        //    `eval_int_cmp` returns `Result<bool, opt::ErrorKind>`; bridge
        //    that failure through `crate::pattern::Error::rewrite_closure(...)`.
        {
            let op = Capture::new();
            let l = Capture::new();
            let r = Capture::new();
            boxed_rule(rewrite_rule(
                int_cmp_any(op, any_int_const(l), any_int_const(r)),
                bool_const_with!([op: int_cmp_op, l: uint, r: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(crate::pattern::skip)?;
                    eval_int_cmp(op, l, r, input_ty)?
                }),
            ))
        },
        // 4. Truncate(IntConst(v)) => int_const(v masked to ty, ty)
        //    The wider IntConst's raw value is *not* automatically masked
        //    to the truncate's output width — `make_int_const` stores raw
        //    u64s. Mask explicitly here so we don't plant an unmasked
        //    narrow IntConst into the IR. Skip when ty is I128/I256 (the
        //    truncate output is always narrower than I64 in practice, but
        //    the skip costs nothing and is consistent with other rules).
        {
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                truncate(any_int_const(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(crate::pattern::skip)?
                ),
            ))
        },
        // 5. ZeroExtend(IntConst(v)) => int_const(v, ty)
        //
        // `v: uint` already masks to the IntConst input's width, and
        // ZeroExtend's output width is by definition >= the input
        // width, so `v` is already small enough to fit the output.
        // Mask defensively against the output type anyway: rule 4
        // (Truncate) does the same thing and the symmetry keeps the
        // build path safe under future widenings of `IntConst`'s
        // u128 storage.
        {
            let v = Capture::new();
            boxed_rule(rewrite_rule(
                zero_extend(any_int_const(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(crate::pattern::skip)?
                ),
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
                    let input_ty = in_ty.ok_or_else(crate::pattern::skip)?;
                    let signed = input_ty
                        .get_signed_int(v)
                        .ok_or_else(|| anyhow::anyhow!("expected integer type, got {input_ty:?}"))?
                        as u128;
                    ty.get_unsigned_int(signed).ok_or_else(crate::pattern::skip)?
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
                    let input_ty = in_ty.ok_or_else(crate::pattern::skip)?;
                    let masked = input_ty
                        .get_unsigned_int(v)
                        .ok_or_else(crate::pattern::skip)?;
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
                    let input_ty = in_ty.ok_or_else(crate::pattern::skip)?;
                    let masked = input_ty
                        .get_unsigned_int(v)
                        .ok_or_else(crate::pattern::skip)?;
                    let bits = input_ty.bit_width() as u32;
                    // Lzcount fold is only computable when the input type
                    // fits in u128.  Wider widths (I256) skip cleanly — the
                    // rule simply doesn't fire and the IR keeps the Lzcount
                    // node as opaque.
                    if bits > 128 {
                        return Err(crate::pattern::skip());
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
    ];
    rules
}

/// Full constant evaluation for integer binary ops, integer unary ops,
/// integer comparisons, truncate, extend (zero/sign), popcount, and lzcount.
/// Boolean ops are 1-bit integers, so they fold through the same integer
/// rules at `I1`.
static CONST_EVAL_RULES: LazyLock<Vec<crate::pattern::BoxedRule>> = LazyLock::new(build_const_eval_rules);

/// Builds the rule vec for [`BOOL_FLOAT_RULES`].
fn build_bool_float_rules() -> Vec<crate::pattern::BoxedRule> {
    use crate::pattern::{
        BoxedRule, Capture, any_float_const, bool_not, boxed_rule, float_binary_any,
        float_cmp_any, float_unary_any, rewrite_rule, var,
    };
    use crate::pattern::macros::{bool_const_with, float_const_with};

    // Booleans are 1-bit (`I1`) integers in this IR.  Most boolean
    // const-folds / identities are therefore already covered by the
    // generic integer rules in `build_const_eval_rules` /
    // `build_identity_rules`, which fire at any width incl. `I1`:
    //   - `BAnd/BOr/BXor(IntConst, IntConst)`     → integer rule 1
    //     (`IntBinaryOp(op)(IntConst, IntConst)`).
    //   - `BAnd(false, _) → false`                → `x & 0 → 0`.
    //   - `BitNot(IntConst) → !v` at `I1` (logical not) → integer rule 2
    //     (`IntUnaryOp(op)(IntConst)`, with `BitNot` masked to `I1`).
    //   - `x ^ true → !x`                         → `x ^ all_ones → ~x`.
    // Only the rules with no integer analogue are re-expressed here at
    // `I1`: `BOr(true, _) → true` (no `x | all_ones → all_ones` integer
    // rule) and `!!x → x` (no double-`BitNot` integer rule).
    let rules: Vec<BoxedRule> = vec![
        // (`x | true → true` is handled generically by the integer
        // `x | all_ones → all_ones` rule — at I1, `true` is the all-ones
        // value — so no I1-specific Or-absorbing rule is needed here.)
        // !!x → x  (double-negation elimination — `BitNot(BitNot(x))` at
        // I1).  Compilers can produce chained NOTs through pcode lifting of
        // compare-and-invert idioms.  No general double-`BitNot` integer
        // rule exists, so this is re-expressed via the I1 `bool_not` ctor.
        {
            let x = Capture::new();
            boxed_rule(rewrite_rule(
                bool_not(bool_not(var(x))),
                var(x),
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
                        .ok_or_else(crate::pattern::skip)?
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
                        .ok_or_else(crate::pattern::skip)?
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
                    let input_ty = in_ty.ok_or_else(crate::pattern::skip)?;
                    eval_float_cmp(op, l, r, input_ty)
                        .ok_or_else(crate::pattern::skip)?
                }),
            ))
        },
    ];
    rules
}

/// Constant evaluation and absorbing-element rules for the I1 boolean ops
/// (`IntBinaryOp`/`IntUnaryOp` at `I1`) and all float ops.  Also
/// canonicalises:
/// - `x ^ true → !x` (commutative)
/// - `!!x → x`
static BOOL_FLOAT_RULES: LazyLock<Vec<crate::pattern::BoxedRule>> = LazyLock::new(build_bool_float_rules);
