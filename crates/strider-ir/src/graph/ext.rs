//! [`IrGraphExt`] — the strider-specific accessor + control-walk surface on
//! top of the generic [`Graph`].
//!
//! Two families of method live here rather than on the generic graph:
//!
//! 1. **Typed / fallible accessors** whose IR signature returns
//!    `crate::error::Result<…>` (or a `&NodeKind`) — the generic graph's
//!    structural counterparts are payload-agnostic and return `Option`, so the
//!    IR keeps its richer error-carrying shapes here:
//!    `node_outputs_exact`, `node_inputs_exact`, `node_input_id_at`,
//!    `memory_output_of`, `kind_of_value`, `reachable_kind_iter`.
//! 2. **Control-aware walks** (`walk_from`, `reverse_postorder`,
//!    `retain_reachable`) that branch on [`ValueKind::is_control`] and so
//!    cannot live in the payload-agnostic generic crate. `retain_reachable`
//!    additionally seeds the generic structural compaction with the IR's
//!    control-reachable node set so forward-control-only-reachable nodes (e.g.
//!    a `Region` with no live data inputs) survive.

use anyhow::anyhow;

use crate::node::{NodeId, NodeKind, UseId, ValueId, ValueKind};

use super::{Graph, NodeIdRemap};

/// The strider-specific accessor / walk surface on the IR [`Graph`].
///
/// Implemented only for [`Graph`]; bring it into scope (`use
/// crate::graph::IrGraphExt;`) to call these on a graph value.
pub trait IrGraphExt {
    /// Returns exactly `N` output ids for `node_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the node does not have exactly `N` outputs.
    fn node_outputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[ValueId; N]>;

    /// Returns exactly `N` input values for `node_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the node does not have exactly `N` inputs.
    fn node_inputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[ValueId; N]>;

    /// Returns the [`UseId`] of the input slot at position `idx` of `node`.
    ///
    /// # Errors
    ///
    /// Returns an error if `idx` is past the node's current input count.
    fn node_input_id_at(&self, node: NodeId, idx: usize) -> crate::error::Result<UseId>;

    /// Returns the single [`ValueId`] of `node_id` whose kind is
    /// [`ValueKind::Memory`].
    ///
    /// # Errors
    ///
    /// Returns an error if `node_id` has no `Memory` output, or has more than
    /// one.
    fn memory_output_of(&self, node_id: NodeId) -> crate::error::Result<ValueId>;

    /// Returns the [`NodeKind`] of the node that produces `value_id`.
    fn kind_of_value(&self, value_id: ValueId) -> &NodeKind;

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
    fn node_outputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[ValueId; N]> {
        let outputs = self.node_outputs(node_id);
        if outputs.len() != N {
            let actual = outputs.len();
            return Err(anyhow!(
                "node {node_id:?} does not have exactly {N} outputs (has {actual})"
            ));
        }
        let mut result = [ValueId::default(); N];
        result.copy_from_slice(outputs);
        Ok(result)
    }

    fn node_inputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[ValueId; N]> {
        let inputs = self.node_inputs(node_id);
        if inputs.len() != N {
            let actual = inputs.len();
            return Err(anyhow!(
                "node {node_id:?} does not have exactly {N} inputs (has {actual})"
            ));
        }
        let mut result = [ValueId::default(); N];
        for (i, v) in inputs.into_iter().enumerate() {
            result[i] = v;
        }
        Ok(result)
    }

    fn node_input_id_at(&self, node: NodeId, idx: usize) -> crate::error::Result<UseId> {
        self.node_input_id_at_opt(node, idx).ok_or_else(|| {
            let len = self.node_inputs(node).len();
            anyhow!("input index {idx} out of bounds for node {node:?} (len={len})")
        })
    }

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

    fn kind_of_value(&self, value_id: ValueId) -> &NodeKind {
        self.node_kind(self.producer(value_id))
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
