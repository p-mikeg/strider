use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::worklist::WorkSet;

mod eval_float;
mod eval_int;
mod rules;
#[cfg(test)]
mod tests;

use rules::*;

/// Lowers a `CastToFloat` node to the appropriate specific form based on the
/// actual input type:
///
/// - Input is the same float type as output → eliminated (identity).
/// - Input is a different float type → lowered to `FloatToFloat`.
/// - Input is an integer `IntConst(v)` → immediately constant-folded to `FloatConst(v)`.
/// - Input is any other integer type → lowered to `IntBitsToFloat`.
fn try_lower_cast_to_float(
    fg: &mut BuiltFunctionGraph,
    node_id: NodeId,
) -> Result<OptimizationResult> {
    if !matches!(*fg.graph.node_kind(node_id), NodeKind::CastToFloat) {
        return Ok(OptimizationResult::NoChange);
    }

    let [out] = fg.graph.node_outputs_exact::<1>(node_id)?;
    let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;

    let out_kind = fg.graph.output_kind(out);
    let in_kind = fg.graph.output_kind(input);
    let out_ty = out_kind.as_value_or_err()?;
    let in_ty = in_kind.as_value_or_err()?;

    // 1. Identity: input already has the target float type.
    if in_ty == out_ty {
        return Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, input)?));
    }

    // 2. Float→float precision change.
    if in_ty.is_float() {
        let new_out = fg.make_float_to_float_node(input, out_ty)?;
        return Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, new_out)?));
    }

    // Input is integer from here.

    // 3. Integer constant → float constant (same bits).
    if let Some(bits) = fg.int_const_val(input) {
        let new_out = fg.make_float_const(bits, out_ty)?;
        return Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, new_out)?));
    }

    // 4. Non-constant integer → explicit IntBitsToFloat.
    let new_out = fg.make_int_bits_to_float_node(input, out_ty)?;
    Ok(OptimizationResult::from_changed(fg.replace_all_uses(out, new_out)?))
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Folds constant expressions and applies algebraic identities.
///
/// Handles full constant evaluation for all arithmetic, comparison, boolean,
/// truncation, and extension operations.  Also applies identities such as
/// `x + 0 → x`, `x ^ x → 0`, and nested AND-mask merging `(a & C1) & C2 →
/// a & (C1 & C2)`.
pub struct ConstantFold;

impl Optimizer for ConstantFold {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let mut work = WorkSet::seeded(function.preorder());
        let mut result = OptimizationResult::NoChange;
        // Reused per iteration to snapshot consumer NodeIds BEFORE running
        // rules. After a rule rewrites the node, `output_uses(old_out)` is
        // empty (uses were rewired to the replacement), so we must capture
        // consumers ahead of time to re-enqueue them.
        let mut consumers: Vec<NodeId> = Vec::new();
        while let Some(node_id) = work.pop() {
            consumers.clear();
            for out in function.graph.node_outputs(node_id) {
                for (consumer, _) in function.graph.output_uses(out) {
                    consumers.push(consumer);
                }
            }
            let r = apply_identity_rules(function, node_id)?
                | apply_const_eval_rules(function, node_id)?
                | apply_bool_float_rules(function, node_id)?
                | apply_reassoc_and_mask_rules(function, node_id)?
                | apply_bitcast_extend_rules(function, node_id)?;
            if r.changed() {
                result |= r;
                for &consumer in &consumers {
                    work.push(consumer);
                }
            }
        }
        Ok(result)
    }
}
