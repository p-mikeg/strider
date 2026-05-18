//! Egg-based ConstantFold rewriter — Phase 3 Task 3.2.
//!
//! Built alongside the existing imperative [`crate::opt::ConstantFold`] —
//! NOT a replacement.  The parity test
//! `crates/strider-analyze/tests/constant_fold_egg_parity.rs` proves both
//! produce structurally identical IR for the supported shapes.
//!
//! # Scope (first pass)
//!
//! Pure constant evaluation only:
//!
//! - `IntBinaryOp(op)(IntConst(a), IntConst(b))` → `IntConst(eval(op, a, b))`
//!   for `op ∈ {Add, Mul, And, Or, Xor, ShiftLeft, ShiftRight, SShiftRight}`.
//!   `Div` / `Sdiv` / `Rem` / `Srem` are folded only when the divisor is
//!   non-zero and no signed-overflow occurs — same skip predicates as v1.
//! - `IntUnaryOp(op)(IntConst(a))` → `IntConst(eval(op, a))` for `BitNot` / `Neg`.
//! - `IntCmpOp(op)(IntConst(a), IntConst(b))` → `BoolConst(eval(op, a, b))`
//!   for `op ∈ {Equal, Less, Sless, Carry, Scarry, Sborrow}`.
//!
//! Identity rewrites (`x + 0 → x`, `x ^ x → 0`, AND-mask merging, …) and
//! casts / truncates / extends are NOT yet covered — those land in
//! follow-up commits.
//!
//! # Design
//!
//! Three-step in-place rewrite loop, NOT a full graph round-trip:
//!
//! 1. Build an `EGraphAdapter` from the value-slice subgraph reachable
//!    from `entry` (see [`strider_ir::egraph_adapter`]).
//! 2. Manually scan e-classes for foldable shapes; when one matches,
//!    evaluate the result, add the resulting `IntConst` / `BoolConst`
//!    e-node, and `egraph.union(...)` it with the original e-class.
//!    Rebuild + repeat until no rewrite fires.
//! 3. Walk the original graph's value outputs; for each whose e-class
//!    now contains an `IntConst` / `BoolConst` representative,
//!    materialise a strider const node and `replace_all_uses` to rewire
//!    every consumer.
//!
//! Step 3 sidesteps the `extract_into_graph` arity-mismatch problem
//! (an extracted `IntConst` has zero inputs, but the original
//! `IntBinaryOp` has two — pushing through `create_node` with two
//! inputs would violate the validator).  Instead we synthesise fresh
//! const nodes only where the egraph proves an equivalence to a const,
//! and the old `IntBinaryOp` nodes are left as detached zombies (same
//! end state as the imperative `ConstantFold` produces, modulo node
//! identity).
//!
//! # Why egg at all if we're walking the graph anyway?
//!
//! Phase 3.2 is the **first real use of egg-rewrites** in strider; the
//! constant-fold case is the simplest place to prove the pipeline
//! (build → rewrite → reflect-back) works end-to-end.  Subsequent
//! identity / algebraic-simplification passes will introduce
//! many-to-many rewrites that benefit from egg's e-class union
//! semantics (e.g. `x+y ≡ y+x` discovered by the matcher applies to
//! every consumer at once).  The infrastructure built here is shared.

use anyhow::Result as AnyResult;
use egg::{EGraph, Id};
use strider_ir::IntUnaryOp;
use strider_ir::egraph_adapter::{EGraphAdapter, StriderLang};
use strider_ir::node::{NodeId, NodeKind, NodeOutputType};

use crate::opt::constant_fold::eval_int::{eval_int_binary, eval_int_cmp};
use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};

/// Egg-based constant-folding optimizer.  See module docs for the design.
///
/// Stateless — `new()` is the canonical constructor and is `const`-eligible
/// in spirit.  The "rewrite set" is encoded directly in `apply_constant_folds`
/// rather than carried as a `Vec<Rewrite<...>>` because egg's `Pattern` parser
/// requires `Display` / `FromOp` impls on the language, which `StriderLang`
/// doesn't (yet) provide.  Hand-rolled e-class scanning gives us the same
/// behaviour with less boilerplate for the constant-evaluation case.
pub struct ConstantFoldEgg;

impl ConstantFoldEgg {
    /// Construct a fresh ConstantFoldEgg.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConstantFoldEgg {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizerRaw for ConstantFoldEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // ── Step 1: build the egraph snapshot ──────────────────────────────
        let mut adapter = EGraphAdapter::from_graph(graph, entry);

        // ── Step 2: saturate constant-fold rewrites ────────────────────────
        // The loop is bounded by an iteration cap to defend against any
        // pathological case where a rewrite never reaches a fixed point.
        // In practice constant folding converges in one or two passes (an
        // IntBinaryOp can't produce a new IntBinaryOp).
        let mut total_changed = false;
        for _ in 0..32 {
            let changed = apply_constant_folds(&mut adapter.egraph)?;
            if !changed {
                break;
            }
            total_changed = true;
            adapter.egraph.rebuild();
        }

        if !total_changed {
            return Ok(OptimizationResult::NoChange);
        }

        // ── Step 3: reflect folds back into the original Graph ────────────
        let any_replaced = reflect_const_folds(graph, &adapter)?;
        Ok(if any_replaced {
            OptimizationResult::Changed
        } else {
            // Egraph rewrites happened but they didn't change any
            // consumer-visible value (e.g. all folded e-classes were
            // dead).  Treat as NoChange for the pipeline.
            OptimizationResult::NoChange
        })
    }
}

/// One sweep of constant-fold rewrites over the egraph.  Returns `true`
/// if any rewrite fired.  Does NOT call `rebuild()` — the caller does
/// so between sweeps.
fn apply_constant_folds(egraph: &mut EGraph<StriderLang, ()>) -> AnyResult<bool> {
    // Collect candidate rewrites first (so the egraph isn't mutated
    // while we iterate its classes).  Each entry pins:
    //   - the eclass id to union into
    //   - the new enode to add
    let mut pending: Vec<(Id, StriderLang)> = Vec::new();

    for class in egraph.classes() {
        let class_id = class.id;
        for node in class.iter() {
            if let Some(folded) = try_fold_node(egraph, node) {
                pending.push((class_id, folded));
            }
        }
    }

    if pending.is_empty() {
        return Ok(false);
    }

    let mut any_unioned = false;
    for (class_id, folded_enode) in pending {
        let new_id = egraph.add(folded_enode);
        // egg::union returns true when it actually merged two distinct
        // eclasses; if the new const e-node was already in the same
        // class (e.g. from a previous iteration), no progress.
        if egraph.union(class_id, new_id) {
            any_unioned = true;
        }
    }
    Ok(any_unioned)
}

/// Attempts to fold a single egraph e-node into a constant.  Returns
/// `Some(new_enode)` if the inputs are all known constants AND the
/// evaluation succeeds.  Returns `None` otherwise (non-foldable shape,
/// non-const inputs, or undefined op like div-by-zero).
fn try_fold_node(
    egraph: &EGraph<StriderLang, ()>,
    node: &StriderLang,
) -> Option<StriderLang> {
    use StriderLang as L;
    match node {
        L::IntBin(op, ty, [a, b]) => {
            let av = lookup_int_const(egraph, *a)?;
            let bv = lookup_int_const(egraph, *b)?;
            let folded = eval_int_binary(*op, av, bv, *ty)?;
            Some(L::IntConst(folded, *ty))
        }
        L::IntUn(op, ty, [a]) => {
            let av = lookup_int_const(egraph, *a)?;
            let raw: u128 = match op {
                IntUnaryOp::BitNot => !av,
                IntUnaryOp::Neg => av.wrapping_neg(),
            };
            // Mask to output type's width.  Returns None for U256/U512
            // (the spike's StriderLang::IntConst payload is u128 only,
            // so we skip those — consistent with v1's `int_const_with!`
            // skip arm).
            let folded = ty.get_unsigned_int(raw)?;
            Some(L::IntConst(folded, *ty))
        }
        L::IntCmp(op, [a, b]) => {
            // For IntCmp we need the input type — every IntCmp has two
            // value inputs, both of which must be the same type for a
            // well-formed graph.  Look it up from either operand's
            // const e-node payload (we already require const inputs to
            // fire the fold).
            let (av, a_ty) = lookup_int_const_with_ty(egraph, *a)?;
            let (bv, _b_ty) = lookup_int_const_with_ty(egraph, *b)?;
            let folded = eval_int_cmp(*op, av, bv, a_ty).ok()?;
            Some(L::BoolConst(folded))
        }
        _ => None,
    }
}

/// If e-class `id` contains an `IntConst` e-node, returns its value;
/// otherwise `None`.  Iterates the class's enodes — if the class has
/// been folded to a constant in a previous sweep, the `IntConst` will
/// be one of them.
fn lookup_int_const(egraph: &EGraph<StriderLang, ()>, id: Id) -> Option<u128> {
    let class = &egraph[id];
    for node in class.iter() {
        if let StriderLang::IntConst(v, _) = *node {
            return Some(v);
        }
    }
    None
}

/// Like `lookup_int_const` but also returns the const's declared output
/// type (needed by IntCmp folding because the cmp e-node itself
/// doesn't carry the input type — only `Bool` as its result).
fn lookup_int_const_with_ty(
    egraph: &EGraph<StriderLang, ()>,
    id: Id,
) -> Option<(u128, NodeOutputType)> {
    let class = &egraph[id];
    for node in class.iter() {
        if let StriderLang::IntConst(v, ty) = *node {
            return Some((v, ty));
        }
    }
    None
}

/// Walk the original graph; for every value-producing node whose egraph
/// e-class now contains an `IntConst` / `BoolConst` representative,
/// materialise a strider const node and `replace_all_uses` to rewire
/// every consumer to the new const.
///
/// Returns `true` if at least one `replace_all_uses` succeeded.
fn reflect_const_folds(
    graph: &mut strider_ir::Graph,
    adapter: &EGraphAdapter,
) -> crate::opt::Result<bool> {
    // Snapshot the per-output e-class data BEFORE mutating the graph.
    // Each pending rewrite is (old_output, new_kind, fingerprint_src_node).
    enum Folded {
        Int(u128, NodeOutputType),
        Bool(bool),
    }
    let mut pending: Vec<(strider_ir::node::NodeOutputId, Folded, NodeId)> = Vec::new();

    for (&oid, &eclass) in &adapter.output_to_eclass {
        // Skip if the original node was *already* a const — no fold needed.
        let producer = graph.get_node_from_output(oid);
        let producer_kind = *graph.node_kind(producer);
        if matches!(
            producer_kind,
            NodeKind::IntConst(_) | NodeKind::BoolConst(_) | NodeKind::FloatConst(_)
        ) {
            continue;
        }

        // What's the output type of this slot?  Only value outputs
        // were registered in `output_to_eclass`, so this is always
        // `OutputType(_)`.
        let out_kind = graph.output_kind(oid);
        let out_ty = match out_kind.as_value() {
            Some(t) => t,
            None => continue, // defensive
        };

        // Resolve the e-class through union-find to its canonical id.
        let canon = adapter.egraph.find(eclass);
        let class = &adapter.egraph[canon];
        // Pick the first const e-node in the class, if any.  Note: a
        // single class can contain multiple `IntConst`s with different
        // *type* payloads if multiple ops with different output widths
        // landed in the same e-class.  We pick the one matching the
        // original output type so we don't accidentally narrow.
        let mut folded: Option<Folded> = None;
        for n in class.iter() {
            match n {
                StriderLang::IntConst(v, ty) if *ty == out_ty => {
                    folded = Some(Folded::Int(*v, *ty));
                    break;
                }
                StriderLang::BoolConst(b) if out_ty == NodeOutputType::Bool => {
                    folded = Some(Folded::Bool(*b));
                    break;
                }
                _ => {}
            }
        }
        if let Some(f) = folded {
            pending.push((oid, f, producer));
        }
    }

    if pending.is_empty() {
        return Ok(false);
    }

    let mut any_replaced = false;
    for (old_out, folded, producer_node) in pending {
        // Materialise the strider-side const.
        let new_out = match folded {
            Folded::Int(v, ty) => graph.make_int_const(v, ty)?,
            Folded::Bool(b) => graph.make_bool_const(b)?,
        };
        // Propagate the asm-fingerprint of the producer being replaced
        // into the new const (superset invariant).
        let new_producer = graph.get_node_from_output(new_out);
        graph.extend_asm_fingerprint_from(new_producer, producer_node);
        let replaced = graph.replace_all_uses(old_out, new_out)?;
        if replaced {
            any_replaced = true;
        }
    }
    Ok(any_replaced)
}

#[cfg(test)]
mod tests {
    //! White-box smoke test — full parity test lives in
    //! `crates/strider-analyze/tests/constant_fold_egg_parity.rs`.
    use super::*;
    use crate::opt::test_support::{make_fn, return_kind};
    use strider_ir::IntBinaryOp;

    #[test]
    fn smoke_fold_add() {
        let mut fg = make_fn(|b| {
            let c3 = b.build_int_const(3u64, NodeOutputType::U64).unwrap();
            let c4 = b.build_int_const(4u64, NodeOutputType::U64).unwrap();
            b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, NodeOutputType::U64)
        })
        .expect("build fixture");
        let res = ConstantFoldEgg::new()
            .optimize_raw(&mut fg.graph, fg.entry)
            .expect("optimize must not error");
        assert!(res.changed(), "expected ConstantFoldEgg to fold const Add");
        assert_eq!(return_kind((&fg).into()).unwrap(), NodeKind::IntConst(7));
    }
}
