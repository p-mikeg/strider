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

    /// Returns the boolean constant value of `out`, or `None` if it is not an
    /// `I1`-typed `IntConst` node.  Booleans are 1-bit integers, so `true` is
    /// `IntConst(1):I1` and `false` is `IntConst(0):I1`.
    #[must_use]
    pub fn bool_const_val(&self, out: NodeOutputId) -> Option<bool> {
        if !self.output_kind(out).is_bool() {
            return None;
        }
        match *self.kind_of_output(out) {
            NodeKind::IntConst(v) => Some(v != 0),
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
    /// Returns an error when `ty` is not an integer type, or is `I256` /
    /// `I512` (neither is representable in the `u128` storage that
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
        if matches!(ty, NodeOutputType::I256 | NodeOutputType::I512) {
            return Err(anyhow!(
                "make_int_const({ty:?}) not supported - IntConst storage is u128; \
                 use build_int_const_wide for I256/I512"
            ));
        }
        // Mask `val` to the declared output type's bit width so the
        // dedup-cache key sees the same `IntConst(u128)` payload for
        // semantically-equal constants — `make_int_const(0x1FF, I8)`
        // and `make_int_const(0xFF, I8)` must dedup to the same node.
        let masked = val.into() & ty.bit_mask_u128();
        let node = self.create_node(
            NodeKind::IntConst(masked),
            [],
            [NodeOutputKind::OutputType(ty)],
        );
        Ok(self.node_outputs_exact::<1>(node)?[0])
    }


    /// Creates (or retrieves) an integer constant whose value is the
    /// all-ones bit pattern of integer type `ty` — `(2^bit_width) - 1`.
    ///
    /// For widths ≤ 128 bits the result is an [`crate::node::NodeKind::IntConst`]
    /// node carrying `ty.bit_mask_u128()` (which is `u128::MAX` for I128 and
    /// the appropriate narrower mask for smaller widths).  For `I256` /
    /// `I512` the result is an [`crate::node::NodeKind::IntConstWide`] node
    /// pointing at a [`crate::wide_const::WideConstStorage::all_ones`] entry
    /// interned in `Graph::wide_const_interner`.
    ///
    /// Used by the pcode lifter to materialise the second operand of
    /// `Xor(x, all_ones)` — the canonical IR form of bitwise complement
    /// (`~x`) after the the former BitNot unary-op variant was deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when `ty` is not an integer type.
    pub fn make_all_ones_const(
        &mut self,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if !ty.is_integer() {
            return Err(anyhow!(
                "make_all_ones_const called with non-integer type {ty:?}"
            ));
        }
        match ty {
            NodeOutputType::I256 | NodeOutputType::I512 => {
                let storage = crate::wide_const::WideConstStorage::all_ones(ty.byte_size())
                    .ok_or_else(|| anyhow!(
                        "make_all_ones_const: WideConstStorage::all_ones rejected byte size {}",
                        ty.byte_size()
                    ))?;
                let id = self.intern_wide_const(storage);
                let node = self.create_node(
                    NodeKind::IntConstWide(id),
                    [],
                    [NodeOutputKind::OutputType(ty)],
                );
                Ok(self.node_outputs_exact::<1>(node)?[0])
            }
            _ => self.make_int_const(ty.bit_mask_u128(), ty),
        }
    }

    /// Creates (or retrieves) a `FloatConst(bits)` node of float type `ty`.
    ///
    /// # Dedup precondition (zero high bits)
    ///
    /// Unlike [`Self::make_int_const`] — which masks `val` to the type's bit
    /// width so semantically-equal constants share one dedup-cache entry —
    /// this function does **not** mask `bits` to the float width.  The
    /// dedup key is the raw `(FloatConst(bits), ty)` pair, so two `F32`
    /// constants with identical low-32 bits but differing high-32 bits would
    /// fail to dedup.  Callers must therefore zero the unused high bits for a
    /// narrow float (e.g. clear bits 32..64 for an `F32`).  In practice this
    /// holds: the bit-cast builders route an `F32` payload through an `I32`
    /// `IntConst` first, which carries zero high bits, so every `F32`
    /// `FloatConst` reaches here already zero-extended.
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
