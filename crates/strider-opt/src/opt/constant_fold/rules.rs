use super::eval_float::{eval_float_binary, eval_float_cmp, eval_float_unary};

use crate::EditFunction;
use crate::Result;
use crate::const_eval::eval_node_const;
use crate::opt::width_guards::same_width;
use crate::rewrite_rule;
use smallvec::SmallVec;
use strider_ir::eval;
use strider_ir::node::{NodeId, ValueId};
use strider_ir::{IRBuilderExt, IRViewer};
use strider_pattern::{
    Capture, CaptureExt, any_float_binary, any_float_cmp, any_float_unary, bool_const_with,
    bool_not, float_bits_to_int, float_const, float_const_with, int_add, int_and,
    int_bits_to_float, int_const, int_const_with, int_mul, int_neg, int_or, int_sborrow,
    int_scarry, int_shl, int_shr, int_sign_extend, int_sshr, int_sub, int_truncate, int_xor,
    int_zero_extend, one_of, template, var,
};

/// Every constant-fold rule, in application order.
pub(super) fn build_rules() -> Vec<crate::BoxedRule> {
    let mut rules = build_identity_rules();
    rules.push(Box::new(const_eval_rule));
    rules.extend(build_bool_float_rules());
    rules.extend(build_reassoc_and_mask_rules());
    rules.extend(build_factor_rules());
    rules.extend(build_bitcast_extend_rules());
    rules
}

/// Collecting a value against itself: `x + x`, `x*C1 + x*C2` and everything
/// between become one `Mul`. Each side is the same alternation, `x*C`, `x<<C`
/// or bare `x`, contributing the coefficient `C`, `2^C` or 1. The sum is
/// computed in `u128`, so the root's type must have a `u128` modulus.
///
/// Subtraction arrives lowered to `Add(a, Neg(b))`, so the difference form
/// matches that shape rather than an `IntSub`.
fn build_factor_rules() -> Vec<crate::BoxedRule> {
    let (x, c1, c2, z1, z2) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    // The product and shift arms come first so a match binds the coefficient
    // rather than swallowing the whole product into `x` through the bare arm,
    // which would leave the inner `Mul` standing. An arm whose continuation
    // fails still falls through: `one_of` is a union, not a cut.
    let term = move |c, z| {
        one_of![
            int_mul(var(x), int_const(c)),
            int_shl(var(x), int_const(z)),
            var(x),
        ]
    };
    // The per-arm width preconditions, each vacuous where its arm did not
    // fire: a coefficient must be as wide as the root, and a shift count must
    // name a bit of it.
    let widths = move |edit: &strider_pattern::Matcher,
                       ty: strider_ir::node::ValueType,
                       binds: &strider_pattern::Bindings| {
        let f = edit.function();
        let w = ty.bit_width();
        ty.int_mask_u128().is_some()
            && same_width(f, binds, &[x]) == Some(w)
            && [c1, c2]
                .iter()
                .all(|&c| binds.get_type(c, f).is_none_or(|t| t.bit_width() == w))
            && [z1, z2]
                .iter()
                .all(|&z| binds.get_uint(z, f).is_none_or(|n| n < w as u128))
    };

    vec![
        // one_of(x, x*C1, x<<Z1) + one_of(x, x*C2, x<<Z2)
        rewrite_rule(
            int_add(term(c1, z1), term(c2, z2)).when_match(widths),
            template::int_mul(
                var(x),
                int_const_with!([c1: uint?, z1: uint?, c2: uint?, z2: uint?] =>
                    coefficient(c1, z1).wrapping_add(coefficient(c2, z2))),
            ),
        ),
        // The same difference. `Neg` pins which side is subtracted, so
        // commutative matching cannot reach it from the sum.
        rewrite_rule(
            int_add(term(c1, z1), int_neg(term(c2, z2))).when_match(widths),
            template::int_mul(
                var(x),
                int_const_with!([c1: uint?, z1: uint?, c2: uint?, z2: uint?] =>
                    coefficient(c1, z1).wrapping_sub(coefficient(c2, z2))),
            ),
        ),
    ]
}

/// What one side of a factor rule multiplies `x` by: the bound constant of an
/// `x*C` arm, `2^C` for an `x<<C` arm, or 1 for the bare `x`. Exactly one arm
/// fires, so at most one of the two binds, and the guard has already put the
/// shift count below the root's width.
fn coefficient(mul: Option<u128>, shift: Option<u128>) -> u128 {
    debug_assert!(mul.is_none() || shift.is_none(), "one arm fires per side");
    match (mul, shift) {
        (Some(c), _) => c,
        (None, Some(z)) => eval::bit(z.min(128) as u32),
        (None, None) => 1,
    }
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
    let mut rules: Vec<crate::BoxedRule> = Vec::new();

    // (x + C1) + C2 -> x + (C1 + C2)
    rules.push(rewrite_rule(
        int_add(int_add(var(x), int_const(c1)), int_const(c2)).when_match(
            move |edit, ty, binds| {
                ty.int_mask_u128().is_some()
                    && same_width(edit.function(), binds, &[x, c1, c2]) == Some(ty.bit_width())
            },
        ),
        template::int_add(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        ),
    ));

    // (x * C1) * C2 -> x * (C1 * C2)
    rules.push(rewrite_rule(
        int_mul(int_mul(var(x), int_const(c1)), int_const(c2)).when_match(
            move |edit, ty, binds| {
                ty.int_mask_u128().is_some()
                    && same_width(edit.function(), binds, &[x, c1, c2]) == Some(ty.bit_width())
            },
        ),
        template::int_mul(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_mul(c2)),
        ),
    ));

    // (x & C1) & C2 -> x & (C1 & C2)
    rules.push(rewrite_rule(
        int_and(int_and(var(x), int_const(c1)), int_const(c2)).when_match(
            move |edit, ty, binds| {
                same_width(edit.function(), binds, &[x, c1, c2]) == Some(ty.bit_width())
            },
        ),
        template::int_and(var(x), int_const_with!([c1: uint, c2: uint] => c1 & c2)),
    ));

    // ((x & C1) | (y & C2)) & C3 -> (x & (C1 & C3)) | (y & (C2 & C3)),
    // gated on some product `Ci & C3` being zero, the case where a disjunct
    // folds away.
    rules.push(rewrite_rule(
        int_and(
            int_or(
                int_and(var(x), int_const(c1)),
                int_and(var(y), int_const(c2)),
            ),
            int_const(c3),
        )
        .when_match(move |edit, ty, binds| {
            let (Some(v1), Some(v2), Some(v3)) = (
                binds.get_uint(c1, edit.function()),
                binds.get_uint(c2, edit.function()),
                binds.get_uint(c3, edit.function()),
            ) else {
                return false;
            };
            if same_width(edit.function(), binds, &[x, y, c1, c2, c3]) != Some(ty.bit_width()) {
                return false;
            }
            (v1 & v3) == 0 || (v2 & v3) == 0
        }),
        template::int_or(
            template::int_and(var(x), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
            template::int_and(var(y), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
        ),
    ));

    // (x | A) & B -> x & B   when A & B == 0: every surviving bit (`B_i = 1`)
    // has `A_i = 0`.
    rules.push(rewrite_rule(
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
            same_width(edit.function(), binds, &[x, c1, c2]) == Some(ty.bit_width())
                && (set_bits & mask) == 0
        }),
        template::int_and(var(x), var(c2)),
    ));

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
    rules.push(const_on_right!(int_add));
    rules.push(const_on_right!(int_mul));
    rules.push(const_on_right!(int_and));
    rules.push(const_on_right!(int_or));
    rules.push(const_on_right!(int_xor));

    rules
}

/// Bitcast, extend/truncate round-trip, and truncate-folding rules.
fn build_bitcast_extend_rules() -> Vec<crate::BoxedRule> {
    let (x, a, b, c) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let mut rules: Vec<crate::BoxedRule> = Vec::new();

    // IntBitsToFloat(FloatBitsToInt(x)) -> x
    rules.push(rewrite_rule(
        int_bits_to_float(float_bits_to_int(var(x))),
        var(x),
    ));

    // FloatBitsToInt(IntBitsToFloat(x)) -> x
    rules.push(rewrite_rule(
        float_bits_to_int(int_bits_to_float(var(x))),
        var(x),
    ));

    // A bitcast of a constant is a constant of the same bits, up to the
    // `u64` a `FloatConst` carries, as the builder folds one.
    let fits_u64 =
        |_: &strider_pattern::Matcher, ty: strider_ir::node::ValueType, _: &_| ty.byte_size() <= 8;
    rules.push(rewrite_rule(
        int_bits_to_float(int_const(c)).when_match(fits_u64),
        float_const_with!([c: uint] => u64::try_from(c).map_err(|_| strider_pattern::skip())?),
    ));
    rules.push(rewrite_rule(
        float_bits_to_int(float_const(c)).when_match(fits_u64),
        int_const_with!([c: float_bits] => u128::from(c)),
    ));

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
    rules.push(ext_round_trip!(int_zero_extend));
    rules.push(ext_round_trip!(int_sign_extend));

    // Truncate(Extend(x)) to any other width is one cast: the extend keeps
    // `x`'s bits low, so a narrower truncate is a truncate of `x`, and a wider
    // one is the same extend of `x` to that width.
    macro_rules! trunc_of_ext {
        ($ext:ident) => {{
            let x_width_is = move |ord: std::cmp::Ordering| {
                move |edit: &strider_pattern::Matcher,
                      ty: strider_ir::node::ValueType,
                      bnd: &strider_pattern::Bindings| {
                    bnd.get_type(x, edit.function())
                        .is_some_and(|x_ty| x_ty.bit_width().cmp(&ty.bit_width()) == ord)
                }
            };
            [
                rewrite_rule(
                    int_truncate($ext(var(x))).when_match(x_width_is(std::cmp::Ordering::Greater)),
                    template::int_truncate(var(x)),
                ),
                rewrite_rule(
                    int_truncate($ext(var(x))).when_match(x_width_is(std::cmp::Ordering::Less)),
                    template::$ext(var(x)),
                ),
            ]
        }};
    }
    rules.extend(trunc_of_ext!(int_zero_extend));
    rules.extend(trunc_of_ext!(int_sign_extend));

    // Nested SAME-kind casts are transitive, so the intermediate width drops out
    // and the RHS inherits the outer width. MIXED-kind nests (zext of sext, and
    // vice versa) are not a single cast and are left alone.
    rules.push(rewrite_rule(
        int_zero_extend(int_zero_extend(var(x))),
        template::int_zero_extend(var(x)),
    ));
    rules.push(rewrite_rule(
        int_sign_extend(int_sign_extend(var(x))),
        template::int_sign_extend(var(x)),
    ));
    rules.push(rewrite_rule(
        int_truncate(int_truncate(var(x))),
        template::int_truncate(var(x)),
    ));

    // Narrowing through a binop: `Truncate_W(op(SignExt(a), SignExt(b)))` ->
    // `op_W(a, b)`, valid only for ops whose lower W bits don't depend on the
    // upper bits (Add/Sub/Mul/And/Or/Xor). Only the (SignExt, SignExt) Mul
    // permutation is covered.
    rules.push({
        let pat = int_truncate(int_mul(int_sign_extend(var(a)), int_sign_extend(var(b))))
            .when_match(move |edit, ty, bnd| {
                bnd.get_type(a, edit.function())
                    .is_some_and(|a_ty| a_ty == ty)
                    && bnd
                        .get_type(b, edit.function())
                        .is_some_and(|b_ty| b_ty == ty)
            });
        rewrite_rule(pat, template::int_mul(var(a), var(b)))
    });

    // Drop the high half of an x86 register-merge Or when truncating to the low
    // half's width: `Truncate_W(Or(And(high_mask, old), And(low_mask, new)))`
    // -> `Truncate_W(...)`, since the high mask contributes nothing below bit
    // W. The guard picks the high-mask And by requiring its low-`W` bits to be
    // zero. A single operand orientation suffices: on guard failure the matcher
    // re-drives the `Or`'s operand order.
    rules.push({
        let guard = move |edit: &strider_pattern::Matcher,
                          ty: strider_ir::node::ValueType,
                          bnd: &strider_pattern::Bindings| {
            let Some(c_val) = bnd.get_uint(c, edit.function()) else {
                return false;
            };
            let Some(low_mask) = ty.int_mask_u128() else {
                return false;
            };
            c_val & low_mask == 0
        };
        rewrite_rule(
            int_truncate(int_or(var(a), int_and(int_const(c), var(b)))).when_match(guard),
            template::int_truncate(var(a)),
        )
    });

    // `Truncate_W(And(low_W_mask, x)) -> Truncate_W(x)`: the And zeroes bits the
    // truncate discards anyway. One orientation suffices because the `And`'s
    // commutative retry binds `c` to the const on either side.
    rules.push({
        let guard = move |edit: &strider_pattern::Matcher,
                          ty: strider_ir::node::ValueType,
                          bnd: &strider_pattern::Bindings| {
            let Some(c_val) = bnd.get_uint(c, edit.function()) else {
                return false;
            };
            let Some(low_mask) = ty.int_mask_u128() else {
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
    });

    rules
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
        // Neither `x - 0` nor `x + 0` overflows.
        rewrite_rule(int_sborrow(var(x), int_const(0u128)), int_const(0u128)),
        rewrite_rule(int_scarry(var(x), int_const(0u128)), int_const(0u128)),
        all_ones_rule,
        or_all_ones_rule,
    ]
}

/// Folds an integer node over constant operands through [`eval_node_const`].
///
/// Mirrors `rewrite_rule`: the operands' fingerprints go into the result,
/// `replace_value` absorbs the root's, and a root with no uses falls through
/// to the later rules.
fn const_eval_rule(edit: &mut EditFunction<'_>, root: NodeId) -> Result<Option<ValueId>> {
    let f = edit.function();
    let Ok((out, ty)) = f.single_value_output(root) else {
        return Ok(None);
    };
    // The fold builds an integer constant; a float node keeps the float rules.
    if !ty.is_integer() {
        return Ok(None);
    }
    let Some(k) = eval_node_const(f, out, &|v| f.int_const_u128(v), None) else {
        return Ok(None);
    };
    let operands: SmallVec<[NodeId; 2]> = f
        .node_inputs(root)
        .into_iter()
        .map(|v| f.producer(v))
        .collect();
    let new = edit.build_int_const(k, ty)?;
    let producer = edit.producer(new);
    for from in operands {
        edit.absorb_node_fingerprint(producer, from);
    }
    let changed = edit.replace_value(out, new);
    Ok(changed.then_some(new))
}

/// Const-eval rules for the I1 boolean ops and all float ops. The absorbing
/// element lives in `build_identity_rules` (`or_all_ones_rule`), not here.
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
