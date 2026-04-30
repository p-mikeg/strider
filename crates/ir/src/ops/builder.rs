//! Ergonomic node-construction helpers defined on [`crate::graph::Graph`].

use crate::Result;
use crate::graph::Graph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

impl Graph {
    /// Creates (or retrieves from the dedup cache) a node with a single value
    /// output of `ty` and returns the output id directly.
    ///
    /// # Errors
    ///
    /// Returns `WrongOutputCount` if the freshly-created
    /// node does not have exactly one output (this would indicate a graph or
    /// signature-table bug, not user error).
    pub fn make_value_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let inputs: Vec<NodeOutputId> = inputs.into_iter().collect();
        let node = self.create_node(kind, inputs, [NodeOutputKind::OutputType(ty)]);
        Ok(self.node_outputs_exact::<1>(node)?[0])
    }

    /// Convenience: create an `IntBitsToFloat` node with the given result type.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::make_value_node`].
    pub fn make_int_bits_to_float_node(
        &mut self,
        input: NodeOutputId,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.make_value_node(NodeKind::IntBitsToFloat, [input], ty)
    }

    /// Convenience: create a `FloatToFloat` node with the given result type.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::make_value_node`].
    pub fn make_float_to_float_node(
        &mut self,
        input: NodeOutputId,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.make_value_node(NodeKind::FloatToFloat, [input], ty)
    }
}

