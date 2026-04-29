//! Constant-output inspection and creation helpers.
//!
//! Canonical definitions live on [`crate::graph::Graph`] so opt passes
//! that take `&mut Graph` (F2 trait refactor) can use them directly.
//! [`BuiltFunctionGraph`] retains thin wrappers for back-compat with
//! existing call sites.

use crate::Result;
use crate::function::BuiltFunctionGraph;
use crate::graph::Graph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

impl Graph {
    /// Returns the integer constant value of `out` (masked to its declared
    /// type), or `None` if the output is not an integer constant.
    #[must_use]
    pub fn int_const_val(&self, out: NodeOutputId) -> Option<u64> {
        let ty = self.output_kind(out).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        match *self.kind_of_output(out) {
            // IntConst stores u128; narrow to u64 for callers that only need <=64-bit values.
            NodeKind::IntConst(v) => ty.get_unsigned_int_u128(v).and_then(|w| u64::try_from(w).ok()),
            _ => None,
        }
    }

    /// Returns the boolean constant value of `out`, or `None` if it is not a
    /// `BoolConst` node.
    #[must_use]
    pub fn bool_const_val(&self, out: NodeOutputId) -> Option<bool> {
        if !self.output_kind(out).is_bool() {
            return None;
        }
        match *self.kind_of_output(out) {
            NodeKind::BoolConst(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the raw bits of a float constant, or `None` if the output is
    /// not a `FloatConst` node.
    #[must_use]
    pub fn float_const_val(&self, out: NodeOutputId) -> Option<u64> {
        let ty = self.output_kind(out).as_value()?;
        if !ty.is_float() {
            return None;
        }
        match *self.kind_of_output(out) {
            NodeKind::FloatConst(bits) => Some(bits),
            _ => None,
        }
    }

    /// Creates (or retrieves from the dedup cache) an `IntConst(val)` node of
    /// type `ty` and returns its single output.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorKind::WrongOutputCount`] if the freshly-created
    /// node does not have exactly one output (would indicate a graph bug).
    pub fn make_int_const(&mut self, val: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        let node = self.create_node(
            NodeKind::IntConst(u128::from(val)),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.node_outputs_exact::<1>(node)?[0])
    }

    /// Creates (or retrieves) a `BoolConst(val)` node.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorKind::WrongOutputCount`] if the freshly-created
    /// node does not have exactly one output (would indicate a graph bug).
    pub fn make_bool_const(&mut self, val: bool) -> Result<NodeOutputId> {
        let node = self.create_node(
            NodeKind::BoolConst(val),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::Bool)],
        );
        Ok(self.node_outputs_exact::<1>(node)?[0])
    }

    /// Creates (or retrieves) a `FloatConst(bits)` node of float type `ty`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorKind::WrongOutputCount`] if the freshly-created
    /// node does not have exactly one output (would indicate a graph bug).
    pub fn make_float_const(&mut self, bits: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        let node = self.create_node(
            NodeKind::FloatConst(bits),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.node_outputs_exact::<1>(node)?[0])
    }
}

impl BuiltFunctionGraph {
    /// Back-compat wrapper around [`Graph::int_const_val`].
    #[must_use]
    pub fn int_const_val(&self, out: NodeOutputId) -> Option<u64> {
        self.graph.int_const_val(out)
    }

    /// Back-compat wrapper around [`Graph::bool_const_val`].
    #[must_use]
    pub fn bool_const_val(&self, out: NodeOutputId) -> Option<bool> {
        self.graph.bool_const_val(out)
    }

    /// Back-compat wrapper around [`Graph::float_const_val`].
    #[must_use]
    pub fn float_const_val(&self, out: NodeOutputId) -> Option<u64> {
        self.graph.float_const_val(out)
    }

    /// Back-compat wrapper around [`Graph::make_int_const`].
    ///
    /// # Errors
    ///
    /// Propagates [`Graph::make_int_const`].
    pub fn make_int_const(&mut self, val: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        self.graph.make_int_const(val, ty)
    }

    /// Back-compat wrapper around [`Graph::make_bool_const`].
    ///
    /// # Errors
    ///
    /// Propagates [`Graph::make_bool_const`].
    pub fn make_bool_const(&mut self, val: bool) -> Result<NodeOutputId> {
        self.graph.make_bool_const(val)
    }

    /// Back-compat wrapper around [`Graph::make_float_const`].
    ///
    /// # Errors
    ///
    /// Propagates [`Graph::make_float_const`].
    pub fn make_float_const(&mut self, bits: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        self.graph.make_float_const(bits, ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FunctionBuilder;

    fn empty_built() -> BuiltFunctionGraph {
        FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .unwrap()
            .build()
            .unwrap()
    }

    #[test]
    fn make_and_read_int_const() {
        let mut fg = empty_built();
        let out = fg.make_int_const(0x1234, NodeOutputType::U32).unwrap();
        assert_eq!(fg.int_const_val(out), Some(0x1234));
        assert_eq!(fg.bool_const_val(out), None);
        assert_eq!(fg.float_const_val(out), None);
    }

    #[test]
    fn make_and_read_bool_const() {
        let mut fg = empty_built();
        let out = fg.make_bool_const(true).unwrap();
        assert_eq!(fg.bool_const_val(out), Some(true));
        assert_eq!(fg.int_const_val(out), None);
    }

    #[test]
    fn make_and_read_float_const() {
        let mut fg = empty_built();
        let bits = 0x4049_0fdbu64; // pi as f32 bits
        let out = fg.make_float_const(bits, NodeOutputType::F32).unwrap();
        assert_eq!(fg.float_const_val(out), Some(bits));
    }
}
