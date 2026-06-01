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
//! in [`crate::opt::indirect_branch_resolve`] needs.
//!
//! ## Pipeline placement
//!
//! Run after `ConstantFold` (so `BitNot(BitNot(x)) → x` at `I1` collapses
//! before we look for the canonical shape) and before `IfCondInversion`
//! (so the cond it sees has only one possible BitNot-wrapping layer).
//!
//! ## Asm-fingerprint preservation
//!
//! Every rule is built via [`strider_pattern::rewrite_rule`], which absorbs the
//! matched root's fingerprint into **every** freshly-created interior
//! node of the RHS subtree (not just the outermost root).  This makes
//! the per-rule fingerprint discipline automatic; previously the pass
//! carried a bespoke `Rule { build_rhs: fn(...) -> NodeOutputId }`
//! infrastructure that hand-rolled the per-node fingerprint absorption
//! — see `strider_pattern::rewrite::rewrite_rule` for the central walk.


use strider_ir::node::{NodeId, NodeKind};
use strider_pattern::{
    BoxedRule, Capture, add, apply_rules_in_order, bool_and, bool_not, bool_or, boxed_rule,
    int_const, int_eq, int_lt, int_sborrow, int_slt, neg, rewrite_rule, var, zero_extend,
};

use crate::opt::error::Result;
use crate::opt::peephole::{PeepholePass, impl_optimizer_from_peephole};
use crate::opt::pipeline::OptimizationResult;

/// Pass that rewrites flag-tree `If` conds into single `IntCmpOp`s.
#[derive(Clone)]
pub struct FlagCmpCanonicalize;

impl PeepholePass for FlagCmpCanonicalize {
    /// Rules walk arbitrary boolean / arith subtrees; no useful kind
    /// filter at the root — defer to the per-rule matcher.
    fn matches_kind(&self, _kind: &NodeKind) -> bool {
        true
    }

    fn try_rewrite(
        &self,
        ctx: &mut strider_pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        RULES.with(|rules| {
            let fired = apply_rules_in_order(rules)(ctx, root)?;
            Ok(if fired {
                OptimizationResult::Changed
            } else {
                OptimizationResult::NoChange
            })
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

impl_optimizer_from_peephole!(FlagCmpCanonicalize);

// ── Rule table ────────────────────────────────────────────────────────────
//
// Each entry is `rewrite_rule(lhs, rhs)`: pattern crate matches the LHS,
// builds the RHS template, and rewires uses with full asm-fingerprint
// absorption into every fresh interior node.

thread_local! {
    /// Per-thread cache of the rewrite rule table.  `BoxedRule` captures
    /// `Pat`-shaped state that is `!Send + !Sync` now that strider runs
    /// single-threaded; the thread-local cache preserves the
    /// build-once-per-process feel without needing a `Sync` rule type.
    static RULES: Vec<BoxedRule> = build_rules();
}

fn build_rules() -> Vec<BoxedRule> {
    // Fresh captures per rule so cross-rule binding state can't leak.
    // `Capture::new` allocates from a process-wide atomic counter.
    let r1_a = Capture::new();
    let r1_b = Capture::new();
    let r2_a = Capture::new();
    let r2_b = Capture::new();
    let r3_a = Capture::new();
    let r3_b = Capture::new();
    let r4_a = Capture::new();
    let r4_b = Capture::new();
    let r5_a = Capture::new();
    let r5_b = Capture::new();
    let r6_a = Capture::new();
    let r6_b = Capture::new();
    let r7_a = Capture::new();
    let r7_b = Capture::new();
    let r8_b = Capture::new();
    let r9_b = Capture::new();
    let r10_a = Capture::new();
    let r10_b = Capture::new();
    let r11_a = Capture::new();
    let r11_b = Capture::new();
    let r12_a = Capture::new();
    let r12_b = Capture::new();
    let r13_a = Capture::new();
    let r13_b = Capture::new();

    vec![
        // 1. EQ / ZR identity:  Equal(Add(a, Neg(b)), 0) → Equal(a, b)
        boxed_rule(rewrite_rule(
            int_eq(add(var(r1_a), neg(var(r1_b))), int_const(0)),
            int_eq(var(r1_a), var(r1_b)),
        )),
        // 2. HI:  BoolAnd(BitNot(IntLess(a, b)), BitNot(Equal(diff, 0))) → IntLess(b, a)
        boxed_rule(rewrite_rule(
            bool_and(
                bool_not(int_lt(var(r2_a), var(r2_b))),
                bool_not(int_eq(add(var(r2_a), neg(var(r2_b))), int_const(0))),
            ),
            int_lt(var(r2_b), var(r2_a)),
        )),
        // 3. LS:  BoolOr(IntLess(a, b), Equal(diff, 0)) → BitNot(IntLess(b, a))
        //    Assumes ConstantFold has cancelled the `BitNot(BitNot(IntLess(a, b)))`
        //    chain that `BitNot(CY)` produces.
        boxed_rule(rewrite_rule(
            bool_or(
                int_lt(var(r3_a), var(r3_b)),
                int_eq(add(var(r3_a), neg(var(r3_b))), int_const(0)),
            ),
            bool_not(int_lt(var(r3_b), var(r3_a))),
        )),
        // 4. LT:  BitNot(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b)))) → IntSless(a, b)
        boxed_rule(rewrite_rule(
            bool_not(int_eq(
                zero_extend(int_slt(add(var(r4_a), neg(var(r4_b))), int_const(0))),
                zero_extend(int_sborrow(var(r4_a), var(r4_b))),
            )),
            int_slt(var(r4_a), var(r4_b)),
        )),
        // 5. GE:  Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))) → BitNot(IntSless(a, b))
        boxed_rule(rewrite_rule(
            int_eq(
                zero_extend(int_slt(add(var(r5_a), neg(var(r5_b))), int_const(0))),
                zero_extend(int_sborrow(var(r5_a), var(r5_b))),
            ),
            bool_not(int_slt(var(r5_a), var(r5_b))),
        )),
        // 6. GT:  BoolAnd(BitNot(Equal(diff, 0)),
        //                 Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))))
        //         → IntSless(b, a)
        boxed_rule(rewrite_rule(
            bool_and(
                bool_not(int_eq(add(var(r6_a), neg(var(r6_b))), int_const(0))),
                int_eq(
                    zero_extend(int_slt(add(var(r6_a), neg(var(r6_b))), int_const(0))),
                    zero_extend(int_sborrow(var(r6_a), var(r6_b))),
                ),
            ),
            int_slt(var(r6_b), var(r6_a)),
        )),
        // 7. LE:  BoolOr(Equal(diff, 0),
        //                BitNot(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b)))))
        //         → BitNot(IntSless(b, a))
        boxed_rule(rewrite_rule(
            bool_or(
                int_eq(add(var(r7_a), neg(var(r7_b))), int_const(0)),
                bool_not(int_eq(
                    zero_extend(int_slt(add(var(r7_a), neg(var(r7_b))), int_const(0))),
                    zero_extend(int_sborrow(var(r7_a), var(r7_b))),
                )),
            ),
            bool_not(int_slt(var(r7_b), var(r7_a))),
        )),
        // 8. Thumb "false" flag test:  IntEqual(ZeroExtend(b), 0)  →  BitNot(b)
        //    Lifted by Thumb BNE / BCC / BPL / BVC, where the cond is
        //    `IntEqual(flag, 0)` rather than `BitNot(flag)` directly.
        //    Only sound when `b` is the 1-bit flag itself: `zext(b) == 0`
        //    equals `!b` only for an `I1` `b`.  Without the guard a chained
        //    zero-extend (e.g. `I1 → I8 → I32`) would bind `b` to the wider
        //    intermediate, yielding a malformed `BitNot` of a non-`I1` value.
        boxed_rule(rewrite_rule(
            int_eq(zero_extend(var(r8_b)), int_const(0)).when_match(move |ctx, _ty, b| {
                b.get(r8_b)
                    .and_then(|o| ctx.function.output_kind(o).as_value())
                    .is_some_and(|t| t.bit_width() == 1)
            }),
            bool_not(var(r8_b)),
        )),
        // 9. Thumb "true" flag test:  BitNot(IntEqual(ZeroExtend(b), 0))  →  b
        //    Lifted by Thumb BEQ / BCS / BMI / BVS — the lift-time
        //    canonicalisation `IntNotEqual(b, 0) → BitNot(IntEqual(b, 0))`
        //    plus our cast-to-int coercion gives this shape.  Same `I1`
        //    guard as rule 8: replacing the test with `b` only preserves
        //    booleanness when `b` is the 1-bit flag.
        boxed_rule(rewrite_rule(
            bool_not(int_eq(zero_extend(var(r9_b)), int_const(0))).when_match(move |ctx, _ty, b| {
                b.get(r9_b)
                    .and_then(|o| ctx.function.output_kind(o).as_value())
                    .is_some_and(|t| t.bit_width() == 1)
            }),
            var(r9_b),
        )),
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
        boxed_rule(rewrite_rule(
            bool_and(
                bool_not(int_eq(var(r10_a), var(r10_b))),
                bool_not(int_slt(var(r10_a), var(r10_b))),
            ),
            int_slt(var(r10_b), var(r10_a)),
        )),
        // 11. LE (signed):  Or(Equal(a,b), Sless(a,b)) → BitNot(Sless(b,a))
        //     (a=b) ∨ (a<b)  ≡  a≤b  ≡  ¬(b<a)
        boxed_rule(rewrite_rule(
            bool_or(
                int_eq(var(r11_a), var(r11_b)),
                int_slt(var(r11_a), var(r11_b)),
            ),
            bool_not(int_slt(var(r11_b), var(r11_a))),
        )),
        // 12. HI (unsigned):  And(BitNot(Equal(a,b)), BitNot(Less(a,b))) → Less(b,a)
        boxed_rule(rewrite_rule(
            bool_and(
                bool_not(int_eq(var(r12_a), var(r12_b))),
                bool_not(int_lt(var(r12_a), var(r12_b))),
            ),
            int_lt(var(r12_b), var(r12_a)),
        )),
        // 13. LS (unsigned):  Or(Equal(a,b), Less(a,b)) → BitNot(Less(b,a))
        boxed_rule(rewrite_rule(
            bool_or(
                int_eq(var(r13_a), var(r13_b)),
                int_lt(var(r13_a), var(r13_b)),
            ),
            bool_not(int_lt(var(r13_b), var(r13_a))),
        )),
    ]
}

#[cfg(test)]
mod tests;
