//! `SubToAdd` — opt-in canonicalisation pass that rewrites
//! `sub(a, IntConst(K)) → add(a, IntConst(-K))`.
//!
//! Strider's IR mirrors what the compiler emitted, so the same
//! source-level decrement (`x--`) appears as either `sub(x, 1)` or
//! `add(x, -1)` depending on instruction-encoding heuristics.  The
//! duplication leaks into pattern queries: a user matching `x--` has
//! to write a disjunction to cover both shapes.
//!
//! This pass canonicalises the constant-RHS subtraction form into the
//! addition form so a single `add(x, signed_int_const(-K))` query
//! covers both.  It's **opt-in**: not part of any default pipeline,
//! since the canonicalisation discards the binary's exact instruction
//! shape (a reverse engineer asking "what did the compiler emit"
//! gets the canonicalised view, not the truth).
//!
//! # Scope
//!
//! Only the constant-RHS case (`sub(a, IntConst(K))` →
//! `add(a, IntConst(-K))`) is rewritten.  The variable-RHS case
//! (`sub(a, b)` → `add(a, neg(b))`) is intentionally NOT rewritten
//! because it would *add* a node and pessimise downstream queries.
//! Users wanting "match either subtraction shape" for variable RHS
//! can write a pattern combinator (`union(sub(a, b), add(a, neg(b)))`).

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputKind};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, OptimizerOnBuilt};
use crate::worklist::WorkSet;

/// Opt-in pattern-canonicalisation pass.  Add it to your pipeline
/// after the default-stable subset when you want
/// `sub(_, IntConst)` and `add(_, signed_int_const(-K))` to match
/// the same shape.  See the module-level doc for the rationale.
pub struct SubToAdd;

impl OptimizerOnBuilt for SubToAdd {
    fn optimize_built(&self, fg: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let mut work = WorkSet::seeded(fg.preorder());
        let mut result = OptimizationResult::NoChange;
        while let Some(node_id) = work.pop() {
            let r = try_rewrite(fg, node_id)?;
            if r.changed() {
                result |= r;
            }
        }
        Ok(result)
    }
}

fn try_rewrite(fg: &mut BuiltFunctionGraph, node_id: NodeId) -> Result<OptimizationResult> {
    // Match `IntBinaryOp(Sub)`.
    if !matches!(
        *fg.graph.node_kind(node_id),
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Sub)
    ) {
        return Ok(OptimizationResult::NoChange);
    }
    let inputs = fg.graph.node_inputs(node_id);
    if inputs.len() != 2 {
        return Ok(OptimizationResult::NoChange);
    }
    let lhs_in = inputs[0];
    let rhs_in = inputs[1];

    // RHS must be an IntConst.  Variable RHS isn't rewritten — see
    // the module doc for the rationale.
    let rhs_node = fg.graph.get_node_from_output(rhs_in);
    let NodeKind::IntConst(rhs_val) = *fg.graph.node_kind(rhs_node) else {
        return Ok(OptimizationResult::NoChange);
    };

    // Determine the output type of the Sub (also the type of the
    // new IntConst we'll synthesise).
    let [old_out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let Some(out_ty) = fg.graph.output_kind(old_out).as_value() else {
        return Ok(OptimizationResult::NoChange);
    };
    if !out_ty.is_integer() {
        return Ok(OptimizationResult::NoChange);
    }

    // Compute -K mod 2^width via two's-complement:
    //   neg = (!K + 1) & width_mask
    let mask = out_ty.bit_mask_u128();
    let neg_val = rhs_val.wrapping_neg() & mask;

    // Avoid creating a no-op pair when the negation didn't actually
    // change the bit pattern — the only case is K = 0 (sub(a, 0) is
    // a separate identity ConstantFold already collapses).  Skipping
    // here keeps SubToAdd from creating a redundant Add(a, 0) chain
    // when run after a graph that already contains a literal
    // Sub(a, 0) (rare but possible).
    if rhs_val == 0 {
        return Ok(OptimizationResult::NoChange);
    }

    // Build the new IntConst node.
    let neg_const_node = fg.graph.create_node(
        NodeKind::IntConst(neg_val),
        [],
        [NodeOutputKind::OutputType(out_ty)],
    );
    let [neg_const_out] = fg.graph.node_outputs_exact::<1>(neg_const_node)?;

    // Build the new Add node.
    let add_node = fg.graph.create_node(
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Add),
        [lhs_in, neg_const_out],
        [NodeOutputKind::OutputType(out_ty)],
    );
    let [add_out] = fg.graph.node_outputs_exact::<1>(add_node)?;

    // Rewire every consumer of the old Sub's output to point at the
    // new Add's output.  The old Sub node + its old IntConst
    // operand become dead but stay alive in the arena (matches
    // ConstantFold's discipline).
    let changed = fg.graph.replace_all_uses(old_out, add_out)?;
    Ok(OptimizationResult::from_changed(changed))
}

#[cfg(test)]
mod tests;
