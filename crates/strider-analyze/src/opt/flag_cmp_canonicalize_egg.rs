//! Egg-based `FlagCmpCanonicalize` — Phase 3 Task 3.3a.
//!
//! Built alongside the imperative
//! [`crate::opt::FlagCmpCanonicalize`] — NOT a replacement.  The
//! parity test
//! `crates/strider-analyze/tests/flag_cmp_egg_parity.rs` proves
//! both produce structurally identical IR for every supported
//! flag-tree shape.
//!
//! # Rules
//!
//! Mirrors v1's `RULES` table verbatim — 9 rewrites canonicalising
//! AArch64 NZCV-style flag trees (plus the ARM-Thumb cast-to-int
//! shapes) into single direct `IntCmpOp` expressions:
//!
//! 1. EQ identity:        `Equal(Add(a, Neg(b)), 0)` → `Equal(a, b)`
//! 2. HI:                 `And(Not(Less(a,b)), Not(Equal(diff,0)))` → `Less(b, a)`
//! 3. LS:                 `Or(Less(a,b), Equal(diff,0))` → `Not(Less(b, a))`
//! 4. LT:                 `Not(Equal(Cast(Sless(diff,0)), Cast(Sborrow(a,b))))` → `Sless(a, b)`
//! 5. GE:                 `Equal(Cast(Sless(diff,0)), Cast(Sborrow(a,b)))` → `Not(Sless(a, b))`
//! 6. GT:                 `And(Not(Equal(diff,0)), GE_lhs)` → `Sless(b, a)`
//! 7. LE:                 `Or(Equal(diff,0), Not(GE_lhs))` → `Not(Sless(b, a))`
//! 8. Thumb-false:        `Equal(Cast(b), 0)` → `Not(b)`
//! 9. Thumb-true:         `Not(Equal(Cast(b), 0))` → `b`
//!
//! # Design
//!
//! Three-step rewrite loop following Phase 3.2's
//! `constant_fold_egg` pattern:
//!
//! 1. Build an [`EGraphAdapter`] snapshot of the value-slice
//!    reachable from `entry`.
//! 2. Walk e-classes, find LHS root matches (op + child e-class
//!    shape).  Extract the LHS captures (the `a`/`b` leaf outputs)
//!    by resolving the matched e-class ids back to strider
//!    `NodeOutputId`s via [`EGraphAdapter::leaf_to_output`].
//! 3. For each match, materialise the RHS subtree as fresh strider
//!    nodes (children sourced from the captured leaves) and call
//!    `Graph::replace_all_uses` to rewire every consumer of the LHS
//!    root.  The asm-fingerprint of the LHS root is absorbed into
//!    every materialised RHS node — mirrors v1's per-rule
//!    `rewrite_rule` engine semantics.
//!
//! # Saturation vs single-pass
//!
//! Rules can chain — e.g. Thumb BEQ requires Rule 9 then Rule 1.
//! Like v1 (whose `optimize` is called inside the pipeline's
//! fixed-point loop), this pass is single-pass; the caller's
//! fixed-point loop re-runs it as needed.  The parity test's
//! `run_to_fp` helper exercises this.

use std::collections::HashMap;

use egg::Id;
use strider_ir::IntCmpOp;
use strider_ir::egraph_adapter::{EGraphAdapter, StriderLang};
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use strider_ir::{BoolBinaryOp, BoolUnaryOp, IntBinaryOp, IntUnaryOp};

use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};

/// Egg-based FlagCmpCanonicalize.  Stateless.
pub struct FlagCmpCanonicalizeEgg;

impl FlagCmpCanonicalizeEgg {
    /// Construct a fresh `FlagCmpCanonicalizeEgg`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for FlagCmpCanonicalizeEgg {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizerRaw for FlagCmpCanonicalizeEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // Step 1: snapshot the value-slice egraph.
        let adapter = EGraphAdapter::from_graph(graph, entry);

        // Reverse map: e-class canonical id → strider NodeOutputIds.
        // Built once and reused across rule matchers.  Maps e-class to
        // every strider value output whose e-class is that canonical
        // id (typically one entry but multi-output Calls etc. can
        // share).
        let canon_to_outputs = build_canon_to_outputs(&adapter);

        // Step 2: scan e-classes for each rule's LHS root pattern.
        // `pending[i] = (lhs_root_output, rhs_recipe, root_node)`.
        let pending = scan_rules(graph, &adapter, &canon_to_outputs);

        if pending.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // Step 3: materialise each RHS into the strider graph and rewire.
        let mut changed = false;
        for Pending {
            lhs_root_output,
            recipe,
            root_node,
        } in pending
        {
            // Materialise the RHS subtree.  Children come from
            // captured strider leaf outputs (the original `a`/`b`).
            let new_out = recipe.materialise(graph, root_node)?;
            // Rewire every consumer of the LHS root's output.
            if graph.replace_all_uses(lhs_root_output, new_out)? {
                changed = true;
            }
        }
        Ok(if changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        })
    }
}

/// One pending RHS materialisation.
struct Pending {
    /// The original LHS root's value output — what we
    /// `replace_all_uses` on.
    lhs_root_output: NodeOutputId,
    /// Description of the RHS subtree to materialise.
    recipe: Recipe,
    /// The original LHS root node id — its asm-fingerprint is
    /// absorbed into every materialised RHS node.
    root_node: NodeId,
}

/// Description of an RHS shape to materialise on the strider graph.
///
/// Every shape carries the captured `(a, b)` strider leaf outputs
/// at its leaves; the materialiser builds fresh interior nodes.
///
/// Output type for the RHS shapes is derived from the LHS root
/// match — for cmp shapes the bool output is `Bool`, and for the
/// thumb `b` passthrough we use `b`'s declared output type.
enum Recipe {
    /// `IntCmpOp(op)(lhs, rhs)`.
    IntCmp {
        op: IntCmpOp,
        lhs: NodeOutputId,
        rhs: NodeOutputId,
    },
    /// `BoolUnaryOp::Neg(IntCmpOp(op)(lhs, rhs))`.
    NegIntCmp {
        op: IntCmpOp,
        lhs: NodeOutputId,
        rhs: NodeOutputId,
    },
    /// `BoolUnaryOp::Neg(b)` — for Rule 8 (Thumb false).
    NegLeaf { b: NodeOutputId },
    /// Passthrough: replace with `b` directly — for Rule 9 (Thumb true).
    Passthrough { b: NodeOutputId },
}

impl Recipe {
    fn materialise(
        self,
        graph: &mut strider_ir::Graph,
        root_node: NodeId,
    ) -> crate::opt::Result<NodeOutputId> {
        match self {
            Recipe::IntCmp { op, lhs, rhs } => {
                let cmp = graph.create_node_attributed(
                    NodeKind::IntCmpOp(op),
                    [lhs, rhs],
                    [NodeOutputKind::OutputType(NodeOutputType::Bool)],
                    &[root_node],
                );
                Ok(graph.node_outputs_exact::<1>(cmp)?[0])
            }
            Recipe::NegIntCmp { op, lhs, rhs } => {
                let cmp = graph.create_node_attributed(
                    NodeKind::IntCmpOp(op),
                    [lhs, rhs],
                    [NodeOutputKind::OutputType(NodeOutputType::Bool)],
                    &[root_node],
                );
                let cmp_out = graph.node_outputs_exact::<1>(cmp)?[0];
                let neg = graph.create_node_attributed(
                    NodeKind::BoolUnaryOp(BoolUnaryOp::Neg),
                    [cmp_out],
                    [NodeOutputKind::OutputType(NodeOutputType::Bool)],
                    &[root_node],
                );
                Ok(graph.node_outputs_exact::<1>(neg)?[0])
            }
            Recipe::NegLeaf { b } => {
                let neg = graph.create_node_attributed(
                    NodeKind::BoolUnaryOp(BoolUnaryOp::Neg),
                    [b],
                    [NodeOutputKind::OutputType(NodeOutputType::Bool)],
                    &[root_node],
                );
                Ok(graph.node_outputs_exact::<1>(neg)?[0])
            }
            Recipe::Passthrough { b } => {
                // No new node; we'll rewire consumers directly to `b`.
                // The fingerprint absorption still happens — onto `b`'s
                // producer — so the BoolNeg(Eq(...)) chain's history
                // survives in the destination.
                let dst_node = graph.get_node_from_output(b);
                graph.extend_asm_fingerprint_from(dst_node, root_node);
                Ok(b)
            }
        }
    }
}

/// Build the reverse map: canonical e-class id → strider NodeOutputIds.
fn build_canon_to_outputs(adapter: &EGraphAdapter) -> HashMap<Id, Vec<NodeOutputId>> {
    let mut canon_to_outputs: HashMap<Id, Vec<NodeOutputId>> = HashMap::new();
    for (&oid, &eclass) in &adapter.output_to_eclass {
        let canon = adapter.egraph.find(eclass);
        canon_to_outputs.entry(canon).or_default().push(oid);
    }
    canon_to_outputs
}

/// Scan the egraph for each rule's LHS root pattern.  Returns one
/// [`Pending`] entry per LHS-root match found.
fn scan_rules(
    graph: &strider_ir::Graph,
    adapter: &EGraphAdapter,
    canon_to_outputs: &HashMap<Id, Vec<NodeOutputId>>,
) -> Vec<Pending> {
    let mut out = Vec::new();

    // Iterate egraph e-classes; for each enode, check if it matches
    // one of the 9 LHS patterns.  When it does, push a Pending entry
    // bound to the strider-side root output.
    for class in adapter.egraph.classes() {
        let canon_id = adapter.egraph.find(class.id);
        // Find the strider-side roots whose e-class is this canonical id.
        let strider_outputs = match canon_to_outputs.get(&canon_id) {
            Some(v) => v,
            None => continue,
        };

        for enode in class.iter() {
            if let Some(recipe) = try_match_rule(graph, adapter, canon_to_outputs, enode) {
                // Bind to each strider root in this e-class — typically
                // there's exactly one (no rewrites have unioned in yet),
                // but defensively handle multiple.
                for &lhs_root_output in strider_outputs {
                    // Skip if the strider-side producer's kind doesn't
                    // match the enode's op family — a guard against
                    // a union-introduced sharing accident.
                    let producer = graph.get_node_from_output(lhs_root_output);
                    if !strider_kind_matches_enode(graph.node_kind(producer), enode) {
                        continue;
                    }
                    out.push(Pending {
                        lhs_root_output,
                        recipe: clone_recipe(&recipe),
                        root_node: producer,
                    });
                }
            }
        }
    }

    out
}

/// Returns `true` when the strider-side `NodeKind` is the same
/// "head" as the egg-side enode.  Used to drop spurious matches when
/// an e-class has multiple strider-side producers (one is the LHS
/// shape, another isn't).
fn strider_kind_matches_enode(kind: &NodeKind, enode: &StriderLang) -> bool {
    match (kind, enode) {
        (NodeKind::IntCmpOp(a), StriderLang::IntCmp(b, _)) => a == b,
        (NodeKind::BoolBinaryOp(a), StriderLang::BoolBin(b, _)) => a == b,
        (NodeKind::BoolUnaryOp(a), StriderLang::BoolUn(b, _)) => a == b,
        (NodeKind::IntBinaryOp(a), StriderLang::IntBin(b, _, _)) => a == b,
        (NodeKind::IntUnaryOp(a), StriderLang::IntUn(b, _, _)) => a == b,
        (NodeKind::IntConst(_), StriderLang::IntConst(_, _)) => true,
        (NodeKind::BoolConst(_), StriderLang::BoolConst(_)) => true,
        _ => false,
    }
}

/// Recipe is non-Copy because `NodeOutputId` is non-Copy in some
/// versions.  This helper allows us to "clone" a recipe by manually
/// reconstructing it (the fields are all `Copy`).
fn clone_recipe(r: &Recipe) -> Recipe {
    match r {
        Recipe::IntCmp { op, lhs, rhs } => Recipe::IntCmp {
            op: *op,
            lhs: *lhs,
            rhs: *rhs,
        },
        Recipe::NegIntCmp { op, lhs, rhs } => Recipe::NegIntCmp {
            op: *op,
            lhs: *lhs,
            rhs: *rhs,
        },
        Recipe::NegLeaf { b } => Recipe::NegLeaf { b: *b },
        Recipe::Passthrough { b } => Recipe::Passthrough { b: *b },
    }
}

/// Try each rule against `enode`; return the first matching RHS recipe.
fn try_match_rule(
    graph: &strider_ir::Graph,
    adapter: &EGraphAdapter,
    canon_to_outputs: &HashMap<Id, Vec<NodeOutputId>>,
    enode: &StriderLang,
) -> Option<Recipe> {
    // Order matters: more-specific (deeper) rules first so the simpler
    // rules don't shadow them.  Mirrors v1's RULES order.
    None.or_else(|| match_rule1(adapter, enode))
        .or_else(|| match_rule2(adapter, enode))
        .or_else(|| match_rule3(adapter, enode))
        .or_else(|| match_rule4(adapter, enode))
        .or_else(|| match_rule5(adapter, enode))
        .or_else(|| match_rule6(adapter, enode))
        .or_else(|| match_rule7(adapter, enode))
        .or_else(|| match_rule8(graph, adapter, enode))
        .or_else(|| match_rule9(graph, adapter, canon_to_outputs, enode))
}

/// Resolve an e-class id to a strider `NodeOutputId` — either via an
/// opaque leaf payload (for register reads / phi) or by looking up
/// any strider value output whose e-class is the given canonical id.
/// Used by Rule 9 / Rule 8 where the v1 capture is `var(_)` which
/// matches any value (not just leaves).
fn resolve_value(
    adapter: &EGraphAdapter,
    canon_to_outputs: &HashMap<Id, Vec<NodeOutputId>>,
    id: Id,
) -> Option<NodeOutputId> {
    // Try opaque leaf first for stability — if there's a leaf in this
    // class it's a register read / phi, the most stable strider output.
    if let Some(out) = resolve_leaf(adapter, id) {
        return Some(out);
    }
    // Fallback: any strider output mapped to this canonical e-class.
    let canon = adapter.egraph.find(id);
    canon_to_outputs.get(&canon).and_then(|v| v.first().copied())
}

// ── E-class structural helpers ──────────────────────────────────────────────

/// Find an enode in `class` that satisfies `pred`.
fn find_enode_in<'a, F>(adapter: &'a EGraphAdapter, id: Id, pred: F) -> Option<&'a StriderLang>
where
    F: Fn(&StriderLang) -> bool,
{
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    class.iter().find(|n| pred(n))
}

/// If e-class `id` contains an `IntConst(0, _)` enode, returns its e-class id
/// for upstream identity comparisons.  Returns `None` otherwise.
fn is_int_const_zero(adapter: &EGraphAdapter, id: Id) -> bool {
    find_enode_in(adapter, id, |n| matches!(n, StriderLang::IntConst(0, _))).is_some()
}

/// If `id` contains an `Add(_, Neg(b))` shape, returns `(a_eclass, b_eclass)`.
fn match_diff(adapter: &EGraphAdapter, id: Id) -> Option<(Id, Id)> {
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        if let StriderLang::IntBin(IntBinaryOp::Add, _, [a, b_or_neg]) = n {
            // The Add is commutative; try both orderings.
            // Order 1: a + Neg(b)
            if let Some(b_inner) = match_neg(adapter, *b_or_neg) {
                return Some((*a, b_inner));
            }
            // Order 2: Neg(b) + a  (commute)
            if let Some(b_inner) = match_neg(adapter, *a) {
                return Some((*b_or_neg, b_inner));
            }
        }
    }
    None
}

/// If `id` contains a `IntUnaryOp::Neg(b)` enode, returns `b`'s e-class id.
fn match_neg(adapter: &EGraphAdapter, id: Id) -> Option<Id> {
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        if let StriderLang::IntUn(IntUnaryOp::Neg, _, [b]) = n {
            return Some(*b);
        }
    }
    None
}

/// If `id` contains an `IntCmp(op, [a, b])` enode (commutative for Equal),
/// returns `(a, b)`.
fn match_cmp(adapter: &EGraphAdapter, id: Id, want_op: IntCmpOp) -> Option<(Id, Id)> {
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        if let StriderLang::IntCmp(op, [a, b]) = n
            && *op == want_op
        {
            return Some((*a, *b));
        }
    }
    None
}

/// If `id` contains a `BoolUnaryOp::Neg(x)` enode, returns `x`'s e-class id.
fn match_bool_neg(adapter: &EGraphAdapter, id: Id) -> Option<Id> {
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        if let StriderLang::BoolUn(BoolUnaryOp::Neg, [x]) = n {
            return Some(*x);
        }
    }
    None
}

/// If `id` contains a `CastToInt(_, [x])` enode, returns `x`'s e-class id.
fn match_cast_to_int(adapter: &EGraphAdapter, id: Id) -> Option<Id> {
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        if let StriderLang::CastToInt(_, [x]) = n {
            return Some(*x);
        }
    }
    None
}

/// If `id` contains an `IntCmpOp::Sborrow(a, b)` enode, returns `(a, b)`.
fn match_sborrow(adapter: &EGraphAdapter, id: Id) -> Option<(Id, Id)> {
    match_cmp(adapter, id, IntCmpOp::Sborrow)
}

/// Match the inner shape of Rule 5's LHS:
/// `Equal(CastToInt(Sless(diff, 0)), CastToInt(Sborrow(a, b)))`.
/// Returns `(a_eclass, b_eclass)` on success.
fn match_ge_lhs(adapter: &EGraphAdapter, id: Id) -> Option<(Id, Id)> {
    // `id` must be an `IntCmp(Equal, [x, y])` enode (commute either side).
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        let StriderLang::IntCmp(IntCmpOp::Equal, [x, y]) = n else {
            continue;
        };
        // Try (x, y) and (y, x) orderings.
        for (slt_side, sborrow_side) in [(*x, *y), (*y, *x)] {
            let Some(slt_inner) = match_cast_to_int(adapter, slt_side) else {
                continue;
            };
            let Some((diff_eclass, zero_eclass)) = match_cmp(adapter, slt_inner, IntCmpOp::Sless)
            else {
                continue;
            };
            if !is_int_const_zero(adapter, zero_eclass) {
                continue;
            }
            let Some((a1, b1)) = match_diff(adapter, diff_eclass) else {
                continue;
            };
            let Some(sborrow_inner) = match_cast_to_int(adapter, sborrow_side) else {
                continue;
            };
            let Some((a2, b2)) = match_sborrow(adapter, sborrow_inner) else {
                continue;
            };
            if adapter.egraph.find(a1) == adapter.egraph.find(a2)
                && adapter.egraph.find(b1) == adapter.egraph.find(b2)
            {
                return Some((a1, b1));
            }
        }
    }
    None
}

/// Resolve an e-class id to a strider `NodeOutputId` by looking up the
/// opaque-leaf payload.  Returns `None` if the e-class doesn't contain
/// an `Opaque` enode (= not a leaf, so we can't capture it as
/// `var(_)` in v1's rules).
fn resolve_leaf(adapter: &EGraphAdapter, id: Id) -> Option<NodeOutputId> {
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        if let StriderLang::Opaque(payload) = n {
            return adapter.leaf_to_output.get(payload).copied();
        }
    }
    None
}

// ── Per-rule matchers ───────────────────────────────────────────────────────

/// Rule 1: `Equal(Add(a, Neg(b)), 0)` → `Equal(a, b)`.
fn match_rule1(adapter: &EGraphAdapter, enode: &StriderLang) -> Option<Recipe> {
    let StriderLang::IntCmp(IntCmpOp::Equal, [x, y]) = enode else {
        return None;
    };
    // Equal is commutative — try both orderings.
    for (diff_side, zero_side) in [(*x, *y), (*y, *x)] {
        if !is_int_const_zero(adapter, zero_side) {
            continue;
        }
        let Some((a_id, b_id)) = match_diff(adapter, diff_side) else {
            continue;
        };
        let (Some(a), Some(b)) = (resolve_leaf(adapter, a_id), resolve_leaf(adapter, b_id)) else {
            continue;
        };
        return Some(Recipe::IntCmp {
            op: IntCmpOp::Equal,
            lhs: a,
            rhs: b,
        });
    }
    None
}

/// Rule 2: `And(Not(Less(a,b)), Not(Equal(diff,0)))` → `Less(b, a)`.
fn match_rule2(adapter: &EGraphAdapter, enode: &StriderLang) -> Option<Recipe> {
    let StriderLang::BoolBin(BoolBinaryOp::And, [x, y]) = enode else {
        return None;
    };
    // And is commutative.
    for (less_side, eq_side) in [(*x, *y), (*y, *x)] {
        let Some(less_inner) = match_bool_neg(adapter, less_side) else {
            continue;
        };
        let Some((a1, b1)) = match_cmp(adapter, less_inner, IntCmpOp::Less) else {
            continue;
        };
        let Some(eq_inner) = match_bool_neg(adapter, eq_side) else {
            continue;
        };
        let Some((diff_side2, zero_side2)) = first_eq_with_zero(adapter, eq_inner) else {
            continue;
        };
        let Some((a2, b2)) = match_diff(adapter, diff_side2) else {
            continue;
        };
        let _ = zero_side2; // already verified
        if adapter.egraph.find(a1) != adapter.egraph.find(a2)
            || adapter.egraph.find(b1) != adapter.egraph.find(b2)
        {
            continue;
        }
        let (Some(a), Some(b)) = (resolve_leaf(adapter, a1), resolve_leaf(adapter, b1)) else {
            continue;
        };
        return Some(Recipe::IntCmp {
            op: IntCmpOp::Less,
            lhs: b,
            rhs: a,
        });
    }
    None
}

/// Rule 3: `Or(Less(a,b), Equal(diff, 0))` → `Not(Less(b, a))`.
fn match_rule3(adapter: &EGraphAdapter, enode: &StriderLang) -> Option<Recipe> {
    let StriderLang::BoolBin(BoolBinaryOp::Or, [x, y]) = enode else {
        return None;
    };
    for (less_side, eq_side) in [(*x, *y), (*y, *x)] {
        let Some((a1, b1)) = match_cmp(adapter, less_side, IntCmpOp::Less) else {
            continue;
        };
        let Some((diff_side2, _)) = first_eq_with_zero(adapter, eq_side) else {
            continue;
        };
        let Some((a2, b2)) = match_diff(adapter, diff_side2) else {
            continue;
        };
        if adapter.egraph.find(a1) != adapter.egraph.find(a2)
            || adapter.egraph.find(b1) != adapter.egraph.find(b2)
        {
            continue;
        }
        let (Some(a), Some(b)) = (resolve_leaf(adapter, a1), resolve_leaf(adapter, b1)) else {
            continue;
        };
        return Some(Recipe::NegIntCmp {
            op: IntCmpOp::Less,
            lhs: b,
            rhs: a,
        });
    }
    None
}

/// Rule 4: `Not(Equal(Cast(Sless(diff,0)), Cast(Sborrow(a,b))))` → `Sless(a, b)`.
fn match_rule4(adapter: &EGraphAdapter, enode: &StriderLang) -> Option<Recipe> {
    let StriderLang::BoolUn(BoolUnaryOp::Neg, [inner]) = enode else {
        return None;
    };
    let (a_id, b_id) = match_ge_lhs(adapter, *inner)?;
    let (Some(a), Some(b)) = (resolve_leaf(adapter, a_id), resolve_leaf(adapter, b_id)) else {
        return None;
    };
    Some(Recipe::IntCmp {
        op: IntCmpOp::Sless,
        lhs: a,
        rhs: b,
    })
}

/// Rule 5: `Equal(Cast(Sless(diff,0)), Cast(Sborrow(a,b)))` → `Not(Sless(a, b))`.
fn match_rule5(adapter: &EGraphAdapter, enode: &StriderLang) -> Option<Recipe> {
    // Match the GE_lhs shape directly as an enode that IS the
    // Equal(...).  Rule 5's LHS is exactly the GE_lhs shape used by
    // rules 6 / 7 as an inner subtree.
    let StriderLang::IntCmp(IntCmpOp::Equal, _) = enode else {
        return None;
    };
    // We need the e-class id for the enode — but `enode` is a
    // borrowed reference; we look up via the existing match_ge_lhs
    // helper which walks the class.  Re-create the e-class by adding
    // the enode lookup via the egraph.  Workaround: since enode is in
    // some e-class, we can find that class by searching for the
    // enode.  Simpler: re-invoke `match_ge_lhs` on every Equal enode
    // found at LHS root.
    //
    // To do this we need a way to ask: "for this specific enode
    // payload, what e-class is it in?"  Egg's `EGraph::lookup`
    // requires `&mut`, so instead we scan the egraph for the matching
    // e-class.  This is O(N) but the egraph is small (per-function).
    //
    // Actually, the cleanest approach: the rule fires when there
    // EXISTS an e-class whose enodes match the GE_lhs shape.  We
    // record that fact at scan time.  But our `try_match_rule` API
    // is per-enode — so we need to traverse e-classes from inside
    // the matcher.  We do this lazily by walking the entire egraph's
    // classes here, scanning for the GE_lhs shape, and binding it.
    //
    // This is correct but produces one match per GE_lhs instance in
    // the egraph — and since enodes within an e-class are
    // structurally distinct, the GE_lhs shape appears at most once
    // per e-class.  We rely on the canonical strider-side
    // disambiguation in `scan_rules` to avoid double rewrites.
    for class in adapter.egraph.classes() {
        if let Some((a_id, b_id)) = match_ge_lhs(adapter, class.id)
            && let (Some(a), Some(b)) = (resolve_leaf(adapter, a_id), resolve_leaf(adapter, b_id))
        {
            // The enode passed in must be in this class for the match
            // to apply (so the scan_rules caller binds the right
            // strider root).
            if class.iter().any(|n| n == enode) {
                return Some(Recipe::NegIntCmp {
                    op: IntCmpOp::Sless,
                    lhs: a,
                    rhs: b,
                });
            }
        }
    }
    None
}

/// Rule 6: `And(Not(Equal(diff,0)), GE_lhs)` → `Sless(b, a)`.
fn match_rule6(adapter: &EGraphAdapter, enode: &StriderLang) -> Option<Recipe> {
    let StriderLang::BoolBin(BoolBinaryOp::And, [x, y]) = enode else {
        return None;
    };
    for (eq_side, ge_side) in [(*x, *y), (*y, *x)] {
        let Some(eq_inner) = match_bool_neg(adapter, eq_side) else {
            continue;
        };
        let Some((diff_side2, _)) = first_eq_with_zero(adapter, eq_inner) else {
            continue;
        };
        let Some((a1, b1)) = match_diff(adapter, diff_side2) else {
            continue;
        };
        let Some((a2, b2)) = match_ge_lhs(adapter, ge_side) else {
            continue;
        };
        if adapter.egraph.find(a1) != adapter.egraph.find(a2)
            || adapter.egraph.find(b1) != adapter.egraph.find(b2)
        {
            continue;
        }
        let (Some(a), Some(b)) = (resolve_leaf(adapter, a1), resolve_leaf(adapter, b1)) else {
            continue;
        };
        return Some(Recipe::IntCmp {
            op: IntCmpOp::Sless,
            lhs: b,
            rhs: a,
        });
    }
    None
}

/// Rule 7: `Or(Equal(diff,0), Not(GE_lhs))` → `Not(Sless(b, a))`.
fn match_rule7(adapter: &EGraphAdapter, enode: &StriderLang) -> Option<Recipe> {
    let StriderLang::BoolBin(BoolBinaryOp::Or, [x, y]) = enode else {
        return None;
    };
    for (eq_side, neg_side) in [(*x, *y), (*y, *x)] {
        let Some((diff_side, _)) = first_eq_with_zero(adapter, eq_side) else {
            continue;
        };
        let Some((a1, b1)) = match_diff(adapter, diff_side) else {
            continue;
        };
        let Some(inner) = match_bool_neg(adapter, neg_side) else {
            continue;
        };
        let Some((a2, b2)) = match_ge_lhs(adapter, inner) else {
            continue;
        };
        if adapter.egraph.find(a1) != adapter.egraph.find(a2)
            || adapter.egraph.find(b1) != adapter.egraph.find(b2)
        {
            continue;
        }
        let (Some(a), Some(b)) = (resolve_leaf(adapter, a1), resolve_leaf(adapter, b1)) else {
            continue;
        };
        return Some(Recipe::NegIntCmp {
            op: IntCmpOp::Sless,
            lhs: b,
            rhs: a,
        });
    }
    None
}

/// Rule 8 (Thumb false): `Equal(CastToInt(b), 0)` → `Not(b)`.
fn match_rule8(
    _graph: &strider_ir::Graph,
    adapter: &EGraphAdapter,
    enode: &StriderLang,
) -> Option<Recipe> {
    let StriderLang::IntCmp(IntCmpOp::Equal, [x, y]) = enode else {
        return None;
    };
    for (cast_side, zero_side) in [(*x, *y), (*y, *x)] {
        if !is_int_const_zero(adapter, zero_side) {
            continue;
        }
        let Some(inner) = match_cast_to_int(adapter, cast_side) else {
            continue;
        };
        // `b` must resolve to a leaf — i.e. v1's `var(r8_b)` captures
        // any value-typed node, but in this egraph adapter only opaque
        // leaves have a back-reference.  Restricting to leaves means
        // we cover the lifted case (`CastToInt(b_phi)`) but skip
        // rare cases where `b` is a computed expression — same
        // limitation as v1 for the purpose of branch-cond
        // canonicalisation (the lift always reads a phi/varread first
        // before CastToInt).
        //
        // Crucially we must EXCLUDE the case where the CastToInt is
        // wrapping a flag-producing IntCmpOp shape — i.e. for Rule 4 /
        // 5's `CastToInt(Sless(...))` and `CastToInt(Sborrow(...))`.
        // Those are NOT Thumb shapes and Rule 8 shouldn't fire.  Our
        // leaf-only restriction handles this: a `Sless` / `Sborrow`
        // inner has no opaque-leaf payload.
        let Some(b) = resolve_leaf(adapter, inner) else {
            continue;
        };
        return Some(Recipe::NegLeaf { b });
    }
    None
}

/// Rule 9 (Thumb true): `Not(Equal(CastToInt(b), 0))` → `b`.
///
/// v1's `var(r9_b)` captures any value-typed node (not just leaves) —
/// for the Thumb BEQ shape the inner `b` is `ZR = Equal(diff, 0)`,
/// which is a computed expression, NOT an opaque leaf.  We therefore
/// use `resolve_value` (leaf preferred, fallback to any strider
/// output for the e-class) so this rule fires for the Thumb chains.
fn match_rule9(
    _graph: &strider_ir::Graph,
    adapter: &EGraphAdapter,
    canon_to_outputs: &HashMap<Id, Vec<NodeOutputId>>,
    enode: &StriderLang,
) -> Option<Recipe> {
    let StriderLang::BoolUn(BoolUnaryOp::Neg, [inner]) = enode else {
        return None;
    };
    // Inner must be an IntCmp(Equal, CastToInt(b), 0).
    let canon = adapter.egraph.find(*inner);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        let StriderLang::IntCmp(IntCmpOp::Equal, [x, y]) = n else {
            continue;
        };
        for (cast_side, zero_side) in [(*x, *y), (*y, *x)] {
            if !is_int_const_zero(adapter, zero_side) {
                continue;
            }
            let Some(b_inner) = match_cast_to_int(adapter, cast_side) else {
                continue;
            };
            let Some(b) = resolve_value(adapter, canon_to_outputs, b_inner) else {
                continue;
            };
            return Some(Recipe::Passthrough { b });
        }
    }
    None
}

/// If `id`'s e-class contains an `Equal(side1, side2)` with one side being
/// `IntConst(0, _)`, returns `(non_zero_side, zero_side)`.
fn first_eq_with_zero(adapter: &EGraphAdapter, id: Id) -> Option<(Id, Id)> {
    let canon = adapter.egraph.find(id);
    let class = &adapter.egraph[canon];
    for n in class.iter() {
        let StriderLang::IntCmp(IntCmpOp::Equal, [a, b]) = n else {
            continue;
        };
        if is_int_const_zero(adapter, *b) {
            return Some((*a, *b));
        }
        if is_int_const_zero(adapter, *a) {
            return Some((*b, *a));
        }
    }
    None
}
