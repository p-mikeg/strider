//! Edge-kind discriminator for node outputs.

use anyhow::anyhow;

use super::output_type::NodeOutputType;

/// The kind of data carried by a node output edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeOutputKind {
    /// A concrete value output with an associated [`NodeOutputType`].
    OutputType(NodeOutputType),
    /// Control-flow token.  Every region consumes one control edge per
    /// predecessor and every branch node produces one per successor.
    Control,
    /// Phi-dispatch token produced by `ControlState` nodes and consumed by
    /// `ControlPhi` nodes.  Carries no data — it is a synchronisation edge
    /// that links each phi to exactly one `ControlState`.
    ControlPhi,
    /// Memory token tracking the current state of memory through the graph.
    Memory,
}

impl NodeOutputKind {
    /// Returns `true` if this is a value output (`OutputType` variant).
    #[inline]
    #[must_use]
    pub fn is_value(self) -> bool {
        matches!(self, Self::OutputType(..))
    }

    /// Returns the inner [`NodeOutputType`] if this is a value output,
    /// otherwise `None`.
    #[inline]
    #[must_use]
    pub fn as_value(self) -> Option<NodeOutputType> {
        match self {
            Self::OutputType(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the value type, or an error whose payload is `self` if this
    /// kind is not a value edge.
    ///
    /// # Errors
    ///
    /// Returns an error when `self` is `Control`, `Memory`, or `ControlPhi`.
    pub fn as_value_or_err(self) -> crate::Result<NodeOutputType> {
        self.as_value()
            .ok_or_else(|| anyhow!("expected value output, got {self:?}"))
    }

    /// Returns the value type, asserting it is integer.
    ///
    /// # Errors
    ///
    /// Returns an error when `self` is not a value edge, or when the value
    /// is `Bool`, `F32`, or `F64`.
    pub fn as_integer_or_err(self) -> crate::Result<NodeOutputType> {
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

    /// Returns `true` if this is a control-phi dispatch edge.
    #[inline]
    #[must_use]
    pub(crate) fn is_control_phi(self) -> bool {
        self == Self::ControlPhi
    }

    /// Returns `true` if this is a memory edge.
    #[inline]
    #[must_use]
    pub fn is_memory(self) -> bool {
        self == Self::Memory
    }

    /// Returns `true` if this is a value output carrying a `Bool` type.
    #[inline]
    #[must_use]
    pub fn is_bool(self) -> bool {
        if let Some(output_type) = self.as_value() {
            output_type.is_bool()
        } else {
            false
        }
    }

    /// Returns `true` if this is a value output carrying an integer type.
    #[inline]
    #[must_use]
    pub fn is_integer(self) -> bool {
        if let Some(output_type) = self.as_value() {
            output_type.is_integer()
        } else {
            false
        }
    }
}
