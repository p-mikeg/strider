//! Read-only typed accessors — the local-typing lookup surface.
//!
//! Returning typed slices and iterators over the per-node `inputs` /
//! `outputs` lists kept inside `Graph::nodes`, plus a few cheap lookups
//! (`output_definition`, `get_node_from_output`). The exact-arity helpers
//! return `Result<[…; N]>` rather than panicking so callers in production
//! code don't have to defend against shape errors with `unwrap`.

use anyhow::anyhow;

use crate::iterators::Inputs;
use crate::node::{NodeId, NodeInputId, NodeKind, NodeOutputId, NodeOutputKind};

use super::Graph;

impl Graph {
    /// Returns the [`NodeOutputKind`] of `output_id`.
    #[inline]
    #[must_use]
    pub fn output_kind(&self, output_id: NodeOutputId) -> NodeOutputKind {
        self.outputs[output_id].kind
    }

    /// Returns the `(NodeId, output_index)` pair that defines `output_id`.
    #[inline]
    #[must_use]
    pub fn output_definition(&self, output_id: NodeOutputId) -> (NodeId, u32) {
        let data = &self.outputs[output_id];
        (data.source_id, data.output_index)
    }

    /// Returns the slice of output ids for `node_id`.
    #[inline]
    #[must_use]
    pub fn node_outputs(&self, node_id: NodeId) -> &[NodeOutputId] {
        self.nodes[node_id].outputs.as_slice(&self.output_pool)
    }

    /// Returns exactly `N` output ids for `node_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the node does not have exactly `N` outputs.
    #[inline]
    pub fn node_outputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[NodeOutputId; N]> {
        let outputs = self.node_outputs(node_id);
        if outputs.len() != N {
            let actual = outputs.len();
            return Err(anyhow!(
                "node {node_id:?} does not have exactly {N} outputs (has {actual})"
            ));
        }
        let mut result = [NodeOutputId::default(); N];
        for (i, &v) in outputs.iter().enumerate() {
            result[i] = v;
        }
        Ok(result)
    }

    /// Returns an iterator over the values consumed by `node_id`'s inputs.
    #[inline]
    #[must_use]
    pub fn node_inputs(&self, node_id: NodeId) -> Inputs<'_> {
        Inputs {
            graph: self,
            use_list: self.nodes[node_id].inputs.as_slice(&self.input_pool),
        }
    }

    /// Returns exactly `N` input values for `node_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the node does not have exactly `N` inputs.
    #[inline]
    pub fn node_inputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[NodeOutputId; N]> {
        let inputs = self.node_inputs(node_id);
        if inputs.len() != N {
            let actual = inputs.len();
            return Err(anyhow!(
                "node {node_id:?} does not have exactly {N} inputs (has {actual})"
            ));
        }
        let mut result = [NodeOutputId::default(); N];
        for (i, v) in inputs.into_iter().enumerate() {
            result[i] = v;
        }
        Ok(result)
    }

    /// Returns the [`NodeId`] that produces `output_id`.
    #[inline]
    #[must_use]
    pub fn get_node_from_output(&self, output_id: NodeOutputId) -> NodeId {
        self.outputs[output_id].source_id
    }

    /// Returns the [`NodeId`] that the next [`Self::create_node`] (cache
    /// miss) would assign.
    ///
    /// Snapshot before a rewrite RHS build to identify which `NodeId`s
    /// are freshly created during the build versus pre-existing — every
    /// `id >= snapshot` is fresh, every `id < snapshot` was already in
    /// the arena.  Used by `pattern::rewrite_rule` to walk the freshly
    /// built RHS subtree and propagate asm-fingerprint contributors
    /// into every interior new node, not just the outermost root.
    #[inline]
    #[must_use]
    pub fn next_node_id(&self) -> NodeId {
        self.nodes.next_key()
    }

    /// Returns the single [`NodeOutputId`] of `node_id` whose kind is
    /// [`NodeOutputKind::Memory`].
    ///
    /// Replaces the magic-index pattern `node_outputs(node)[1]` at sites
    /// that pull the memory token out of a `CallOther` (modeled) or
    /// `Load` node.  Failing kind-aware lookup over a brittle positional
    /// access also catches downstream signature drift — if a future
    /// `node_signature` change reorders outputs, this returns
    /// `MissingMemoryOutput` instead of silently returning the
    /// (now-wrong) Control or Value slot.
    ///
    /// # Errors
    ///
    /// Returns an error if `node_id` has no `Memory` output, or has
    /// more than one (no current node kind does, but the explicit check
    /// keeps the contract auditable).
    #[inline]
    pub fn memory_output_of(&self, node_id: NodeId) -> crate::error::Result<NodeOutputId> {
        let mut found: Option<NodeOutputId> = None;
        for &out in self.node_outputs(node_id) {
            if matches!(self.output_kind(out), NodeOutputKind::Memory) {
                if found.is_some() {
                    return Err(anyhow!(
                        "node {node_id:?} has more than one Memory output"
                    ));
                }
                found = Some(out);
            }
        }
        found.ok_or_else(|| anyhow!("node {node_id:?} has no Memory output"))
    }

    /// Returns the [`NodeKind`] of the node that produces `output_id`.
    ///
    /// Shorthand for `node_kind(get_node_from_output(output_id))` — the
    /// most common two-step lookup in pattern-matching and validation
    /// code paths.
    #[inline]
    #[must_use]
    pub fn kind_of_output(&self, output_id: NodeOutputId) -> &NodeKind {
        &self.nodes[self.outputs[output_id].source_id].kind
    }

    /// Returns the [`NodeOutputId`] driving the `idx`-th input slot of `node`,
    /// or `None` if `idx` is past the node's input count.
    ///
    /// O(1) alternative to
    /// `node_inputs(node).into_iter().collect::<Vec<_>>().into_iter().nth(idx)`
    /// — avoids the intermediate allocation that several call sites
    /// performed just to grab slot 0 (or N).
    #[inline]
    #[must_use]
    pub fn nth_input(&self, node: NodeId, idx: usize) -> Option<NodeOutputId> {
        let slice = self.nodes[node].inputs.as_slice(&self.input_pool);
        let input_id = *slice.get(idx)?;
        Some(self.inputs[input_id].output_id)
    }

    /// Returns the [`NodeInputId`] of the input slot at position `idx` of `node`.
    ///
    /// # Errors
    ///
    /// Returns an error if `idx` is past the node's current input count.
    #[inline]
    pub fn node_input_id_at(
        &self,
        node: NodeId,
        idx: usize,
    ) -> crate::error::Result<NodeInputId> {
        let slice = self.nodes[node].inputs.as_slice(&self.input_pool);
        let len = slice.len();
        slice.get(idx).copied().ok_or_else(|| {
            anyhow!("input index {idx} out of bounds for node {node:?} (len={len})")
        })
    }

    /// Returns the [`NodeOutputId`] that `input` currently references.
    #[inline]
    #[must_use]
    pub fn input_output_id(&self, input: NodeInputId) -> NodeOutputId {
        self.inputs[input].output_id
    }

    /// Yields `(NodeId, &NodeKind)` for every node in the arena whose id
    /// is in `reachable`.  The single source of truth for the
    /// reachability-scoped iteration policy shared by the validator's
    /// per-node graph-invariants checks.
    ///
    /// Callers that need to detect detached zombies (e.g. the
    /// `Entry`/`InitialMemory` uniqueness check) must iterate the
    /// arena directly via `Graph::all_node_ids` instead — this helper
    /// deliberately excludes them.
    pub fn reachable_kind_iter<'a>(
        &'a self,
        reachable: &'a crate::walk::NodeIdSet,
    ) -> impl Iterator<Item = (NodeId, &'a NodeKind)> + 'a {
        self.nodes
            .keys()
            .filter(move |&n| reachable.contains(n))
            .map(move |n| (n, self.node_kind(n)))
    }
}
