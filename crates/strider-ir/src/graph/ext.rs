//! [`IrGraphExt`] — the strider-specific accessor + control-walk surface on
//! top of the generic [`Graph`].
//!
//! Two families of method live here rather than on the generic graph:
//!
//! 1. **Semantic accessors** that branch on the IR's `ValueKind` / `NodeKind`
//!    payloads (the pure-structural exact/at/`kind_of_value` accessors now live
//!    as inherent methods on the generic [`Graph`]):
//!    `memory_output_of` (selects the lone `ValueKind::Memory` output) and
//!    `reachable_kind_iter`.
//! 2. **Control-aware walks** (`walk_from`, `reverse_postorder`,
//!    `retain_reachable`) that branch on [`ValueKind::is_control`] and so
//!    cannot live in the payload-agnostic generic crate. `retain_reachable`
//!    additionally seeds the generic structural compaction with the IR's
//!    control-reachable node set so forward-control-only-reachable nodes (e.g.
//!    a `Region` with no live data inputs) survive.

use anyhow::anyhow;

use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

use super::{Graph, NodeIdRemap};

/// The strider-specific accessor / walk surface on the IR [`Graph`].
///
/// Implemented only for [`Graph`]; bring it into scope (`use
/// crate::graph::IrGraphExt;`) to call these on a graph value.
pub trait IrGraphExt {
    /// Returns the single [`ValueId`] of `node_id` whose kind is
    /// [`ValueKind::Memory`].
    ///
    /// # Errors
    ///
    /// Returns an error if `node_id` has no `Memory` output, or has more than
    /// one.
    fn memory_output_of(&self, node_id: NodeId) -> crate::error::Result<ValueId>;

    /// Yields `(NodeId, &NodeKind)` for every node in the arena whose id is in
    /// `reachable`, in ascending-`NodeId` order.
    fn reachable_kind_iter<'a>(
        &'a self,
        reachable: &'a crate::walk::NodeIdSet,
    ) -> impl Iterator<Item = (NodeId, &'a NodeKind)> + 'a;

    /// Returns a pre-order walk over every node reachable from `entry`
    /// (control-out forward + data-in backward).
    fn walk_from(&self, entry: NodeId) -> crate::walk::GraphWalk<'_>;

    /// Real reverse-post-order of every node reachable from `seed`.
    fn reverse_postorder(&self, seed: NodeId) -> Vec<NodeId>;

    /// Compacts the arena down to the nodes reachable from `entry` via
    /// [`Self::walk_from`] (control-out forward + data-in backward), returning
    /// the old→new id translation table.
    ///
    /// Pre-compaction `NodeId` / `ValueId` / `UseId` values are invalidated;
    /// callers holding any such id MUST rewrite it through the returned
    /// [`NodeIdRemap`].
    ///
    /// # Errors
    ///
    /// Currently infallible in practice; the `Result` is kept so a future
    /// invariant check has a typed channel and Python callers see a clean
    /// exception rather than a panic.
    fn retain_reachable(&mut self, entry: NodeId) -> crate::Result<NodeIdRemap>;
}

impl IrGraphExt for Graph {
    fn memory_output_of(&self, node_id: NodeId) -> crate::error::Result<ValueId> {
        let mut found: Option<ValueId> = None;
        for &out in self.node_outputs(node_id) {
            if matches!(self.value_kind(out), ValueKind::Memory) {
                if found.is_some() {
                    return Err(anyhow!("node {node_id:?} has more than one Memory output"));
                }
                found = Some(out);
            }
        }
        found.ok_or_else(|| anyhow!("node {node_id:?} has no Memory output"))
    }

    fn reachable_kind_iter<'a>(
        &'a self,
        reachable: &'a crate::walk::NodeIdSet,
    ) -> impl Iterator<Item = (NodeId, &'a NodeKind)> + 'a {
        // Iterate the reachable set directly (ascending NodeId order, sized to
        // the reachable set, not the zombie-bloated arena).
        reachable.iter().map(move |n| (n, self.node_kind(n)))
    }

    fn walk_from(&self, entry: NodeId) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(self, entry)
    }

    fn reverse_postorder(&self, seed: NodeId) -> Vec<NodeId> {
        crate::walk::GraphWalkInfo::compute_full(self, seed).reverse_postorder(self)
    }

    fn retain_reachable(&mut self, entry: NodeId) -> crate::Result<NodeIdRemap> {
        // The generic `retain_reachable` keeps the backward-input closure of
        // its `roots`. The IR's reachability also follows forward-control
        // edges (so a `Region` reached only via control survives). Seed the
        // generic compaction with the FULL control-aware reachable set: that
        // set is already closed under data inputs, so its backward-input
        // closure is itself — the generic pass then retains exactly the IR
        // reachable set, and its cacher rebuild re-keys the dedup cache over
        // the survivors.
        let reachable: Vec<NodeId> = self.walk_from(entry).collect();
        Ok(self.retain_reachable_roots(reachable))
    }
}
