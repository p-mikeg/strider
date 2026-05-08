//! `FlagCmpCanonicalize` — recognises the multi-node "flag-tree" shapes that
//! AArch64 (and similar flag-register architectures) emit when lifting
//! `cmp`-then-branch sequences, and rewrites them into a single direct
//! [`ir::IntCmpOp`] node against the original `(a, b)` pair.
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
//! in [`crate::indirect_branch_resolve`] needs.
//!
//! ## Pipeline placement
//!
//! Run after `ConstantFold` (so `BoolNeg(BoolNeg(x)) → x` collapses
//! before we look for the canonical shape) and before `IfCondInversion`
//! (so the cond it sees has only one possible BoolNeg-wrapping layer).
//!
//! ## Asm-fingerprint preservation
//!
//! Every newly-created node has its asm-fingerprint extended with the
//! matched root's fingerprint via [`ir::Graph::extend_asm_fingerprint_from`].
//! Multi-node RHS rules call the helper on each intermediate node, not
//! just the outer one, so the validator's
//! [`ir::validate::ValidateOptions::check_asm_fingerprints`] post-check
//! stays clean.

use std::sync::LazyLock;

use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use ir::{BuiltFunctionGraph, Graph, IntCmpOp};
use pattern::{
    Capture, Pat, add, bool_and, bool_not, bool_or, cast_to_int, int_const, int_eq, int_lt,
    int_sborrow, int_slt, neg, var, Matcher,
};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, OptimizerOnBuilt};

/// Pass that rewrites flag-tree `If` conds into single `IntCmpOp`s.
pub struct FlagCmpCanonicalize;

impl OptimizerOnBuilt for FlagCmpCanonicalize {
    fn optimize_built(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        // Pre-collect candidate roots: rules mutate the graph (rewire
        // uses), so we can't walk the live iterator.  Forward preorder
        // visits parents before children — for the larger flag-tree
        // rules this lets the outer `BoolAnd` / `BoolOr` / `BoolNeg`
        // rule fire before rule 1 (the ZR identity) shrinks the inner
        // `Equal(diff, 0)` and breaks the outer match.  Same pattern as
        // `strider::GraphRewriter::apply_rule`.
        let candidates: Vec<NodeId> = function.graph.preorder(function.entry).collect();
        let mut any = false;
        for node in candidates {
            for rule in RULES.iter() {
                if try_apply_rule(function, node, rule)? {
                    any = true;
                    break; // node was rewritten; stop trying rules at this root
                }
            }
        }
        Ok(if any {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        })
    }
}

/// One flag-tree rewrite rule.  Holds the LHS pattern + an RHS builder
/// that constructs replacement nodes manually so it can extend
/// asm-fingerprints on every new node.
struct Rule {
    lhs: Pat,
    /// Builds the RHS subtree.  Receives the matched `(a, b)` outputs
    /// and the original root's `NodeId` (for fingerprint absorption),
    /// returns the new value-output to redirect the root's uses to.
    build_rhs: fn(&mut Graph, NodeOutputId, NodeOutputId, NodeId) -> NodeOutputId,
    /// Captures used by `lhs` for `a` and `b`.
    lhs_capture: Capture,
    rhs_capture: Capture,
}

fn try_apply_rule(function: &mut BuiltFunctionGraph, node: NodeId, rule: &Rule) -> Result<bool> {
    // Snapshot the matched bindings inside a tight scope so the borrow
    // ends before we mutate the graph.  Some rules (Thumb's "test bool
    // against 0") use only `lhs_capture`; `rhs_capture` defaults to the
    // same output in that case so the RHS builder, which ignores it,
    // still gets a valid argument.
    let (a_out, b_out) = {
        let matcher = Matcher::new(function);
        let m = match matcher.match_at(node, &rule.lhs) {
            Some(m) => m,
            None => return Ok(false),
        };
        // `match_at` succeeded above, and the rule's `lhs` always
        // captures `lhs_capture` at a value-producing position; the
        // matcher contract guarantees `output(lhs_capture)` returns
        // `Some` whenever the capture appears in a successful match.
        #[allow(clippy::expect_used)]
        let a = m
            .output(rule.lhs_capture)
            .expect("Capture a must bind to a value output");
        let b = m.output(rule.rhs_capture).unwrap_or(a);
        (a, b)
    };

    let [root_out] = function.graph.node_outputs_exact::<1>(node)?;
    let new_out = (rule.build_rhs)(&mut function.graph, a_out, b_out, node);
    function.graph.replace_all_uses(root_out, new_out)?;
    Ok(true)
}

// ── RHS builders ──────────────────────────────────────────────────────────
//
// Each builder constructs the replacement subtree and extends every new
// node's asm-fingerprint with the original root's fingerprint via
// `Graph::extend_asm_fingerprint_from`.  The validator's
// `check_asm_fingerprints` Layer-C invariant requires every reachable
// non-exempt node to have a non-empty fingerprint, so multi-node RHS
// rules must touch every intermediate node — `pattern::rewrite_rule`
// only absorbs into the outermost.

fn build_int_cmp(graph: &mut Graph, op: IntCmpOp, lhs: NodeOutputId, rhs: NodeOutputId, root: NodeId) -> NodeOutputId {
    let n = graph.create_node(
        NodeKind::IntCmpOp(op),
        [lhs, rhs],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    graph.extend_asm_fingerprint_from(n, root);
    // `IntCmpOp` is constructed above with exactly one
    // `NodeOutputKind::OutputType(Bool)`; `node_outputs_exact::<1>`
    // enforces and returns that single output.  The expect cannot
    // fire short of an internal `create_node` invariant violation.
    #[allow(clippy::expect_used)]
    let [out] = graph.node_outputs_exact::<1>(n).expect("IntCmpOp produces 1 output");
    out
}

fn build_bool_neg(graph: &mut Graph, inner: NodeOutputId, root: NodeId) -> NodeOutputId {
    let n = graph.create_node(
        NodeKind::BoolUnaryOp(ir::BoolUnaryOp::Neg),
        [inner],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    graph.extend_asm_fingerprint_from(n, root);
    // Same invariant as `build_int_cmp` — single Bool output by
    // construction.
    #[allow(clippy::expect_used)]
    let [out] = graph.node_outputs_exact::<1>(n).expect("BoolNeg produces 1 output");
    out
}

// EQ:  Equal(Add(a, Neg(b)), 0) → Equal(a, b)
fn rhs_eq(graph: &mut Graph, a: NodeOutputId, b: NodeOutputId, root: NodeId) -> NodeOutputId {
    build_int_cmp(graph, IntCmpOp::Equal, a, b, root)
}

// HI:  → IntLess(b, a)
fn rhs_hi(graph: &mut Graph, a: NodeOutputId, b: NodeOutputId, root: NodeId) -> NodeOutputId {
    build_int_cmp(graph, IntCmpOp::Less, b, a, root)
}

// LS:  → BoolNeg(IntLess(b, a))
fn rhs_ls(graph: &mut Graph, a: NodeOutputId, b: NodeOutputId, root: NodeId) -> NodeOutputId {
    let inner = build_int_cmp(graph, IntCmpOp::Less, b, a, root);
    build_bool_neg(graph, inner, root)
}

// LT:  → IntSless(a, b)
fn rhs_lt(graph: &mut Graph, a: NodeOutputId, b: NodeOutputId, root: NodeId) -> NodeOutputId {
    build_int_cmp(graph, IntCmpOp::Sless, a, b, root)
}

// GE:  → BoolNeg(IntSless(a, b))
fn rhs_ge(graph: &mut Graph, a: NodeOutputId, b: NodeOutputId, root: NodeId) -> NodeOutputId {
    let inner = build_int_cmp(graph, IntCmpOp::Sless, a, b, root);
    build_bool_neg(graph, inner, root)
}

// GT:  → IntSless(b, a)
fn rhs_gt(graph: &mut Graph, a: NodeOutputId, b: NodeOutputId, root: NodeId) -> NodeOutputId {
    build_int_cmp(graph, IntCmpOp::Sless, b, a, root)
}

// LE:  → BoolNeg(IntSless(b, a))
fn rhs_le(graph: &mut Graph, a: NodeOutputId, b: NodeOutputId, root: NodeId) -> NodeOutputId {
    let inner = build_int_cmp(graph, IntCmpOp::Sless, b, a, root);
    build_bool_neg(graph, inner, root)
}

// ── Thumb-style "test bool flag against 0" simplification ─────────────────
//
// ARM Thumb's `B<cond>` lifts the flag-test as `IntNotEqual(flag, 0:1)` or
// `IntEqual(flag, 0:1)` (instead of a direct `flag` / `BoolNeg(flag)` read).
// The size-1 immediate forces a `CastToInt(flag, U8)` insertion via
// `build_int_cmp_operation`'s coercion.  Two simplifications drop the
// dance:
//
//   IntEqual(CastToInt(b), 0)              → BoolNeg(b)        // false test
//   BoolNeg(IntEqual(CastToInt(b), 0))     → b                 // true test
//
// The lift-time canonicalisation lowers `IntNotEqual` into the second
// shape, so the second rule covers Thumb's "true" branches (BEQ, BCS,
// BMI, BVS) and the first rule covers the "false" branches (BNE, BCC,
// BPL, BVC).  After these fire, the inner Thumb cond reads its named
// flag varnode directly — same as AArch64 — and the AArch64 rules
// (1, 2, 3, …) take over.

// `var(b)` here is captured as `lhs_capture` in the `Rule` shape; I
// reuse the `lhs_capture` slot for the bool input and leave
// `rhs_capture` unused.  This is a special-case unary rewrite using
// the shared two-cap rule struct.

fn rhs_thumb_neg_b(graph: &mut Graph, a: NodeOutputId, _b: NodeOutputId, root: NodeId) -> NodeOutputId {
    build_bool_neg(graph, a, root)
}

fn rhs_thumb_b(_graph: &mut Graph, a: NodeOutputId, _b: NodeOutputId, _root: NodeId) -> NodeOutputId {
    // Returning `a` directly redirects the root's uses straight to the
    // captured `b` output — no new node, no fingerprint absorption
    // needed because the captured node already has its own fingerprint.
    a
}

// ── Rule table ────────────────────────────────────────────────────────────

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(build_rules);

/// Helper: one rule entry — fresh captures + LHS + RHS builder.
fn rule(lhs_builder: impl FnOnce(Capture, Capture) -> Pat,
        build_rhs: fn(&mut Graph, NodeOutputId, NodeOutputId, NodeId) -> NodeOutputId)
        -> Rule {
    let lhs_capture = Capture::new();
    let rhs_capture = Capture::new();
    Rule {
        lhs: lhs_builder(lhs_capture, rhs_capture),
        build_rhs,
        lhs_capture,
        rhs_capture,
    }
}

fn build_rules() -> Vec<Rule> {
    vec![
        // 1. EQ / ZR identity:  Equal(Add(a, Neg(b)), 0) → Equal(a, b)
        rule(
            |a, b| int_eq(add(var(a), neg(var(b))), int_const(0)),
            rhs_eq,
        ),
        // 2. HI:  BoolAnd(BoolNeg(IntLess(a, b)), BoolNeg(Equal(diff, 0))) → IntLess(b, a)
        rule(
            |a, b| bool_and(
                bool_not(int_lt(var(a), var(b))),
                bool_not(int_eq(add(var(a), neg(var(b))), int_const(0))),
            ).into(),
            rhs_hi,
        ),
        // 3. LS:  BoolOr(IntLess(a, b), Equal(diff, 0)) → BoolNeg(IntLess(b, a))
        //    Assumes ConstantFold has cancelled the `BoolNeg(BoolNeg(IntLess(a, b)))`
        //    chain that `BoolNeg(CY)` produces.
        rule(
            |a, b| bool_or(
                int_lt(var(a), var(b)),
                int_eq(add(var(a), neg(var(b))), int_const(0)),
            ).into(),
            rhs_ls,
        ),
        // 4. LT:  BoolNeg(Equal(CastToInt(IntSless(diff, 0)), CastToInt(IntSborrow(a, b)))) → IntSless(a, b)
        rule(
            |a, b| bool_not(int_eq(
                cast_to_int(int_slt(add(var(a), neg(var(b))), int_const(0))),
                cast_to_int(int_sborrow(var(a), var(b))),
            )),
            rhs_lt,
        ),
        // 5. GE:  Equal(CastToInt(IntSless(diff, 0)), CastToInt(IntSborrow(a, b))) → BoolNeg(IntSless(a, b))
        rule(
            |a, b| int_eq(
                cast_to_int(int_slt(add(var(a), neg(var(b))), int_const(0))),
                cast_to_int(int_sborrow(var(a), var(b))),
            ),
            rhs_ge,
        ),
        // 6. GT:  BoolAnd(BoolNeg(Equal(diff, 0)),
        //                 Equal(CastToInt(IntSless(diff, 0)), CastToInt(IntSborrow(a, b))))
        //         → IntSless(b, a)
        rule(
            |a, b| bool_and(
                bool_not(int_eq(add(var(a), neg(var(b))), int_const(0))),
                int_eq(
                    cast_to_int(int_slt(add(var(a), neg(var(b))), int_const(0))),
                    cast_to_int(int_sborrow(var(a), var(b))),
                ),
            ).into(),
            rhs_gt,
        ),
        // 7. LE:  BoolOr(Equal(diff, 0),
        //                BoolNeg(Equal(CastToInt(IntSless(diff, 0)), CastToInt(IntSborrow(a, b)))))
        //         → BoolNeg(IntSless(b, a))
        rule(
            |a, b| bool_or(
                int_eq(add(var(a), neg(var(b))), int_const(0)),
                bool_not(int_eq(
                    cast_to_int(int_slt(add(var(a), neg(var(b))), int_const(0))),
                    cast_to_int(int_sborrow(var(a), var(b))),
                )),
            ).into(),
            rhs_le,
        ),
        // 8. Thumb "false" flag test:  IntEqual(CastToInt(b), 0)  →  BoolNeg(b)
        //    Lifted by Thumb BNE / BCC / BPL / BVC, where the cond is
        //    `IntEqual(flag, 0)` rather than `BoolNeg(flag)` directly.
        rule(
            |a, _b| int_eq(cast_to_int(var(a)), int_const(0)),
            rhs_thumb_neg_b,
        ),
        // 9. Thumb "true" flag test:  BoolNeg(IntEqual(CastToInt(b), 0))  →  b
        //    Lifted by Thumb BEQ / BCS / BMI / BVS — the lift-time
        //    canonicalisation `IntNotEqual(b, 0) → BoolNeg(IntEqual(b, 0))`
        //    plus our cast-to-int coercion gives this shape.
        rule(
            |a, _b| bool_not(int_eq(cast_to_int(var(a)), int_const(0))),
            rhs_thumb_b,
        ),
    ]
}

#[cfg(test)]
mod tests;
