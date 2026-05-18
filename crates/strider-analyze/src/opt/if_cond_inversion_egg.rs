//! Egg-based `IfCondInversion` rewriter — Phase 3 Task 3.3b.
//!
//! Built alongside the imperative [`crate::opt::IfCondInversion`] —
//! NOT a replacement.  The parity test
//! `crates/strider-analyze/tests/if_cond_inversion_egg_parity.rs`
//! proves both produce structurally identical IR for the supported
//! shapes.
//!
//! # Design — why this pass does NOT use the egraph
//!
//! Section A's egraph design pins **control** nodes outside the
//! egraph.  `If` is a control node (it produces two `Control`-typed
//! outputs).  The rewrite this pass performs is:
//!
//! 1. Redirect the `If`'s cond input from `BoolNeg(C)` to `C`.
//! 2. Swap the consumers of the two control outputs.
//!
//! Neither step is a value-slice rewrite — both are use-list
//! mutations on control edges, which the egraph adapter discards by
//! construction.  Per the Phase 3.3 plan's BLOCK clause:
//!
//! > "If after reading v1's IfCondInversion you find it doesn't need
//! > the egraph at all (it's a simple structural match), you can
//! > skip the egg integration for this pass and just port the
//! > structural rewrite.  That's a valid outcome."
//!
//! That outcome applies here.  The egraph could in principle answer
//! "this `If`'s cond e-class contains `BoolNeg(X)` for some `X`" —
//! but that's exactly the local structural check the pattern crate
//! already does cheaply, and the cond producer is uniquely
//! determined by the input edge, not by union-find membership.
//! Spinning up an `EGraphAdapter` per pass invocation would add
//! latency without changing the matched set.
//!
//! v2 is therefore a faithful straight port of v1's structural
//! rewrite — same algorithm, separately importable, drop-in
//! testable.  The parity test pins identical output for every
//! supported shape, including the asm-fingerprint absorption that
//! v1 ships.

use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};

/// Pass that rewrites `If(BoolNeg(C))` into `If(C)` with branches swapped.
///
/// Drop-in replacement for [`crate::opt::IfCondInversion`] with the
/// same observable semantics — only the module path differs.
pub struct IfCondInversionEgg;

impl IfCondInversionEgg {
    /// Construct a fresh `IfCondInversionEgg`.  Stateless.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for IfCondInversionEgg {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizerRaw for IfCondInversionEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // Walk the reachable graph and collect every `If` whose cond
        // producer is a `BoolUnaryOp::Neg`.  We collect first because
        // `invert` mutates the graph (rewires uses), and a live walk
        // over the use-list while it's being mutated would invalidate
        // the iterator.
        let candidates: Vec<NodeId> = strider_ir::walk::walk_graph(graph, entry)
            .filter(|&node| matches!(graph.node_kind(node), NodeKind::If))
            .filter(|&node| is_inverted_cond(graph, node))
            .collect();

        let mut changed = false;
        for if_node in candidates {
            invert(graph, if_node)?;
            changed = true;
        }
        Ok(if changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        })
    }
}

/// Returns `true` when the `If` node's cond input (slot 1) consumes the
/// output of a `BoolUnaryOp::Neg` node.
fn is_inverted_cond(graph: &strider_ir::Graph, if_node: NodeId) -> bool {
    let Ok([_ctrl, cond_out]) = graph.node_inputs_exact::<2>(if_node) else {
        return false;
    };
    let cond_node = graph.get_node_from_output(cond_out);
    matches!(
        graph.node_kind(cond_node),
        NodeKind::BoolUnaryOp(strider_ir::BoolUnaryOp::Neg)
    )
}

/// In-place rewrite: redirect cond, swap control-output consumers,
/// absorb BoolNeg's fingerprint into the inner-cond node.
///
/// Mirrors v1 exactly — see `crate::opt::if_cond_inversion::invert`.
fn invert(graph: &mut strider_ir::Graph, if_node: NodeId) -> Result<()> {
    // Step 1: redirect cond input.
    let cond_input_id = graph.node_input_id_at(if_node, 1)?;
    let cond_out = graph.input_output_id(cond_input_id);
    let bool_neg_node = graph.get_node_from_output(cond_out);
    let [inner] = graph.node_inputs_exact::<1>(bool_neg_node)?;
    // Absorb the BoolNeg's asm-fingerprint into the surviving
    // inner-cond node BEFORE redirecting the input.  This upholds the
    // asm-fingerprint superset contract: if the BoolNeg becomes dead
    // after the rewrite, its contributing-asm addresses must survive
    // in whatever node takes over its semantic role.
    let inner_node = graph.get_node_from_output(inner);
    graph.extend_asm_fingerprint_from(inner_node, bool_neg_node);
    graph.update_input(cond_input_id, inner);

    // Step 2: swap consumers between output[0] (true) and output[1] (false).
    let [true_out, false_out] = graph.node_outputs_exact::<2>(if_node)?;
    let true_use_ids: smallvec::SmallVec<[strider_ir::node::NodeInputId; 4]> = graph
        .output_uses(true_out)
        .map(|(consumer, idx)| graph.node_input_id_at(consumer, idx as usize))
        .collect::<Result<_>>()?;
    let false_use_ids: smallvec::SmallVec<[strider_ir::node::NodeInputId; 4]> = graph
        .output_uses(false_out)
        .map(|(consumer, idx)| graph.node_input_id_at(consumer, idx as usize))
        .collect::<Result<_>>()?;
    for use_id in true_use_ids {
        graph.update_input(use_id, false_out);
    }
    for use_id in false_use_ids {
        graph.update_input(use_id, true_out);
    }
    Ok(())
}
