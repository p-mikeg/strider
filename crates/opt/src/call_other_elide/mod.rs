//! Removes [`NodeKind::CallOther`] nodes whose user-op name is in
//! [`NO_OP_USER_OPS`] — Sleigh user-defined pcode ops whose semantic effect
//! is a true no-op in the IR's control / memory / value model.
//!
//! Why: Sleigh emits a `callother` for any CPU intrinsic whose pcode it
//! doesn't fully model.  Many of these intrinsics affect only state the IR
//! does not track (decoder ISA-mode bits, monitor-mode flags, ...), so they
//! show up in the IR as opaque control-thread "speed bumps" that block
//! pattern-matcher walks like `if_node().true_branch(call())` even though
//! the underlying data-flow is correct.
//!
//! The IR builder doesn't apply this transformation directly because
//! per-architecture user-op semantics are an analysis concern, not a lifting
//! concern (mirrors the existing `StackStoreDetect` / `LoadReadOnly`
//! split).  The user-op name is recorded into `Graph::call_other_names` at
//! IR construction time by the analyzer; this pass consults that side-table.
//!
//! Composes naturally with `RedundantPhis`: if elision drops a CallOther
//! between two `ControlState` join points, the resulting single-input
//! `ControlState` is collapsed by `RedundantPhis` on the next fixed-point
//! iteration.

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputKind};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, OptimizerOnBuilt};

/// User-op names whose semantic effect is a true no-op in the IR's
/// data-flow / control-flow / memory model.
///
/// Currently:
/// * `setISAMode` — ARM / Thumb-2: copies the `ISAModeSwitch` register to
///   `TMode`.  Affects only the parser's ISA-mode context bit (used by
///   `bx`/`blx`/IT-block decoding); no IR-visible memory or value effect.
///   Sleigh emits this between an `If` and the following `Call` on Thumb
///   targets, blocking pattern walks like
///   `if_node().true_branch(call())`.
/// * `setEndianState` — ARM SETEND instruction: only updates the decoder's
///   endianness context bit.  No IR-visible effect.
///
/// New names can be added here as additional architectures are exercised.
/// Keep ASCII-sorted within their group for diffability.
pub const NO_OP_USER_OPS: &[&str] = &[
    "setEndianState",
    "setISAMode",
];

/// Pass that elides [`NodeKind::CallOther`] nodes whose recorded user-op
/// name is in [`NO_OP_USER_OPS`].
///
/// Runs in the fixed-point loop alongside `ConstantFold` etc. so that
/// downstream passes (especially `RedundantPhis`) get to clean up the
/// single-predecessor `ControlState` nodes left behind.
pub struct CallOtherElide;

impl OptimizerOnBuilt for CallOtherElide {
    fn optimize_built(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        // Collect candidates first — we can't iterate while mutating the
        // graph, and `preorder` borrows `function` immutably.
        let candidates: Vec<NodeId> = function
            .preorder_kind(|k| matches!(k, NodeKind::CallOther { .. }))
            .filter(|&n| {
                function
                    .graph
                    .call_other_name(n)
                    .is_some_and(|name| NO_OP_USER_OPS.contains(&name))
            })
            .collect();

        let mut result = OptimizationResult::NoChange;
        for node_id in candidates {
            result |= elide_call_other(function, node_id)?;
        }
        Ok(result)
    }
}

/// Elides a single `CallOther` node if it is safe to do so:
///   * Rewires control-out uses → control-in producer
///   * Rewires memory-out uses → memory-in producer
///   * If the node has a value-typed output AND that output has consumers,
///     the elision is skipped (we have no value to forward).  In practice
///     no-op user-ops don't produce values (e.g. `setISAMode` returns void),
///     so this guard is purely defensive.
///
/// Returns `Changed` iff the node's inputs were detached.
fn elide_call_other(
    fg: &mut BuiltFunctionGraph,
    node_id: NodeId,
) -> Result<OptimizationResult> {
    // CallOther signature: inputs[0]=Control, inputs[1]=Memory, inputs[2..]=args.
    //                     outputs[0]=Control, outputs[1]=Memory, outputs[2]=Value (optional).
    let inputs = fg.graph.node_inputs(node_id);
    let outputs: Vec<_> = fg.graph.node_outputs(node_id).into_iter().collect();
    if inputs.len() < 2 || outputs.len() < 2 {
        // Malformed CallOther — leave alone, validate will catch it.
        return Ok(OptimizationResult::NoChange);
    }

    // Defensive: if any value-typed output has consumers, skip.  The
    // signature `expected_signature` for CallOther uses an `ANY_VAL` tail
    // so the slot count past Memory is variadic (currently always 0 or 1
    // via `build_call_other`, but a future code path could synthesise
    // more).  Inspect every value-typed slot, not just slot 2, so the
    // guard stays sound regardless of how many values the node carries.
    let has_live_value_output = outputs
        .iter()
        .skip(2)
        .copied()
        .filter(|&out| fg.graph.output_kind(out).is_value())
        .any(|out| fg.graph.output_uses(out).next().is_some());
    if has_live_value_output {
        return Ok(OptimizationResult::NoChange);
    }

    let ctrl_in = inputs[0];
    let mem_in = inputs[1];
    let ctrl_out = outputs[0];
    let mem_out = outputs[1];

    // Sanity: the slot kinds should match what the signature promises.  If
    // they don't, the graph is malformed; bail out without modifying it.
    if !matches!(fg.graph.output_kind(ctrl_out), NodeOutputKind::Control)
        || !matches!(fg.graph.output_kind(mem_out), NodeOutputKind::Memory)
    {
        return Ok(OptimizationResult::NoChange);
    }

    // Rewire all consumers of ctrl_out → ctrl_in, mem_out → mem_in.
    let ctrl_changed = fg.graph.replace_all_uses(ctrl_out, ctrl_in)?;
    let mem_changed = fg.graph.replace_all_uses(mem_out, mem_in)?;
    let changed = ctrl_changed || mem_changed;

    // Detach our own inputs so the node becomes a zero-input zombie that
    // walk_graph won't reach (matches the convention used by
    // `RedundantPhis`).  We rely on validate's reachability scoping to
    // skip Layer A on zombies.
    if changed {
        fg.graph.detach_node_inputs(node_id);
    }
    Ok(if changed {
        OptimizationResult::Changed
    } else {
        OptimizationResult::NoChange
    })
}

#[cfg(test)]
mod tests;
