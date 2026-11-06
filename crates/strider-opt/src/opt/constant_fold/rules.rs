use super::eval_float::{eval_float_binary, eval_float_cmp, eval_float_unary};

use crate::EditFunction;
use crate::Result;
use crate::const_eval::eval_node_const;
use crate::opt::width_guards::at_root_width;
use crate::rewrite_rule::Rule;
use smallvec::SmallVec;
use strider_ir::UBig;
use strider_ir::eval;
use strider_ir::node::{IntUnaryOp, NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRBuilderExt, IRViewer};
use strider_pattern::{
    Bindings, Capture, CaptureExt, Matcher, any_float_binary, any_float_cmp, any_float_unary,
    bool_const_with, capture_typed, float_bits_to_int, float_const, float_const_with, int_add,
    int_and, int_bits_to_float, int_const, int_const_with, int_mul, int_neg, int_or, int_sborrow,
    int_scarry, int_shl, int_shr, int_sign_extend, int_sshr, int_sub, int_truncate, int_xor,
    int_zero_extend, one_of, template, var,
};

/// Every constant-fold rule, in application order.
pub(super) fn build_rules() -> Vec<Rule> {
    let mut rules = build_identity_rules();
    rules.push(Rule::Fold {
        roots: |k| {
            matches!(
                k,
                NodeKind::IntUnaryOp(_)
                    | NodeKind::IntBinaryOp(_)
                    | NodeKind::IntCmpOp(_)
                    | NodeKind::Truncate
                    | NodeKind::Popcount
                    | NodeKind::Lzcount
                    | NodeKind::Extend(_)
            )
        },
        fold: const_eval_rule,
    });
    rules.extend(build_float_rules());
    rules.extend(build_reassoc_and_mask_rules());
    rules.extend(build_factor_rules());
    rules.extend(build_bitcast_extend_rules());
    rules
}

/// Whether `cap` is not an `IntConst` (true when unbound).
pub(super) fn binds_no_int_const(b: &Bindings, f: &strider_ir::Function, cap: Capture) -> bool {
    !b.get_node(cap, f.graph())
        .is_some_and(|n| matches!(f.node_kind(n), NodeKind::IntConst(_)))
}

/// Collects a value against itself: each side of an `Add` (or `Add(a, Neg(b))`)
/// is `x*C`, `x<<C` or bare `x`, and the sum becomes one `Mul` by the summed
/// coefficients, masked to the root's width when interned.
fn build_factor_rules() -> Vec<Rule> {
    let (x, c1, c2, z1, z2) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    // Product and shift arms first, so the bare arm does not swallow the
    // product into `x`. `one_of` is a union, not a cut.
    let term = move |c, z| {
        one_of![
            int_mul(var(x), int_const(c)),
            int_shl(var(x), int_const(z)),
            var(x),
        ]
    };
    // A coefficient must be as wide as the root and a shift count below its
    // width; each check is vacuous where its arm did not fire.
    let widths = move |edit: &Matcher, ty: ValueType, binds: &Bindings| {
        let f = edit.function();
        let w = ty.bit_width();
        at_root_width(f, binds, ty, &[x])
            && [c1, c2]
                .iter()
                .all(|&c| binds.get_type(c, f).is_none_or(|t| t.bit_width() == w))
            && [z1, z2]
                .iter()
                .all(|&z| binds.get_uint(z, f).is_none_or(|n| n < UBig::from(w)))
    };

    vec![
        // one_of(x, x*C1, x<<Z1) + one_of(x, x*C2, x<<Z2)
        Rule::rewrite(
            int_add(term(c1, z1), term(c2, z2)).when_match(widths),
            template::int_mul(
                var(x),
                int_const_with!([c1: uint?, z1: uint?, c2: uint?, z2: uint?] =>
                    coefficient(c1, z1) + coefficient(c2, z2)),
            ),
        ),
        // The difference.
        Rule::rewrite(
            int_add(term(c1, z1), int_neg(term(c2, z2))).when_match(widths),
            template::int_mul(
                var(x),
                int_const_with!([c1: uint?, z1: uint?, c2: uint?, z2: uint?, ty] =>
                    coefficient(c1, z1)
                        + eval::int_unary(IntUnaryOp::Neg, &coefficient(c2, z2), ty)
                            .ok_or_else(strider_pattern::skip)?),
            ),
        ),
    ]
}

/// What one side of a factor rule multiplies `x` by: `C` for `x*C`, `2^C` for
/// `x<<C`, or 1 for bare `x`. At most one of the two binds; the guard bounds
/// the shift count by the root's width.
fn coefficient(mul: Option<UBig>, shift: Option<UBig>) -> UBig {
    debug_assert!(mul.is_none() || shift.is_none(), "one arm fires per side");
    match (mul, shift) {
        (Some(c), _) => c,
        (None, Some(z)) => {
            UBig::ONE << usize::try_from(&z).expect("the guard bounds the count by the width")
        }
        (None, None) => UBig::ONE,
    }
}

/// Reassociation, shift merging, mask distribution and const-on-right
/// canonicalisation.
fn build_reassoc_and_mask_rules() -> Vec<Rule> {
    let (x, y, c1, c2, c3) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let mut rules: Vec<Rule> = Vec::new();

    // `op(op(x, C1), C2) -> op(x, C1 op C2)` for an associative `op`, the whole
    // shape at the root's width.
    macro_rules! reassoc {
        ($op:ident, $merged:expr) => {
            rules.push(Rule::rewrite(
                $op($op(var(x), int_const(c1)), int_const(c2)).when_match(
                    move |edit, ty, binds| at_root_width(edit.function(), binds, ty, &[x, c1, c2]),
                ),
                template::$op(var(x), $merged),
            ));
        };
    }
    reassoc!(int_add, int_const_with!([c1: uint, c2: uint] => c1 + c2));
    reassoc!(int_mul, int_const_with!([c1: uint, c2: uint] => c1 * c2));
    reassoc!(int_and, int_const_with!([c1: uint, c2: uint] => c1 & c2));
    reassoc!(int_xor, int_const_with!([c1: uint, c2: uint] => c1 ^ c2));

    // `shift(shift(x, C1), C2) -> shift(x, C1 + C2)` while the sum is below the
    // width. Past it, known bits fold the logical shifts to 0 only up to 128
    // bits; a wider chain is left as is.
    macro_rules! shift_chain {
        ($op:ident) => {
            rules.push(Rule::rewrite(
                $op($op(var(x), int_const(c1)), int_const(c2)).when_match(
                    move |edit, ty, binds| {
                        let f = edit.function();
                        at_root_width(f, binds, ty, &[x])
                            && binds.get_uint(c1, f).zip(binds.get_uint(c2, f)).is_some_and(
                                |(a, b)| a + b < UBig::from(ty.bit_width()),
                            )
                    },
                ),
                template::$op(
                    var(x),
                    capture_typed(x, int_const_with!([c1: uint, c2: uint] => c1 + c2)),
                ),
            ));
        };
    }
    shift_chain!(int_shl);
    shift_chain!(int_shr);
    // An arithmetic shift saturates instead: from `W - 1` on every bit is the
    // sign bit.
    rules.push(Rule::rewrite(
        int_sshr(int_sshr(var(x), int_const(c1)), int_const(c2))
            .when_match(move |edit, ty, binds| at_root_width(edit.function(), binds, ty, &[x])),
        template::int_sshr(
            var(x),
            capture_typed(
                x,
                int_const_with!([c1: uint, c2: uint, ty] =>
                    (c1 + c2).min(UBig::from(ty.bit_width() - 1))),
            ),
        ),
    ));

    // ((x & C1) | (y & C2)) & C3 -> (x & (C1 & C3)) | (y & (C2 & C3)),
    // when some `Ci & C3` is zero.
    rules.push(Rule::rewrite(
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
            if !at_root_width(edit.function(), binds, ty, &[x, y, c1, c2, c3]) {
                return false;
            }
            (&v1 & &v3) == UBig::ZERO || (&v2 & &v3) == UBig::ZERO
        }),
        template::int_or(
            template::int_and(var(x), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
            template::int_and(var(y), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
        ),
    ));

    // (x | A) & B -> x & B   when A & B == 0.
    rules.push(Rule::rewrite(
        int_and(int_or(var(x), int_const(c1)), int_const(c2)).when_match(move |edit, ty, binds| {
            let (Some(set_bits), Some(mask)) = (
                binds.get_uint(c1, edit.function()),
                binds.get_uint(c2, edit.function()),
            ) else {
                return false;
            };
            // `get_uint` masks each constant to its own width.
            at_root_width(edit.function(), binds, ty, &[x, c1, c2])
                && (set_bits & mask) == UBig::ZERO
        }),
        template::int_and(var(x), var(c2)),
    ));

    // `op(C, x) -> op(x, C)` for each commutative int op. `.ordered()` keeps
    // the LHS off the canonical `op(x, C)`; the guard prevents a `(C1, C2)`
    // ping-pong.
    macro_rules! const_on_right {
        ($op:ident) => {
            Rule::rewrite(
                $op(int_const(c1), var(x))
                    .ordered()
                    .when_match(move |edit, _ty, b| binds_no_int_const(b, edit.function(), x)),
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
fn build_bitcast_extend_rules() -> Vec<Rule> {
    let (x, a, b, c) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );
    let mut rules: Vec<Rule> = Vec::new();

    // IntBitsToFloat(FloatBitsToInt(x)) -> x
    rules.push(Rule::rewrite(
        int_bits_to_float(float_bits_to_int(var(x))),
        var(x),
    ));

    // FloatBitsToInt(IntBitsToFloat(x)) -> x
    rules.push(Rule::rewrite(
        float_bits_to_int(int_bits_to_float(var(x))),
        var(x),
    ));

    // A bitcast of a constant, up to the `u64` a `FloatConst` carries.
    let fits_u64 = |_: &Matcher, ty: ValueType, _: &_| ty.byte_size() <= 8;
    rules.push(Rule::rewrite(
        int_bits_to_float(int_const(c)).when_match(fits_u64),
        float_const_with!([c: uint] => u64::try_from(&c).map_err(|_| strider_pattern::skip())?),
    ));
    rules.push(Rule::rewrite(
        float_bits_to_int(float_const(c)).when_match(fits_u64),
        int_const_with!([c: float_bits] => UBig::from(c)),
    ));

    // Truncate(Extend(x)) is `x`, a truncate of `x` or the same extend of `x`,
    // by the truncate's width against `x`'s.
    macro_rules! trunc_of_ext {
        ($ext:ident) => {{
            let x_width_is = move |ord: std::cmp::Ordering| {
                move |edit: &Matcher, ty: ValueType, bnd: &Bindings| {
                    bnd.get_type(x, edit.function())
                        .is_some_and(|x_ty| x_ty.bit_width().cmp(&ty.bit_width()) == ord)
                }
            };
            [
                Rule::rewrite(
                    int_truncate($ext(var(x))).when_match(x_width_is(std::cmp::Ordering::Equal)),
                    var(x),
                ),
                Rule::rewrite(
                    int_truncate($ext(var(x))).when_match(x_width_is(std::cmp::Ordering::Greater)),
                    template::int_truncate(var(x)),
                ),
                Rule::rewrite(
                    int_truncate($ext(var(x))).when_match(x_width_is(std::cmp::Ordering::Less)),
                    template::$ext(var(x)),
                ),
            ]
        }};
    }
    rules.extend(trunc_of_ext!(int_zero_extend));
    rules.extend(trunc_of_ext!(int_sign_extend));

    // Nested same-kind casts are transitive; the RHS inherits the outer width.
    rules.push(Rule::rewrite(
        int_zero_extend(int_zero_extend(var(x))),
        template::int_zero_extend(var(x)),
    ));
    rules.push(Rule::rewrite(
        int_sign_extend(int_sign_extend(var(x))),
        template::int_sign_extend(var(x)),
    ));
    rules.push(Rule::rewrite(
        int_truncate(int_truncate(var(x))),
        template::int_truncate(var(x)),
    ));
    // `SExt(ZExt(x)) -> ZExt(x)` for a strictly widening zext (top bit 0).
    rules.push(Rule::rewrite(
        int_sign_extend(int_zero_extend(var(x)).capture(c)).when_match(move |edit, _ty, bnd| {
            let f = edit.function();
            bnd.get_type(x, f)
                .zip(bnd.get_type(c, f))
                .is_some_and(|(x_ty, z_ty)| x_ty.bit_width() < z_ty.bit_width())
        }),
        template::int_zero_extend(var(x)),
    ));

    // `Truncate_W(Or(a, And(C, b))) -> Truncate_W(a)` when `C`'s low W bits
    // are zero (the high half of an x86 register merge). The matcher retries
    // the `Or`'s other operand order.
    rules.push({
        let guard = move |edit: &Matcher, ty: ValueType, bnd: &Bindings| {
            let Some(c_val) = bnd.get_uint(c, edit.function()) else {
                return false;
            };
            eval::mask(&c_val, ty) == UBig::ZERO
        };
        Rule::rewrite(
            int_truncate(int_or(var(a), int_and(int_const(c), var(b)))).when_match(guard),
            template::int_truncate(var(a)),
        )
    });

    // `Truncate_W(And(C, x)) -> Truncate_W(x)` when `C` covers the low W bits.
    rules.push({
        let guard = move |edit: &Matcher, ty: ValueType, bnd: &Bindings| {
            let Some(c_val) = bnd.get_uint(c, edit.function()) else {
                return false;
            };
            eval::mask(&c_val, ty) == eval::all_ones(ty)
        };
        Rule::rewrite(
            int_truncate(int_and(int_const(c), var(x))).when_match(guard),
            template::int_truncate(var(x)),
        )
    });

    // `Truncate_W(op(Ext(a), Ext(b))) -> op_W(a, b)` and
    // `Truncate_W(op(Ext(a), C)) -> op_W(a, C mod 2^W)` for an op whose low W
    // bits read only its operands' low W bits, `a` and `b` of the root type.
    let ext = |v| one_of![int_zero_extend(var(v)), int_sign_extend(var(v))];
    let of_root_type = move |caps: [Capture; 2]| {
        move |edit: &Matcher, ty: ValueType, bnd: &Bindings| {
            caps.iter()
                .all(|&v| bnd.get_type(v, edit.function()) == Some(ty))
        }
    };
    macro_rules! narrow_through {
        ($op:ident) => {
            rules.push(Rule::rewrite(
                int_truncate($op(ext(a), ext(b))).when_match(of_root_type([a, b])),
                template::$op(var(a), var(b)),
            ));
            rules.push(Rule::rewrite(
                int_truncate($op(ext(a), int_const(c))).when_match(of_root_type([a, a])),
                template::$op(var(a), capture_typed(a, int_const_with!([c: uint] => c))),
            ));
        };
    }
    narrow_through!(int_add);
    narrow_through!(int_mul);
    narrow_through!(int_and);
    narrow_through!(int_or);
    narrow_through!(int_xor);

    rules
}

/// Algebraic identities for integer binary operations.
fn build_identity_rules() -> Vec<Rule> {
    let (x, c) = (Capture::new(), Capture::new());
    // All-ones at the matched output width.
    let is_all_ones = move |edit: &Matcher, ty: ValueType, b: &Bindings| {
        b.get_uint(c, edit.function()) == Some(eval::all_ones(ty))
    };
    // x & all_ones -> x
    let all_ones_rule = Rule::rewrite(
        int_and(var(x), int_const(c)).when_match(is_all_ones),
        var(x),
    );

    // x | all_ones -> all_ones
    let or_all_ones_rule =
        Rule::rewrite(int_or(var(x), int_const(c)).when_match(is_all_ones), var(c));

    vec![
        Rule::rewrite(int_add(var(x), int_const(0u128)), var(x)),
        Rule::rewrite(int_sub(var(x), var(x)), int_const(0u128)),
        Rule::rewrite(int_xor(var(x), var(x)), int_const(0u128)),
        Rule::rewrite(int_xor(var(x), int_const(0u128)), var(x)),
        Rule::rewrite(int_mul(var(x), int_const(0u128)), int_const(0u128)),
        Rule::rewrite(int_mul(var(x), int_const(1u128)), var(x)),
        Rule::rewrite(int_and(var(x), int_const(0u128)), int_const(0u128)),
        Rule::rewrite(int_and(var(x), var(x)), var(x)),
        Rule::rewrite(int_or(var(x), int_const(0u128)), var(x)),
        Rule::rewrite(int_or(var(x), var(x)), var(x)),
        Rule::rewrite(int_shl(var(x), int_const(0u128)), var(x)),
        Rule::rewrite(int_shr(var(x), int_const(0u128)), var(x)),
        Rule::rewrite(int_sshr(var(x), int_const(0u128)), var(x)),
        // Neither `x - 0` nor `x + 0` overflows.
        Rule::rewrite(int_sborrow(var(x), int_const(0u128)), int_const(0u128)),
        Rule::rewrite(int_scarry(var(x), int_const(0u128)), int_const(0u128)),
        all_ones_rule,
        or_all_ones_rule,
    ]
}

/// Folds an integer node over constant operands through [`eval_node_const`],
/// absorbing the operands' fingerprints into the result. A root with no uses
/// falls through to the later rules.
fn const_eval_rule(edit: &mut EditFunction<'_>, root: NodeId) -> Result<Option<ValueId>> {
    let f = edit.function();
    let Ok((out, ty)) = f.single_value_output(root) else {
        return Ok(None);
    };
    if !ty.is_integer() || edit.value_uses(out).next().is_none() {
        return Ok(None);
    }
    let Some(k) = eval_node_const(f, out, &|v| f.int_const(v).cloned(), None) else {
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

/// Const-eval rules for the float ops.
fn build_float_rules() -> Vec<Rule> {
    let (op, l, r, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    vec![
        Rule::rewrite(
            any_float_binary(float_const(l), float_const(r)).capture(op),
            float_const_with!([op: float_binary_op, l: float_bits, r: float_bits, ty] =>
                eval_float_binary(op, l, r, ty)
                    .ok_or_else(strider_pattern::skip)?
            ),
        ),
        Rule::rewrite(
            any_float_unary(float_const(v)).capture(op),
            float_const_with!([op: float_unary_op, v: float_bits, ty] =>
                eval_float_unary(op, v, ty)
                    .ok_or_else(strider_pattern::skip)?
            ),
        ),
        // `in_ty` is the float operand type, not the I1 output.
        Rule::rewrite(
            any_float_cmp(float_const(l), float_const(r)).capture(op),
            bool_const_with!([op: float_cmp_op, l: float_bits, r: float_bits, in_ty] => {
                let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                eval_float_cmp(op, l, r, input_ty)
                    .ok_or_else(strider_pattern::skip)?
            }),
        ),
    ]
}
