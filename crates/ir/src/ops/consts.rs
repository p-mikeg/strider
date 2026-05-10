//! Constant-output inspection and creation helpers defined on
//! [`crate::graph::Graph`].

use anyhow::anyhow;

use crate::Result;
use crate::graph::Graph;
use crate::node::{NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

impl Graph {
    /// Returns the integer constant value of `out` (masked to its declared
    /// type) narrowed to `u64`, or `None` if the output is not an integer
    /// constant or its value does not fit in `u64`.
    #[must_use]
    pub fn int_const_val(&self, out: NodeOutputId) -> Option<u64> {
        let ty = self.output_kind(out).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        match *self.kind_of_output(out) {
            NodeKind::IntConst(v) => ty.get_unsigned_int(v).and_then(|w| u64::try_from(w).ok()),
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
    /// Single source of truth for constructing primitive integer constants:
    /// [`crate::FunctionBuilder::build_int_const`] delegates here and adds
    /// asm-fingerprint plumbing on top.
    ///
    /// # Errors
    ///
    /// Returns an error when `ty` is not an integer type, or is `U256` /
    /// `U512` (neither is representable in the `u128` storage that
    /// `IntConst` uses — wide constants must go through
    /// [`crate::FunctionBuilder::build_int_const_wide`]), or when the
    /// freshly-created node does not have exactly one output.
    pub fn make_int_const(
        &mut self,
        val: impl Into<u128>,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if !ty.is_integer() {
            return Err(anyhow!(
                "make_int_const called with non-integer type {ty:?}"
            ));
        }
        if matches!(ty, NodeOutputType::U256 | NodeOutputType::U512) {
            return Err(anyhow!(
                "make_int_const({ty:?}) not supported - IntConst storage is u128; \
                 use build_int_const_wide for U256/U512"
            ));
        }
        // Mask `val` to the declared output type's bit width so the
        // dedup-cache key sees the same `IntConst(u128)` payload for
        // semantically-equal constants — `make_int_const(0x1FF, U8)`
        // and `make_int_const(0xFF, U8)` must dedup to the same node.
        let masked = val.into() & ty.bit_mask_u128();
        let node = self.create_node(
            NodeKind::IntConst(masked),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.node_outputs_exact::<1>(node)?[0])
    }

    /// Creates (or retrieves) a `BoolConst(val)` node.
    ///
    /// # Errors
    ///
    /// Returns an error when the freshly-created node does not have exactly
    /// one output.
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
    /// Returns an error when `ty` is not a float type, or when the
    /// freshly-created node does not have exactly one output.
    pub fn make_float_const(&mut self, bits: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
        if !ty.is_float() {
            return Err(anyhow!(
                "make_float_const called with non-float type {ty:?}"
            ));
        }
        let node = self.create_node(
            NodeKind::FloatConst(bits),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.node_outputs_exact::<1>(node)?[0])
    }
}
