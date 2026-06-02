//! Edge-kind discriminator for node outputs.

use anyhow::anyhow;

use super::value_type::ValueType;

/// The kind of data carried by a node output edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    /// A concrete value output with an associated [`ValueType`].
    Typed(ValueType),
    /// Control-flow token.  Every region consumes one control edge per
    /// predecessor and every branch node produces one per successor.
    Control,
    /// Synchronisation edge produced by `Region` and consumed by
    /// every `VarPhi`/`MemPhi` in the same join.
    /// Carries no data — it says "fire your phi for this region."
    PhiToken,
    /// Memory token tracking the current state of memory through the graph.
    Memory,
}

impl ValueKind {
    /// Returns `true` if this is a value output (`Typed` variant).
    #[inline]
    #[must_use]
    pub fn is_value(self) -> bool {
        matches!(self, Self::Typed(..))
    }

    /// Returns the inner [`ValueType`] if this is a value output,
    /// otherwise `None`.
    #[inline]
    #[must_use]
    pub fn as_value(self) -> Option<ValueType> {
        match self {
            Self::Typed(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the value type, or an error whose payload is `self` if this
    /// kind is not a value edge.
    ///
    /// # Errors
    ///
    /// Returns an error when `self` is `Control`, `Memory`, or `PhiToken`.
    pub fn as_value_or_err(self) -> crate::Result<ValueType> {
        self.as_value()
            .ok_or_else(|| anyhow!("expected value output, got {self:?}"))
    }

    /// Returns the value type, asserting it is integer.
    ///
    /// # Errors
    ///
    /// Returns an error when `self` is not a value edge, or when the value is
    /// a float (`F32`/`F64`/`F80`).  Booleans are the integer `I1`, so an
    /// `I1` value is accepted.
    pub fn as_integer_or_err(self) -> crate::Result<ValueType> {
        let ty = self.as_value_or_err()?;
        if ty.is_integer() {
            Ok(ty)
        } else {
            Err(anyhow!("type {ty:?} is not an integer type"))
        }
    }

    /// Returns `true` if this is a control-flow edge.
    #[inline]
    #[must_use]
    pub fn is_control(self) -> bool {
        self == Self::Control
    }

    /// Returns `true` if this is a phi-token dispatch edge.
    #[inline]
    #[must_use]
    pub(crate) fn is_phi_token(self) -> bool {
        self == Self::PhiToken
    }

    /// Returns `true` if this is a memory edge.
    #[inline]
    #[must_use]
    pub fn is_memory(self) -> bool {
        matches!(self, Self::Memory)
    }

    /// Returns `true` if this is a value output carrying a `Bool` type.
    #[inline]
    #[must_use]
    pub fn is_bool(self) -> bool {
        self.as_value().is_some_and(ValueType::is_bool)
    }

    /// Returns `true` if this is a value output carrying an integer type.
    #[inline]
    #[must_use]
    pub fn is_integer(self) -> bool {
        self.as_value().is_some_and(ValueType::is_integer)
    }
}
