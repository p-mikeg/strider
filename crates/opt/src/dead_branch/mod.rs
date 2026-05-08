
use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, OptimizerOnBuilt};
use crate::worklist::WorkSet;

#[cfg(test)]
mod tests;

// ── Dead-branch elimination ───────────────────────────────────────────────────

/// Eliminates `If` nodes whose condition is a `BoolConst`.
///
/// For `If(ctrl_in, BoolConst(b))` with outputs `[ctrl_true, ctrl_false]`:
///
/// * The **live** control output (`ctrl_true` when `b=true`, `ctrl_false` when
///   `b=false`) is replaced with `ctrl_in` so the successor region receives
///   control directly without going through the `If`.
/// * When the dead-branch subgraph is self-contained (no data outputs flow
///   to live consumers), the **dead** control output is removed from the
///   successor `ControlState`'s input list, the corresponding position is
///   removed from every `VarPhi` of that region, and the If's own inputs
///   are detached so the outer fixed-point loop stops re-visiting it.
/// * When the dead-branch subgraph escapes (e.g. a dead `Call`'s
///   `mem_out` flows into the join's `MemPhi`), the dead branch is left
///   wired untouched.  Detaching the If or stripping its `ControlState`
///   predecessor would create zero-input zombies that the walker
///   re-reaches through backward-data from the live consumers, breaking
///   Layer A / Layer C invariants.  `RedundantPhis` is responsible for
///   tearing the live ↔ dead data edges apart on subsequent iterations;
///   a later DBE pass then sees a non-escaping subgraph and finishes
///   the job.
///
/// After this pass, dead `ControlState` nodes end up with zero control inputs
/// and `VarPhi` nodes with a single value input; `RedundantPhis` then
/// cleans those up.
fn try_eliminate_dead_branch(
    fg: &mut BuiltFunctionGraph,
    node_id: NodeId,
) -> Result<OptimizationResult> {
    // Only handle If nodes.
    if !matches!(*fg.node_kind(node_id), NodeKind::If) {
        return Ok(OptimizationResult::NoChange);
    }

    // If inputs: [ctrl_in, condition].
    let inputs = fg.node_inputs(node_id);
    if inputs.len() < 2 {
        return Ok(OptimizationResult::NoChange);
    }
    let ctrl_in = inputs[0];
    let cond_out = inputs[1];

    let Some(cond_val) = fg.bool_const_val(cond_out) else {
        return Ok(OptimizationResult::NoChange);
    };

    // If outputs: [ctrl_true (index 0), ctrl_false (index 1)].
    let [ctrl_true, ctrl_false] = fg.node_outputs_exact::<2>(node_id)?;

    let (live_ctrl, dead_ctrl) = if cond_val {
        (ctrl_true, ctrl_false)
    } else {
        (ctrl_false, ctrl_true)
    };

    // Snapshot the dead- and live-side state BEFORE any mutation so we can
    // decide whether work is left to do this iteration.  Each `dead_uses`
    // entry is `(consumer_node, input_index)`.
    let dead_uses: Vec<(NodeId, u32)> = fg.output_uses(dead_ctrl).collect();
    let live_uses_count = fg.output_uses(live_ctrl).count();

    // Detaching the If's inputs (the old "Step 4") severs the only edge
    // keeping the now-folded If attached to the live walk.  That's normally
    // exactly what we want — the outer pipeline stops revisiting the If on
    // every iteration.  But when the dead-branch subgraph has data outputs
    // consumed by *live* nodes (e.g. a dead `Call`'s `mem_out` flowing into
    // the join's `MemPhi`), backward-data from those live consumers walks
    // back into the dead subgraph and reaches the now-zero-input If, which
    // makes the validator's Layer A check fire `expected: 2, actual: 0`
    // (see `dead_branch_with_non_control_state_dead_consumer`).
    //
    // To stay correct in both cases: forward-control walk the dead subgraph
    // starting from each non-CS dead consumer, then check whether any data
    // output of that subgraph escapes to a node *outside* it.  If yes, leave
    // the If's inputs intact (`RedundantPhis` will pull the live ↔ dead data
    // edges apart on subsequent iterations as joins collapse, then a later
    // DBE iteration will be free to detach).  If no, the dead subgraph is
    // self-contained and detaching is safe.
    let dead_subgraph = collect_dead_subgraph(fg, &dead_uses);
    let dead_subgraph_escapes = dead_subgraph_has_live_data_consumer(fg, &dead_subgraph);

    // Idempotency:
    //   * `live_uses_count == 0` ⇒ live side already rewired.
    //   * Either there's no dead-side work to do (`dead_uses` empty or every
    //     CS already stripped), or the dead subgraph escapes — in which
    //     case we deliberately won't strip / detach this iteration so
    //     re-visiting can't make further progress.
    if live_uses_count == 0
        && (dead_uses.is_empty() || dead_subgraph_escapes || dead_uses_all_zero_input(fg, &dead_uses))
    {
        return Ok(OptimizationResult::NoChange);
    }

    // ── Step 1: replace live ctrl with ctrl_in (bypass the If) ───────────────
    // Absorb the If's asm-fingerprint into the surviving control producer
    // (typically a ControlState — exempt from the non-empty check, but
    // unioning the address there preserves the contributing-asm-instruction
    // history so consumers can recover it from the side-table later).
    let if_node = fg.get_node_from_output(live_ctrl);
    let ctrl_in_node = fg.get_node_from_output(ctrl_in);
    fg.extend_asm_fingerprint_from(ctrl_in_node, if_node);
    fg.replace_all_uses(live_ctrl, ctrl_in)?;

    // The dead-side cleanup is **all-or-nothing** based on whether the dead
    // subgraph escapes.  When it doesn't escape we strip every CS predecessor
    // slot and detach the If; the resulting zero-input zombies are
    // unreachable from the live walk, so the validator never sees them.
    //
    // When it *does* escape (the kernel-bug case: a dead `Call`'s `mem_out`
    // flows into the join's `MemPhi`), we leave every dead `ControlState`'s
    // input alone and leave the If attached.  Stripping would create
    // zero-input `ControlState`s that the walker still reaches through
    // backward-data from a live `MemPhi` → dead `Call` → dead phi token,
    // tripping Layer C's `EmptyControlStatePredecessors` check.  Letting
    // `RedundantPhis` collapse the live join's phis on subsequent iterations
    // tears the live ↔ dead data edges apart; once they're gone a future
    // DBE iteration sees a non-escaping subgraph and finishes the job.
    if !dead_subgraph_escapes {
        for (cs_node, dead_idx) in &dead_uses {
            let cs_node = *cs_node;
            let dead_idx = *dead_idx;
            if !matches!(*fg.node_kind(cs_node), NodeKind::ControlState) {
                continue;
            }

            // ControlState outputs: [ctrl_out, phi_out].
            let cs_outputs = fg.node_outputs(cs_node);
            if cs_outputs.len() < 2 {
                continue;
            }
            let cs_phi_out = cs_outputs[1];

            // Collect VarPhi nodes that consume the phi token before we mutate.
            let phi_nodes: Vec<NodeId> = fg
                .graph
                .output_uses(cs_phi_out)
                .map(|(phi, _)| phi)
                .collect();

            // Remove the dead variable-value input from each VarPhi.
            // VarPhi inputs: [phi_token, val_from_pred0, val_from_pred1, …]
            // So the variable value for predecessor at ControlState index
            // `dead_idx` lives at VarPhi index `dead_idx + 1`.  Removals at
            // different consumers don't interact (each `remove_node_input`
            // only shifts its own later indices), and the
            // `phi_input_idx < phi_len` / `dead_idx < cs_len` guards catch
            // per-consumer indices already shifted by an earlier removal.
            let phi_input_idx = dead_idx + 1;
            for phi_node in phi_nodes {
                let phi_len = fg.node_inputs(phi_node).len() as u32;
                if phi_input_idx < phi_len {
                    fg.remove_node_input(phi_node, phi_input_idx)?;
                }
            }

            let cs_len = fg.node_inputs(cs_node).len() as u32;
            if dead_idx < cs_len {
                fg.remove_node_input(cs_node, dead_idx)?;
            }
        }

        fg.detach_node_inputs(node_id);
    }

    Ok(OptimizationResult::Changed)
}

/// Returns `true` if every CS-typed dead consumer in `dead_uses` already
/// has zero inputs — i.e. a previous DBE iteration already stripped them.
/// Used by the idempotency check to avoid spinning the outer pipeline loop.
fn dead_uses_all_zero_input(
    fg: &BuiltFunctionGraph,
    dead_uses: &[(NodeId, u32)],
) -> bool {
    dead_uses.iter().all(|(n, _)| {
        !matches!(*fg.node_kind(*n), NodeKind::ControlState)
            || fg.node_inputs(*n).is_empty()
    })
}

/// Forward-control walk from each non-`ControlState` dead consumer to
/// collect every node that lies in the dead subgraph downstream of the If.
/// `ControlState`s mark merge points and are *not* recursed through — they
/// are part of the "boundary" where dead and live control flow can rejoin.
fn collect_dead_subgraph(
    fg: &BuiltFunctionGraph,
    dead_uses: &[(NodeId, u32)],
) -> entity_utils::DenseEntitySet<NodeId> {
    let mut subgraph: entity_utils::DenseEntitySet<NodeId> = entity_utils::DenseEntitySet::new();
    let mut worklist: Vec<NodeId> = dead_uses
        .iter()
        .filter(|(n, _)| !matches!(*fg.node_kind(*n), NodeKind::ControlState))
        .map(|(n, _)| *n)
        .collect();
    while let Some(node) = worklist.pop() {
        if !subgraph.insert(node) {
            continue;
        }
        for output in fg.node_outputs(node) {
            if !fg.output_kind(output).is_control() {
                continue;
            }
            for (consumer, _) in fg.output_uses(output) {
                if matches!(*fg.node_kind(consumer), NodeKind::ControlState) {
                    continue; // Boundary — don't walk past joins.
                }
                worklist.push(consumer);
            }
        }
    }
    subgraph
}

/// True iff some node in `subgraph` has a non-Control output consumed by a
/// node *outside* `subgraph`.  When true, detaching the If would leave the
/// dead subgraph reachable through backward-data from those live consumers
/// and the still-attached If would fail Layer A's input-count check.
fn dead_subgraph_has_live_data_consumer(
    fg: &BuiltFunctionGraph,
    subgraph: &entity_utils::DenseEntitySet<NodeId>,
) -> bool {
    subgraph.iter().any(|node| {
        fg.node_outputs(node).into_iter().any(|out| {
            if fg.output_kind(out).is_control() {
                return false;
            }
            fg.graph
                .output_uses(out)
                .any(|(consumer, _)| !subgraph.contains(consumer))
        })
    })
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Eliminates branches whose condition is a compile-time boolean constant.
///
/// Works together with [`crate::RedundantPhis`]: after dead-branch elimination
/// the previously-live successor region may have a single-input `ControlState`
/// and `VarPhi` nodes, which `RedundantPhis` can then collapse.
pub struct DeadBranchElimination;

impl OptimizerOnBuilt for DeadBranchElimination {
    fn optimize_built(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        // DBE only fires on `If` nodes whose outputs are control edges. We
        // drain the seeded preorder once: chained constant-branch patterns
        // (where one elimination exposes another) are caught by the outer
        // OptimizerPipeline fixed-point loop, which re-runs this pass until
        // it reports NoChange.
        let mut work = WorkSet::seeded(function.preorder());
        let mut result = OptimizationResult::NoChange;
        while let Some(node_id) = work.pop() {
            result |= try_eliminate_dead_branch(function, node_id)?;
        }
        Ok(result)
    }
}
