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
use strider_ir::node::{NodeId, NodeKind};
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
            bool_or(
                int_eq(var(a), var(b)),
                int_slt(var(a), var(b)),
            ),
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
            bool_or(
                int_eq(var(a), var(b)),
                int_lt(var(a), var(b)),
            ),
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
                let (Some(n_val), Some(m_val)) =
                    (binds.get_uint(n, ctx.function()), binds.get_uint(m, ctx.function()))
                else {
                    return false;
                };
                // The compare operand width is `a`'s type (the Add / Less input).
                let Some(width) = binds
                    .get_type(a, ctx.function())
                    .map(|t| t.bit_mask_u128())
                else {
                    return false;
                };
                // M must be the two's-complement negation of N at that width.
                (m_val & width) == (n_val.wrapping_neg() & width)
            }),
            template::bool_not(template::int_lt(var(n), var(a))),
        ),
    ]
}

#[cfg(test)]
mod tests;
