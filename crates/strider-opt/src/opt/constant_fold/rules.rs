use super::eval_float::{eval_float_binary, eval_float_cmp, eval_float_unary};
use super::eval_int::{eval_int_binary, eval_int_cmp};

use crate::rewrite_rule;
use strider_pattern::{
    Capture, CaptureExt, add, and, any_float_const, any_int_const, bool_const_with, bool_not,
    float_binary_any, float_bits_to_int, float_cmp_any, float_const_with, float_unary_any,
    int_binary_any, int_bits_to_float, int_cmp_any, int_const, int_const_with, int_unary_any,
    lzcount, mul, or, popcount, shl, shr, sign_extend, sshr, sub, template, truncate, var, xor,
    zero_extend,
};

/// Every constant-fold rule, in application order. The concatenation order
/// below is the application order; the group names carry no dispatch.
pub(super) fn build_rules() -> Vec<crate::BoxedRule> {
    let mut rules = build_identity_rules();
    rules.extend(build_const_eval_rules());
    rules.extend(build_bool_float_rules());
    rules.extend(build_reassoc_and_mask_rules());
    rules.extend(build_bitcast_extend_rules());
    rules
}

/// Reassociation and mask-merging: constant-folding across nested
/// `Add`/`Sub`/`And`, mask distribution, and const-on-right canonicalisation.
fn build_reassoc_and_mask_rules() -> Vec<crate::BoxedRule> {
    // One shared capture pool: each rule matches as an independent query with
    // fresh `Bindings`, so there is no cross-rule state.
    let (x, y, c1, c2, c3) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    // (x + C1) + C2 -> x + (C1 + C2)
    let rule_add_add = rewrite_rule(
        add(
            add(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        ),
        template::add(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        ),
    );

    // (x - C1) - C2 -> x - (C1 + C2)
    let rule_sub_sub = rewrite_rule(
        sub(
            sub(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        ),
        template::sub(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        ),
    );

    // (x + C1) - C2 -> x + (C1 - C2)
    let rule_add_sub = rewrite_rule(
        sub(
            add(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        ),
        template::add(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_sub(c2)),
        ),
    );

    // (x - C1) + C2 -> x + (C2 - C1)
    let rule_sub_add = rewrite_rule(
        add(
            sub(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        ),
        template::add(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c2.wrapping_sub(c1)),
        ),
    );

    // (x & C1) & C2 -> x & (C1 & C2)
    let rule_and_merge = rewrite_rule(
        and(
            and(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        ),
        template::and(var(x), int_const_with!([c1: uint, c2: uint] => c1 & c2)),
    );

    // ((x & C1) | (y & C2)) & C3 -> (x & (C1 & C3)) | (y & (C2 & C3))
    //
    // Gated on at least one product `Ci & C3` being zero, so that disjunct
    // collapses via `x & 0 -> 0` / `x | 0 -> x`. With both products non-zero the
    // distribution only pushes `& C3` inward, neither disjunct shrinks, the
    // factored `And(Or, C3)` shape regenerates, and the rule re-fires forever.
    let rule_and_dist = rewrite_rule(
        and(
            or(
                and(var(x), any_int_const().capture(c1)),
                and(var(y), any_int_const().capture(c2)),
            ),
            any_int_const().capture(c3),
        )
        .when_match(move |edit, _ty, binds| {
            let (Some(v1), Some(v2), Some(v3)) = (
                binds.get_uint(c1, edit.function()),
                binds.get_uint(c2, edit.function()),
                binds.get_uint(c3, edit.function()),
            ) else {
                return false;
            };
            (v1 & v3) == 0 || (v2 & v3) == 0
        }),
        template::or(
            template::and(var(x), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
            template::and(var(y), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
        ),
    );

    // (x | A) & B -> x & B   when A & B == 0
    //
    // Alignment idiom: ARM/Thumb dispatch emits `(load | 1) & 0xFFFFFFFE`, and
    // folding it lets the jump-table classifier see a single masked load.
    // Sound because every surviving bit (`B_i = 1`) has `A_i = 0`. Terminates
    // because the RHS is a plain `And`, never the `(_ | _) & _` LHS shape.
    let rule_align_or_removal = rewrite_rule(
        and(
            or(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        )
        .when_match(move |edit, _ty, binds| {
            let (Some(set_bits), Some(mask)) = (
                binds.get_uint(c1, edit.function()),
                binds.get_uint(c2, edit.function()),
            ) else {
                return false;
            };
            (set_bits & mask) == 0
        }),
        template::and(var(x), var(c2)),
    );

    // `op(C, x) -> op(x, C)` for each commutative int op, so equal `op(C, x)` /
    // `op(x, C)` dedup to one node.
    //
    // `.ordered()` is REQUIRED: it forbids the commutative operand swap so the
    // LHS matches only the const-on-left shape. Without it the matcher also
    // matches the already-canonical `op(x, C)` and the rule never terminates.
    // The `x`-not-const guard then prevents a `(C1, C2)` ping-pong; const-eval
    // folds those instead.
    //
    // One rule per op because the template DSL can't rebuild a binary node from
    // a captured op variant. `$op` names both the matcher builder and its
    // `template::` counterpart.
    macro_rules! const_on_right {
        ($op:ident) => {
            rewrite_rule(
                $op(any_int_const().capture(c1), var(x))
                    .ordered()
                    .when_match(move |edit, _ty, b| b.get_uint(x, edit.function()).is_none()),
                template::$op(var(x), var(c1)),
            )
        };
    }
    let const_on_right_add = const_on_right!(add);
    let const_on_right_mul = const_on_right!(mul);
    let const_on_right_and = const_on_right!(and);
    let const_on_right_or = const_on_right!(or);
    let const_on_right_xor = const_on_right!(xor);

    vec![
        rule_add_add,
        rule_sub_sub,
        rule_add_sub,
        rule_sub_add,
        rule_and_merge,
        rule_and_dist,
        rule_align_or_removal,
        const_on_right_add,
        const_on_right_mul,
        const_on_right_and,
        const_on_right_or,
        const_on_right_xor,
    ]
}

/// The low-`W` all-ones mask for a truncate's output width, or `None` at a
/// degenerate (0) or >= 128-bit width. Unlike
/// [`strider_ir::node::ValueType::bit_mask_u128`], which saturates to
/// `u128::MAX`, this bails so the truncate-fold guards skip those widths.
fn truncate_low_mask(ty: strider_ir::node::ValueType) -> Option<u128> {
    let bits = ty.bit_width();
    if bits == 0 || bits >= 128 {
        return None;
    }
    Some((1u128 << bits) - 1)
}

/// Bitcast, extend/truncate round-trip, and truncate-folding rules.
fn build_bitcast_extend_rules() -> Vec<crate::BoxedRule> {
    let (x, a, b, c) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    // IntBitsToFloat(FloatBitsToInt(x)) -> x
    let rule_int_float = rewrite_rule(int_bits_to_float(float_bits_to_int(var(x))), var(x));

    // FloatBitsToInt(IntBitsToFloat(x)) -> x
    let rule_float_int = rewrite_rule(float_bits_to_int(int_bits_to_float(var(x))), var(x));

    // Truncate(Extend(x)) -> x, but only when `x`'s type equals the truncate's
    // output type; otherwise this is a real narrowing, not an identity. The
    // guard checks exactly that against the rule root's `ty`.
    //
    // Without this, the extend-then-truncate round-trips write_reg_vn emits to
    // land in container width stay in the IR, and the matcher's data-flow walk
    // can't cross them to reach an inner Mul/Add.
    macro_rules! ext_round_trip {
        ($ext:ident) => {{
            let pat = truncate($ext(var(x))).when_match(move |edit, ty, bnd| {
                bnd.get_type(x, edit.function())
                    .is_some_and(|x_ty| x_ty == ty)
            });
            rewrite_rule(pat, var(x))
        }};
    }
    let zext_round_trip = ext_round_trip!(zero_extend);
    let sext_round_trip = ext_round_trip!(sign_extend);

    // Narrowing through a binop: `Truncate_W(op(SignExt(a), SignExt(b)))` ->
    // `op_W(a, b)`, valid only for ops whose lower W bits don't depend on the
    // upper bits (Add/Sub/Mul/And/Or/Xor). MIPS32 lifts `mul a, b` as a 32x32->64
    // IntMul plus a 32-bit Truncate, which otherwise blocks the matcher's
    // data-flow walk for `add(mul(_,_), _)`.
    //
    // Each (lhs_extend_kind, rhs_extend_kind) permutation needs its own rule:
    // the pattern crate's RHS can't reconstruct a non-const node from a captured
    // op variant. Only the (SignExt, SignExt) Mul case is needed so far.
    let narrow_mul_through_sext = {
        let pat = truncate(mul(sign_extend(var(a)), sign_extend(var(b)))).when_match(
            move |edit, ty, bnd| {
                bnd.get_type(a, edit.function())
                    .is_some_and(|a_ty| a_ty == ty)
                    && bnd
                        .get_type(b, edit.function())
                        .is_some_and(|b_ty| b_ty == ty)
            },
        );
        rewrite_rule(pat, template::mul(var(a), var(b)))
    };

    // Drop the high half of an x86 register-merge Or when truncating to the low
    // half's width: a read of `$eax` after `mov $eax, ...` lifts to
    // `Truncate_U32(Or(And(high_mask, rax_old), And(low_mask, zext(eax))))`, and
    // the high mask contributes nothing below bit 32. The guard picks the
    // high-mask And by requiring its low-`W` bits to be zero.
    //
    // BOTH `Or` operands are `And`s, so the `and(...)` subpattern matches either
    // one structurally and only the guard disambiguates. A single orientation
    // still suffices: the matcher is continuation-passing (matcher/walk.rs), so
    // a guard failure re-drives the `Or`'s operand order even though the guard
    // sits on the truncate ancestor. Regression-guarded by
    // `test_narrow_widths::x64`.
    let mk_drop_high_half = {
        let guard = move |edit: &strider_pattern::Matcher,
                          ty: strider_ir::node::ValueType,
                          bnd: &strider_pattern::Bindings| {
            let Some(c_val) = bnd.get_uint(c, edit.function()) else {
                return false;
            };
            let Some(low_mask) = truncate_low_mask(ty) else {
                return false;
            };
            c_val & low_mask == 0
        };
        rewrite_rule(
            truncate(or(var(a), and(any_int_const().capture(c), var(b)))).when_match(guard),
            template::truncate(var(a)),
        )
    };

    // `Truncate_W(And(low_W_mask, x)) -> Truncate_W(x)`: the And zeroes bits the
    // truncate discards anyway. One orientation suffices because the non-const
    // operand fails `any_int_const` structurally, so the `And`'s commutative
    // retry binds `c` to the const on either side (a two-const `And` is
    // const-folded before this rule sees it).
    let mk_drop_low_mask_under_truncate = {
        let guard = move |edit: &strider_pattern::Matcher,
                          ty: strider_ir::node::ValueType,
                          bnd: &strider_pattern::Bindings| {
            let Some(c_val) = bnd.get_uint(c, edit.function()) else {
                return false;
            };
            let Some(low_mask) = truncate_low_mask(ty) else {
                return false;
            };
            // The mask must cover at least the low W bits; anything beyond that
            // is fine since the truncate drops those bits.
            c_val & low_mask == low_mask
        };
        rewrite_rule(
            truncate(and(any_int_const().capture(c), var(x))).when_match(guard),
            template::truncate(var(x)),
        )
    };

    // Nested SAME-kind casts are transitive, so the intermediate width drops out
    // and the RHS inherits the outer width. MIXED-kind nests (zext of sext, and
    // vice versa) are not a single cast and are left alone.
    //
    // This is what lets the doubly-zero-extended compare MIPS emits for `sltu`
    // collapse to a single extend, so FlagCmpCanonicalize's
    // `Equal(ZeroExtend(b:I1), 0)` rule can fire.
    let zext_zext = rewrite_rule(
        zero_extend(zero_extend(var(x))),
        template::zero_extend(var(x)),
    );
    let sext_sext = rewrite_rule(
        sign_extend(sign_extend(var(x))),
        template::sign_extend(var(x)),
    );
    let trunc_trunc = rewrite_rule(truncate(truncate(var(x))), template::truncate(var(x)));

    vec![
        rule_int_float,
        rule_float_int,
        zext_round_trip,
        sext_round_trip,
        zext_zext,
        sext_sext,
        trunc_trunc,
        narrow_mul_through_sext,
        mk_drop_high_half,
        mk_drop_low_mask_under_truncate,
    ]
}

/// Algebraic identities for integer binary operations.
fn build_identity_rules() -> Vec<crate::BoxedRule> {
    let (x, c) = (Capture::new(), Capture::new());
    // All-ones is output-width-dependent, so the guard has to compare `c`
    // against the per-match output type rather than a fixed constant.
    let is_all_ones = move |edit: &strider_pattern::Matcher,
                            ty: strider_ir::node::ValueType,
                            b: &strider_pattern::Bindings| {
        b.get_uint(c, edit.function()) == ty.get_unsigned_int(u128::MAX)
    };
    // x & all_ones -> x
    let all_ones_rule = rewrite_rule(
        and(var(x), any_int_const().capture(c)).when_match(is_all_ones),
        var(x),
    );
    // There is deliberately no `x ^ all_ones -> ~x` rule: `Xor(x, all_ones)` IS
    // the canonical complement shape, and both compiler lowerings (`nor`,
    // `xor a, -1`) already lift straight to it.

    // x | all_ones -> all_ones. At I1 this subsumes `x | true -> true`.
    let or_all_ones_rule = rewrite_rule(
        or(var(x), any_int_const().capture(c)).when_match(is_all_ones),
        var(c),
    );

    // The commutative ops below match both operand orders; the shift rules do
    // not, so only a zero on the right is an identity for them.
    vec![
        rewrite_rule(add(var(x), int_const(0u128)), var(x)),
        rewrite_rule(sub(var(x), int_const(0u128)), var(x)),
        rewrite_rule(sub(var(x), var(x)), int_const(0u128)),
        rewrite_rule(xor(var(x), var(x)), int_const(0u128)),
        rewrite_rule(xor(var(x), int_const(0u128)), var(x)),
        rewrite_rule(mul(var(x), int_const(0u128)), int_const(0u128)),
        rewrite_rule(mul(var(x), int_const(1u128)), var(x)),
        rewrite_rule(and(var(x), int_const(0u128)), int_const(0u128)),
        rewrite_rule(and(var(x), var(x)), var(x)),
        rewrite_rule(or(var(x), int_const(0u128)), var(x)),
        rewrite_rule(or(var(x), var(x)), var(x)),
        rewrite_rule(shl(var(x), int_const(0u128)), var(x)),
        rewrite_rule(shr(var(x), int_const(0u128)), var(x)),
        rewrite_rule(sshr(var(x), int_const(0u128)), var(x)),
        all_ones_rule,
        or_all_ones_rule,
    ]
}

/// Width-consistency guard for the integer const-eval folds.
///
/// [`eval_int_binary`] / [`eval_int_cmp`] mask every operand to a single width
/// (the output width, or the LHS width for comparisons), which is only correct
/// when every operand already carries that width. The lifter guarantees that;
/// the validator does not, since `IntBinaryOp` / `IntCmpOp` inputs are typed
/// `AnyInt`. Skip rather than fold against a silently re-masked operand.
fn require_operand_widths(
    edit: &strider_pattern::TemplateCtx<'_>,
    operands: &[Capture],
    expected: strider_ir::node::ValueType,
) -> crate::error::Result<()> {
    let expected_bits = expected.bit_width();
    for &c in operands {
        let ty = edit
            .bindings
            .get_type(c, edit.function)
            .ok_or_else(strider_pattern::skip)?;
        if ty.bit_width() != expected_bits {
            return Err(strider_pattern::skip());
        }
    }
    Ok(())
}

fn build_const_eval_rules() -> Vec<crate::BoxedRule> {
    let (op, l, r, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    vec![
        // `eval_int_binary` returns `None` for div-by-zero, signed overflow and
        // I128+ masking failures; the closure turns that into a rewrite skip.
        // The width guard protects against a wider operand (say a shift amount)
        // whose value the output-width mask would silently change.
        {
            rewrite_rule(
                int_binary_any(any_int_const().capture(l), any_int_const().capture(r)).capture(op),
                strider_pattern::int_const_with_fn(move |edit| {
                    let ty = edit.root_ty;
                    require_operand_widths(edit, &[l, r], ty)?;
                    let op = edit
                        .bindings
                        .get_int_binary_op(op, edit.function.graph())
                        .ok_or_else(|| strider_pattern::missing_binding("int_binary_op"))?;
                    let l = edit
                        .bindings
                        .get_uint(l, edit.function)
                        .ok_or_else(strider_pattern::skip)?;
                    let r = edit
                        .bindings
                        .get_uint(r, edit.function)
                        .ok_or_else(strider_pattern::skip)?;
                    eval_int_binary(op, l, r, ty).ok_or_else(strider_pattern::skip)
                }),
            )
        },
        // `IntUnaryOp` has only `Neg`; bitwise complement is `Xor(x, all_ones)`
        // and folds through the int-binary rule above.
        {
            rewrite_rule(
                int_unary_any(any_int_const().capture(v)).capture(op),
                int_const_with!([op: int_unary_op, v: uint, ty] =>
                    super::eval_int::eval_int_unary(op, v, ty).ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        // The root's first-value-input type is the LHS operand type, which is
        // what `eval_int_cmp` masks both operands to. A wider RHS masked down
        // that far can flip a `Less` / `Sless` / `Carry` verdict, hence the
        // width guard.
        {
            rewrite_rule(
                int_cmp_any(any_int_const().capture(l), any_int_const().capture(r)).capture(op),
                strider_pattern::bool_const_with_fn(move |edit| {
                    let input_ty = strider_pattern::first_value_input_type(edit)
                        .ok_or_else(strider_pattern::skip)?;
                    require_operand_widths(edit, &[l, r], input_ty)?;
                    let op = edit
                        .bindings
                        .get_int_cmp_op(op, edit.function.graph())
                        .ok_or_else(|| strider_pattern::missing_binding("int_cmp_op"))?;
                    let l = edit
                        .bindings
                        .get_uint(l, edit.function)
                        .ok_or_else(strider_pattern::skip)?;
                    let r = edit
                        .bindings
                        .get_uint(r, edit.function)
                        .ok_or_else(strider_pattern::skip)?;
                    eval_int_cmp(op, l, r, input_ty)
                }),
            )
        },
        // The wider IntConst's raw value is not masked to the truncate's output
        // width for us, so mask explicitly; otherwise a narrow IntConst holding
        // wide bits lands in the IR.
        {
            rewrite_rule(
                truncate(any_int_const().capture(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        // `v: uint` already masks to the input's width and ZeroExtend only
        // widens, so the mask below is redundant today. Kept for symmetry with
        // the truncate rule, and to stay safe if `IntConst`'s u128 storage ever
        // widens.
        {
            rewrite_rule(
                zero_extend(any_int_const().capture(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        {
            rewrite_rule(
                sign_extend(any_int_const().capture(v)),
                int_const_with!([v: uint, in_ty, ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    super::eval_int::eval_sign_extend(v, input_ty, ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
            )
        },
        {
            rewrite_rule(
                popcount(any_int_const().capture(v)),
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    super::eval_int::eval_popcount(v, input_ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
            )
        },
        {
            rewrite_rule(
                lzcount(any_int_const().capture(v)),
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    super::eval_int::eval_lzcount(v, input_ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
            )
        },
    ]
}

/// Const-eval and absorbing-element rules for the I1 boolean ops and all float
/// ops.
fn build_bool_float_rules() -> Vec<crate::BoxedRule> {
    let (op, l, r, v, x) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    // Booleans are 1-bit integers here, so the generic integer const-eval and
    // identity rules already cover almost every boolean fold: they fire at any
    // width including I1. Only shapes with no integer analogue belong here.
    vec![
        // !!x -> x. Nothing collapses a double complement at integer width, and
        // pcode lifting of compare-and-invert idioms does produce chained NOTs.
        { rewrite_rule(bool_not(bool_not(var(x))), var(x)) },
        {
            rewrite_rule(
                float_binary_any(any_float_const().capture(l), any_float_const().capture(r))
                    .capture(op),
                float_const_with!([op: float_binary_op, l: float_bits, r: float_bits, ty] =>
                    eval_float_binary(op, l, r, ty)
                        .ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        {
            rewrite_rule(
                float_unary_any(any_float_const().capture(v)).capture(op),
                float_const_with!([op: float_unary_op, v: float_bits, ty] =>
                    eval_float_unary(op, v, ty)
                        .ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        // `in_ty` is the root's first-value-input type, i.e. the float operand
        // type, not the I1 output.
        {
            rewrite_rule(
                float_cmp_any(any_float_const().capture(l), any_float_const().capture(r))
                    .capture(op),
                bool_const_with!([op: float_cmp_op, l: float_bits, r: float_bits, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    eval_float_cmp(op, l, r, input_ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
            )
        },
    ]
}
