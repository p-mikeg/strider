use strider_ir::node::{NodeId, ValueId};

use crate::error::Result;

use super::eval_float::{eval_float_binary, eval_float_cmp, eval_float_unary};
use super::eval_int::{eval_int_binary, eval_int_cmp};

use crate::{BoxedRule, rewrite_rule};
use strider_pattern::template;
use strider_pattern::{
    Capture, CaptureExt, add, and, any_float_const, any_int_const, bool_const_with, bool_not,
    float_binary_any, float_bits_to_int, float_cmp_any, float_const_with, float_unary_any,
    int_binary_any, int_bits_to_float, int_cmp_any, int_const, int_const_with, int_unary_any,
    lzcount, mul, or, popcount, shl, shr, sign_extend, sshr, sub, truncate, var, xor, zero_extend,
};

/// The five constant-fold rule groups, built once and owned by a
/// [`super::ConstantFold`] instance.
///
/// The groups keep their semantic grouping (identity / const-eval /
/// bool-float / reassoc-and-mask / bitcast-extend) for readers.  The
/// bitcast-extend group includes the `IntBitsToFloat`/`FloatBitsToInt`
/// round-trip identities, so int↔float bitcasts are folded inline; there
/// is no separate lowering step.
///
/// A [`crate::BoxedRule`] captures patterns whose inner
/// [`strider_pattern::Pattern`] is `!Send + !Sync` (strider runs
/// single-threaded), and the boxed rule closures are not `Clone`.  The
/// owning pass holds this set behind an [`std::rc::Rc`] so the pass stays
/// cheaply `Clone` while building the rule closures only once.
pub(super) struct ConstFoldRules {
    identity: Vec<crate::BoxedRule>,
    const_eval: Vec<crate::BoxedRule>,
    bool_float: Vec<crate::BoxedRule>,
    reassoc_and_mask: Vec<crate::BoxedRule>,
    bitcast_extend: Vec<crate::BoxedRule>,
}

impl ConstFoldRules {
    /// Builds every rule group once.  Called from [`super::ConstantFold::new`].
    pub(super) fn build() -> Self {
        Self {
            identity: build_identity_rules(),
            const_eval: build_const_eval_rules(),
            bool_float: build_bool_float_rules(),
            reassoc_and_mask: build_reassoc_and_mask_rules(),
            bitcast_extend: build_bitcast_extend_rules(),
        }
    }

    /// Runs every constant-fold rule group on `node`.  Returns
    /// `Some(new_out)` — the output produced by the **last** group to
    /// fire (the surviving redirect) — when any group fired, else
    /// `None`.  The peephole driver re-examines the node behind
    /// `new_out` for cascading folds.
    pub(super) fn apply_all(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        node: NodeId,
    ) -> Result<Option<ValueId>> {
        use crate::apply_rules_in_order;
        let mut last: Option<ValueId> = None;
        for group in [
            &self.identity,
            &self.const_eval,
            &self.bool_float,
            &self.reassoc_and_mask,
            &self.bitcast_extend,
        ] {
            if let Some(out) = apply_rules_in_order(group)(ctx, node)? {
                last = Some(out);
            }
        }
        Ok(last)
    }
}

// ── per-node folding ──────────────────────────────────────────────────────────

/// Builds the rule vec for [`REASSOC_AND_MASK_RULES`].
fn build_reassoc_and_mask_rules() -> Vec<crate::BoxedRule> {
    // Shared captures: every rule here is matched as an independent query (its
    // own fresh `Bindings`), so reusing one pool carries no cross-rule state.
    // `x` / `y` are variable operands, `c1` / `c2` / `c3` the constant operands
    // a given rule binds.
    let (x, y, c1, c2, c3) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    // (x + C1) + C2 → x + (C1 + C2)
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

    // (x - C1) - C2 → x - (C1 + C2)
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

    // (x + C1) - C2 → x + (C1 - C2)
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

    // (x - C1) + C2 → x + (C2 - C1)
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

    // (x & C1) & C2 → x & (C1 & C2)
    let rule_and_merge = rewrite_rule(
        and(
            and(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        ),
        template::and(var(x), int_const_with!([c1: uint, c2: uint] => c1 & c2)),
    );

    // ((x & C1) | (y & C2)) & C3 → (x & (C1 & C3)) | (y & (C2 & C3))
    //
    // Only fire when the distribution actually simplifies — i.e. at least
    // one product `Ci & C3` is zero, so that disjunct collapses (via the
    // `x & 0 → 0` / `x | 0 → x` identities) to leave a single masked term.
    // When BOTH products are non-zero the distribution is pure churn: it
    // merely pushes `& C3` inward, and the identities can't shrink either
    // disjunct, so the factored `And(Or, C3)` shape regenerates and the
    // rule re-fires forever (non-confluence).  Gating on "a disjunct
    // collapses" makes the rule strictly progress-reducing.
    let rule_and_dist = rewrite_rule(
        and(
            or(
                and(var(x), any_int_const().capture(c1)),
                and(var(y), any_int_const().capture(c2)),
            ),
            any_int_const().capture(c3),
        )
        .when_match(move |ctx, _ty, binds| {
            let (Some(v1), Some(v2), Some(v3)) = (
                binds.get_uint(c1, ctx.function()),
                binds.get_uint(c2, ctx.function()),
                binds.get_uint(c3, ctx.function()),
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

    // (x | A) & B → x & B   when A & B == 0
    //
    // Alignment idiom: the OR sets bits `A` that the mask `B` then clears
    // (`A & B == 0`), so the OR is a no-op for the masked value.  ARM/Thumb
    // dispatch emits `(load | 1) & 0xFFFFFFFE` (set then clear the Thumb bit);
    // folding it lets the jump-table classifier see a single masked load
    // instead of hand-stripping the `Or`.  Sound: when `A & B == 0`, every
    // surviving bit (`B_i = 1`) has `A_i = 0`, so `(x_i | A_i) & B_i =
    // x_i & B_i`.  Confluent: the RHS is a plain `And`, never the `(_ | _) & _`
    // LHS shape, so it cannot re-fire (unlike the unguarded full distribution).
    let rule_align_or_removal = rewrite_rule(
        and(
            or(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        )
        .when_match(move |ctx, _ty, binds| {
            let (Some(set_bits), Some(mask)) = (
                binds.get_uint(c1, ctx.function()),
                binds.get_uint(c2, ctx.function()),
            ) else {
                return false;
            };
            (set_bits & mask) == 0
        }),
        template::and(var(x), var(c2)),
    );

    // Commutative const-on-right canonicalisation: `op(C, x) → op(x, C)` for
    // each commutative int op, so a constant operand is always the right one
    // (a normalisation that lets equal `op(C, x)` / `op(x, C)` dedup to one
    // node).  `.ordered()` is REQUIRED: it forbids the commutative operand
    // swap so the LHS matches ONLY the const-on-left shape — without it the
    // matcher would also match the already-canonical `op(x, C)` and the rule
    // would re-fire forever (non-termination).  The `x`-not-const guard then
    // prevents a `(C1, C2)` ping-pong (const-eval folds those instead).
    //
    // `x` / `c1` are reused from the shared pool: each rule is an independent
    // query.  One rule per op because the template DSL can't rebuild a binary
    // node from a captured op variant.
    // One rule per op (the template DSL can't rebuild a binary node from a
    // captured op variant); the macro removes the five-fold copy.  `$op` names
    // both the matcher builder (`add`) and its template counterpart
    // (`template::add`).
    macro_rules! const_on_right {
        ($op:ident) => {
            rewrite_rule(
                $op(any_int_const().capture(c1), var(x))
                    .ordered()
                    .when_match(move |ctx, _ty, b| b.get_uint(x, ctx.function()).is_none()),
                template::$op(var(x), var(c1)),
            )
        };
    }
    let const_on_right_add = const_on_right!(add);
    let const_on_right_mul = const_on_right!(mul);
    let const_on_right_and = const_on_right!(and);
    let const_on_right_or = const_on_right!(or);
    let const_on_right_xor = const_on_right!(xor);

    let rules: Vec<BoxedRule> = vec![
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
    ];
    rules
}

/// The low-`W` all-ones mask for a truncate's output width, or `None` when the
/// width is degenerate (0) or at-or-past 128 bits.  Distinct from
/// [`strider_ir::node::ValueType::bit_mask_u128`], which saturates to
/// `u128::MAX` at >= 128 bits — the truncate-fold guards bail on those widths
/// instead.
fn truncate_low_mask(ty: strider_ir::node::ValueType) -> Option<u128> {
    let bits = ty.bit_width();
    if bits == 0 || bits >= 128 {
        return None;
    }
    Some((1u128 << bits) - 1)
}

/// Builds the bitcast, extend/truncate round-trip, and truncate-folding
/// rule vec.
fn build_bitcast_extend_rules() -> Vec<crate::BoxedRule> {
    // Shared captures: every rule here is an independent query (fresh
    // `Bindings`), so one pool serves all rules. Within any single rule the
    // captures it binds are distinct ids.
    let (x, a, b, c) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    // IntBitsToFloat(FloatBitsToInt(x)) → x
    let rule_int_float = rewrite_rule(int_bits_to_float(float_bits_to_int(var(x))), var(x));

    // FloatBitsToInt(IntBitsToFloat(x)) → x
    let rule_float_int = rewrite_rule(float_bits_to_int(int_bits_to_float(var(x))), var(x));

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
        let pat = truncate(zero_extend(var(x))).when_match(move |ctx, ty, bnd| {
            bnd.get_type(x, ctx.function())
                .is_some_and(|x_ty| x_ty == ty)
        });
        rewrite_rule(pat, var(x))
    };

    // Truncate(SignExtend(x)) → x — same identity at the bit level when
    // widths match (sign-extension's added bits are sign replication; the
    // truncate cuts them off and recovers the original bits).
    let sext_round_trip = {
        let pat = truncate(sign_extend(var(x))).when_match(move |ctx, ty, bnd| {
            bnd.get_type(x, ctx.function())
                .is_some_and(|x_ty| x_ty == ty)
        });
        rewrite_rule(pat, var(x))
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
    let narrow_mul_through_sext = {
        let pat = truncate(mul(sign_extend(var(a)), sign_extend(var(b)))).when_match(
            move |ctx, ty, bnd| {
                bnd.get_type(a, ctx.function())
                    .is_some_and(|a_ty| a_ty == ty)
                    && bnd
                        .get_type(b, ctx.function())
                        .is_some_and(|b_ty| b_ty == ty)
            },
        );
        rewrite_rule(pat, template::mul(var(a), var(b)))
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
    // Two orientations are REQUIRED — a single pattern + commutative matching
    // is not enough.  The real x86-64 register-merge truncate is
    //   Truncate(Or( And(high_mask, rax_old), And(low_mask, zext(eax)) ))
    // i.e. BOTH `Or` operands are `And`s.  This rule wants the high-mask And
    // (its low W bits are zero, so it contributes nothing under the truncate).
    // With only the const-on-right-of-Or pattern, the matcher's commutative
    // `attempt(false)` greedily binds the `and(...)` subpattern to the FIRST
    // matching `Or` operand — the low-mask And — and the `low-bits-zero` guard
    // then rejects it.  Crucially, a `when_match` guard failure unwinds the
    // match WITHOUT re-driving the swapped operand order (matcher/walk.rs), so
    // the matcher never tries binding the subpattern to the high-mask And on
    // the other side.  The explicit swap rule puts `and(...)` on the other `Or`
    // operand, matching the high-mask And directly.  Same reasoning for the
    // And's own operand order below.  Regression-guarded by
    // `test_narrow_widths::x64` in the orchestrator's `calling_convention`
    // tests — it fails the moment either swap is dropped.
    let mk_drop_high_half = |swap: bool| -> BoxedRule {
        let guard = move |ctx: &strider_pattern::Matcher,
                          ty: strider_ir::node::ValueType,
                          bnd: &strider_pattern::Bindings| {
            let Some(c_val) = bnd.get_uint(c, ctx.function()) else {
                return false;
            };
            let Some(low_mask) = truncate_low_mask(ty) else {
                return false;
            };
            c_val & low_mask == 0
        };
        if swap {
            let pat =
                truncate(or(and(any_int_const().capture(c), var(b)), var(a))).when_match(guard);
            rewrite_rule(pat, template::truncate(var(a)))
        } else {
            let pat =
                truncate(or(var(a), and(any_int_const().capture(c), var(b)))).when_match(guard);
            rewrite_rule(pat, template::truncate(var(a)))
        }
    };

    // `Truncate_<W>(And(low_W_mask, x)) → Truncate_<W>(x)` — the AND's
    // effect of zeroing all bits above W is redundant when the truncate
    // is going to discard those bits anyway.  Two orientations for the same
    // first-success/no-backtrack reason as above.
    let mk_drop_low_mask_under_truncate = |swap: bool| -> BoxedRule {
        let guard = move |ctx: &strider_pattern::Matcher,
                          ty: strider_ir::node::ValueType,
                          bnd: &strider_pattern::Bindings| {
            let Some(c_val) = bnd.get_uint(c, ctx.function()) else {
                return false;
            };
            let Some(low_mask) = truncate_low_mask(ty) else {
                return false;
            };
            // The mask must cover at least the low W bits — anything beyond
            // that is fine since the truncate will drop those bits.
            c_val & low_mask == low_mask
        };
        if swap {
            let pat = truncate(and(var(x), any_int_const().capture(c))).when_match(guard);
            rewrite_rule(pat, template::truncate(var(x)))
        } else {
            let pat = truncate(and(any_int_const().capture(c), var(x))).when_match(guard);
            rewrite_rule(pat, template::truncate(var(x)))
        }
    };

    // Nested SAME-kind extends/truncates collapse to one at the outer width —
    // each of these casts is transitive, so the intermediate width drops out:
    //   ZeroExtend(ZeroExtend(x)) → ZeroExtend(x)   (zero-fill is transitive)
    //   SignExtend(SignExtend(x)) → SignExtend(x)   (sign replication is too)
    //   Truncate(Truncate(x))     → Truncate(x)     (narrowing twice == once)
    // The RHS cast inherits the rewrite root's (outer) width.  MIXED-kind nests
    // (zext∘sext / sext∘zext) are NOT a single cast, so they are left alone.
    // This lets the doubly-zero-extended compare MIPS emits for `sltu`
    // (`Equal(ZeroExtend(ZeroExtend(Less:I1)), 0)`) collapse to a single extend,
    // so FlagCmpCanonicalize's `Equal(ZeroExtend(b:I1), 0) → BitNot(b)` rule fires.
    let zext_zext = rewrite_rule(
        zero_extend(zero_extend(var(x))),
        template::zero_extend(var(x)),
    );
    let sext_sext = rewrite_rule(
        sign_extend(sign_extend(var(x))),
        template::sign_extend(var(x)),
    );
    let trunc_trunc = rewrite_rule(truncate(truncate(var(x))), template::truncate(var(x)));

    let rules: Vec<BoxedRule> = vec![
        rule_int_float,
        rule_float_int,
        zext_round_trip,
        sext_round_trip,
        zext_zext,
        sext_sext,
        trunc_trunc,
        narrow_mul_through_sext,
        mk_drop_high_half(false),
        mk_drop_high_half(true),
        mk_drop_low_mask_under_truncate(false),
        mk_drop_low_mask_under_truncate(true),
    ];
    rules
}

/// Builds the algebraic-identity rule vec for integer binary operations.
fn build_identity_rules() -> Vec<crate::BoxedRule> {
    let (x, c) = (Capture::new(), Capture::new());
    // x & all_ones → x  (commutative). The all-ones mask depends on the
    // output width, so we use `.when_match()` to compare the captured
    // constant against the node's output-type all-ones value.
    let all_ones_rule = {
        let pat = and(var(x), any_int_const().capture(c));
        let pat = pat.when_match(move |ctx, ty, b| {
            b.get_uint(c, ctx.function()) == ty.get_unsigned_int(u128::MAX)
        });
        rewrite_rule(pat, var(x))
    };
    // (No `x ^ all_ones → ~x` rule: `~x` IS `Xor(x, all_ones)` — the
    // canonical form — since the former BitNot unary-op was removed in favour
    // of the Xor shape.  Both compiler lowerings (`nor` and `xor a, -1`)
    // now lift to the same Xor shape directly at lift time.)
    // x | all_ones → all_ones  (commutative; absorbing element).  The
    // all-ones value depends on the output width, so match the captured
    // constant against the node's all-ones mask and rewrite to that same
    // constant.  At `I1` this subsumes the boolean `x | true → true`.
    let or_all_ones_rule = {
        let pat = or(var(x), any_int_const().capture(c));
        let pat = pat.when_match(move |ctx, ty, b| {
            b.get_uint(c, ctx.function()) == ty.get_unsigned_int(u128::MAX)
        });
        rewrite_rule(pat, var(c))
    };

    let rules: Vec<BoxedRule> = vec![
        // x + 0 → x  (commutative: also covers 0 + x)
        rewrite_rule(add(var(x), int_const(0u128)), var(x)),
        // x - 0 → x
        rewrite_rule(sub(var(x), int_const(0u128)), var(x)),
        // x - x → 0
        rewrite_rule(sub(var(x), var(x)), int_const(0u128)),
        // x ^ x → 0
        rewrite_rule(xor(var(x), var(x)), int_const(0u128)),
        // x ^ 0 → x  (commutative)
        rewrite_rule(xor(var(x), int_const(0u128)), var(x)),
        // x * 0 → 0  (commutative)
        rewrite_rule(mul(var(x), int_const(0u128)), int_const(0u128)),
        // x * 1 → x  (commutative)
        rewrite_rule(mul(var(x), int_const(1u128)), var(x)),
        // x & 0 → 0  (commutative)
        rewrite_rule(and(var(x), int_const(0u128)), int_const(0u128)),
        // x & x → x
        rewrite_rule(and(var(x), var(x)), var(x)),
        // x | 0 → x  (commutative)
        rewrite_rule(or(var(x), int_const(0u128)), var(x)),
        // x | x → x
        rewrite_rule(or(var(x), var(x)), var(x)),
        // x << 0 → x  (non-commutative — only RHS 0 is the identity)
        rewrite_rule(shl(var(x), int_const(0u128)), var(x)),
        // x >> 0 → x  (logical shift right)
        rewrite_rule(shr(var(x), int_const(0u128)), var(x)),
        // x >>> 0 → x  (arithmetic / signed shift right)
        rewrite_rule(sshr(var(x), int_const(0u128)), var(x)),
        all_ones_rule,
        or_all_ones_rule,
    ];
    rules
}

/// Builds the full constant-evaluation rule vec for integer binary ops,
/// integer unary ops, integer comparisons, truncate, extend (zero/sign),
/// popcount, and lzcount.
fn build_const_eval_rules() -> Vec<crate::BoxedRule> {
    let (op, l, r, v) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    let rules: Vec<BoxedRule> = vec![
        // 1. IntBinaryOp(op)(IntConst(l), IntConst(r)) =>
        //        int_const(eval_int_binary(op, l, r, ty)?, ty)
        //    `eval_int_binary` returns `None` for div-by-zero / signed
        //    overflow / I128+ masking failures; the closure opts out of the
        //    rewrite in that case via `strider_pattern::skip()`.
        {
            rewrite_rule(
                int_binary_any(any_int_const().capture(l), any_int_const().capture(r)).capture(op),
                int_const_with!([op: int_binary_op, l: uint, r: uint, ty] =>
                    eval_int_binary(op, l, r, ty)
                        .ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        // 2. IntUnaryOp(op)(IntConst(v)) => int_const(op(v) masked to ty, ty)
        //
        // `IntUnaryOp` now has only the `Neg` variant (bitwise complement is
        // `Xor(x, all_ones)`, handled by rule 1 above as an int-binary
        // const-fold), so the closure unconditionally evaluates `wrapping_neg`.
        {
            rewrite_rule(
                int_unary_any(any_int_const().capture(v)).capture(op),
                int_const_with!([op: int_unary_op, v: uint, ty] => {
                    let raw = match op {
                        strider_ir::IntUnaryOp::Neg => v.wrapping_neg(),
                    };
                    ty.get_unsigned_int(raw).ok_or_else(strider_pattern::skip)?
                }),
            )
        },
        // 3. IntCmpOp(op)(IntConst(l), IntConst(r)) =>
        //        bool_const(eval_int_cmp(op, l, r, in_ty)?)
        //    `in_ty` = root's first-value-input type, which on an IntCmp is
        //    the LHS operand type — exactly what `eval_int_cmp` expects.
        //    `eval_int_cmp` returns `Result<bool, opt::ErrorKind>`; the `?`
        //    in the closure bridges that failure into a rewrite skip.
        {
            rewrite_rule(
                int_cmp_any(any_int_const().capture(l), any_int_const().capture(r)).capture(op),
                bool_const_with!([op: int_cmp_op, l: uint, r: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    eval_int_cmp(op, l, r, input_ty)?
                }),
            )
        },
        // 4. Truncate(IntConst(v)) => int_const(v masked to ty, ty)
        //    The wider IntConst's raw value is *not* automatically masked
        //    to the truncate's output width here. Mask explicitly so we
        //    don't plant an unmasked
        //    narrow IntConst into the IR. Skip when ty is I128/I256 (the
        //    truncate output is always narrower than I64 in practice, but
        //    the skip costs nothing and is consistent with other rules).
        {
            rewrite_rule(
                truncate(any_int_const().capture(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(strider_pattern::skip)?
                ),
            )
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
            rewrite_rule(
                zero_extend(any_int_const().capture(v)),
                int_const_with!([v: uint, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        // 6. SignExtend(IntConst(v)) =>
        //        int_const(sign_extend(v, in_ty) masked to ty, ty)
        //    `in_ty` is the narrower input type; `get_signed_int` produces
        //    the sign-extended i128 value, which `get_unsigned_int` then
        //    masks to the wider output width.
        {
            rewrite_rule(
                sign_extend(any_int_const().capture(v)),
                int_const_with!([v: uint, in_ty, ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    let signed = super::eval_int::require_signed(input_ty, v)? as u128;
                    ty.get_unsigned_int(signed).ok_or_else(strider_pattern::skip)?
                }),
            )
        },
        // 7. Popcount(IntConst(v)) =>
        //        int_const(masked(v, in_ty).count_ones(), ty)
        {
            rewrite_rule(
                popcount(any_int_const().capture(v)),
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    let masked = input_ty
                        .get_unsigned_int(v)
                        .ok_or_else(strider_pattern::skip)?;
                    u128::from(masked.count_ones())
                }),
            )
        },
        // 8. Lzcount(IntConst(v)) =>
        //        int_const(N if masked == 0 else (masked << (128 - N)).leading_zeros(), ty)
        //    The `masked == 0` case must return the input type's bit width;
        //    shifting by (128 - bits) aligns to the u128's MSB so
        //    `leading_zeros()` gives the correct count within the type's width.
        {
            rewrite_rule(
                lzcount(any_int_const().capture(v)),
                int_const_with!([v: uint, in_ty] => {
                    let input_ty = in_ty.ok_or_else(strider_pattern::skip)?;
                    let masked = input_ty
                        .get_unsigned_int(v)
                        .ok_or_else(strider_pattern::skip)?;
                    let bits = input_ty.bit_width() as u32;
                    // Lzcount fold is only computable when the input type
                    // fits in u128.  Wider widths (I256) skip cleanly — the
                    // rule simply doesn't fire and the IR keeps the Lzcount
                    // node as opaque.
                    if bits > 128 {
                        return Err(strider_pattern::skip());
                    }
                    if masked == 0 {
                        u128::from(bits)
                    } else if bits == 128 {
                        u128::from(masked.leading_zeros())
                    } else {
                        u128::from((masked << (128 - bits)).leading_zeros())
                    }
                }),
            )
        },
    ];
    rules
}

/// Builds the constant-evaluation and absorbing-element rule vec for the
/// I1 boolean ops and all float ops.
fn build_bool_float_rules() -> Vec<crate::BoxedRule> {
    let (op, l, r, v, x) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

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
        { rewrite_rule(bool_not(bool_not(var(x))), var(x)) },
        // FloatBinaryOp(op)(FloatConst(l), FloatConst(r)) =>
        //     float_const(eval_float_binary(op, l, r, ty)?)
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
        // FloatUnaryOp(op)(FloatConst(v)) => float_const(eval_float_unary(op, v, ty)?)
        {
            rewrite_rule(
                float_unary_any(any_float_const().capture(v)).capture(op),
                float_const_with!([op: float_unary_op, v: float_bits, ty] =>
                    eval_float_unary(op, v, ty)
                        .ok_or_else(strider_pattern::skip)?
                ),
            )
        },
        // FloatCmpOp(op)(FloatConst(l), FloatConst(r)) =>
        //     bool_const(eval_float_cmp(op, l, r, in_ty)?)
        //   `in_ty` = root's first-value-input type (the float operand type).
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
    ];
    rules
}
