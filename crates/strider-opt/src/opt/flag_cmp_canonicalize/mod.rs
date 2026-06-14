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
use strider_pattern::template;
use strider_pattern::{
    Capture, CaptureExt, add, any_int_const, bool_and, bool_not, bool_or, int_const, int_eq,
    int_lt, int_sborrow, int_slt, neg, var, zero_extend,
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
        ctx: &mut crate::EditFunction<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        // PowerPC condition-register bit test: an imperative arm, because the
        // variable-arity OR pack `Or(ShiftLeft(ZeroExtend(cmp_i), pos_i)…)` does
        // not fit the fixed-shape `rewrite_rule` DSL.
        if let Some(cmp) = canonicalize_cr_bit_test(ctx, root)? {
            return Ok(PeepholeRewrite::Changed {
                new_node: Some(ctx.producer(cmp)),
            });
        }
        Ok(match apply_rules_in_order(&self.rules)(ctx, root)? {
            Some(new_value) => PeepholeRewrite::Changed {
                new_node: Some(ctx.producer(new_value)),
            },
            None => PeepholeRewrite::NoChange,
        })
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

    vec![
        // 1. EQ / ZR identity:  Equal(Add(a, Neg(b)), 0) → Equal(a, b)
        rewrite_rule(
            int_eq(add(var(a), neg(var(b))), int_const(0u128)),
            template::int_eq(var(a), var(b)),
        ),
        // 2. HI:  BoolAnd(BitNot(IntLess(a, b)), BitNot(Equal(diff, 0))) → IntLess(b, a)
        rewrite_rule(
            bool_and(
                bool_not(int_lt(var(a), var(b))),
                bool_not(int_eq(add(var(a), neg(var(b))), int_const(0u128))),
            ),
            template::int_lt(var(b), var(a)),
        ),
        // 3. LS:  BoolOr(IntLess(a, b), Equal(diff, 0)) → BitNot(IntLess(b, a))
        //    Assumes ConstantFold has cancelled the `BitNot(BitNot(IntLess(a, b)))`
        //    chain that `BitNot(CY)` produces.
        rewrite_rule(
            bool_or(
                int_lt(var(a), var(b)),
                int_eq(add(var(a), neg(var(b))), int_const(0u128)),
            ),
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
        // 6. GT:  BoolAnd(BitNot(Equal(diff, 0)),
        //                 Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))))
        //         → IntSless(b, a)
        rewrite_rule(
            bool_and(
                bool_not(int_eq(add(var(a), neg(var(b))), int_const(0u128))),
                int_eq(
                    zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                    zero_extend(int_sborrow(var(a), var(b))),
                ),
            ),
            template::int_slt(var(b), var(a)),
        ),
        // 7. LE:  BoolOr(Equal(diff, 0),
        //                BitNot(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b)))))
        //         → BitNot(IntSless(b, a))
        rewrite_rule(
            bool_or(
                int_eq(add(var(a), neg(var(b))), int_const(0u128)),
                bool_not(int_eq(
                    zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                    zero_extend(int_sborrow(var(a), var(b))),
                )),
            ),
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
        // ── Decomposed flag-tree shapes ──────────────────────────────────
        //
        // Rules 2/3/6/7 match the *raw* flag tree (with `Equal(diff, 0)` and
        // `Equal(zext(Sless), zext(Sborrow))`).  When the branch is lifted with
        // inverted sense (ARM/Thumb wrap the tree in an outer `BitNot`), this
        // pass can't fire until `IfCondInversion` strips that `BitNot`, and by
        // then ConstantFold rule 1 (`Equal(a-b,0) → Equal(a,b)`) and rule 5
        // (`Equal(zext N, zext V) → BitNot(Sless(a,b))`) have already
        // decomposed the sub-terms into direct comparisons on `(a, b)`.  These
        // four rules canonicalise that decomposed form.  They are sound
        // arch-independent identities, so they are harmless where the raw
        // rules already fired (the decomposed shape simply never appears).
        //
        // 10. GT (signed):  And(BitNot(Equal(a,b)), BitNot(Sless(a,b))) → Sless(b,a)
        //     (a≠b) ∧ ¬(a<b)  ≡  a>b  ≡  b<a
        rewrite_rule(
            bool_and(
                bool_not(int_eq(var(a), var(b))),
                bool_not(int_slt(var(a), var(b))),
            ),
            template::int_slt(var(b), var(a)),
        ),
        // 11. LE (signed):  Or(Equal(a,b), Sless(a,b)) → BitNot(Sless(b,a))
        //     (a=b) ∨ (a<b)  ≡  a≤b  ≡  ¬(b<a)
        rewrite_rule(
            bool_or(int_eq(var(a), var(b)), int_slt(var(a), var(b))),
            template::bool_not(template::int_slt(var(b), var(a))),
        ),
        // 12. HI (unsigned):  And(BitNot(Equal(a,b)), BitNot(Less(a,b))) → Less(b,a)
        rewrite_rule(
            bool_and(
                bool_not(int_eq(var(a), var(b))),
                bool_not(int_lt(var(a), var(b))),
            ),
            template::int_lt(var(b), var(a)),
        ),
        // 13. LS (unsigned):  Or(Equal(a,b), Less(a,b)) → BitNot(Less(b,a))
        rewrite_rule(
            bool_or(int_eq(var(a), var(b)), int_lt(var(a), var(b))),
            template::bool_not(template::int_lt(var(b), var(a))),
        ),
        // 14. LS (unsigned), constant-folded ZF term:
        //         Or(Less(a, IntConst(N)), Equal(Add(a, IntConst(M)), 0)) → BitNot(Less(N, a))
        //     i.e. (a < N) ∨ (a == N)  ≡  a ≤ N  ≡  ¬(N < a).
        //
        //     This is the `ja`/`jbe` (`cmp a, N; ja`) flag tree with a CONSTANT
        //     compare operand.  Rules 11/13 expect the ZF term as
        //     `Equal(a, b)`, but `ConstantFold` has already collapsed the lifted
        //     `Equal(Add(a, Neg(IntConst(N))), 0)` to `Equal(Add(a, IntConst(M)), 0)`
        //     with `M = -N` (it folds `Neg(IntConst(N))` first), so neither rule
        //     1 (`Equal(Add(a, Neg(b)), 0) → Equal(a, b)`) nor rule 13 can match.
        //     This rule recognises the folded shape directly and REUSES the
        //     captured `IntConst(N)` node — width-correct by construction — as
        //     the rewritten comparison's operand, so no width-typed constant has
        //     to be synthesised.  The `when_match` guard pins `M ≡ -N` (mod the
        //     operand width) so the Equal genuinely tests `a == N`.
        rewrite_rule(
            bool_or(
                int_lt(var(a), any_int_const().capture(n)),
                int_eq(add(var(a), any_int_const().capture(m)), int_const(0u128)),
            )
            .when_match(move |ctx, _ty, binds| {
                let (Some(n_val), Some(m_val)) = (
                    binds.get_uint(n, ctx.function()),
                    binds.get_uint(m, ctx.function()),
                ) else {
                    return false;
                };
                // The compare operand width is `a`'s type (the Add / Less input).
                let Some(width) = binds.get_type(a, ctx.function()).map(|t| t.bit_mask_u128())
                else {
                    return false;
                };
                // M must be the two's-complement negation of N at that width.
                (m_val & width) == (n_val.wrapping_neg() & width)
            }),
            template::bool_not(template::int_lt(var(n), var(a))),
        ),
        // 15. LS (unsigned), offset-base + constant-folded ZF term:
        //         Or(Less(Add(b, C1), IntConst(N)), Equal(Add(b, C2), 0))
        //         → BitNot(Less(N, Add(b, C1)))
        //     i.e. with `X = Add(b, C1)`: `(X < N) ∨ (X == N)` ≡ `X ≤ N`.
        //
        //     The generalisation of rule 14 to a `switch` whose cases start at a
        //     nonzero base: gcc emits `sub b, K; cmp (b-K), N; ja`, so the
        //     compared value is the OFFSET index `X = Add(b, -K)`, not `b`
        //     itself.  The ZF term `X == N` lifts to `Equal(Add(X, Neg(N)), 0)`,
        //     which `ConstantFold` flattens to `Equal(Add(b, C2), 0)` with
        //     `C2 = C1 - N` — so the Less operand `Add(b, C1)` and the Equal's
        //     add base `b` are DISTINCT nodes (rule 14's shared `a` cannot bind
        //     both).  This rule keys on the shared base `b` and reuses the
        //     captured compared value `X` on the RHS, so the canonicalised
        //     `X ≤ N` lands on the very value the jump-table index uses.  The
        //     `when_match` guard pins `C2 ≡ C1 - N` (mod width) so the Equal
        //     genuinely tests `X == N`.
        rewrite_rule(
            bool_or(
                int_lt(
                    add(var(b), any_int_const().capture(c1)).capture(x),
                    any_int_const().capture(n),
                ),
                int_eq(add(var(b), any_int_const().capture(m)), int_const(0u128)),
            )
            .when_match(move |ctx, _ty, binds| {
                let (Some(c1_val), Some(n_val), Some(m_val)) = (
                    binds.get_uint(c1, ctx.function()),
                    binds.get_uint(n, ctx.function()),
                    binds.get_uint(m, ctx.function()),
                ) else {
                    return false;
                };
                // The compare operand width is the shared base `b`'s type.
                let Some(width) = binds.get_type(b, ctx.function()).map(|t| t.bit_mask_u128())
                else {
                    return false;
                };
                // C2 must equal C1 - N (mod width): the ZF term tests X == N
                // where X = Add(b, C1).
                (m_val & width) == (c1_val.wrapping_sub(n_val) & width)
            }),
            template::bool_not(template::int_lt(var(n), var(x))),
        ),
        // 16. HI (unsigned), constant-folded ZF term — the dual of rule 14:
        //         And(BitNot(Less(a, IntConst(N))), BitNot(Equal(Add(a, IntConst(M)), 0)))
        //         → Less(N, a)
        //     i.e. `(a >= N) ∧ (a != N) ≡ a > N ≡ N < a`.  This is the
        //     `cmp a, N; bhi`/`ja` flag tree (Thumb / decomposed) once
        //     `ConstantFold` has collapsed the lifted `Equal(Add(a, Neg(N)), 0)`
        //     to `Equal(Add(a, IntConst(M)), 0)` with `M = -N` — so neither the
        //     raw HI rule 2 nor the decomposed HI rule 12 (both expecting the ZF
        //     term as `Equal(a, b)`) can match.  Same `M ≡ -N` guard as rule 14.
        rewrite_rule(
            bool_and(
                bool_not(int_lt(var(a), any_int_const().capture(n))),
                bool_not(int_eq(
                    add(var(a), any_int_const().capture(m)),
                    int_const(0u128),
                )),
            )
            .when_match(move |ctx, _ty, binds| {
                let (Some(n_val), Some(m_val)) = (
                    binds.get_uint(n, ctx.function()),
                    binds.get_uint(m, ctx.function()),
                ) else {
                    return false;
                };
                let Some(width) = binds.get_type(a, ctx.function()).map(|t| t.bit_mask_u128())
                else {
                    return false;
                };
                (m_val & width) == (n_val.wrapping_neg() & width)
            }),
            template::int_lt(var(n), var(a)),
        ),
        // 17. HI (unsigned), offset-base + constant-folded ZF term — the dual of
        //     rule 15 and the offset sibling of rule 16:
        //         And(BitNot(Less(Add(b, C1), N)), BitNot(Equal(Add(b, C2), 0)))
        //         → Less(N, Add(b, C1))
        //     with `X = Add(b, C1)`: `(X >= N) ∧ (X != N) ≡ X > N`.  A masked /
        //     offset switch (e.g. Thumb `and r0,#7; subs r0,#1; cmp r0,#N-1;
        //     bhi`) compares the OFFSET index `X = Add(b, -K)`, and the ZF term
        //     `X == N` folds to `Equal(Add(b, C2), 0)` with `C2 = C1 - N` — so
        //     the `Less` operand and the `Equal` base are distinct nodes.  Keys
        //     on the shared base `b` and reuses the captured `X` on the RHS, so
        //     the canonical `X > N` lands on the value the jump-table index uses.
        rewrite_rule(
            bool_and(
                bool_not(int_lt(
                    add(var(b), any_int_const().capture(c1)).capture(x),
                    any_int_const().capture(n),
                )),
                bool_not(int_eq(
                    add(var(b), any_int_const().capture(m)),
                    int_const(0u128),
                )),
            )
            .when_match(move |ctx, _ty, binds| {
                let (Some(c1_val), Some(n_val), Some(m_val)) = (
                    binds.get_uint(c1, ctx.function()),
                    binds.get_uint(n, ctx.function()),
                    binds.get_uint(m, ctx.function()),
                ) else {
                    return false;
                };
                let Some(width) = binds.get_type(b, ctx.function()).map(|t| t.bit_mask_u128())
                else {
                    return false;
                };
                (m_val & width) == (c1_val.wrapping_sub(n_val) & width)
            }),
            template::int_lt(var(n), var(x)),
        ),
    ]
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
    ctx: &mut crate::EditFunction<'_>,
    root: NodeId,
) -> Result<Option<ValueId>> {
    let Some((cond_out, cmp)) = cr_bit_comparison(ctx, root) else {
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
    absorb_cr_pack_fingerprints(ctx, cond_out, cmp);
    ctx.replace_value(cond_out, cmp)?;
    Ok(Some(cmp))
}

/// Folds every CR-pack interior node's asm-fingerprint into `cmp`'s producer
/// (the surviving comparison) so the rewrite preserves the superset-only
/// fingerprint contract once the pack is culled.  Walks the input cone from
/// `cond_out`'s producer (the `Truncate`) toward the comparison terms,
/// stopping the descent at each `IntCmpOp` — a comparison carries its
/// instruction's address on its own node, and its operands are the unrelated
/// compared values (often live elsewhere), not pack-building instructions.
fn absorb_cr_pack_fingerprints(ctx: &mut crate::EditFunction<'_>, cond_out: ValueId, cmp: ValueId) {
    let into = ctx.producer(cmp);
    let mut stack = vec![ctx.producer(cond_out)];
    let mut interior: Vec<NodeId> = Vec::new();
    while let Some(n) = stack.pop() {
        if interior.contains(&n) {
            continue;
        }
        interior.push(n);
        // A comparison term (including `cmp` itself) ends the descent.
        if matches!(ctx.node_kind(n), NodeKind::IntCmpOp(_)) {
            continue;
        }
        let input_producers: Vec<NodeId> = ctx
            .node_inputs(n)
            .into_iter()
            .map(|v| ctx.producer(v))
            .collect();
        stack.extend(input_producers);
    }
    for n in interior {
        if n != into {
            ctx.function_mut().extend_asm_fingerprint_from(into, n);
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
    let (pack, bit) = match *f.node_kind(f.producer(inner)) {
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => {
            let [x, amt] = f.node_inputs_exact::<2>(f.producer(inner)).ok()?;
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

/// Flattens a (possibly nested) `Or` tree into its leaf terms.
fn flatten_or(f: &impl IRViewer, value: ValueId, out: &mut Vec<ValueId>, depth: u32) {
    const MAX_DEPTH: u32 = 32;
    if depth <= MAX_DEPTH
        && let NodeKind::IntBinaryOp(IntBinaryOp::Or) = f.node_kind(f.producer(value))
        && let Ok([a, b]) = f.node_inputs_exact::<2>(f.producer(value))
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
    match *f.node_kind(f.producer(value)) {
        // `ShiftLeft(v, pos)` moves v's single bit up by `pos`.
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            let [v, amt] = f.node_inputs_exact::<2>(f.producer(value)).ok()?;
            let pos = u32::try_from(f.int_const_u128(amt)?).ok()?;
            let (q, cmp) = single_bit_term(f, v, depth + 1)?;
            Some((q.checked_add(pos)?, cmp))
        }
        // `ZeroExtend` zero-fills above the source, so the set-bit position is
        // unchanged; a 1-bit (I1) source sets only bit 0.
        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            let [src] = f.node_inputs_exact::<1>(f.producer(value)).ok()?;
            match f.value_type_opt(src) {
                Some(ValueType::I1) => Some((0, comparison_leaf(f, src))),
                _ => single_bit_term(f, src, depth + 1),
            }
        }
        // `And(v, single-bit const)` keeps only that bit (value unknown → no cmp).
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            let [a, b] = f.node_inputs_exact::<2>(f.producer(value)).ok()?;
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

/// Returns `value` if it is produced by an `IntCmpOp` (the comparison leaf).
fn comparison_leaf(f: &impl IRViewer, value: ValueId) -> Option<ValueId> {
    matches!(f.node_kind(f.producer(value)), NodeKind::IntCmpOp(_)).then_some(value)
}

#[cfg(test)]
mod tests;
