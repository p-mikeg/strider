//! Constant-output inspection and creation helpers on
//! [`crate::function::BuiltFunctionGraph`].

use crate::Result;
use crate::function::BuiltFunctionGraph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

impl BuiltFunctionGraph {
    /// Returns the integer constant value of `out` (masked to its declared
    /// type), or `None` if the output is not an integer constant.
    pub fn int_const_val(&self, out: NodeOutputId) -> Option<u64> {
        let ty = self.graph.output_kind(out).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        let node = self.graph.get_node_from_output(out);
        match *self.graph.node_kind(node) {
            NodeKind::IntConst(v) => ty.get_unsigned_int(v),
            _ => None,
        }
    }

    /// Returns the boolean constant value of `out`, or `None` if it is not a
    /// `BoolConst` node.
    pub fn bool_const_val(&self, out: NodeOutputId) -> Option<bool> {
        if !self.graph.output_kind(out).is_bool() {
            return None;
        }
        let node = self.graph.get_node_from_output(out);
        match *self.graph.node_kind(node) {
            NodeKind::BoolConst(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the raw bits of a float constant, or `None` if the output is
    /// not a `FloatConst` node.
    pub fn float_const_val(&self, out: NodeOutputId) -> Option<u64> {
        let ty = self.graph.output_kind(out).as_value()?;
        if !ty.is_float() {
            return None;
        }
        let node = self.graph.get_node_from_output(out);
        match *self.graph.node_kind(node) {
            NodeKind::FloatConst(bits) => Some(bits),
            _ => None,
        }
    }

    /// Creates (or retrieves from the dedup cache) an `IntConst(val)` node of
    /// type `ty` and returns its single output.
    pub fn make_int_const(&mut self, val: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        let node = self.graph.create_node(
            NodeKind::IntConst(val),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.graph.node_outputs_exact::<1>(node)?[0])
    }

    /// Creates (or retrieves) a `BoolConst(val)` node.
    pub fn make_bool_const(&mut self, val: bool) -> Result<NodeOutputId> {
        let node = self.graph.create_node(
            NodeKind::BoolConst(val),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::Bool)],
        );
        Ok(self.graph.node_outputs_exact::<1>(node)?[0])
    }

    /// Creates (or retrieves) a `FloatConst(bits)` node of float type `ty`.
    pub fn make_float_const(&mut self, bits: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        let node = self.graph.create_node(
            NodeKind::FloatConst(bits),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.graph.node_outputs_exact::<1>(node)?[0])
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
