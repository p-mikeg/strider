//! Constant-output inspection and creation helpers defined on
//! [`crate::graph::Graph`].

use anyhow::anyhow;

use crate::Result;
use crate::graph::Graph;
use crate::node::{NodeKind, ValueId, ValueKind, ValueType};

impl Graph {
    /// Returns the integer constant value of `value` (masked to its declared
    /// type) narrowed to `u64`, or `None` if the output is not an integer
    /// constant or its value does not fit in `u64`.
    pub fn int_const_val(&self, value: ValueId) -> Option<u64> {
        let ty = self.value_kind(value).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        match *self.kind_of_value(value) {
            NodeKind::IntConst(v) => ty.get_unsigned_int(v).and_then(|w| u64::try_from(w).ok()),
            _ => None,
        }
    }

    /// Returns the boolean constant value of `value`, or `None` if it is not an
    /// `I1`-typed `IntConst` node.  Booleans are 1-bit integers, so `true` is
    /// `IntConst(1):I1` and `false` is `IntConst(0):I1`.
    pub fn bool_const_val(&self, value: ValueId) -> Option<bool> {
        if !self.value_kind(value).is_bool() {
            return None;
        }
        match *self.kind_of_value(value) {
            NodeKind::IntConst(v) => Some(v != 0),
            _ => None,
        }
    }

    /// Creates (or retrieves) a `FloatConst(bits)` node of float type `ty`.
    ///
    /// # Dedup precondition (zero high bits)
    ///
    /// Unlike [`crate::IRBuilderExt::build_int_const`] — which masks `val` to
    /// the type's bit width so semantically-equal constants share one
    /// dedup-cache entry —
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
    pub fn make_float_const(&mut self, bits: u64, ty: ValueType) -> Result<ValueId> {
        if !ty.is_float() {
            return Err(anyhow!(
                "make_float_const called with non-float type {ty:?}"
            ));
        }
        let node = self.create_node(
            NodeKind::FloatConst(bits),
            [],
            [ValueKind::Typed(ty)],
        );
        Ok(self.node_outputs_exact::<1>(node)?[0])
    }
}
