//! Ergonomic node-construction helpers — defined on
//! [`crate::graph::Graph`] so opt passes that take `&mut Graph` (F2
//! trait refactor) can use them directly. [`BuiltFunctionGraph`]
//! retains thin wrappers for back-compat.

use crate::Result;
use crate::function::BuiltFunctionGraph;
use crate::graph::Graph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

impl Graph {
    /// Creates (or retrieves from the dedup cache) a node with a single value
    /// output of `ty` and returns the output id directly.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorKind::WrongOutputCount`] if the freshly-created
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

impl BuiltFunctionGraph {
    /// Back-compat wrapper around [`Graph::make_value_node`].
    ///
    /// ```rust
    /// use ir::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
    /// use ir::FunctionBuilder;
    ///
    /// let mut fb = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    /// let region = fb.create_region().unwrap();
    /// fb.set_entry_region(region).unwrap();
    /// fb.set_region(region);
    /// fb.build_return(None, &[]).unwrap();
    /// let mut built = fb.build().unwrap();
    /// let g = &mut built.graph;
    ///
    /// let kind = NodeKind::IntConst(42);
    /// let inputs: [NodeOutputId; 0] = [];
    /// let ty = NodeOutputType::U64;
    /// let n = g.create_node(kind, inputs, [NodeOutputKind::OutputType(ty)]);
    /// let [out] = g.node_outputs_exact::<1>(n)?;
    /// # let _ = out;
    /// # Ok::<(), ir::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Propagates [`Graph::make_value_node`].
    pub fn make_value_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.graph.make_value_node(kind, inputs, ty)
    }

    /// Back-compat wrapper around [`Graph::make_int_bits_to_float_node`].
    ///
    /// # Errors
    ///
    /// Propagates [`Graph::make_int_bits_to_float_node`].
    pub fn make_int_bits_to_float_node(
        &mut self,
        input: NodeOutputId,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.graph.make_int_bits_to_float_node(input, ty)
    }

    /// Back-compat wrapper around [`Graph::make_float_to_float_node`].
    ///
    /// # Errors
    ///
    /// Propagates [`Graph::make_float_to_float_node`].
    pub fn make_float_to_float_node(
        &mut self,
        input: NodeOutputId,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        self.graph.make_float_to_float_node(input, ty)
    }
}
