use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::peephole::{PeepholePass, impl_optimizer_from_peephole};
use crate::opt::pipeline::OptimizationResult;

pub(crate) mod eval_float;
pub(crate) mod eval_int;
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
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    node_id: NodeId,
) -> Result<OptimizationResult> {
    if !matches!(*ctx.node_kind(node_id), NodeKind::CastToFloat) {
        return Ok(OptimizationResult::NoChange);
    }

    let [out] = ctx.node_outputs_exact::<1>(node_id)?;
    let [input] = ctx.node_inputs_exact::<1>(node_id)?;

    let out_ty = ctx.output_kind(out).as_value_or_err()?;
    let in_ty = ctx.output_kind(input).as_value_or_err()?;

    let new_out = if in_ty == out_ty {
        input
    } else if in_ty.is_float() {
        ctx.make_float_to_float_node(input, out_ty)?
    } else if let Some(bits) = ctx.int_const_val(input) {
        ctx.make_float_const(bits, out_ty)?
    } else {
        ctx.make_int_bits_to_float_node(input, out_ty)?
    };
    // Absorb the rewritten cast node's asm-fingerprint into the new producer
    // via `after_replace` (handles fingerprint union + replace_all_uses).
    OptimizationResult::NoChange.after_replace(ctx, out, new_out)
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Folds constant expressions and applies algebraic identities.
///
/// Handles full constant evaluation for all arithmetic, comparison, boolean,
/// truncation, and extension operations.  Also applies identities such as
/// `x + 0 → x`, `x ^ x → 0`, and nested AND-mask merging `(a & C1) & C2 →
/// a & (C1 & C2)`.
pub struct ConstantFold;

impl PeepholePass for ConstantFold {
    fn name(&self) -> &'static str {
        "ConstantFold"
    }

    /// Constant-fold rule groups cover most node kinds (int / bool / float
    /// arithmetic + cmp, casts, truncate / extend, identity rewrites on
    /// just about any binary op).  Seeding the worklist with every
    /// reachable node preserves the prior behaviour of the hand-written
    /// `optimize` impl; the per-group rules already kind-filter
    /// internally.
    fn matches_kind(&self, _kind: &NodeKind) -> bool {
        true
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        Ok(apply_identity_rules(ctx, root)?
            | apply_const_eval_rules(ctx, root)?
            | apply_bool_float_rules(ctx, root)?
            | apply_reassoc_and_mask_rules(ctx, root)?
            | apply_bitcast_extend_rules(ctx, root)?)
    }
}

impl_optimizer_from_peephole!(ConstantFold);
