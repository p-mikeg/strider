//! Shared per-pass worklist used by every fold-style optimizer.
//!
//! Replaces the `for node_id in preorder().collect()` pattern with a
//! drain-and-re-enqueue loop: passes seed the worklist with the preorder
//! traversal, then on every successful rewrite call back to push consumers
//! of the rewritten outputs so cascading folds reach fixed point in a single
//! `optimize` invocation.

use std::collections::VecDeque;

use rustc_hash::FxHashSet;

use ir::node::NodeId;

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
