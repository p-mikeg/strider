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
//! CY   = BoolNeg(IntLess(a, b))   // C flag (post lower of IntLessEqual)
//! OV   = IntSborrow(a, b)         // V flag
//! ```
//!
//! The 14 conditional-branch codes each read a fixed boolean tree of
//! these flags.  Of those:
//!
//! * `EQ`/`NE`   — bare `ZR` (and its negation).
//! * `CS/CC`     — bare `CY` / `BoolNeg(CY)` — already in `(a, b)` form;
//!   `ConstantFold` collapses `BoolNeg(BoolNeg(IntLess(a, b))) → IntLess(a, b)`.
//! * `MI/PL`     — bare `NG` / `BoolNeg(NG)`.  `NG` is `Sless(a-b, 0)`,
//!   which is *not* the same as `Sless(a, b)` due to subtraction overflow.
//!   Left untouched.
//! * `VS/VC`     — bare `OV` / `BoolNeg(OV)` — already in `(a, b)` form.
//! * `HI/LS`     — `BoolAnd(CY, BoolNeg(ZR))` / its De Morgan dual.
//! * `GE/LT`     — `Equal(NG, OV)` / its negation.
//! * `GT/LE`     — `BoolAnd(BoolNeg(ZR), Equal(NG, OV))` / its De Morgan dual.
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
//! Run after `ConstantFold` (so `BoolNeg(BoolNeg(x)) → x` collapses
//! before we look for the canonical shape) and before `IfCondInversion`
//! (so the cond it sees has only one possible BoolNeg-wrapping layer).
//!
//! ## Asm-fingerprint preservation
//!
//! Every rule is built via [`crate::pattern::rewrite_rule`], which absorbs the
//! matched root's fingerprint into **every** freshly-created interior
//! node of the RHS subtree (not just the outermost root).  This makes
//! the per-rule fingerprint discipline automatic; previously the pass
//! carried a bespoke `Rule { build_rhs: fn(...) -> NodeOutputId }`
//! infrastructure that hand-rolled the per-node fingerprint absorption
//! — see `crate::pattern::rewrite::rewrite_rule` for the central walk.

use std::sync::LazyLock;

use strider_ir::node::{NodeId, NodeKind};
use crate::pattern::{
    BoxedRule, Capture, add, apply_rules_in_order, bool_and, bool_not, bool_or, boxed_rule,
    cast_to_int, int_const, int_eq, int_lt, int_sborrow, int_slt, neg, rewrite_rule, var,
};

use crate::opt::error::Result;
use crate::opt::peephole::{PeepholePass, run_peephole};
use crate::opt::pipeline::{OptimizationResult, Optimizer};

/// Pass that rewrites flag-tree `If` conds into single `IntCmpOp`s.
pub struct FlagCmpCanonicalize;

impl PeepholePass for FlagCmpCanonicalize {
    fn name(&self) -> &'static str {
        "FlagCmpCanonicalize"
    }

    /// Rules walk arbitrary boolean / arith subtrees; no useful kind
    /// filter at the root — defer to the per-rule matcher.
    fn matches_kind(&self, _kind: &NodeKind) -> bool {
        true
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        let apply = apply_rules_in_order(&RULES);
        let fired = apply(ctx, root)?;
        Ok(if fired {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
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

impl Optimizer for FlagCmpCanonicalize {
    fn optimize(&self, ctx: &mut crate::pattern::RewriteCtx<'_>) -> Result<OptimizationResult> {
        run_peephole(self, ctx)
    }
}

// ── Rule table ────────────────────────────────────────────────────────────
//
// Each entry is `rewrite_rule(lhs, rhs)`: pattern crate matches the LHS,
// builds the RHS template, and rewires uses with full asm-fingerprint
// absorption into every fresh interior node.

static RULES: LazyLock<Vec<BoxedRule>> = LazyLock::new(build_rules);

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

    vec![
        // 1. EQ / ZR identity:  Equal(Add(a, Neg(b)), 0) → Equal(a, b)
        boxed_rule(rewrite_rule(
            int_eq(add(var(r1_a), neg(var(r1_b))), int_const(0)),
            int_eq(var(r1_a), var(r1_b)),
        )),
        // 2. HI:  BoolAnd(BoolNeg(IntLess(a, b)), BoolNeg(Equal(diff, 0))) → IntLess(b, a)
        boxed_rule(rewrite_rule(
            bool_and(
                bool_not(int_lt(var(r2_a), var(r2_b))),
                bool_not(int_eq(add(var(r2_a), neg(var(r2_b))), int_const(0))),
            ),
            int_lt(var(r2_b), var(r2_a)),
        )),
        // 3. LS:  BoolOr(IntLess(a, b), Equal(diff, 0)) → BoolNeg(IntLess(b, a))
        //    Assumes ConstantFold has cancelled the `BoolNeg(BoolNeg(IntLess(a, b)))`
        //    chain that `BoolNeg(CY)` produces.
        boxed_rule(rewrite_rule(
            bool_or(
                int_lt(var(r3_a), var(r3_b)),
                int_eq(add(var(r3_a), neg(var(r3_b))), int_const(0)),
            ),
            bool_not(int_lt(var(r3_b), var(r3_a))),
        )),
        // 4. LT:  BoolNeg(Equal(CastToInt(IntSless(diff, 0)), CastToInt(IntSborrow(a, b)))) → IntSless(a, b)
        boxed_rule(rewrite_rule(
            bool_not(int_eq(
                cast_to_int(int_slt(add(var(r4_a), neg(var(r4_b))), int_const(0))),
                cast_to_int(int_sborrow(var(r4_a), var(r4_b))),
            )),
            int_slt(var(r4_a), var(r4_b)),
        )),
        // 5. GE:  Equal(CastToInt(IntSless(diff, 0)), CastToInt(IntSborrow(a, b))) → BoolNeg(IntSless(a, b))
        boxed_rule(rewrite_rule(
            int_eq(
                cast_to_int(int_slt(add(var(r5_a), neg(var(r5_b))), int_const(0))),
                cast_to_int(int_sborrow(var(r5_a), var(r5_b))),
            ),
            bool_not(int_slt(var(r5_a), var(r5_b))),
        )),
        // 6. GT:  BoolAnd(BoolNeg(Equal(diff, 0)),
        //                 Equal(CastToInt(IntSless(diff, 0)), CastToInt(IntSborrow(a, b))))
        //         → IntSless(b, a)
        boxed_rule(rewrite_rule(
            bool_and(
                bool_not(int_eq(add(var(r6_a), neg(var(r6_b))), int_const(0))),
                int_eq(
                    cast_to_int(int_slt(add(var(r6_a), neg(var(r6_b))), int_const(0))),
                    cast_to_int(int_sborrow(var(r6_a), var(r6_b))),
                ),
            ),
            int_slt(var(r6_b), var(r6_a)),
        )),
        // 7. LE:  BoolOr(Equal(diff, 0),
        //                BoolNeg(Equal(CastToInt(IntSless(diff, 0)), CastToInt(IntSborrow(a, b)))))
        //         → BoolNeg(IntSless(b, a))
        boxed_rule(rewrite_rule(
            bool_or(
                int_eq(add(var(r7_a), neg(var(r7_b))), int_const(0)),
                bool_not(int_eq(
                    cast_to_int(int_slt(add(var(r7_a), neg(var(r7_b))), int_const(0))),
                    cast_to_int(int_sborrow(var(r7_a), var(r7_b))),
                )),
            ),
            bool_not(int_slt(var(r7_b), var(r7_a))),
        )),
        // 8. Thumb "false" flag test:  IntEqual(CastToInt(b), 0)  →  BoolNeg(b)
        //    Lifted by Thumb BNE / BCC / BPL / BVC, where the cond is
        //    `IntEqual(flag, 0)` rather than `BoolNeg(flag)` directly.
        boxed_rule(rewrite_rule(
            int_eq(cast_to_int(var(r8_b)), int_const(0)),
            bool_not(var(r8_b)),
        )),
        // 9. Thumb "true" flag test:  BoolNeg(IntEqual(CastToInt(b), 0))  →  b
        //    Lifted by Thumb BEQ / BCS / BMI / BVS — the lift-time
        //    canonicalisation `IntNotEqual(b, 0) → BoolNeg(IntEqual(b, 0))`
        //    plus our cast-to-int coercion gives this shape.
        boxed_rule(rewrite_rule(
            bool_not(int_eq(cast_to_int(var(r9_b)), int_const(0))),
            var(r9_b),
        )),
    ]
}

#[cfg(test)]
mod tests;
