//! `FlagCmpCanonicalize` — recognises the multi-node "flag-tree" shapes that
//! AArch64 (and similar flag-register architectures) emit when lifting
//! `cmp`-then-branch sequences, and rewrites them into a single direct
//! [`strider_ir::IntCmpOp`] node against the original `(a, b)` pair.
//!
//! AArch64 `cmp a, b` lifts (post canonicalisation of `IntSub` and
//! `IntLessEqual`) to four flag computations:
//!
//! ```text
//! diff = Add(a, Neg(b))           // post-canonical IntSub
//! ZR   = Equal(diff, 0)           // Z flag
//! NG   = IntSless(diff, 0)        // N flag
//! CY   = BitNot(IntLess(a, b))    // C flag, at I1 (post lower of IntLessEqual)
//! OV   = IntSborrow(a, b)         // V flag
//! ```
//!
//! The 14 conditional-branch codes each read a fixed boolean tree of
//! these flags.  Of those:
//!
//! * `EQ`/`NE`   — bare `ZR` (and its negation).
//! * `CS/CC`     — bare `CY` / `BitNot(CY)` — already in `(a, b)` form;
//!   `ConstantFold` collapses `BitNot(BitNot(IntLess(a, b))) → IntLess(a, b)`.
//! * `MI/PL`     — bare `NG` / `BitNot(NG)`.  `NG` is `Sless(a-b, 0)`,
//!   which is *not* the same as `Sless(a, b)` due to subtraction overflow.
//!   Left untouched.
//! * `VS/VC`     — bare `OV` / `BitNot(OV)` — already in `(a, b)` form.
//! * `HI/LS`     — `BoolAnd(CY, BitNot(ZR))` / its De Morgan dual.
//! * `GE/LT`     — `Equal(NG, OV)` / its negation.
//! * `GT/LE`     — `BoolAnd(BitNot(ZR), Equal(NG, OV))` / its De Morgan dual.
//!
//! This pass owns the `ZR`-leaf simplification and the seven flag-tree
//! shapes (`EQ` / `HI` / `LS` / `LT` / `GE` / `GT` / `LE`).  After this
//! pass and `IfCondInversion` run, every recognised flag-test branch
//! consumes a direct `IntCmpOp::{Equal, Less, Sless}` against the
//! original operands — which is exactly what the jump-table bound walker
//! in [`crate::indirect_branch_resolve`] needs.
//!
//! ## Pipeline placement
//!
//! Run after `ConstantFold` (so `BitNot(BitNot(x)) → x` at `I1` collapses
//! before we look for the canonical shape) and before `IfCondInversion`
//! (so the cond it sees has only one possible BitNot-wrapping layer).
//!
//! ## Asm-fingerprint preservation
//!
//! Every rule is built via [`crate::rewrite_rule`], which absorbs the
//! matched root's fingerprint into **every** freshly-created interior
//! node of the RHS subtree (not just the outermost root).  This makes
//! the per-rule fingerprint discipline automatic; previously the pass
//! carried a bespoke `Rule { build_rhs: fn(...) -> ValueId }`
//! infrastructure that hand-rolled the per-node fingerprint absorption
//! — see `strider_pattern::rewrite::rewrite_rule` for the central walk.

use std::rc::Rc;

use crate::{BoxedRule, apply_rules_in_order, rewrite_rule};
use strider_ir::IRViewer;
use strider_ir::node::{ExtendOp, IntBinaryOp, NodeId, NodeKind, ValueId, ValueType};
use strider_pattern::{
    Bindings, Capture, CaptureExt, add, any_int_const, bool_and, bool_not, bool_or, capture_typed,
    int_const, int_const_with, int_eq, int_lt, int_sborrow, int_slt, neg, one_of, shl, template,
    var, xor, zero_extend,
};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite, SeedOrder};

/// Pass that rewrites flag-tree `If` conds into single `IntCmpOp`s.
///
/// The rewrite-rule table is built once by [`FlagCmpCanonicalize::new`]
/// and held behind an [`Rc`] so the pass stays cheaply `Clone` (the boxed
/// rule closures are not `Clone`); cloning the pass shares the same table.
#[derive(Clone)]
pub struct FlagCmpCanonicalize {
    rules: Rc<Vec<BoxedRule>>,
}

impl FlagCmpCanonicalize {
    /// Builds the flag-tree rewrite-rule table once and returns a pass
    /// that owns it.
    pub fn new() -> Self {
        Self {
            rules: Rc::new(build_rules()),
        }
    }
}

impl Default for FlagCmpCanonicalize {
    fn default() -> Self {
        Self::new()
    }
}

impl PeepholePass for FlagCmpCanonicalize {
    /// Rules walk arbitrary boolean / arith subtrees; no useful kind
    /// filter at the root — defer to the per-rule matcher.
    fn matches_kind(&self, _kind: &NodeKind) -> bool {
        true
    }

    /// Flag-tree rules collapse an outer shape (e.g. `Xor(Equal(NG,OV),1)`)
    /// to a single `IntCmpOp`.  Seed top-down so the outermost node is
    /// visited first: a bottom-up (reverse-post-order) seed would rewrite an
    /// inner sub-pattern first and destroy the outer match.
    fn seed_order(&self) -> SeedOrder {
        SeedOrder::Postorder
    }

    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        // PowerPC condition-register bit test: an imperative arm, because the
        // variable-arity OR pack `Or(ShiftLeft(ZeroExtend(cmp_i), pos_i)…)` does
        // not fit the fixed-shape `rewrite_rule` DSL.
        if let Some(cmp) = canonicalize_cr_bit_test(edit, root)? {
            return Ok(PeepholeRewrite::from_new_value(edit, Some(cmp)));
        }
        let opt = apply_rules_in_order(&self.rules)(edit, root)?;
        Ok(PeepholeRewrite::from_new_value(edit, opt))
    }

    /// Flag-tree rules fire at the outermost root; once a tree
    /// collapses to a single `IntCmpOp`, its consumers cannot match a
    /// fresh flag-tree shape.  Skip the consumer re-enqueue — the
    /// `ConstantFold` / `IfCondInversion` passes that run alongside in
    /// the fixed-point loop handle any follow-on simplification.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

// ── Rule table ────────────────────────────────────────────────────────────
//
// Each entry is `rewrite_rule(lhs, rhs)`: pattern crate matches the LHS,
// builds the RHS template, and rewires uses with full asm-fingerprint
// absorption into every fresh interior node.

/// Shared `when_match` guard for the constant-folded-ZF rules whose Equal
/// offset is the two's-complement negation of the compare constant
/// (`M ≡ -N`, rules 14/16).  `width_src` is the capture whose output type
/// supplies the operand width.  Returns `false` unless all three bindings
/// resolve.
fn neg_relation(
    binds: &Bindings,
    func: &strider_ir::Function,
    m: Capture,
    n: Capture,
    width_src: Capture,
) -> bool {
    let (Some(m_val), Some(n_val), Some(width)) = (
        binds.get_uint(m, func),
        binds.get_uint(n, func),
        binds.get_type(width_src, func).map(|t| t.bit_mask_u128()),
    ) else {
        return false;
    };
    (m_val & width) == (n_val.wrapping_neg() & width)
}

/// Shared `when_match` guard for the offset-base constant-folded-ZF rules
/// whose Equal offset is the compare-base offset minus the compare constant
/// (`C2 ≡ C1 - N`, rules 15/17).  `width_src` is the capture whose output
/// type supplies the operand width.  Returns `false` unless all four
/// bindings resolve.
fn sub_relation(
    binds: &Bindings,
    func: &strider_ir::Function,
    m: Capture,
    n: Capture,
    c1: Capture,
    width_src: Capture,
) -> bool {
    let (Some(m_val), Some(n_val), Some(c1_val), Some(width)) = (
        binds.get_uint(m, func),
        binds.get_uint(n, func),
        binds.get_uint(c1, func),
        binds.get_type(width_src, func).map(|t| t.bit_mask_u128()),
    ) else {
        return false;
    };
    (m_val & width) == (c1_val.wrapping_sub(n_val) & width)
}

fn build_rules() -> Vec<BoxedRule> {
    // Captures are shared across rules: each rule is matched as an independent
    // query (its own fresh `Bindings`), so reusing one `(a, b)` pair carries no
    // cross-rule state.  `a` / `b` are the two distinct operands any single
    // rule binds (a few rules use only `b`).  Within one rule a capture still
    // means "the same node everywhere it appears" — that link is intra-rule.
    let a = Capture::new();
    let b = Capture::new();
    // Extra captures for the constant-folded LS rule (rule 14): the `Less`
    // constant `N` and the `Add` constant `M = -N`.
    let n = Capture::new();
    let m = Capture::new();
    // Extra captures for the offset-base LS rule (rule 15): the compared
    // value's own add-offset `C1` (so the compared value is `Add(b, C1)`) and
    // the whole compared value `X` reused on the RHS.
    let c1 = Capture::new();
    let x = Capture::new();

    // Emits an LS rule and its De-Morgan HI dual (rules 14/16 and 15/17) as a
    // `[BoxedRule; 2]`.  The `$less` / `$eq` / `$guard` / `$cmp` fragments are
    // re-evaluated in each arm (the pattern builders are not `Clone`), so both
    // rules bind the identical captures under the identical guard:
    //   LS:  Or(less, eq).when_match(guard)            → BitNot(cmp)
    //   HI:  And(BitNot(less), BitNot(eq)).when_match(guard) → cmp
    macro_rules! ls_hi_pair {
        ($less:expr, $eq:expr, $guard:expr, $cmp:expr $(,)?) => {
            [
                rewrite_rule(
                    bool_or($less, $eq).when_match($guard),
                    template::bool_not($cmp),
                ),
                rewrite_rule(
                    bool_and(bool_not($less), bool_not($eq)).when_match($guard),
                    $cmp,
                ),
            ]
        };
    }

    let mut rules: Vec<BoxedRule> = vec![
        // 1. EQ / ZR identity:  Equal(Add(a, Neg(b)), 0) → Equal(a, b)
        rewrite_rule(
            int_eq(add(var(a), neg(var(b))), int_const(0u128)),
            template::int_eq(var(a), var(b)),
        ),
        // 2+12. HI → IntLess(b, a), whichever arch shape the flag tree has:
        //    raw NZCV:    BoolAnd(BitNot(IntLess(a, b)), BitNot(Equal(diff, 0)))
        //    decomposed:  BoolAnd(BitNot(Equal(a, b)), BitNot(IntLess(a, b)))
        //    (ARM/Thumb + post-ConstantFold leave the decomposed form; both are
        //    sound HI ≡ b<a identities, so one rule with a `one_of` LHS covers
        //    both — the raw and decomposed shapes are structurally disjoint.)
        rewrite_rule(
            one_of![
                bool_and(
                    bool_not(int_lt(var(a), var(b))),
                    bool_not(int_eq(add(var(a), neg(var(b))), int_const(0u128))),
                ),
                bool_and(
                    bool_not(int_eq(var(a), var(b))),
                    bool_not(int_lt(var(a), var(b))),
                ),
            ],
            template::int_lt(var(b), var(a)),
        ),
        // 3+13. LS → BitNot(IntLess(b, a)), whichever arch shape:
        //    raw NZCV:    BoolOr(IntLess(a, b), Equal(diff, 0))
        //    decomposed:  BoolOr(Equal(a, b), IntLess(a, b))
        //    (raw assumes ConstantFold cancelled the `BitNot(BitNot(IntLess))`
        //    chain that `BitNot(CY)` produces.)
        rewrite_rule(
            one_of![
                bool_or(
                    int_lt(var(a), var(b)),
                    int_eq(add(var(a), neg(var(b))), int_const(0u128)),
                ),
                bool_or(int_eq(var(a), var(b)), int_lt(var(a), var(b))),
            ],
            template::bool_not(template::int_lt(var(b), var(a))),
        ),
        // 4. LT:  BitNot(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b)))) → IntSless(a, b)
        rewrite_rule(
            bool_not(int_eq(
                zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                zero_extend(int_sborrow(var(a), var(b))),
            )),
            template::int_slt(var(a), var(b)),
        ),
        // 5. GE:  Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))) → BitNot(IntSless(a, b))
        rewrite_rule(
            int_eq(
                zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                zero_extend(int_sborrow(var(a), var(b))),
            ),
            template::bool_not(template::int_slt(var(a), var(b))),
        ),
        // 6+10. GT → IntSless(b, a), whichever arch shape:
        //    raw NZCV:    BoolAnd(BitNot(Equal(diff, 0)),
        //                    Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))))
        //    decomposed:  BoolAnd(BitNot(Equal(a, b)), BitNot(IntSless(a, b)))
        //                    ≡ (a≠b) ∧ ¬(a<b) ≡ a>b ≡ b<a
        rewrite_rule(
            one_of![
                bool_and(
                    bool_not(int_eq(add(var(a), neg(var(b))), int_const(0u128))),
                    int_eq(
                        zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                        zero_extend(int_sborrow(var(a), var(b))),
                    ),
                ),
                bool_and(
                    bool_not(int_eq(var(a), var(b))),
                    bool_not(int_slt(var(a), var(b))),
                ),
            ],
            template::int_slt(var(b), var(a)),
        ),
        // 7+11. LE → BitNot(IntSless(b, a)), whichever arch shape:
        //    raw NZCV:    BoolOr(Equal(diff, 0),
        //                    BitNot(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b)))))
        //    decomposed:  BoolOr(Equal(a, b), IntSless(a, b))  ≡ (a=b) ∨ (a<b) ≡ a≤b ≡ ¬(b<a)
        rewrite_rule(
            one_of![
                bool_or(
                    int_eq(add(var(a), neg(var(b))), int_const(0u128)),
                    bool_not(int_eq(
                        zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                        zero_extend(int_sborrow(var(a), var(b))),
                    )),
                ),
                bool_or(int_eq(var(a), var(b)), int_slt(var(a), var(b))),
            ],
            template::bool_not(template::int_slt(var(b), var(a))),
        ),
        // 8. Thumb "false" flag test:  IntEqual(ZeroExtend(b), 0)  →  BitNot(b)
        //    Lifted by Thumb BNE / BCC / BPL / BVC, where the cond is
        //    `IntEqual(flag, 0)` rather than `BitNot(flag)` directly.
        //    Only sound when `b` is the 1-bit flag itself: `zext(b) == 0`
        //    equals `!b` only for an `I1` `b`.  Without the guard a chained
        //    zero-extend (e.g. `I1 → I8 → I32`) would bind `b` to the wider
        //    intermediate, yielding a malformed `BitNot` of a non-`I1` value.
        rewrite_rule(
            int_eq(zero_extend(var(b).of_width(1)), int_const(0u128)),
            template::bool_not(var(b)),
        ),
        // 9. Thumb "true" flag test:  BitNot(IntEqual(ZeroExtend(b), 0))  →  b
        //    Lifted by Thumb BEQ / BCS / BMI / BVS — the lift-time
        //    canonicalisation `IntNotEqual(b, 0) → BitNot(IntEqual(b, 0))`
        //    plus our cast-to-int coercion gives this shape.  Same `I1`
        //    guard as rule 8: replacing the test with `b` only preserves
        //    booleanness when `b` is the 1-bit flag.
        rewrite_rule(
            bool_not(int_eq(zero_extend(var(b).of_width(1)), int_const(0u128))),
            var(b),
        ),
        // (The decomposed `(a≠b)∧¬(a<b)` / `(a=b)∨(a<b)` flag-tree shapes that
        // ARM/Thumb and post-ConstantFold trees leave are folded into rules
        // 2/3/6/7 above as the second `one_of` alternative — see the note there.
        // ConstantFold does NOT pre-decompose symbolic flag trees; that
        // decomposition is performed entirely by `FlagCmpCanonicalize`.)
    ];

    // 14/16 and 15/17 are exact De-Morgan duals: the LS form is
    //   Or(LessTerm, EqTerm) → BitNot(Cmp)
    // and the HI form negates each leaf and flips the connective + the RHS:
    //   And(BitNot(LessTerm), BitNot(EqTerm)) → Cmp
    // binding the IDENTICAL captures under the IDENTICAL guard.  `ls_hi_pair!`
    // takes the shared less-term / eq-term / guard / RHS-comparison fragments
    // (re-evaluated in each arm, since the pattern builders are not `Clone`)
    // and emits the LS rule followed by its HI dual — collapsing the former four
    // hand-spelled `bool_or`/`bool_and`/`bool_not` trees into two invocations.
    // Mirrors the `const_on_right!` / `ext_round_trip!` precedents in
    // `constant_fold/rules.rs`.  Order (14, 16, 15, 17) is immaterial: the LS
    // (`Or`-rooted) and HI (`And`-rooted) shapes are structurally disjoint, so
    // any one node matches at most one rule.
    //
    // 14. LS:  Or(Less(a, IntConst(N)), Equal(Add(a, IntConst(M)), 0)) → BitNot(Less(N, a))
    //     (a < N) ∨ (a == N) ≡ a ≤ N ≡ ¬(N < a).  The `cmp a, N; ja`/`jbe`
    //     flag tree with a CONSTANT compare operand, after `ConstantFold`
    //     collapsed the lifted `Equal(Add(a, Neg(N)), 0)` to
    //     `Equal(Add(a, IntConst(M)), 0)` with `M = -N`.  The guard pins
    //     `M ≡ -N` (mod width); the captured `IntConst(N)` is reused on the
    //     RHS (width-correct by construction, no synthesised constant).
    // 16. HI (the dual):  And(BitNot(Less(a, N)), BitNot(Equal(Add(a, M), 0))) → Less(N, a)
    //     (a >= N) ∧ (a != N) ≡ a > N ≡ N < a.  The `cmp a, N; bhi`/`ja`
    //     tree; same `M ≡ -N` guard.
    rules.extend(ls_hi_pair!(
        int_lt(var(a), any_int_const().capture(n)),
        int_eq(add(var(a), any_int_const().capture(m)), int_const(0u128)),
        move |edit, _ty, binds| neg_relation(binds, edit.function(), m, n, a),
        template::int_lt(var(n), var(a)),
    ));
    // 15. LS, offset-base:  Or(Less(Add(b, C1), N), Equal(Add(b, C2), 0))
    //     → BitNot(Less(N, Add(b, C1))).  With `X = Add(b, C1)`:
    //     (X < N) ∨ (X == N) ≡ X ≤ N.  gcc emits `sub b, K; cmp (b-K), N; ja`,
    //     so the compared value is the OFFSET index `X`; the ZF term folds to
    //     `Equal(Add(b, C2), 0)` with `C2 = C1 - N`, so the Less operand and
    //     the Equal base are DISTINCT nodes.  Keys on the shared base `b`,
    //     reuses the captured `X` on the RHS; the guard pins `C2 ≡ C1 - N`.
    // 17. HI (the dual + offset sibling of 16):
    //     And(BitNot(Less(Add(b, C1), N)), BitNot(Equal(Add(b, C2), 0)))
    //     → Less(N, Add(b, C1)).  With `X = Add(b, C1)`: (X >= N) ∧ (X != N) ≡
    //     X > N.  A masked / offset switch (Thumb `and r0,#7; subs r0,#1;
    //     cmp r0,#N-1; bhi`); same `C2 ≡ C1 - N` guard.
    rules.extend(ls_hi_pair!(
        int_lt(
            add(var(b), any_int_const().capture(c1)).capture(x),
            any_int_const().capture(n),
        ),
        int_eq(add(var(b), any_int_const().capture(m)), int_const(0u128)),
        move |edit, _ty, binds| sub_relation(binds, edit.function(), m, n, c1, b),
        template::int_lt(var(n), var(x)),
    ));

    // Comparison-with-constant canonicalisation: `Equal(Add(x, C1), C2) →
    // Equal(x, C2 - C1)` ("solve for x").  Sound for any width/signedness —
    // fixed-width add wraps mod 2^W and `Equal` tests that residue, so
    // `x + C1 ≡ C2` iff `x ≡ C2 - C1 (mod 2^W)`.  `Equal`/`Add` are commutative,
    // so the matcher also covers the `C2 == Add(x, C1)` / `Add(C1, x)` orderings.
    //
    // Placed in FlagCmp — not ConstantFold — because ConstantFold runs it too
    // early and starves the `Or(Less, Equal(diff, 0))` flag-idiom (rules 15/17
    // above) of the `Equal(diff, 0)` shape.  Here it is safe: FlagCmp seeds
    // OUTERMOST-first (see `seed_order`), so on an `idx ≤ N` flag idiom the `Or`
    // root is rewritten by the LS rule (which consumes the inner `Equal`) BEFORE
    // this rule can reach that `Equal`.  A standalone `Equal(Add(x,C1),C2)` — not
    // under such an `Or` — folds here as intended.
    rules.push(rewrite_rule(
        int_eq(
            add(var(a), any_int_const().capture(n)),
            any_int_const().capture(m),
        ),
        template::int_eq(
            var(a),
            // `of_input_type`: the fresh `C2 - C1` const takes the operand
            // width, not the `Equal` root's `I1` output width.
            int_const_with!([n: uint, m: uint] => m.wrapping_sub(n)).of_input_type(),
        ),
    ));

    // Sibling "solve for x" canonicalisations across `Equal`, same seed-order
    // safety and `of_input_type` width handling as the `Add` rule above.
    //
    // `Equal(Xor(x, C1), C2) → Equal(x, C1 ^ C2)` — xor-with-C1 is a bijection,
    // so applying it to both sides is value-preserving.  `Xor` is commutative,
    // so `C1` on either operand of the xor matches.
    rules.push(rewrite_rule(
        int_eq(
            xor(var(a), any_int_const().capture(n)),
            any_int_const().capture(m),
        ),
        template::int_eq(
            var(a),
            int_const_with!([n: uint, m: uint] => n ^ m).of_input_type(),
        ),
    ));

    // `Equal(Neg(x), C) → Equal(x, -C)` — two's-complement negation is a
    // bijection, so it moves across `Equal` value-preservingly.
    rules.push(rewrite_rule(
        int_eq(neg(var(a)), any_int_const().capture(m)),
        template::int_eq(
            var(a),
            int_const_with!([m: uint] => m.wrapping_neg()).of_input_type(),
        ),
    ));

    // `Sless(ShiftLeft(x, C), 0):I1 → Xor(Equal(And(x, mask), 0), 1):I1`,
    // mask = 1 << (W-1-C).  A signed `< 0` on a left-shifted value tests the
    // sign bit of `x << C`, which is bit (W-1-C) of `x`; canonicalising to the
    // explicit single-bit mask test makes it match the shape a plain
    // `if (x & mask)` lifts to.  The `And`/mask/`0` are width `W` (from
    // `capture_typed(x, ..)` — the `Xor` root is `I1` and exposes no `x`-wide
    // input), the `Xor`/`1` are `I1`.  Guarded to a constant `C < W` (else
    // `x << C` is 0 and the test is const-false — a different rewrite).
    rules.push(rewrite_rule(
        int_slt(shl(var(x), any_int_const().capture(n)), int_const(0u128)).when_match(
            move |edit, _ty, b| {
                let (Some(c), Some(ty)) =
                    (b.get_uint(n, edit.function()), b.get_type(x, edit.function()))
                else {
                    return false;
                };
                c < ty.bit_width() as u128
            },
        ),
        template::bool_not(template::int_eq(
            capture_typed(
                x,
                template::and(
                    var(x),
                    capture_typed(
                        x,
                        int_const_with!([n: uint, in_ty] => {
                            let w = in_ty.ok_or_else(strider_pattern::skip)?.bit_width() as u128;
                            1u128 << (w - 1 - n)
                        }),
                    ),
                ),
            ),
            capture_typed(x, int_const(0u128)),
        )),
    ));

    rules
}

// ── PowerPC condition-register bit test ──────────────────────────────────────
//
// `cmpwi a, N` writes a 4-bit CR field — LT/GT/EQ/SO — and a conditional branch
// tests one bit.  The lifter models the field as a packed word
// `Or(ShiftLeft(ZeroExtend(cmp_i:I1), pos_i) …)` and the branch reads
// `Truncate(ShiftRight(pack, k)):I1` (the `Truncate` keeps bit 0, i.e. bit `k`
// of the pack).  We rewrite that to the single `IntCmpOp` sitting at bit `k`,
// the bare-comparison form every other architecture already produces.

/// If `root` is a PowerPC CR-bit test `Truncate(ShiftRight(pack, k)):I1`,
/// rewrite the condition to the comparison at bit `k` and return it.  `None`
/// (no change) on any shape it can't prove a true identity for.
fn canonicalize_cr_bit_test(
    edit: &mut crate::EditFunction<'_>,
    root: NodeId,
) -> Result<Option<ValueId>> {
    let Some((cond_out, cmp)) = cr_bit_comparison(edit, root) else {
        return Ok(None);
    };
    // The CR-pack interior nodes (`Truncate` / `ShiftRight` / the `Or` tree /
    // each term's `ShiftLeft(ZeroExtend(..))` and its comparison) carry the asm
    // addresses of the `crset` / `cror` / `cmpwi` instructions that built the
    // CR field.  `replace_value` below absorbs only the immediate `Truncate`'s
    // fingerprint into `cmp`, so fold the rest of the pack in first — otherwise
    // those addresses vanish when the pack is culled, violating the
    // superset-only asm-fingerprint contract. (The declarative flag rules get
    // this for free from the rewrite engine's matched-interior absorption; this
    // hand-written arm must do it explicitly.)
    absorb_cr_pack_fingerprints(edit, cond_out, cmp);
    edit.replace_value(cond_out, cmp)?;
    Ok(Some(cmp))
}

/// Folds every CR-pack interior node's asm-fingerprint into `cmp`'s producer
/// (the surviving comparison) so the rewrite preserves the superset-only
/// fingerprint contract once the pack is culled.  Walks the input cone from
/// `cond_out`'s producer (the `Truncate`) toward the comparison terms,
/// stopping the descent at each `IntCmpOp` — a comparison carries its
/// instruction's address on its own node, and its operands are the unrelated
/// compared values (often live elsewhere), not pack-building instructions.
fn absorb_cr_pack_fingerprints(
    edit: &mut crate::EditFunction<'_>,
    cond_out: ValueId,
    cmp: ValueId,
) {
    let into = edit.producer(cmp);
    let mut stack = vec![edit.producer(cond_out)];
    // Dense visited set + ordered interior list: O(1) membership instead of the
    // former O(pack²) `Vec::contains`, honouring the "prefer entity-utils" /
    // O(n) convention for the pack-interior walk.
    let mut visited: entity_utils::DenseEntitySet<NodeId> = entity_utils::DenseEntitySet::new();
    let mut interior: Vec<NodeId> = Vec::new();
    while let Some(n) = stack.pop() {
        if !visited.insert(n) {
            continue;
        }
        interior.push(n);
        // A comparison term (including `cmp` itself) ends the descent.
        if matches!(edit.node_kind(n), NodeKind::IntCmpOp(_)) {
            continue;
        }
        stack.extend(crate::peephole::input_producers_iter(edit, n));
    }
    for n in interior {
        if n != into {
            edit.function_mut()
                .side_tables_mut()
                .extend_asm_fingerprint_from(into, n);
        }
    }
}

/// Reads (without mutating) the `(condition-output, comparison)` pair for a CR-
/// bit test rooted at `root`.  Returns `Some` only when every OR term of the
/// pack is a provable single-bit value at a DISTINCT position and exactly one —
/// at the tested bit — carries a comparison; then bit `k` of the pack equals
/// that comparison for ALL inputs, so replacing the condition with it is a true
/// identity (no KnownBits needed: a `ZeroExtend` of a 1-bit value is ∈{0,1}, so
/// `ShiftLeft(zext(I1), pos)` provably sets only bit `pos`).
fn cr_bit_comparison(f: &impl IRViewer, root: NodeId) -> Option<(ValueId, ValueId)> {
    if !matches!(f.node_kind(root), NodeKind::Truncate) {
        return None;
    }
    let cond_out = *f.node_outputs(root).first()?;
    if f.value_type_opt(cond_out) != Some(ValueType::I1) {
        return None;
    }
    let [inner] = f.node_inputs_exact::<1>(root).ok()?;
    // `Truncate(_):I1` exposes bit 0 of its input; a `ShiftRight(x, k)` input
    // shifts bit k of `x` down to bit 0.
    let (pack, bit) = match *f.kind_of_value(inner) {
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => {
            let [x, amt] = f.producer_inputs_exact::<2>(inner).ok()?;
            let k = u32::try_from(f.int_const_u128(amt)?).ok()?;
            (x, k)
        }
        _ => (inner, 0),
    };

    let mut terms = Vec::new();
    flatten_or(f, pack, &mut terms, 0);
    let mut positions: Vec<u32> = Vec::new();
    let mut found: Option<ValueId> = None;
    for term in terms {
        // A structural zero contributes no bit.
        if f.int_const_u128(term) == Some(0) {
            continue;
        }
        let (pos, cmp) = single_bit_term(f, term, 0)?;
        if positions.contains(&pos) {
            return None; // two terms overlap a bit — can't isolate `bit`
        }
        positions.push(pos);
        if pos == bit {
            // The tested bit must carry a comparison (not an opaque masked bit).
            found = Some(cmp?);
        }
    }
    found.map(|cmp| (cond_out, cmp))
}

/// Flattens the CR-field `Or` tree into its leaf terms.  A single CR field is
/// 4 bits (LT/GT/EQ/SO), so the pack is at most 4 terms — at most 3 binary
/// `Or` nodes.  The recursion cap is set to `MAX_OR_DEPTH = 4` (one level of
/// slack over the structurally-needed depth of 3) and rejects anything wider:
/// a misrouted full-CR `mfcr` pack never reaches this single-field shape, and
/// any over-deep `Or` is pushed as an opaque leaf, which `single_bit_term`
/// then rejects → no fold.
fn flatten_or(f: &impl IRViewer, value: ValueId, out: &mut Vec<ValueId>, depth: u32) {
    const MAX_OR_DEPTH: u32 = 4;
    if depth <= MAX_OR_DEPTH
        && let NodeKind::IntBinaryOp(IntBinaryOp::Or) = f.kind_of_value(value)
        && let Ok([a, b]) = f.producer_inputs_exact::<2>(value)
    {
        flatten_or(f, a, out, depth + 1);
        flatten_or(f, b, out, depth + 1);
        return;
    }
    out.push(value);
}

/// Classifies a CR-pack term as `(bit position, comparison at that bit)`,
/// proving structurally that the term sets ONLY that one bit.  Returns `None`
/// for any shape that isn't a provable single-bit value.  The comparison is
/// `Some` only for a `ZeroExtend(IntCmpOp)` leaf; an opaque masked bit (the SO
/// flag) yields `Some((pos, None))` — a known single bit, but not a comparison.
fn single_bit_term(
    f: &impl IRViewer,
    value: ValueId,
    depth: u32,
) -> Option<(u32, Option<ValueId>)> {
    const MAX_DEPTH: u32 = 32;
    if depth > MAX_DEPTH {
        return None;
    }
    match *f.kind_of_value(value) {
        // `ShiftLeft(v, pos)` moves v's single bit up by `pos`.
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            let [v, amt] = f.producer_inputs_exact::<2>(value).ok()?;
            let pos = u32::try_from(f.int_const_u128(amt)?).ok()?;
            let (q, cmp) = single_bit_term(f, v, depth + 1)?;
            Some((q.checked_add(pos)?, cmp))
        }
        // `ZeroExtend` zero-fills above the source, so the set-bit position is
        // unchanged; a 1-bit (I1) source sets only bit 0.
        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            let [src] = f.producer_inputs_exact::<1>(value).ok()?;
            match f.value_type_opt(src) {
                // A 1-bit source sets bit 0; carry the comparison leaf only
                // when the source is itself an `IntCmpOp`.
                Some(ValueType::I1) => Some((
                    0,
                    matches!(f.kind_of_value(src), NodeKind::IntCmpOp(_)).then_some(src),
                )),
                _ => single_bit_term(f, src, depth + 1),
            }
        }
        // `And(v, single-bit const)` keeps only that bit (value unknown → no cmp).
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            let [a, b] = f.producer_inputs_exact::<2>(value).ok()?;
            let mask = f.int_const_u128(a).or_else(|| f.int_const_u128(b))?;
            (mask.count_ones() == 1).then(|| (mask.trailing_zeros(), None))
        }
        // A 1-bit comparison sits at bit 0.
        NodeKind::IntCmpOp(_) => Some((0, Some(value))),
        // A single-set-bit constant is a known bit (no comparison).
        NodeKind::IntConst(_) => {
            let c = f.int_const_u128(value)?;
            (c.count_ones() == 1).then(|| (c.trailing_zeros(), None))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
