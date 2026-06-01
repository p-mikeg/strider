use anyhow::bail;

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use entity_utils::DenseEntitySet;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

/// If every output of `node_id` has no uses and the node still has inputs,
/// detaches all inputs (severing dead nodes from the graph) and returns
/// `Changed`.  Otherwise returns `NoChange`.
fn try_detach_dead_inputs(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    node_id: NodeId,
) -> OptimizationResult {
    let all_unused = ctx
        .node_outputs(node_id)
        .iter()
        .all(|&out| ctx.output_uses(out).next().is_none());

    if all_unused && !ctx.node_inputs(node_id).is_empty() {
        ctx.detach_node_inputs(node_id);
        OptimizationResult::Changed
    } else {
        OptimizationResult::NoChange
    }
}

/// Handles the `NodeKind::Region` arm of [`try_simplify_phi_like`]: collapses a
/// single-predecessor `Region` by replacing its control output with the lone
/// live input and absorbing its asm-fingerprint into that predecessor's node.
///
/// Separated from `try_simplify_phi_like` to keep the outer match readable.
fn try_collapse_single_pred_region(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    node_id: NodeId,
    reachable: &strider_ir::walk::NodeIdSet,
) -> crate::opt::Result<OptimizationResult> {
    let node_inputs = ctx.node_inputs(node_id);
    let reachable_inputs: DenseEntitySet<NodeOutputId> = node_inputs
        .into_iter()
        .filter(|inp| reachable.contains(ctx.output_definition(*inp).0))
        .collect();

    let mut iter = reachable_inputs.iter();
    let simplified = match (iter.next(), iter.next()) {
        (Some(input), None) => {
            let [output, _phi_token] = ctx.node_outputs_exact::<2>(node_id)?;
            // Region is exempt-empty by default; absorb its fingerprint into
            // the surviving control producer (same rationale as phi-collapse).
            ctx.replace_value(output, input)?
        }
        _ => false,
    };

    // For Region we can only detach when BOTH outputs are unused.
    // try_detach_dead_inputs handles this check.
    if simplified {
        Ok(try_detach_dead_inputs(ctx, node_id) | OptimizationResult::Changed)
    } else {
        Ok(try_detach_dead_inputs(ctx, node_id))
    }
}

/// Attempts to simplify the phi-like node `node_id` given the set of
/// CFG-reachable nodes.  Returns `Changed` if any transformation was applied.
fn try_simplify_phi_like(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    node_id: NodeId,
    reachable: &strider_ir::walk::NodeIdSet,
) -> Result<OptimizationResult> {
    match ctx.node_kind(node_id) {
        // Phi and MemPhi have identical input layouts after the builder
        // links phi_token as inputs[0] for both:
        //
        //   inputs[0]   = PhiToken from the owning Region
        //   inputs[1..] = one value/memory per predecessor, same order as
        //                 Region.inputs[0..]
        //
        // Reachability is determined positionally: predecessor j is live iff
        // Region.inputs[j]'s producer is in the CFG-reachable set.
        // We deduplicate by NodeOutputId so that two edges from the same
        // predecessor (unusual but valid) count as one.
        NodeKind::Phi | NodeKind::MemPhi => {
            let inputs = ctx.node_inputs(node_id);
            if inputs.is_empty() {
                return Ok(OptimizationResult::NoChange);
            }
            let phi_token = inputs[0];
            let region_id = ctx.output_definition(phi_token).0;
            let ctrl_inputs = ctx.node_inputs(region_id);
            let phi_self_output = ctx.node_outputs_exact::<1>(node_id)?[0];

            // Single pass: gather both the deduplicated reachable ctrl edges
            // and their corresponding values (inputs[j + 1]) for live
            // predecessors only.  Self-referential value inputs (where the
            // phi reads its OWN output, the canonical loop back-edge shape
            // for a variable not modified inside the loop) are filtered
            // out of `live_values` — Braun's trivial-phi rule.  This lets
            // the "all distinct values are the same" arm below collapse
            // `phi(v, phi)` to `v`, where the prior code saw two distinct
            // operands and refused to simplify.  The corresponding
            // `reachable_ctrl` entry is still recorded so the
            // single-ctrl arm above doesn't fire for what is logically a
            // multi-edge join.
            let mut reachable_ctrl: DenseEntitySet<NodeOutputId> = DenseEntitySet::new();
            let mut live_values: DenseEntitySet<NodeOutputId> = DenseEntitySet::new();
            for (j, ctrl_in) in ctrl_inputs.into_iter().enumerate() {
                if reachable.contains(ctx.output_definition(ctrl_in).0) {
                    reachable_ctrl.insert(ctrl_in);
                    // Defend against transient mid-opt arity mismatch: a
                    // peer pass running in the same fixed-point loop can
                    // momentarily leave a phi with fewer value inputs than
                    // its owning Region has ctrl edges.  Surface as a
                    // typed error instead of panicking on slice indexing —
                    // the fixed-point loop will rerun and the next
                    // iteration sees the repaired arity.
                    let value = inputs.get(j + 1).copied().ok_or_else(|| {
                        anyhow::anyhow!(
                            "redundant_phis: phi {node_id:?} value-input arity \
                             ({}) does not match owning Region ctrl-edge \
                             count ({}); transient mid-opt invariant violation",
                            inputs.len().saturating_sub(1),
                            ctx.node_inputs(region_id).len()
                        )
                    })?;
                    if value != phi_self_output {
                        live_values.insert(value);
                    }
                }
            }

            // Drive on iterator-singularity rather than `len()==1`: the
            // `(Some(_), None)` match makes "exactly one element" a
            // structural property the compiler enforces, so we don't need
            // a defensive `ok_or` after the count check.
            let mut ctrl_iter = reachable_ctrl.iter();
            let mut value_iter = live_values.iter();
            let simplified = match (ctrl_iter.next(), ctrl_iter.next()) {
                (Some(unique_ctrl), None) => {
                    // Find position j such that ctrl_inputs[j] == unique_ctrl, then
                    // take inputs[j + 1] (skipping the phi_token at inputs[0]).
                    let ctrl_inputs2 = ctx.node_inputs(region_id);
                    let Some(j) = ctrl_inputs2.into_iter().position(|c| c == unique_ctrl)
                    else {
                        bail!("unique control edge not found in Region inputs");
                    };
                    // Same transient-arity defense as the loop above —
                    // bail typed instead of panicking on `[j + 1]`.
                    let value = ctx.node_inputs(node_id).get(j + 1).copied().ok_or_else(|| {
                        anyhow::anyhow!(
                            "redundant_phis: phi {node_id:?} value-input arity \
                             does not cover ctrl-edge index {j}; transient mid-opt \
                             invariant violation"
                        )
                    })?;
                    let [output] = ctx.node_outputs_exact::<1>(node_id)?;
                    // Absorb the phi's asm-fingerprint into the surviving
                    // value producer.  Phis are exempt-empty by default, but
                    // an earlier opt pass (e.g. StackOffsetDetect) may have
                    // unioned addresses into them; preserve those here.
                    ctx.replace_value(output, value)?
                }
                _ => match (value_iter.next(), value_iter.next()) {
                    // Distinct live ctrl predecessors all feed the same data
                    // value: the phi is a no-op.  Replace uses with that single
                    // value.  (The Region still has multiple real
                    // predecessors, so we don't touch it here.)
                    (Some(value), None) => {
                        let [output] = ctx.node_outputs_exact::<1>(node_id)?;
                        ctx.replace_value(output, value)?
                    }
                    _ => false,
                },
            };

            if simplified {
                ctx.detach_node_inputs(node_id);
                Ok(OptimizationResult::Changed)
            } else {
                Ok(try_detach_dead_inputs(ctx, node_id))
            }
        }
        NodeKind::Region => try_collapse_single_pred_region(ctx, node_id, reachable),
        _ => Ok(OptimizationResult::NoChange),
    }
}

/// Eliminates `Phi`, `MemPhi`, and `Region` nodes that have only
/// one reachable predecessor, replacing them with that predecessor's value.
/// Also detaches the inputs of any node that is not reachable from the entry.
///
/// This pass is typically run after [`crate::opt::DeadBranchElimination`], which
/// leaves single-input phis behind.
#[derive(Clone)]
pub struct RedundantPhis;

impl Optimizer for RedundantPhis {
    fn apply(
        &self,
        ctx: &mut strider_pattern::RewriteCtx<'_>,
        _opt_ctx: &crate::opt::OptCtx<'_>,
    ) -> crate::opt::Result<OptimizationResult> {
        let reachable = strider_ir::walk::cfg_reachable(ctx.graph_ref(), ctx.entry());
        let mut res = OptimizationResult::NoChange;
        // Only phi-like nodes can be simplified by `try_simplify_phi_like`, so don't
        // walk every node — pre-filter on the kinds we care about.
        let candidates: Vec<NodeId> = ctx
            .walk()
            .filter(|&n| {
                matches!(
                    ctx.node_kind(n),
                    NodeKind::Phi | NodeKind::MemPhi | NodeKind::Region
                )
            })
            .collect();
        for node_id in candidates {
            res |= try_simplify_phi_like(ctx, node_id, &reachable)?;
        }
        // Detaching unreachable zombies is bookkeeping, not progress: an
        // unreachable node cannot be a consumer of a reachable producer, so
        // no other pass can act on the result.  Run it for hygiene but do
        // NOT escalate it into a `Changed` signal — that just costs the
        // pipeline one extra fixed-point iteration with no work to do.
        let entry = ctx.entry();
        let _ = ctx.detach_unreachable_nodes(entry);
        Ok(res)
    }
}

#[cfg(test)]
mod tests;
