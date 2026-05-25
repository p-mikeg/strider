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
    /// Synchronisation edge produced by `Region` and consumed by
    /// every `VarPhi`/`MemPhi`/`StackStorePhi` in the same join.
    /// Carries no data — it says "fire your phi for this region."
    PhiToken,
    /// Memory token tracking the current state of memory through the graph.
    ///
    /// `None` = unified memory (the default until `AliasSplit` promotes it to a
    /// partition).  `Some(p)` = memory restricted to partition `p`.
    Memory(Option<crate::mem_partition::MemPartitionId>),
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
    /// Returns an error when `self` is `Control`, `Memory(_)`, or `PhiToken`.
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
        matches!(self, Self::Memory(_))
    }

    /// Returns the partition id if this is a partition-typed memory output.
    /// Returns `None` for unified `Memory` (or non-memory).
    #[inline]
    #[must_use]
    pub fn memory_partition(self) -> Option<crate::mem_partition::MemPartitionId> {
        if let NodeOutputKind::Memory(Some(p)) = self {
            Some(p)
        } else {
            None
        }
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

#[cfg(test)]
mod tests {
    use super::NodeOutputKind;
    use crate::mem_partition::{AliasClass, PartitionTable};

    #[test]
    fn memory_variant_carries_optional_partition() {
        // Unified memory (the default)
        let m_unified = NodeOutputKind::Memory(None);
        assert!(m_unified.is_memory());
        assert_eq!(m_unified.memory_partition(), None);

        // Partitioned memory
        let mut pt = PartitionTable::default();
        let p = pt.create(AliasClass::Stack);
        let m_part = NodeOutputKind::Memory(Some(p));
        assert!(m_part.is_memory());
        assert_eq!(m_part.memory_partition(), Some(p));

        // Non-memory variants
        assert!(!NodeOutputKind::Control.is_memory());
        assert_eq!(NodeOutputKind::Control.memory_partition(), None);
    }
}
