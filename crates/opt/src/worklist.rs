//! Shared per-pass worklist used by every fold-style optimizer.
//!
//! Replaces the `for node_id in preorder().collect()` pattern with a
//! drain-and-re-enqueue loop: passes seed the worklist with the preorder
//! traversal, then on every successful rewrite call back to push consumers
//! of the rewritten outputs so cascading folds reach fixed point in a single
//! `optimize` invocation.

use std::collections::VecDeque;

use rustc_hash::FxHashSet;

use ir::BuiltFunctionGraph;
use ir::node::NodeId;

use crate::pipeline::OptimizationResult;

/// FIFO worklist that prevents double-enqueue.
#[derive(Default)]
pub(crate) struct WorkSet {
    queued: FxHashSet<NodeId>,
    queue: VecDeque<NodeId>,
}

impl WorkSet {
    /// Seeds the worklist with `it`.
    pub(crate) fn seeded(it: impl IntoIterator<Item = NodeId>) -> Self {
        let mut q = Self::default();
        for n in it {
            q.push(n);
        }
        q
    }

    /// Adds `n` to the queue if it isn't already pending.
    pub(crate) fn push(&mut self, n: NodeId) {
        if self.queued.insert(n) {
            self.queue.push_back(n);
        }
    }

    /// Pops the next node, removing it from the pending set.
    pub(crate) fn pop(&mut self) -> Option<NodeId> {
        let n = self.queue.pop_front()?;
        self.queued.remove(&n);
        Some(n)
    }
}

/// Detaches the inputs of every node not reachable from the function entry.
///
/// Unreachable nodes can only be consumed by other unreachable nodes, so
/// severing their inputs is always safe. Cleans up dead-block residue and
/// orphaned address-arithmetic chains left behind by passes that rewrite
/// reachable consumers (e.g. `DeadBranchElimination`, `FunctionArgDetect`).
pub(crate) fn detach_unreachable_nodes(fg: &mut BuiltFunctionGraph) -> OptimizationResult {
    let reachable: FxHashSet<NodeId> = fg.preorder().collect();
    let mut changed = false;
    for node_id in fg.all_node_ids().collect::<Vec<_>>() {
        if !reachable.contains(&node_id) && !fg.graph.node_inputs(node_id).is_empty() {
            fg.graph.detach_node_inputs(node_id);
            changed = true;
        }
    }
    if changed {
        OptimizationResult::Changed
    } else {
        OptimizationResult::NoChange
    }
}
