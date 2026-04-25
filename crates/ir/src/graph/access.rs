//! Read-only typed accessors — Layer A's lookup surface.
//!
//! Returning typed slices and iterators over the per-node `inputs` /
//! `outputs` lists kept inside `Graph::nodes`, plus a few cheap lookups
//! (`output_definition`, `get_node_from_output`). The exact-arity helpers
//! return `Result<[…; N]>` rather than panicking so callers in production
//! code don't have to defend against shape errors with `unwrap`.

use crate::iterators::{Inputs, Outputs};
use crate::node::{NodeId, NodeInputId, NodeOutputId, NodeOutputKind};

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
    pub fn node_outputs(&self, node_id: NodeId) -> Outputs<'_> {
        Outputs(self.nodes[node_id].outputs.as_slice(&self.output_pool))
    }

    /// Returns exactly `N` output ids for `node_id`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ErrorKind::WrongOutputCount`] if the node does
    /// not have exactly `N` outputs.
    #[inline]
    pub fn node_outputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[NodeOutputId; N]> {
        let outputs = self.node_outputs(node_id);
        if outputs.len() != N {
            return Err(
                crate::error::ErrorKind::WrongOutputCount(node_id, N, outputs.len()).into(),
            );
        }
        let mut result = [NodeOutputId::default(); N];
        for (i, v) in outputs.into_iter().enumerate() {
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
    /// Returns [`crate::error::ErrorKind::WrongInputCount`] if the node does
    /// not have exactly `N` inputs.
    #[inline]
    pub fn node_inputs_exact<const N: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::error::Result<[NodeOutputId; N]> {
        let inputs = self.node_inputs(node_id);
        if inputs.len() != N {
            return Err(crate::error::ErrorKind::WrongInputCount(node_id, N, inputs.len()).into());
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

    /// Returns the [`NodeInputId`] of the input slot at position `idx` of `node`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ErrorKind::InputIndexOutOfBounds`] if `idx` is
    /// past the node's current input count.
    #[inline]
    pub fn node_input_id_at(
        &self,
        node: NodeId,
        idx: usize,
    ) -> crate::error::Result<NodeInputId> {
        let slice = self.nodes[node].inputs.as_slice(&self.input_pool);
        slice.get(idx).copied().ok_or_else(|| {
            crate::error::ErrorKind::InputIndexOutOfBounds {
                node,
                index: idx,
                len: slice.len(),
            }
            .into()
        })
    }

    /// Returns the [`NodeOutputId`] that `input` currently references.
    #[inline]
    #[must_use]
    pub fn input_output_id(&self, input: NodeInputId) -> NodeOutputId {
        self.inputs[input].output_id
    }
}
