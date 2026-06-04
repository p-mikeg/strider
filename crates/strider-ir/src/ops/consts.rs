//! Constant-output inspection helpers defined on
//! [`crate::graph::Graph`].

use crate::graph::Graph;
use crate::node::{NodeKind, ValueId};

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
}
