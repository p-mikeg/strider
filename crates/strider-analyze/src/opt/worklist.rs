//! Shared per-pass worklist helpers used by every fold-style optimizer.
//!
//! Passes use [`entity_utils::Worklist<NodeId>`] directly (FIFO with
//! dedup-while-queued, backed by a [`DenseEntitySet<NodeId>`] for O(1)
//! membership keyed by `NodeId`'s u32 index).  This module retains the
//! kind-filtered seeding helper plus the reachability sweep that the
//! optimizer pipeline uses to detach orphaned subgraphs.

use strider_ir::Graph;
use entity_utils::{DenseEntitySet, Worklist};
use strider_ir::node::{NodeId, NodeKind};

use crate::opt::pipeline::OptimizationResult;

/// Seeds a worklist with every node reachable from `ctx.entry()` whose
/// [`NodeKind`] satisfies `pred`.
///
/// Replaces the recurring `ctx.preorder_kind(...).collect::<Vec<_>>()`
/// followed by a `for node in collected { ... }` loop: the seeded
/// worklist gives the same one-shot iteration semantics (kind-filtered,
/// no re-enqueue unless a rule explicitly pushes consumers) without
/// allocating an intermediate `Vec`, and lets passes upgrade in place to
/// cascading rewrites by calling [`Worklist::enqueue`] on consumers.
pub(crate) fn seeded_kind<P>(ctx: &crate::pattern::RewriteCtx<'_>, mut pred: P) -> Worklist<NodeId>
where
    P: FnMut(&NodeKind) -> bool,
{
    ctx.preorder_kind(|k| pred(k)).collect()
}

/// Detaches the inputs of every node not reachable from the function entry.
///
/// Unreachable nodes can only be consumed by other unreachable nodes, so
/// severing their inputs is always safe.  Cleans up dead-block residue and
/// orphaned address-arithmetic chains left behind by passes that rewrite
/// reachable consumers (e.g. `DeadBranchElimination`, `FunctionArgDetect`).
///
/// Callers typically discard the result with `let _ = ...`: a Changed
/// verdict here is bookkeeping-only — an unreachable node cannot be a
/// consumer of a reachable producer, so no other pass can act on the
/// result.  Escalating it into the pipeline's `Changed` signal would
/// just buy one extra fixed-point iteration with no work to do.
pub(crate) fn detach_unreachable_nodes(
    graph: &mut Graph,
    entry: NodeId,
) -> OptimizationResult {
    // Use DenseEntitySet (flat bit-vector indexed by raw u32) instead of
    // FxHashSet — same constant-time membership semantics with better
    // cache behaviour at 10k+ nodes.
    let mut reachable: DenseEntitySet<NodeId> = DenseEntitySet::new();
    for n in graph.walk_from(entry) {
        reachable.insert(n);
    }
    // Two-phase: gather the targets up-front (releases the borrow on `graph`
    // and prunes "no inputs to detach" cases) before mutating the graph.
    let to_detach: Vec<NodeId> = graph
        .all_node_ids()
        .filter(|n| !reachable.contains(*n) && !graph.node_inputs(*n).is_empty())
        .collect();
    if to_detach.is_empty() {
        return OptimizationResult::NoChange;
    }
    for node_id in to_detach {
        graph.detach_node_inputs(node_id);
    }
    OptimizationResult::Changed
}
