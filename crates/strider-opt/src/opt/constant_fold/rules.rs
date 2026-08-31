use super::eval_float::{eval_float_binary, eval_float_cmp, eval_float_unary};
use super::eval_int::{eval_int_binary, eval_int_cmp};

use crate::rewrite_rule;
use strider_pattern::{
    Capture, CaptureExt, any_float_binary, any_float_cmp, any_float_unary, any_int_binary,
    any_int_cmp, any_int_unary, bool_const_with, bool_not, float_bits_to_int, float_const,
    float_const_with, int_add, int_and, int_bits_to_float, int_const, int_const_with, int_lzcount,
    int_mul, int_neg, int_or, int_popcount, int_shl, int_shr, int_sign_extend, int_sshr, int_sub,
    int_truncate, int_xor, int_zero_extend, template, var,
};

/// Every constant-fold rule, in application order.
pub(super) fn build_rules() -> Vec<crate::BoxedRule> {
    let mut rules = build_identity_rules();
    rules.extend(build_const_eval_rules());
    rules.extend(build_bool_float_rules());
    rules.extend(build_reassoc_and_mask_rules());
    rules.extend(build_factor_rules());
    rules.extend(build_bitcast_extend_rules());
    rules
}

/// Whether every listed capture carries `ty`'s bit width.
///
/// These rules move a constant across an operator boundary, which is an
/// identity only when the whole shape evaluates at one width: reassociating
/// `Add(Add(x:I8, C1:I8):I8, C2:I16):I16` drops the inner truncation and
/// changes the value. `validate` rejects such a node, but a rewrite can mint
/// one mid-pipeline, where nothing has validated yet.
fn all_same_width(
    edit: &strider_pattern::Matcher,
    binds: &strider_pattern::Bindings,
    ty: strider_ir::node::ValueType,
    caps: &[Capture],
) -> bool {
    caps.iter().all(|&c| {
        binds
            .get_type(c, edit.function())
            .is_some_and(|t| t.bit_width() == ty.bit_width())
    })
}

/// Whether a coefficient computed in `u128` carries `ty`'s modulus.
///
/// `bit_mask_u128` saturates at 128 bits, so `get_uint` still answers `Some` at
/// I256/I512 and the coefficient arithmetic would run in the `u128` modulus
/// rather than the type's: an I256 `C = 2^128 - 1` would give `C + 1 == 0`
/// instead of `2^128`. The shift forms get the same bound from
/// [`shift_in_range`].
fn coefficient_fits_carrier(ty: strider_ir::node::ValueType) -> bool {
    ty.bit_width() <= 128
}

/// Whether `cap` binds a shift count small enough to build `2^C` from.
///
/// A count is an ordinary constant of the shifted type, so an `I64` shift can
/// name 200 and `1u128 << 200` panics. The coefficient would be right without
/// this: an out-of-range count makes the p-code shift produce zero, and `2^C` is
/// also
/// zero once masked to the width, so both give `x`. The guard is about the
/// shift in this rule, not about the shift in the program.
fn shift_in_range(
    edit: &strider_pattern::Matcher,
    binds: &strider_pattern::Bindings,
    ty: strider_ir::node::ValueType,
    cap: Capture,
) -> bool {
    let Ok(width) = i128::try_from(ty.bit_width()) else {
        return false;
    };
    width <= 128
        && binds
            .get_int(cap, edit.function())
            .is_some_and(|c| (0..width).contains(&c))
}

/// Collecting a value against itself: `x + x*C` and friends become one `Mul`.
///
/// Subtraction arrives lowered to `Add(a, Neg(b))`, so the difference forms
/// match that shape rather than an `IntSub`.
fn build_factor_rules() -> Vec<crate::BoxedRule> {
    let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());

    // x + x*C -> x*(C + 1)
    let add_self_mul = rewrite_rule(
        int_add(var(x), int_mul(var(x), int_const(c1))).when_match(move |edit, ty, binds| {
            coefficient_fits_carrier(ty) && all_same_width(edit, binds, ty, &[x, c1])
        }),
        template::int_mul(var(x), int_const_with!([c1: uint] => c1.wrapping_add(1))),
    );

    // x*C1 + x*C2 -> x*(C1 + C2)
    let add_mul_mul = rewrite_rule(
        int_add(
            int_mul(var(x), int_const(c1)),
            int_mul(var(x), int_const(c2)),
        )
        .when_match(move |edit, ty, binds| {
            coefficient_fits_carrier(ty) && all_same_width(edit, binds, ty, &[x, c1, c2])
        }),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        ),
    );

    // x + (x << C) -> x*(2^C + 1)
    let add_self_shl = rewrite_rule(
        int_add(var(x), int_shl(var(x), int_const(c1))).when_match(move |edit, ty, binds| {
            all_same_width(edit, binds, ty, &[x]) && shift_in_range(edit, binds, ty, c1)
        }),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint] => (1u128 << c1).wrapping_add(1)),
        ),
    );

    // x - x*C -> x*(1 - C)
    let sub_self_mul = rewrite_rule(
        int_add(var(x), int_neg(int_mul(var(x), int_const(c1)))).when_match(
            move |edit, ty, binds| {
                coefficient_fits_carrier(ty) && all_same_width(edit, binds, ty, &[x, c1])
            },
        ),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint] => 1u128.wrapping_sub(c1)),
        ),
    );

    // x*C1 - x*C2 -> x*(C1 - C2)
    let sub_mul_mul = rewrite_rule(
        int_add(
            int_mul(var(x), int_const(c1)),
            int_neg(int_mul(var(x), int_const(c2))),
        )
        .when_match(move |edit, ty, binds| {
            coefficient_fits_carrier(ty) && all_same_width(edit, binds, ty, &[x, c1, c2])
        }),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_sub(c2)),
        ),
    );

    // x - (x << C) -> x*(1 - 2^C)
    let sub_self_shl = rewrite_rule(
        int_add(var(x), int_neg(int_shl(var(x), int_const(c1)))).when_match(
            move |edit, ty, binds| {
                all_same_width(edit, binds, ty, &[x]) && shift_in_range(edit, binds, ty, c1)
            },
        ),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint] => 1u128.wrapping_sub(1u128 << c1)),
        ),
    );

    // x*C1 + (x << C2) -> x*(C1 + 2^C2)
    let add_mul_shl = rewrite_rule(
        int_add(
            int_mul(var(x), int_const(c1)),
            int_shl(var(x), int_const(c2)),
        )
        .when_match(move |edit, ty, binds| {
            coefficient_fits_carrier(ty)
                && all_same_width(edit, binds, ty, &[x, c1])
                && shift_in_range(edit, binds, ty, c2)
        }),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(1u128 << c2)),
        ),
    );

    // (x << C1) + (x << C2) -> x*(2^C1 + 2^C2)
    let add_shl_shl = rewrite_rule(
        int_add(
            int_shl(var(x), int_const(c1)),
            int_shl(var(x), int_const(c2)),
        )
        .when_match(move |edit, ty, binds| {
            all_same_width(edit, binds, ty, &[x])
                && shift_in_range(edit, binds, ty, c1)
                && shift_in_range(edit, binds, ty, c2)
        }),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint, c2: uint] => (1u128 << c1).wrapping_add(1u128 << c2)),
        ),
    );

    // x*C1 - (x << C2) -> x*(C1 - 2^C2)
    let sub_mul_shl = rewrite_rule(
        int_add(
            int_mul(var(x), int_const(c1)),
            int_neg(int_shl(var(x), int_const(c2))),
        )
        .when_match(move |edit, ty, binds| {
            coefficient_fits_carrier(ty)
                && all_same_width(edit, binds, ty, &[x, c1])
                && shift_in_range(edit, binds, ty, c2)
        }),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_sub(1u128 << c2)),
        ),
    );

    // (x << C1) - x*C2 -> x*(2^C1 - C2)
    let sub_shl_mul = rewrite_rule(
        int_add(
            int_shl(var(x), int_const(c1)),
            int_neg(int_mul(var(x), int_const(c2))),
        )
        .when_match(move |edit, ty, binds| {
            coefficient_fits_carrier(ty)
                && all_same_width(edit, binds, ty, &[x, c2])
                && shift_in_range(edit, binds, ty, c1)
        }),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint, c2: uint] => (1u128 << c1).wrapping_sub(c2)),
        ),
    );

    // (x << C1) - (x << C2) -> x*(2^C1 - 2^C2)
    let sub_shl_shl = rewrite_rule(
        int_add(
            int_shl(var(x), int_const(c1)),
            int_neg(int_shl(var(x), int_const(c2))),
        )
        .when_match(move |edit, ty, binds| {
            all_same_width(edit, binds, ty, &[x])
                && shift_in_range(edit, binds, ty, c1)
                && shift_in_range(edit, binds, ty, c2)
        }),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint, c2: uint] => (1u128 << c1).wrapping_sub(1u128 << c2)),
        ),
    );

    vec![
        Box::new(add_self_mul),
        Box::new(add_mul_mul),
        Box::new(add_self_shl),
        Box::new(add_mul_shl),
        Box::new(add_shl_shl),
        Box::new(sub_self_mul),
        Box::new(sub_mul_mul),
        Box::new(sub_self_shl),
        Box::new(sub_mul_shl),
        Box::new(sub_shl_mul),
        Box::new(sub_shl_shl),
    ]
}

/// Reassociation and mask-merging: constant-folding across nested
/// `Add`/`Sub`/`And`, mask distribution, and const-on-right canonicalisation.
fn build_reassoc_and_mask_rules() -> Vec<crate::BoxedRule> {
    // One shared capture pool: each rule matches with its own fresh `Bindings`.
    let (x, y, c1, c2, c3) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    // (x + C1) + C2 -> x + (C1 + C2)
    let rule_add_add = rewrite_rule(
        int_add(int_add(var(x), int_const(c1)), int_const(c2))
            .when_match(move |edit, ty, binds| all_same_width(edit, binds, ty, &[x, c1, c2])),
        template::int_add(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        ),
    );

    // (x & C1) & C2 -> x & (C1 & C2)
    let rule_and_merge = rewrite_rule(
        int_and(int_and(var(x), int_const(c1)), int_const(c2))
            .when_match(move |edit, ty, binds| all_same_width(edit, binds, ty, &[x, c1, c2])),
        template::int_and(var(x), int_const_with!([c1: uint, c2: uint] => c1 & c2)),
    );

    // ((x & C1) | (y & C2)) & C3 -> (x & (C1 & C3)) | (y & (C2 & C3)),
    // gated on some product `Ci & C3` being zero: with both non-zero neither
    // disjunct shrinks and the rule re-fires forever.
    let rule_and_dist = rewrite_rule(
        int_and(
            int_or(
                int_and(var(x), int_const(c1)),
                int_and(var(y), int_const(c2)),
            ),
            int_const(c3),
        )
        .when_match(move |edit, _ty, binds| {
            let (Some(v1), Some(v2), Some(v3)) = (
                binds.get_uint(c1, edit.function()),
                binds.get_uint(c2, edit.function()),
                binds.get_uint(c3, edit.function()),
            ) else {
                return false;
            };
            if !all_same_width(edit, binds, _ty, &[x, y, c1, c2, c3]) {
                return false;
            }
            (v1 & v3) == 0 || (v2 & v3) == 0
        }),
        template::int_or(
            template::int_and(var(x), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
            template::int_and(var(y), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
        ),
    );

    // (x | A) & B -> x & B   when A & B == 0: every surviving bit (`B_i = 1`)
    // has `A_i = 0`.
    let rule_align_or_removal = rewrite_rule(
        int_and(int_or(var(x), int_const(c1)), int_const(c2)).when_match(move |edit, ty, binds| {
            let (Some(set_bits), Some(mask)) = (
                binds.get_uint(c1, edit.function()),
                binds.get_uint(c2, edit.function()),
            ) else {
                return false;
            };
            // `get_uint` masks each constant to ITS declared width, so
            // without width agreement the disjointness test runs in the
            // wrong modulus.
            all_same_width(edit, binds, ty, &[x, c1, c2]) && (set_bits & mask) == 0
        }),
        template::int_and(var(x), var(c2)),
    );

    // `op(C, x) -> op(x, C)` for each commutative int op.  `.ordered()` is
    // REQUIRED: without it the LHS also matches the already-canonical
    // `op(x, C)` and the rule never terminates.  The `x`-not-const guard
    // prevents a `(C1, C2)` ping-pong.
    macro_rules! const_on_right {
        ($op:ident) => {
            rewrite_rule(
                $op(int_const(c1), var(x))
                    .ordered()
                    .when_match(move |edit, _ty, b| b.get_uint(x, edit.function()).is_none()),
                template::$op(var(x), var(c1)),
            )
        };
    }
    let const_on_right_add = const_on_right!(int_add);
    let const_on_right_mul = const_on_right!(int_mul);
    let const_on_right_and = const_on_right!(int_and);
    let const_on_right_or = const_on_right!(int_or);
    let const_on_right_xor = const_on_right!(int_xor);

    vec![
        rule_add_add,
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
    // output type; otherwise this is a real narrowing, not an identity.
    macro_rules! ext_round_trip {
        ($ext:ident) => {{
            let pat = int_truncate($ext(var(x))).when_match(move |edit, ty, bnd| {
                bnd.get_type(x, edit.function())
                    .is_some_and(|x_ty| x_ty == ty)
            });
            rewrite_rule(pat, var(x))
        }};
    }
    let zext_round_trip = ext_round_trip!(int_zero_extend);
    let sext_round_trip = ext_round_trip!(int_sign_extend);

    // Narrowing through a binop: `Truncate_W(op(SignExt(a), SignExt(b)))` ->
    // `op_W(a, b)`, valid only for ops whose lower W bits don't depend on the
    // upper bits (Add/Sub/Mul/And/Or/Xor). Only the (SignExt, SignExt) Mul
    // permutation is covered.
    let narrow_mul_through_sext = {
        let pat = int_truncate(int_mul(int_sign_extend(var(a)), int_sign_extend(var(b))))
            .when_match(move |edit, ty, bnd| {
                bnd.get_type(a, edit.function())
                    .is_some_and(|a_ty| a_ty == ty)
                    && bnd
                        .get_type(b, edit.function())
                        .is_some_and(|b_ty| b_ty == ty)
            });
        rewrite_rule(pat, template::int_mul(var(a), var(b)))
    };

    // Drop the high half of an x86 register-merge Or when truncating to the low
    // half's width: `Truncate_W(Or(And(high_mask, old), And(low_mask, new)))`
    // -> `Truncate_W(...)`, since the high mask contributes nothing below bit
    // W. The guard picks the high-mask And by requiring its low-`W` bits to be
    // zero. A single operand orientation suffices: on guard failure the matcher
    // re-drives the `Or`'s operand order.
    let mk_drop_high_half = {
        let guard = move |edit: &strider_pattern::Matcher,
                          ty: strider_ir::node::ValueType,
                          bnd: &strider_pattern::Bindings| {
            let Some(c_val) = bnd.get_uint(c, edit.function()) else {
                return false;
            };
            let Some(low_mask) = crate::opt::known_bits::type_mask_u128(ty) else {
                return false;
            };
            c_val & low_mask == 0
        };
        rewrite_rule(
            int_truncate(int_or(var(a), int_and(int_const(c), var(b)))).when_match(guard),
            template::int_truncate(var(a)),
        )
    };

    // `Truncate_W(And(low_W_mask, x)) -> Truncate_W(x)`: the And zeroes bits the
    // truncate discards anyway. One orientation suffices because the `And`'s
    // commutative retry binds `c` to the const on either side.
    let mk_drop_low_mask_under_truncate = {
        let guard = move |edit: &strider_pattern::Matcher,
                          ty: strider_ir::node::ValueType,
                          bnd: &strider_pattern::Bindings| {
            let Some(c_val) = bnd.get_uint(c, edit.function()) else {
                return false;
            };
            let Some(low_mask) = crate::opt::known_bits::type_mask_u128(ty) else {
                return false;
            };
            // The mask must cover at least the low W bits; anything beyond that
            // is fine since the truncate drops those bits.
            c_val & low_mask == low_mask
        };
        rewrite_rule(
            int_truncate(int_and(int_const(c), var(x))).when_match(guard),
            template::int_truncate(var(x)),
        )
    };

    // Nested SAME-kind casts are transitive, so the intermediate width drops out
    // and the RHS inherits the outer width. MIXED-kind nests (zext of sext, and
    // vice versa) are not a single cast and are left alone.
    let zext_zext = rewrite_rule(
        int_zero_extend(int_zero_extend(var(x))),
        template::int_zero_extend(var(x)),
    );
    let sext_sext = rewrite_rule(
        int_sign_extend(int_sign_extend(var(x))),
        template::int_sign_extend(var(x)),
    );
    let trunc_trunc = rewrite_rule(
        int_truncate(int_truncate(var(x))),
        template::int_truncate(var(x)),
    );

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
    // All-ones is output-width-dependent, so compare `c` against the per-match
    // output type rather than a fixed constant. Past the `u128` carrier the mask
    // saturates, so match on `Some` rather than comparing the `Option`s.
    let is_all_ones = move |edit: &strider_pattern::Matcher,
                            ty: strider_ir::node::ValueType,
                            b: &strider_pattern::Bindings| {
        matches!(
            (b.get_uint(c, edit.function()), ty.get_unsigned_int(u128::MAX)),
            (Some(v), Some(all_ones)) if v == all_ones
        )
    };
    // x & all_ones -> x
    let all_ones_rule = rewrite_rule(
        int_and(var(x), int_const(c)).when_match(is_all_ones),
        var(x),
    );

    // x | all_ones -> all_ones. At I1 this subsumes `x | true -> true`.
    let or_all_ones_rule =
        rewrite_rule(int_or(var(x), int_const(c)).when_match(is_all_ones), var(c));

    // The commutative ops below match both operand orders; the shift rules do
    // not, so only a zero on the right is an identity for them.
    vec![
        rewrite_rule(int_add(var(x), int_const(0u128)), var(x)),
        rewrite_rule(int_sub(var(x), int_const(0u128)), var(x)),
        rewrite_rule(int_sub(var(x), var(x)), int_const(0u128)),
        rewrite_rule(int_xor(var(x), var(x)), int_const(0u128)),
        rewrite_rule(int_xor(var(x), int_const(0u128)), var(x)),
        rewrite_rule(int_mul(var(x), int_const(0u128)), int_const(0u128)),
        rewrite_rule(int_mul(var(x), int_const(1u128)), var(x)),
        rewrite_rule(int_and(var(x), int_const(0u128)), int_const(0u128)),
        rewrite_rule(int_and(var(x), var(x)), var(x)),
        rewrite_rule(int_or(var(x), int_const(0u128)), var(x)),
        rewrite_rule(int_or(var(x), var(x)), var(x)),
        rewrite_rule(int_shl(var(x), int_const(0u128)), var(x)),
        rewrite_rule(int_shr(var(x), int_const(0u128)), var(x)),
        rewrite_rule(int_sshr(var(x), int_const(0u128)), var(x)),
        all_ones_rule,
        or_all_ones_rule,
    ]
}

/// [`crate::const_eval::widths_carry`] over captures, in the rules' carrier.
fn require_operand_widths(
    edit: &strider_pattern::TemplateCtx<'_>,
    operands: &[Capture],
    expected: strider_ir::node::ValueType,
) -> crate::error::Result<()> {
    let carried = crate::const_eval::widths_carry(
        operands
            .iter()
            .map(|&c| edit.bindings.get_type(c, edit.function)),
        expected,
    );
    carried.then_some(()).ok_or_else(strider_pattern::skip)
}

fn build_const_eval_rules() -> Vec<crate::BoxedRule> {
    let (op, l, r, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    vec![
        // The width guard protects against a wider operand (say a shift amount)
        // whose value the output-width mask would silently change.
        {
            rewrite_rule(
                any_int_binary(int_const(l), int_const(r)).capture(op),
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
        {
            rewrite_rule(
                any_int_unary(int_const(v)).capture(op),
                int_const_with!([op: int_unary_op, v: uint, ty] =>
                    super::eval_int::eval_int_unary(op, v, ty).ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        // Both operands are masked to the LHS operand type. A wider RHS masked
        // down that far can flip a `Less` / `Sless` / `Carry` verdict, hence
        // the width guard.
        {
            rewrite_rule(
                any_int_cmp(int_const(l), int_const(r)).capture(op),
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
                    eval_int_cmp(op, l, r, input_ty).ok_or_else(strider_pattern::skip)
                }),
            )
        },
        // The wider IntConst's raw value is not masked to the truncate's output
        // width for us, so mask explicitly; otherwise a narrow IntConst holding
        // wide bits lands in the IR.
        {
            rewrite_rule(
                int_truncate(int_const(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        {
            rewrite_rule(
                int_zero_extend(int_const(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        {
            rewrite_rule(
                int_sign_extend(int_const(v)),
                int_const_with!([v: uint, in_ty, ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    super::eval_int::eval_sign_extend(v, input_ty, ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
            )
        },
        {
            rewrite_rule(
                int_popcount(int_const(v)),
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    super::eval_int::eval_popcount(v, input_ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
            )
        },
        {
            rewrite_rule(
                int_lzcount(int_const(v)),
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

    // Booleans are 1-bit integers, so the generic integer rules already fire at
    // I1. Only shapes with no integer analogue belong here.
    vec![
        // !!x -> x
        { rewrite_rule(bool_not(bool_not(var(x))), var(x)) },
        {
            rewrite_rule(
                any_float_binary(float_const(l), float_const(r)).capture(op),
                float_const_with!([op: float_binary_op, l: float_bits, r: float_bits, ty] =>
                    eval_float_binary(op, l, r, ty)
                        .ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        {
            rewrite_rule(
                any_float_unary(float_const(v)).capture(op),
                float_const_with!([op: float_unary_op, v: float_bits, ty] =>
                    eval_float_unary(op, v, ty)
                        .ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        // `in_ty` is the float operand type, not the I1 output.
        {
            rewrite_rule(
                any_float_cmp(float_const(l), float_const(r)).capture(op),
                bool_const_with!([op: float_cmp_op, l: float_bits, r: float_bits, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    eval_float_cmp(op, l, r, input_ty)
                        .ok_or_else(strider_pattern::skip)?
                }),
            )
        },
    ]
}
